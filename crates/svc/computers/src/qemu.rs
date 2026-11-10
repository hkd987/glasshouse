//! Glasshouse OS computers under QEMU (ADR 0072): a person's computer is a
//! Glasshouse OS install, its desktop drawn on a display of its own, which
//! glassd shows its person and, taken over, lets them drive with a pointer
//! and keys. KVM where the host has it; emulated (TCG) in development,
//! slowly.
//!
//! Each computer's folder holds:
//! - `disk.qcow2`: its disk, a copy-on-write layer over the shared image;
//! - `vars.fd`: its firmware's variables, fresh each start;
//! - `qmp.sock`: QEMU's machine protocol: the display, input, power;
//! - `ssh.port`: where its exec channel listens on this host's loopback;
//! - `qemu.pid`, `console.log` and `frame.png` while it runs.
//!
//! Its network reaches nothing but the exec channel (QEMU's user network,
//! restricted: no route out, no way to the host). Tasks run in it over
//! that channel with the task's socket forwarded in, and the agent runs as
//! the image installs it. The channel is ssh with the image's development
//! key; on hosts it becomes a virtio-serial port (ADR 0072, "not yet").

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use glass_api::{DisplayInput, PointerButton};
use glass_platform::sandbox::{AGENT_SOCKET_PATH, WORK_DIR};
use serde_json::{Value, json};

use crate::hypervisor::{ComputerSpec, Frame, Hypervisor, TaskRun};

const DISK: &str = "disk.qcow2";
const VARS: &str = "vars.fd";
const QMP: &str = "qmp.sock";
const PID: &str = "qemu.pid";
const PORT: &str = "ssh.port";
const CONSOLE: &str = "console.log";
const FRAME: &str = "frame.png";
/// Where each task's socket appears in the guest; the agent finds it
/// through `GLASS_AGENT_SOCKET`.
const GUEST_TASKS: &str = "/run/glasshouse/tasks";

/// What a host needs to run Glasshouse OS computers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QemuConfig {
    pub qemu: PathBuf,
    pub qemu_img: PathBuf,
    /// The Glasshouse OS disk every computer starts from, never written.
    pub image: PathBuf,
    /// UEFI firmware, and the variables a new computer starts with.
    pub firmware_code: PathBuf,
    pub firmware_vars: PathBuf,
    /// `kvm`, or `tcg,thread=multi` where there's no KVM.
    pub accel: String,
    /// `host` with KVM; under TCG a model without AVX (QEMU 8.2 emulates it
    /// wrongly).
    pub cpu: String,
    /// The display, in pixels.
    pub width: u32,
    pub height: u32,
    /// The exec channel's key, as the image trusts it.
    pub ssh_key: PathBuf,
    /// The account the desktop runs as in the guest.
    pub desktop_user: String,
    /// Where the image installs agents.
    pub agents_dir: PathBuf,
    /// How long a computer may take to boot.
    pub boot_timeout: Duration,
}

/// Glasshouse OS computers, one QEMU each.
#[derive(Debug)]
pub struct QemuHypervisor {
    config: QemuConfig,
}

impl QemuHypervisor {
    pub fn new(config: QemuConfig) -> Self {
        Self { config }
    }

    /// Its QEMU's pid, from its folder's pid file, if that process is its
    /// QEMU: a QEMU started with this very pid file. Any QEMU passed
    /// before, so a pid another computer's QEMU had by now (the host
    /// restarted, the file stayed) was taken for this one's, and stopping
    /// this computer stopped that one.
    fn pid(computer: &ComputerSpec) -> Option<u32> {
        use std::os::unix::ffi::OsStrExt;
        let file = computer.dir.join(PID);
        let text = fs::read_to_string(&file).ok()?;
        let pid: u32 = text.trim().parse().ok()?;
        let cmdline = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        if !String::from_utf8_lossy(&cmdline).contains("qemu-system") {
            return None;
        }
        // However the folder was spelled when it started.
        let file = fs::canonicalize(&file).ok()?;
        let mut args = cmdline.split(|b| *b == 0);
        let ours = args.by_ref().any(|a| a == b"-pidfile")
            && args.next().is_some_and(|a| {
                fs::canonicalize(std::ffi::OsStr::from_bytes(a)).is_ok_and(|p| p == file)
            });
        ours.then_some(pid)
    }

