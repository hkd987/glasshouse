//! `glass-kbuild`: the kernel patch build service (ADR 0057, slice 2).
//!
//! A computer's hardware app asks it, over HTTPS with a token:
//!
//! - `POST /v1/source/search` and `/v1/source/read`: the base system's
//!   kernel source of the release it builds for;
//! - `POST /v1/build`: an agent's patch, built into a bundle signed as a
//!   community patch (`origin` `agent:<task>`), or answered with the
//!   compiler's errors.
//!
//! The source and the compiler are the service's: nothing is compiled on a
//! person's computer (BS-4). In development the dev VM stands in for the
//! build machine, as for `os/freebsd/scripts/kernel-patch.sh`, which builds
//! here too, one build at a time; the VM reaches this service through an
//! SSH tunnel to the host (`--listen 127.0.0.1:7500`).

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use glass_managed::hwpatch::Bundle;
use glass_managed::kbuild::{
    BuildRequest, BuildResult, MAX_LINES, MAX_MATCHES, SourceFound, SourceMatch, SourceRead,
    SourceSearch, SourceText, valid_path,
};
use serde_json::{Value, json};

/// The most of a request read: a diff and its fields.
const MAX_REQUEST: usize = glass_managed::kbuild::MAX_DIFF_BYTES + 64 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "glass-kbuild",
    about = "The kernel patch build service (ADR 0057)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Make a token computers present (mode 0600); prints nothing.
    Token {
        #[arg(long)]
        out: PathBuf,
    },
    /// Serve computers' requests until stopped.
    Serve {
        #[arg(long, default_value = "127.0.0.1:7500")]
        listen: String,
        #[arg(long)]
        token_file: PathBuf,
        /// The release it builds for: `15.1-RELEASE`.
        #[arg(long)]
        release: String,
        /// The repository whose `os/freebsd/scripts` build in the dev VM.
        #[arg(long)]
        repo: PathBuf,
        /// The key agents' patches are signed with (`glass-kpatch keygen`).
        #[arg(long)]
        key: PathBuf,
    },
}

/// How the service reaches the build machine: the dev VM.
struct Builder {
    repo: PathBuf,
    release: String,
    key: PathBuf,
    /// One build at a time: each uses the same tree.
    building: Mutex<()>,
}

/// The kernel's source in the build machine, from `sys/`.
const SOURCE: &str = "/var/cache/glasshouse-build/src";

impl Builder {
    fn vm(&self) -> Process {
        let mut command = Process::new(self.repo.join("os/freebsd/scripts/vm.sh"));
        command.arg("ssh");
        command
    }

    fn release_ok(&self, release: &str) -> Result<(), String> {
        if glass_platform::hardware::release_takes(release, &self.release) {
            Ok(())
        } else {
            Err(format!(
                "this service builds for {}, and the computer runs {release}",
                self.release
            ))
        }
    }

    fn search(&self, request: &SourceSearch) -> Result<SourceFound, String> {
        self.release_ok(&request.release)?;
        if request.pattern.is_empty()
            || request.pattern.len() > 200
            || request.pattern.contains('\n')
        {
            return Err("a pattern of 1 to 200 characters on one line".into());
        }
        if !valid_path(&request.path) {
            return Err(format!("{:?} isn't a path under sys/", request.path));
        }
        // The pattern goes on standard input, never on a command line.
        let mut child = self
            .vm()
            .arg(format!(
                "cd {SOURCE} && grep -rnE -m 20 -f /dev/stdin -- sys/{} | head -{}",
                request.path,
                MAX_MATCHES + 1
            ))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(request.pattern.as_bytes());
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut matches: Vec<SourceMatch> = text
            .lines()
            .filter_map(|l| {
                let (file, rest) = l.split_once(':')?;
                let (line, text) = rest.split_once(':')?;
                Some(SourceMatch {
                    file: file.into(),
                    line: line.parse().ok()?,
                    text: text.chars().take(300).collect(),
                })
            })
            .collect();
        let truncated = matches.len() > MAX_MATCHES;
        matches.truncate(MAX_MATCHES);
        Ok(SourceFound { matches, truncated })
    }

