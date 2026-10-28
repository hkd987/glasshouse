//! Embeds the Agents web app (apps/agents: its page, script and style) so
//! the app is one file. `make agents-app` builds the web app first; built
//! without it, the app says so instead of showing an empty window.

use std::path::Path;

fn main() {
    let out = std::env::var("OUT_DIR").unwrap_or_default();
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/agents");
    for (from, to) in [
        ("index.html", "index.html"),
        ("dist/app.js", "app.js"),
        ("dist/app.css", "app.css"),
    ] {
        let source = web.join(from);
        println!("cargo:rerun-if-changed={}", source.display());
        let bytes = std::fs::read(&source).unwrap_or_default();
        if let Err(e) = std::fs::write(Path::new(&out).join(to), bytes) {
            panic!("writing {to} into OUT_DIR: {e}");
        }
    }
}