    fn port(computer: &ComputerSpec) -> Result<u16, String> {
        fs::read_to_string(computer.dir.join(PORT))
            .ok()
            .and_then(|p| p.trim().parse().ok())
            .ok_or_else(|| format!("{} has no exec channel: start it first.", computer.id))
    }

    /// ssh to the computer as root, with `extra` options before the host.
    fn ssh(&self, computer: &ComputerSpec, extra: &[String]) -> Result<Command, String> {
        let port = Self::port(computer)?;
        let mut command = Command::new("ssh");
        command
            .arg("-i")
            .arg(&self.config.ssh_key)
            .args(["-p", &port.to_string()])
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "LogLevel=ERROR",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ControlPath=none",
            ])
            .args(extra)
            .arg("root@127.0.0.1");
        Ok(command)
    }

    /// Runs `script` in the guest as root, and says how it went.
    fn guest(&self, computer: &ComputerSpec, script: &str) -> Result<String, String> {
        let out = self
            .ssh(computer, &[])?
            .args(["sh", "-eu"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                if let Some(mut stdin) = child.stdin.take() {
                    stdin.write_all(script.as_bytes())?;
                }
                child.wait_with_output()
            })
            .map_err(|e| format!("Couldn't reach {}: {e}.", computer.id))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(format!(
                "{} said: {}",
                computer.id,
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    fn boot(&self, computer: &ComputerSpec) -> Result<(), String> {
        let dir = &computer.dir;
        // A free port on loopback for the exec channel.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .map_err(|e| format!("Couldn't find a port for {}: {e}.", computer.id))?;
        fs::write(dir.join(PORT), port.to_string()).map_err(|e| e.to_string())?;
        let _ = fs::remove_file(dir.join(QMP));
        // Fresh each start, as the dev VM's are: stored boot entries name
        // PCI slots, and go stale when the device list changes.
        fs::copy(&self.config.firmware_vars, dir.join(VARS))
            .map_err(|e| format!("Couldn't make {}'s firmware variables: {e}.", computer.id))?;
        let mut qemu = Command::new(&self.config.qemu);
        // Firmware built with SMM keeps its flash to SMM, as the hardware
        // watchdog needs (ADR 0057).
        if self
            .config
            .firmware_code
            .to_string_lossy()
            .contains("secboot")
        {
            qemu.args(["-global", "driver=cfi.pflash01,property=secure,value=on"]);
        }
        let status = qemu
            .args(["-nodefaults", "-name", &computer.host_id()])
            .args(["-machine", "q35"])
            .args(["-accel", &self.config.accel, "-cpu", &self.config.cpu])
            .args(["-smp", &computer.vcpus.to_string()])
            .args(["-m", &computer.memory_mib.to_string()])
            .arg("-drive")
            .arg(drive(
                "if=virtio,format=qcow2,discard=unmap",
                &dir.join(DISK),
            ))
            // Restricted: the guest reaches nothing, not even this host,
            // but the exec channel forwarded to it.
            .arg("-netdev")
            .arg(format!(
                "user,id=net0,restrict=on,hostfwd=tcp:127.0.0.1:{port}-:22"
            ))
            .args([
                "-device",
                "virtio-net-pci,netdev=net0",
                "-device",
                "virtio-rng-pci",
            ])
            .args(["-vga", "none", "-device"])
            .arg(format!(
                "VGA,xres={},yres={}",
                self.config.width, self.config.height
            ))
            .args([
                "-device",
                "qemu-xhci",
                "-device",
                "usb-kbd",
                "-device",
                "usb-tablet",
            ])
            .arg("-drive")
            .arg(drive(
                "if=pflash,format=raw,unit=0,readonly=on",
                &self.config.firmware_code,
            ))
            .arg("-drive")
            .arg(drive("if=pflash,format=raw,unit=1", &dir.join(VARS)))
            .args(["-display", "none", "-qmp"])
            .arg(format!("unix:{},server,nowait", dir.join(QMP).display()))
            .arg("-serial")
            .arg(format!("file:{}", dir.join(CONSOLE).display()))
            .arg("-daemonize")
            .arg("-pidfile")
            .arg(dir.join(PID))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("Couldn't start {}: {e}.", self.config.qemu.display()))?;
        if !status.status.success() {
            return Err(format!(
                "QEMU didn't start {}: {}",
                computer.id,
                String::from_utf8_lossy(&status.stderr).trim()
            ));
        }
        self.ready(computer)
    }

    /// Waits for it to boot: up when the exec channel answers.
    fn ready(&self, computer: &ComputerSpec) -> Result<(), String> {
        let dir = &computer.dir;
        let started = Instant::now();
        loop {
            if self.guest(computer, "true").is_ok() {
                return Ok(());
            }
            if Self::pid(computer).is_none() {
                return Err(format!(
                    "{} stopped as it booted; see {}.",
                    computer.id,
                    dir.join(CONSOLE).display()
                ));
            }
            if started.elapsed() > self.config.boot_timeout {
                return Err(format!(
                    "{} didn't boot within {} s.",
                    computer.id,
                    self.config.boot_timeout.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_secs(3));
        }
    }

    /// The desktop on its display, as its person, from the sign-in
    /// screen's place.
    fn desktop(&self, computer: &ComputerSpec) -> Result<(), String> {
        let user = &self.config.desktop_user;
        if !user.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || user.is_empty() {
            return Err(format!("{user:?} isn't an account name."));
        }
        // Signed in as the sign-in screen does it: the screen and input
        // handed to the person, a fresh runtime directory for the apps'
        // windows, and `su -l` for the session's environment.
        self.guest(
            computer,
            &format!(
                "mkdir -p {WORK_DIR} {GUEST_TASKS}\n\
                 service glasshouse_greeter onestatus >/dev/null 2>&1 && \
                 service glasshouse_greeter onestop >/dev/null 2>&1 || true\n\
                 pgrep -qx glass-shell-fb && exit 0\n\
                 uid=$(id -u {user}) gid=$(id -g {user}) || exit 1\n\
                 chown {user} /dev/ttyv0 /dev/input/event* 2>/dev/null || true\n\
                 mkdir -p -m 0755 /var/run/user && rm -rf /var/run/user/$uid\n\
                 install -d -m 0700 -o $uid -g $gid /var/run/user/$uid\n\
                 install -m 0644 -o {user} /dev/null /var/log/glass-shell-fb.log\n\
                 daemon -o /var/log/glass-shell-fb.log /usr/bin/su -l {user} -c \
                 \"exec env XDG_RUNTIME_DIR=/var/run/user/$uid /usr/local/bin/glass-shell-fb\" \
                 </dev/null >/dev/null 2>&1\n"
            ),
        )
        .map(|_| ())
    }

    /// One QMP command, after the greeting and capabilities.
    fn qmp(&self, computer: &ComputerSpec, command: &Value) -> Result<Value, String> {
        self.qmp_all(computer, std::slice::from_ref(command))
    }

    /// QMP commands in order on one connection; the last one's answer.
    fn qmp_all(&self, computer: &ComputerSpec, commands: &[Value]) -> Result<Value, String> {
        let path = computer.dir.join(QMP);
        let stream = UnixStream::connect(&path)
            .map_err(|e| format!("Couldn't reach {}'s machine: {e}.", computer.id))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .map_err(|e| e.to_string())?;
        let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|e| e.to_string())?;
        let mut reply = Value::Null;
        let capabilities = json!({"execute": "qmp_capabilities"});
        for send in std::iter::once(&capabilities).chain(commands) {
            writeln!(writer, "{send}").map_err(|e| e.to_string())?;
            reply = loop {
                line.clear();
                if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                    return Err(format!("{}'s machine closed the connection.", computer.id));
                }
                let answer: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
                if let Some(error) = answer.get("error") {
                    return Err(format!(
                        "{}'s machine refused: {}",
                        computer.id,
                        error["desc"].as_str().unwrap_or("no reason given")
                    ));
                }
                if let Some(value) = answer.get("return") {
                    break value.clone();
                }
                // Anything else is one of QEMU's events: not the answer.
            };
        }
        Ok(reply)
    }

    /// Where a task's socket appears in the guest, and the line that runs
    /// its agent there: in its folder, with only its own environment.
    fn task_line(&self, run: &TaskRun) -> Result<(String, String), String> {
        // Exactly `/work/<task>`: the folder glassd gives each task.
        let task = run
            .workdir
            .strip_prefix(WORK_DIR)
            .ok()
            .and_then(|rest| rest.to_str())
            .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
            .ok_or_else(|| format!("{} isn't a task's folder.", run.workdir.display()))?;
        let program = run
            .process
            .program
            .file_name()
            .ok_or_else(|| format!("{} isn't a program.", run.process.program.display()))?;
        let program = self.config.agents_dir.join(program);
        let socket = format!("{GUEST_TASKS}/{task}.sock");
        let mut line = format!(
            "mkdir -p {workdir} && cd {workdir} && env -i",
            workdir = quote(&run.workdir.display().to_string())
        );
        for (name, value) in &run.process.env {
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(format!("{name:?} isn't an environment variable's name."));
            }
            let value = if name == "GLASS_AGENT_SOCKET" || value == AGENT_SOCKET_PATH {
                socket.as_str()
            } else {
                value.as_str()
            };
            line.push_str(&format!(" {name}={}", quote(value)));
        }
        line.push(' ');
        line.push_str(&quote(&program.display().to_string()));
        for arg in &run.process.args {
            line.push(' ');
            line.push_str(&quote(arg));
        }
        // Its socket goes with it: sshd leaves the forward's file behind.
        line.push_str(&format!(
            "; status=$?; rm -f {}; exit $status",
            quote(&socket)
        ));
        Ok((socket, line))
    }

    /// QEMU's input events for `events`, in groups: each group is one
    /// `input-send-event`, which the guest gets as one report. A button
    /// change ends its group, with where the pointer is, so a press and its
    /// release sent together still reach the guest as a click.
    fn input_events(&self, events: &[DisplayInput]) -> Result<Vec<Vec<Value>>, String> {
        let axis = |value: u32, size: u32| value.min(size - 1) * 0x7fff / (size - 1).max(1);
        let mut groups = Vec::new();
        let mut group = Vec::new();
        let mut at: Vec<Value> = Vec::new();
        let mut button = |group: &mut Vec<Value>, at: &[Value], button: &str, down: bool| {
            if group.is_empty() {
                group.extend_from_slice(at);
            }
            group.push(json!({"type": "btn", "data": {"down": down, "button": button}}));
            groups.push(std::mem::take(group));
        };
        for event in events {
            match event {
                DisplayInput::Move { x, y } => {
                    at = vec![
                        json!({"type": "abs", "data": {"axis": "x", "value": axis(*x, self.config.width)}}),
                        json!({"type": "abs", "data": {"axis": "y", "value": axis(*y, self.config.height)}}),
                    ];
                    group.extend_from_slice(&at);
                }
                DisplayInput::Button {
                    button: which,
                    down,
                } => {
                    let which = match which {
                        PointerButton::Left => "left",
                        PointerButton::Middle => "middle",
                        PointerButton::Right => "right",
                    };
                    button(&mut group, &at, which, *down);
                }
                DisplayInput::Wheel { dy } => {
                    let which = if *dy < 0 { "wheel-up" } else { "wheel-down" };
                    for _ in 0..dy.unsigned_abs().min(10) {
                        for down in [true, false] {
                            button(&mut group, &at, which, down);
                        }
                    }
                }
                DisplayInput::Key { code, down } => {
                    // A key this computer's keyboard hasn't (a media key,
                    // F13 and up) is left out, not the rest with it.
                    let Some(qcode) = qcode(code) else {
                        continue;
                    };
                    group.push(json!({
                        "type": "key",
                        "data": {"down": down, "key": {"type": "qcode", "data": qcode}}
                    }));
                }
            }
        }
        if !group.is_empty() {
            groups.push(group);
        }
        Ok(groups)
    }
}