    fn read(&self, request: &SourceRead) -> Result<SourceText, String> {
        self.release_ok(&request.release)?;
        if !valid_path(&request.path) || request.path.is_empty() {
            return Err(format!("{:?} isn't a file under sys/", request.path));
        }
        let from = request.from.max(1);
        let to = from + request.lines.clamp(1, MAX_LINES) - 1;
        let out = self
            .vm()
            .arg(format!(
                "cd {SOURCE} && test -f sys/{path} && wc -l < sys/{path} && sed -n '{from},{to}p' sys/{path}",
                path = request.path
            ))
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!("sys/{}: no such file", request.path));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let (total, body) = text.split_once('\n').unwrap_or((&text, ""));
        Ok(SourceText {
            path: request.path.clone(),
            from,
            text: body.to_owned(),
            total_lines: total.trim().parse().unwrap_or(0),
        })
    }

    fn build(&self, request: &BuildRequest) -> Result<BuildResult, String> {
        self.release_ok(&request.release)?;
        request.check()?;
        let _one = self.building.lock().unwrap_or_else(|p| p.into_inner());
        let work = self.repo.join("build/kbuild").join(&request.id);
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let toml = |s: &str| serde_json::to_string(s).unwrap_or_default();
        let mut file = format!(
            "# Written by glass-kbuild for {origin}: nobody here reviewed it.\n\
             id = {id}\nrevision = {revision}\nsummary = {summary}\nrelease = {release}\n\
             origin = {origin_q}\nreview = \"community\"\ntargets = {targets}\nkernel = {kernel}\n",
            origin = request.origin,
            id = toml(&request.id),
            revision = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(1, |d| d.as_secs()),
            summary = toml(&request.summary),
            release = toml(&self.release),
            origin_q = toml(&request.origin),
            targets = serde_json::to_string(&request.targets).unwrap_or_else(|_| "[]".into()),
            kernel = request.kernel,
        );
        for m in &request.modules {
            file += &format!(
                "\n[[module]]\ndir = {}\nfile = {}\nload_at_boot = {}\n",
                toml(&m.dir),
                toml(&m.file),
                m.load_at_boot
            );
        }
        std::fs::write(work.join("patch.toml"), file).map_err(|e| e.to_string())?;
        std::fs::write(
            work.join("patch.diff"),
            format!("{}\n\n{}", request.summary, request.diff),
        )
        .map_err(|e| e.to_string())?;
        let bundle = work.join("bundle.json");
        let out = Process::new(self.repo.join("os/freebsd/scripts/kernel-patch.sh"))
            .arg("build")
            .arg(&work)
            .arg("--out")
            .arg(&bundle)
            .env("KPATCH_KEY", &self.key)
            .output()
            .map_err(|e| e.to_string())?;
        let log = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        // The end of what it said: the compiler's errors, or what it built.
        let tail: Vec<&str> = log.lines().rev().take(40).collect();
        let log = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        if !out.status.success() {
            return Ok(BuildResult {
                ok: false,
                log,
                bundle: None,
            });
        }
        let bytes = std::fs::read(&bundle).map_err(|e| e.to_string())?;
        let bundle: Bundle = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_dir_all(&work);
        Ok(BuildResult {
            ok: true,
            log,
            bundle: Some(bundle),
        })
    }
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Token { out } => {
            let token = hex::encode(glass_crypto::random::<32>()?);
            write_new(&out, token.as_bytes())?;
        }
        Command::Serve {
            listen,
            token_file,
            release,
            repo,
            key,
        } => {
            let token = std::fs::read_to_string(&token_file)
                .with_context(|| token_file.display().to_string())?
                .trim()
                .to_owned();
            let builder = Arc::new(Builder {
                repo,
                release,
                key,
                building: Mutex::new(()),
            });
            let listener = TcpListener::bind(&listen).with_context(|| listen.clone())?;
            eprintln!("glass-kbuild: serving on {listen}");
            for stream in listener.incoming().flatten() {
                let builder = builder.clone();
                let token = token.clone();
                std::thread::spawn(move || {
                    if let Err(e) = serve(stream, &builder, &token) {
                        eprintln!("glass-kbuild: {e}");
                    }
                });
            }
        }
    }
    Ok(())
}

/// One request: its head, its body, the answer.
fn serve(mut stream: TcpStream, builder: &Builder, token: &str) -> anyhow::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let (path, auth, body) = read_request(&mut stream)?;
    let (status, answer) = answer(builder, token, &path, auth.as_deref(), &body);
    let text = serde_json::to_vec(&answer)?;
    write!(
        stream,
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        if status == 200 { "OK" } else { "Error" },
        text.len()
    )?;
    stream.write_all(&text)?;
    Ok(())
}

