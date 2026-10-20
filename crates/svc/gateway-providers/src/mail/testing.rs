//! A real mail server for tests: `mox localserve` (mox 0.0.17, MIT), a
//! build-time test tool that is never in the image. `tools/dev/mox.sh
//! build` puts it in the main checkout's `build/tools/mox-0.0.17/`; `MOX`
//! names another binary. Tests that need it skip, saying so, when it isn't
//! there.
//!
//! Each [`Mox`] is its own server on its own loopback address, with its
//! state (and its log, which traces sign-ins) in a temporary folder that
//! goes when it does. Its one account is `mox@localhost`, password
//! `moxmoxmox`; mail to any address that isn't one is delivered to it, and
//! what is submitted waits in its queue to be delivered back to it (where
//! its untrained junk filter may refuse it).

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use glass_managed::mail::{Account, MailLink, Secret, Server, SyncLimits};

use super::ImapLink;

/// The pinned version; `tools/dev/mox.sh` builds the same.
pub const MOX_VERSION: &str = "0.0.17";
/// The account `mox localserve` makes.
pub const USER: &str = "mox@localhost";
pub const PASSWORD: &str = "moxmoxmox";
/// `mox localserve`'s ports: the usual ones plus 1000.
pub const IMAPS: u16 = 1993;
pub const IMAP: u16 = 1143;
pub const SUBMISSIONS: u16 = 1465;
pub const SUBMISSION: u16 = 1587;

/// A running `mox localserve`, stopped when dropped.
#[derive(Debug)]
pub struct Mox {
    child: Child,
    dir: tempfile::TempDir,
    ip: Ipv4Addr,
    cert: Vec<u8>,
}

/// Where the mox binary is: `MOX`, else `build/tools/mox-<version>/mox`
/// in this checkout or any folder above it (a worktree's main checkout).
pub fn binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MOX") {
        return Some(PathBuf::from(path)).filter(|p| p.is_file());
    }
    let rel = Path::new("build/tools")
        .join(format!("mox-{MOX_VERSION}"))
        .join("mox");
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|dir| dir.join(&rel))
        .find(|p| p.is_file())
}

/// A loopback address no other server in this run has: one per process
/// and call, so tests and test binaries running at once don't collide.
fn fresh_ip() -> Ipv4Addr {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let pid = std::process::id();
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    Ipv4Addr::new(
        127,
        (20 + pid % 200) as u8,
        ((pid / 200) % 250) as u8,
        (n % 250 + 2) as u8,
    )
}

impl Mox {
    /// Starts one, or says why not and returns `None` (no binary).
    pub fn start() -> Option<Mox> {
        let Some(bin) = binary() else {
            eprintln!(
                "skipped: mox {MOX_VERSION} isn't built; run tools/dev/mox.sh build (or set MOX)"
            );
            return None;
        };
        let dir = tempfile::tempdir().expect("a temporary folder for mox");
        let ip = fresh_ip();
        let state = dir.path().join("mox");
        let init = Command::new(&bin)
            .args(["localserve", "-initonly", "-ip", &ip.to_string(), "-dir"])
            .arg(&state)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("mox runs");
        assert!(init.success(), "mox localserve -initonly failed");
        let log = std::fs::File::create(dir.path().join("mox.log")).expect("mox's log");
        let child = Command::new(&bin)
            .args(["localserve", "-dir"])
            .arg(&state)
            .stdin(Stdio::null())
            .stdout(log.try_clone().expect("mox's log"))
            .stderr(log)
            .spawn()
            .expect("mox starts");
        let cert = std::fs::read(state.join("localhost.crt")).expect("mox's certificate");
        let mox = Mox {
            child,
            dir,
            ip,
            cert,
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        for port in [IMAPS, SUBMISSIONS] {
            let addr = SocketAddr::new(IpAddr::V4(ip), port);
            while TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_err() {
                assert!(
                    Instant::now() < deadline,
                    "mox didn't start listening on {addr}; its log is in {}",
                    mox.dir.path().display()
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        Some(mox)
    }

    /// Its address on loopback.
    pub fn ip(&self) -> IpAddr {
        IpAddr::V4(self.ip)
    }

    /// Its self-signed certificate (for `localhost`), in PEM.
    pub fn certificate(&self) -> &[u8] {
        &self.cert
    }

    /// A link that trusts it and finds `localhost` at its address.
    pub fn link(&self) -> ImapLink {
        ImapLink::for_test_server(&self.cert, "localhost", self.ip())
    }

    /// Its account, signed in as `login` on the computer: IMAPS and
    /// submission over TLS from the first byte.
    pub fn account(&self, login: &str) -> Account {
        Account {
            login: login.into(),
            incoming: Server {
                host: "localhost".into(),
                port: IMAPS,
                implicit_tls: true,
            },
            outgoing: Server {
                host: "localhost".into(),
                port: SUBMISSIONS,
                implicit_tls: true,
            },
            user: USER.into(),
            secret: Secret::new(PASSWORD),
        }
    }

    /// Files a message in its Inbox, as if it had arrived; its UID.
    pub fn deliver(&self, message: &[u8]) -> u32 {
        self.link()
            .append(&self.account("test"), "Inbox", message)
            .expect("mox takes the message")
            .expect("mox gives its UID")
    }

    /// The UIDs in one of its folders, now.
    pub fn uids(&self, folder: &str) -> Vec<u32> {
        let sync = self
            .link()
            .sync(
                &self.account("test"),
                folder,
                None,
                &[],
                SyncLimits {
                    newest: 10_000,
                    max_bytes: 0,
                },
            )
            .expect("mox answers");
        sync.new.iter().map(|m| m.uid).collect()
    }

    /// Gives its account another password (`mox setaccountpassword`).
    pub fn set_password(&self, password: &str) {
        use std::io::Write;
        let bin = binary().expect("mox is here: it's running");
        let mut child = Command::new(bin)
            .arg("-config")
            .arg(self.dir.path().join("mox/mox.conf"))
            .args(["setaccountpassword", "mox"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("mox setaccountpassword runs");
        child
            .stdin
            .take()
            .expect("its stdin")
            .write_all(format!("{password}\n").as_bytes())
            .expect("the password reaches it");
        assert!(
            child.wait().expect("it ends").success(),
            "mox refused the password"
        );
    }

    /// What its delivery queue holds: a line per message, from `mox queue
    /// list`.
    pub fn queue(&self) -> Vec<String> {
        let bin = binary().expect("mox is here: it's running");
        let out = Command::new(bin)
            .arg("-config")
            .arg(self.dir.path().join("mox/mox.conf"))
            .args(["queue", "list"])
            .output()
            .expect("mox queue list runs");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with(|c: char| c.is_ascii_digit()))
            .map(str::to_owned)
            .collect()
    }

    /// Its log so far, for a failing test's message.
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("mox.log")).unwrap_or_default()
    }
}

impl Drop for Mox {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