/// A `-drive` value: its options, then the file, its commas doubled.
fn drive(options: &str, file: &Path) -> String {
    format!(
        "{options},file={}",
        file.display().to_string().replace(',', ",,")
    )
}

/// `code` quoted for the guest's shell.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// A key by its place on the keyboard, as browsers name it
/// (`KeyboardEvent.code`), to QEMU's name for it.
pub fn qcode(code: &str) -> Option<String> {
    if let Some(letter) = code.strip_prefix("Key")
        && letter.len() == 1
        && letter.chars().all(|c| c.is_ascii_uppercase())
    {
        return Some(letter.to_ascii_lowercase());
    }
    if let Some(digit) = code.strip_prefix("Digit")
        && digit.len() == 1
        && digit.chars().all(|c| c.is_ascii_digit())
    {
        return Some(digit.to_owned());
    }
    if let Some(n) = code.strip_prefix('F')
        && let Ok(n) = n.parse::<u8>()
        && (1..=12).contains(&n)
    {
        return Some(format!("f{n}"));
    }
    if let Some(digit) = code.strip_prefix("Numpad")
        && digit.len() == 1
        && digit.chars().all(|c| c.is_ascii_digit())
    {
        return Some(format!("kp_{digit}"));
    }
    let named = match code {
        "Enter" => "ret",
        "Escape" => "esc",
        "Backspace" => "backspace",
        "Tab" => "tab",
        "Space" => "spc",
        "Minus" => "minus",
        "Equal" => "equal",
        "BracketLeft" => "bracket_left",
        "BracketRight" => "bracket_right",
        "Backslash" => "backslash",
        "Semicolon" => "semicolon",
        "Quote" => "apostrophe",
        "Backquote" => "grave_accent",
        "Comma" => "comma",
        "Period" => "dot",
        "Slash" => "slash",
        "IntlBackslash" => "less",
        "CapsLock" => "caps_lock",
        "ShiftLeft" => "shift",
        "ShiftRight" => "shift_r",
        "ControlLeft" => "ctrl",
        "ControlRight" => "ctrl_r",
        "AltLeft" => "alt",
        "AltRight" => "alt_r",
        "MetaLeft" => "meta_l",
        "MetaRight" => "meta_r",
        "ContextMenu" => "menu",
        "ArrowUp" => "up",
        "ArrowDown" => "down",
        "ArrowLeft" => "left",
        "ArrowRight" => "right",
        "Home" => "home",
        "End" => "end",
        "PageUp" => "pgup",
        "PageDown" => "pgdn",
        "Insert" => "insert",
        "Delete" => "delete",
        "NumpadEnter" => "kp_enter",
        "NumpadAdd" => "kp_add",
        "NumpadSubtract" => "kp_subtract",
        "NumpadMultiply" => "kp_multiply",
        "NumpadDivide" => "kp_divide",
        "NumpadDecimal" => "kp_decimal",
        "NumLock" => "num_lock",
        "ScrollLock" => "scroll_lock",
        "Pause" => "pause",
        "PrintScreen" => "print",
        _ => return None,
    };
    Some(named.to_owned())
}