fn answer(
    builder: &Builder,
    token: &str,
    path: &str,
    auth: Option<&str>,
    body: &[u8],
) -> (u16, Value) {
    let presented = auth.and_then(|a| a.strip_prefix("Bearer ")).unwrap_or("");
    if !same(presented.as_bytes(), token.as_bytes()) {
        return (
            403,
            json!({ "error": "that token isn't one of this service's" }),
        );
    }
    fn run<T: serde::de::DeserializeOwned, R: serde::Serialize>(
        body: &[u8],
        f: impl FnOnce(&T) -> Result<R, String>,
    ) -> (u16, Value) {
        match serde_json::from_slice::<T>(body) {
            Ok(request) => match f(&request) {
                Ok(r) => (200, serde_json::to_value(r).unwrap_or_default()),
                Err(e) => (400, json!({ "error": e })),
            },
            Err(e) => (
                400,
                json!({ "error": format!("the request doesn't parse: {e}") }),
            ),
        }
    }
    match path {
        "/v1/source/search" => run(body, |r: &SourceSearch| builder.search(r)),
        "/v1/source/read" => run(body, |r: &SourceRead| builder.read(r)),
        "/v1/build" => run(body, |r: &BuildRequest| builder.build(r)),
        _ => (404, json!({ "error": format!("no {path}") })),
    }
}

/// Whether two tokens are the same, in time that doesn't say how much of
/// one matched.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn read_request(stream: &mut TcpStream) -> anyhow::Result<(String, Option<String>, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            anyhow::bail!("the connection closed mid-request");
        }
        buf.extend_from_slice(&chunk[..n]);
        let mut headers = [httparse::EMPTY_HEADER; 32];
        let mut request = httparse::Request::new(&mut headers);
        if let httparse::Status::Complete(at) = request.parse(&buf)? {
            if request.method != Some("POST") {
                anyhow::bail!("only POST");
            }
            let path = request.path.unwrap_or("").to_owned();
            let header = |name: &str| {
                request
                    .headers
                    .iter()
                    .find(|h| h.name.eq_ignore_ascii_case(name))
                    .map(|h| String::from_utf8_lossy(h.value).into_owned())
            };
            let auth = header("authorization");
            let length: usize = header("content-length")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            if length > MAX_REQUEST {
                anyhow::bail!("a request of {length} bytes");
            }
            let mut body = buf[at..].to_vec();
            while body.len() < length {
                let n = stream.read(&mut chunk)?;
                if n == 0 {
                    anyhow::bail!("the connection closed mid-body");
                }
                body.extend_from_slice(&chunk[..n]);
            }
            body.truncate(length);
            return Ok((path, auth, body));
        }
        if buf.len() > 64 * 1024 {
            anyhow::bail!("a request head over 64 KB");
        }
    }
}

/// Writes a new file for its owner alone; never over one already there.
fn write_new(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| path.display().to_string())?;
    file.write_all(bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builder() -> Builder {
        Builder {
            repo: PathBuf::from("/nonexistent"),
            release: "15.1-RELEASE".into(),
            key: PathBuf::from("/nonexistent"),
            building: Mutex::new(()),
        }
    }

    #[test]
    fn adr0057_only_the_token_s_holder_and_its_release_are_served() {
        let b = builder();
        let search = br#"{"release":"15.1-RELEASE-p2","pattern":"x","path":"dev"}"#;
        assert_eq!(
            answer(
                &b,
                "t0ken",
                "/v1/source/search",
                Some("Bearer nope"),
                search
            )
            .0,
            403
        );
        assert_eq!(
            answer(&b, "t0ken", "/v1/source/search", None, search).0,
            403
        );
        let other = br#"{"release":"14.3-RELEASE","pattern":"x","path":"dev"}"#;
        let (status, why) = answer(
            &b,
            "t0ken",
            "/v1/source/search",
            Some("Bearer t0ken"),
            other,
        );
        assert_eq!(status, 400);
        assert!(
            why["error"]
                .as_str()
                .unwrap()
                .contains("builds for 15.1-RELEASE"),
            "{why}"
        );
        let escape = br#"{"release":"15.1-RELEASE","path":"../../etc/master.passwd"}"#;
        assert_eq!(
            answer(&b, "t0ken", "/v1/source/read", Some("Bearer t0ken"), escape).0,
            400
        );
        assert_eq!(
            answer(&b, "t0ken", "/v2/anything", Some("Bearer t0ken"), b"{}").0,
            404
        );
    }
}
