//! `glass-app-try <dir>`: tries the app in `<dir>` (`app.js`, as the
//! bundler made it, and `try.json`, its [`glass_app_try::Setup`]) and
//! writes how it went to `<dir>/tried.json`. glassd runs it in the
//! bundler's jail as nobody, with the folder as its whole filesystem.

use std::path::Path;
use std::process::ExitCode;

/// The largest bundle or setup read.
const MAX_INPUT_BYTES: u64 = 8 * 1024 * 1024;

fn main() -> ExitCode {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: glass-app-try <dir>");
        return ExitCode::FAILURE;
    };
    match run(Path::new(&dir)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("glass-app-try: {e}");
            ExitCode::FAILURE
        }
    }
}

fn read(path: &Path) -> Result<String, String> {
    let size = std::fs::metadata(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .len();
    if size > MAX_INPUT_BYTES {
        return Err(format!("{} is too big", path.display()));
    }
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn run(dir: &Path) -> Result<(), String> {
    let bundle = read(&dir.join("app.js"))?;
    let setup: glass_app_try::Setup = serde_json::from_str(&read(&dir.join("try.json"))?)
        .map_err(|e| format!("try.json: {e}"))?;
    let report = glass_app_try::try_app(&bundle, &setup)?;
    let text = serde_json::to_string(&report).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("tried.json"), text).map_err(|e| format!("tried.json: {e}"))
}