impl Hypervisor for QemuHypervisor {
    fn kind(&self) -> &'static str {
        "Glasshouse OS virtual machine"
    }

    fn contained(&self) -> bool {
        true
    }

    fn start(&self, computer: &ComputerSpec) -> Result<(), String> {
        check_id(&computer.id)?;
        let dir = &computer.dir;
        fs::create_dir_all(dir).map_err(|e| format!("Couldn't make {}: {e}.", dir.display()))?;
        if !dir.join(DISK).exists() {
            let made = Command::new(&self.config.qemu_img)
                .args(["create", "-q", "-f", "qcow2", "-F", "qcow2", "-b"])
                .arg(&self.config.image)
                .arg(dir.join(DISK))
                .output()
                .map_err(|e| format!("Couldn't run {}: {e}.", self.config.qemu_img.display()))?;
            if !made.status.success() {
                return Err(format!(
                    "Couldn't make {}'s disk: {}",
                    computer.id,
                    String::from_utf8_lossy(&made.stderr).trim()
                ));
            }
        }
        if Self::pid(computer).is_none() {
            self.boot(computer)?;
        } else {
            // Booting already, for someone else: up once it answers.
            self.ready(computer)?;
        }
        self.desktop(computer)
    }

    fn running(&self, computer: &ComputerSpec) -> bool {
        Self::pid(computer).is_some()
    }

    /// The agent, as the image installs it, run over the exec channel with
    /// the task's socket forwarded in at a path of its own.
    fn command(&self, computer: &ComputerSpec, run: &TaskRun) -> Result<Command, String> {
        if Self::pid(computer).is_none() {
            return Err(format!(
                "{} isn't running: start it before running a task in it.",
                computer.id
            ));
        }
        let (socket, line) = self.task_line(run)?;
        let mut command = self.ssh(
            computer,
            &[
                "-o".into(),
                "ExitOnForwardFailure=yes".into(),
                "-o".into(),
                "StreamLocalBindUnlink=yes".into(),
                "-R".into(),
                format!("{socket}:{}", run.socket.display()),
            ],
        )?;
        command.arg("--").arg(line);
        Ok(command)
    }

    fn host_path(&self, _computer: &ComputerSpec, _inside: &Path) -> Option<PathBuf> {
        None
    }

    /// Shuts it down as its power button would; its disk stays in its
    /// folder. A computer that doesn't stop in two minutes is stopped.
    fn suspend(&self, computer: &ComputerSpec) -> Result<(), String> {
        if let Some(pid) = Self::pid(computer) {
            let _ = self.qmp(computer, &json!({"execute": "system_powerdown"}));
            let started = Instant::now();
            while Self::pid(computer).is_some() && started.elapsed() < Duration::from_secs(120) {
                std::thread::sleep(Duration::from_millis(500));
            }
            if Self::pid(computer).is_some() {
                let _ = Command::new("kill")
                    .args(["-KILL", &pid.to_string()])
                    .status();
            }
        }
        for file in [QMP, PID, PORT, FRAME] {
            let _ = fs::remove_file(computer.dir.join(file));
        }
        Ok(())
    }

    fn stop_leftovers(&self, dir: &Path) -> Vec<String> {
        let mut stopped = Vec::new();
        let mut folders = vec![dir.to_path_buf()];
        for _ in 0..3 {
            let mut next = Vec::new();
            for folder in &folders {
                for entry in fs::read_dir(folder).into_iter().flatten().flatten() {
                    let path = entry.path();
                    let spec = ComputerSpec {
                        id: String::new(),
                        owner: 0,
                        dir: path.clone(),
                        memory_mib: 0,
                        vcpus: 0,
                    };
                    if let Some(pid) = Self::pid(&spec) {
                        let killed = Command::new("kill")
                            .args(["-KILL", &pid.to_string()])
                            .status()
                            .is_ok_and(|s| s.success());
                        if killed {
                            stopped.push(format!("QEMU {pid} for {}", path.display()));
                        }
                    } else if path.is_dir() {
                        next.push(path);
                    }
                }
            }
            folders = next;
        }
        stopped
    }

    fn display(&self) -> bool {
        true
    }

    fn frame(&self, computer: &ComputerSpec) -> Result<Frame, String> {
        let file = computer.dir.join(FRAME);
        self.qmp(
            computer,
            &json!({
                "execute": "screendump",
                "arguments": {"filename": file.display().to_string(), "format": "png"}
            }),
        )?;
        let png = fs::read(&file)
            .map_err(|e| format!("Couldn't read {}'s display: {e}.", computer.id))?;
        Frame::from_png(png)
    }

    fn send_input(&self, computer: &ComputerSpec, events: &[DisplayInput]) -> Result<(), String> {
        let commands: Vec<Value> = self
            .input_events(events)?
            .into_iter()
            .map(|events| json!({"execute": "input-send-event", "arguments": {"events": events}}))
            .collect();
        self.qmp_all(computer, &commands).map(|_| ())
    }
}

fn check_id(id: &str) -> Result<(), String> {
    let fits = !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if fits {
        Ok(())
    } else {
        Err(format!(
            "A computer's id is letters, digits and dashes, at most 64: {id:?} isn't."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hypervisor() -> QemuHypervisor {
        QemuHypervisor::new(QemuConfig {
            qemu: "qemu-system-x86_64".into(),
            qemu_img: "qemu-img".into(),
            image: "/images/glasshouse.qcow2".into(),
            firmware_code: "/usr/share/OVMF/OVMF_CODE_4M.fd".into(),
            firmware_vars: "/usr/share/OVMF/OVMF_VARS_4M.fd".into(),
            accel: "tcg,thread=multi".into(),
            cpu: "max".into(),
            width: 1280,
            height: 800,
            ssh_key: "/keys/id_ed25519".into(),
            desktop_user: "demo".into(),
            agents_dir: "/usr/local/libexec/glasshouse".into(),
            boot_timeout: Duration::from_secs(1),
        })
    }

    #[test]
    fn adr0072_keys_go_in_by_their_place_and_unknown_ones_are_left_out() {
        assert_eq!(qcode("KeyA").as_deref(), Some("a"));
        assert_eq!(qcode("Digit7").as_deref(), Some("7"));
        assert_eq!(qcode("Enter").as_deref(), Some("ret"));
        assert_eq!(qcode("ShiftRight").as_deref(), Some("shift_r"));
        assert_eq!(qcode("F12").as_deref(), Some("f12"));
        assert_eq!(qcode("Numpad5").as_deref(), Some("kp_5"));
        for unknown in ["KeyAB", "Keya", "F13", "F0", "Digit10", "", "Power"] {
            assert_eq!(qcode(unknown), None, "{unknown}");
        }
        // A key the keyboard hasn't is left out; the keys around it go in.
        let hv = hypervisor();
        let key = |code: &str| DisplayInput::Key {
            code: code.into(),
            down: true,
        };
        let groups = hv
            .input_events(&[key("KeyH"), key("AudioVolumeUp"), key("KeyI")])
            .unwrap();
        assert_eq!(groups.len(), 1);
        let keys: Vec<_> = groups[0]
            .iter()
            .map(|e| e["data"]["key"]["data"].clone())
            .collect();
        assert_eq!(keys, ["h", "i"]);
    }

    #[test]
    fn adr0072_the_pointer_lands_where_the_person_points() {
        let hv = hypervisor();
        let groups = hv
            .input_events(&[
                DisplayInput::Move { x: 1279, y: 0 },
                DisplayInput::Move { x: 5000, y: 400 },
                DisplayInput::Button {
                    button: PointerButton::Left,
                    down: true,
                },
                DisplayInput::Button {
                    button: PointerButton::Left,
                    down: false,
                },
                DisplayInput::Wheel { dy: -2 },
                DisplayInput::Key {
                    code: "KeyA".into(),
                    down: true,
                },
            ])
            .unwrap();
        let press = &groups[0];
        assert_eq!(press[0]["data"]["value"], 0x7fff);
        assert_eq!(press[1]["data"]["value"], 0);
        // Off the screen, the pointer stays at its edge.
        assert_eq!(press[2]["data"]["value"], 0x7fff);
        assert_eq!(press[4]["data"]["button"], "left");
        assert_eq!(press.len(), 5);
        // The release is a report of its own, where the pointer is: sent
        // in the press's report, the guest would see no click.
        let release = &groups[1];
        assert_eq!(release.len(), 3);
        assert_eq!(release[0]["data"]["value"], 0x7fff);
        assert_eq!(release[2]["data"]["down"], false);
        // Two notches up: each a press and a release, each its own report.
        for notch in &groups[2..6] {
            assert_eq!(notch.last().unwrap()["data"]["button"], "wheel-up");
        }
        assert_eq!(groups[6][0]["data"]["key"]["data"], "a");
        assert_eq!(groups.len(), 7);
    }

    #[test]
    fn adr0072_a_task_runs_the_images_agent_with_its_own_socket() {
        let hv = hypervisor();
        let mut run = TaskRun {
            socket: "/var/run/glasshouse/tasks/t1.sock".into(),
            workdir: "/work/01a1b2c3".into(),
            process: glass_platform::sandbox::ProcessSpec {
                program: "/home/dev/target/debug/glass-desktop-agent".into(),
                args: vec!["--quiet".into()],
                env: vec![
                    ("GLASS_AGENT_SOCKET".into(), AGENT_SOCKET_PATH.into()),
                    (
                        "GLASS_TASK_PROMPT".into(),
                        "it's Dana's; $(rm -rf /)".into(),
                    ),
                ],
            },
        };
        let (socket, line) = hv.task_line(&run).unwrap();
        assert_eq!(socket, "/run/glasshouse/tasks/01a1b2c3.sock");
        assert_eq!(
            line,
            "mkdir -p '/work/01a1b2c3' && cd '/work/01a1b2c3' && env -i \
             GLASS_AGENT_SOCKET='/run/glasshouse/tasks/01a1b2c3.sock' \
             GLASS_TASK_PROMPT='it'\\''s Dana'\\''s; $(rm -rf /)' \
             '/usr/local/libexec/glasshouse/glass-desktop-agent' '--quiet'; \
             status=$?; rm -f '/run/glasshouse/tasks/01a1b2c3.sock'; exit $status"
        );
        // Nothing of the host's path, and nothing that isn't a variable.
        assert!(!line.contains("/home/dev"));
        run.process.env.push(("BAD NAME".into(), "x".into()));
        assert!(hv.task_line(&run).is_err());
        run.process.env.pop();
        run.workdir = "/work/../etc".into();
        assert!(hv.task_line(&run).is_err());
        // Not running: refused, not run somewhere else.
        let dir = tempfile::tempdir().unwrap();
        let spec = ComputerSpec {
            id: "computer".into(),
            owner: 1001,
            dir: dir.path().into(),
            memory_mib: 3072,
            vcpus: 2,
        };
        run.workdir = "/work/01a1b2c3".into();
        assert!(hv.command(&spec, &run).is_err());
        assert!(hv.frame(&spec).is_err());
    }

    #[test]
    fn adr0072_a_computers_machine_is_the_qemu_started_with_its_own_pid_file() {
        // Any QEMU passed for a computer's: after the host restarted, with
        // pids given out again, the pid in one computer's file could be
        // another's machine, and stopping the first stopped that.
        let dir = tempfile::tempdir().unwrap();
        let spec = |name: &str| {
            let dir = dir.path().join(name);
            fs::create_dir_all(&dir).unwrap();
            ComputerSpec {
                id: "computer".into(),
                owner: 1001,
                dir,
                memory_mib: 3072,
                vcpus: 2,
            }
        };
        let (dana, omar) = (spec("dana"), spec("omar"));
        // A stand-in for Dana's QEMU: a process whose command line names
        // QEMU and her pid file, as QEMU's own does.
        let mut machine = Command::new("sh")
            // The shell itself waits (on input that never comes): a
            // command it ran would take its place, command line and all,
            // or outlive it.
            .args(["-c", "read line; exit 0", "qemu-system-x86_64", "-pidfile"])
            .arg(dana.dir.join(PID))
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = machine.id();
        // A process just started has no command line to read for a
        // moment (the kernel fills it in after the start returns here).
        // QEMU writes its pid file once it runs; the stand-in's is
        // written when it shows as running.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while fs::read(format!("/proc/{pid}/cmdline")).map_or(true, |c| c.is_empty()) {
            assert!(
                std::time::Instant::now() < deadline,
                "the stand-in never ran"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        fs::write(dana.dir.join(PID), format!("{pid}\n")).unwrap();
        assert_eq!(QemuHypervisor::pid(&dana), Some(pid));
        // Omar's file names the same process: it isn't his.
        fs::write(omar.dir.join(PID), format!("{pid}\n")).unwrap();
        assert_eq!(QemuHypervisor::pid(&omar), None);
        // Nor is a process that isn't QEMU, or one that has ended.
        fs::write(omar.dir.join(PID), format!("{}\n", std::process::id())).unwrap();
        assert_eq!(QemuHypervisor::pid(&omar), None);
        machine.kill().unwrap();
        machine.wait().unwrap();
        assert_eq!(QemuHypervisor::pid(&dana), None);
    }

    #[test]
    fn adr0072_a_frame_is_a_png_with_its_size() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&1280u32.to_be_bytes());
        png.extend_from_slice(&800u32.to_be_bytes());
        let frame = Frame::from_png(png).unwrap();
        assert_eq!((frame.width, frame.height), (1280, 800));
        assert!(Frame::from_png(b"GIF89a".to_vec()).is_err());
    }
}
