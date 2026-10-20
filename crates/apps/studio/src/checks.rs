//! What glassd checks in an app's source before a version exists (GA-4,
//! ADR 0032): where its files are, what its own code may do, and which
//! actions it calls. The SDK is ours and isn't checked here; the bundler's
//! list of inputs proves nothing else was bundled ([`check_inputs`]).
//!
//! The checks read tokens, not text: comments are skipped and strings are
//! values, so a comment that mentions `innerHTML` is fine and a call to it
//! isn't. They are a fast first answer for whoever is writing the app; the
//! bridge's CSP and glassd's manifest checks still hold whatever passes.

use std::collections::{BTreeMap, BTreeSet};

use glass_api::{AppManifest, AppPermission};
use glass_types::ActionName;

/// Files an app's source may have.
pub const MAX_FILES: usize = 64;
/// One file's largest size.
pub const MAX_FILE_BYTES: usize = 256 * 1024;
/// All of an app's source.
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
/// The entry points every app has.
pub const MAIN: &str = "src/main.ts";
pub const STYLES: &str = "src/app.css";

/// What the checks found: refusals stop the save; notes go with it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Findings {
    pub refusals: Vec<String>,
    pub notes: Vec<String>,
}

impl Findings {
    fn refuse(&mut self, why: impl Into<String>) {
        self.refusals.push(why.into());
    }

    fn note(&mut self, what: impl Into<String>) {
        self.notes.push(what.into());
    }

    pub fn passed(&self) -> bool {
        self.refusals.is_empty()
    }
}

/// Where a source file may be: `src/` and below, letters, digits, `.`,
/// `-` and `_` in each part, TypeScript or CSS. The page itself is ours
/// (the bridge serves one made from the manifest), so it isn't source.
fn path_problem(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    let named = parts.iter().all(|p| {
        !p.is_empty()
            && !p.starts_with('.')
            && p.len() <= 64
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    });
    if !named || parts.len() > 5 {
        return Some(format!(
            "{path}: name files with letters, digits, dots, dashes and underscores, under src/"
        ));
    }
    if parts[0] != "src" || parts.len() < 2 {
        return Some(format!("{path}: an app's source lives under src/"));
    }
    if !(path.ends_with(".ts") || path.ends_with(".css")) {
        return Some(format!(
            "{path}: only TypeScript (.ts) and CSS (.css); the page is made from the manifest"
        ));
    }
    None
}

/// Checks `files` (path → text) as an app with `manifest`: paths, sizes,
/// what its own code does and which actions it calls (GA-4).
pub fn check_source(manifest: &AppManifest, files: &BTreeMap<String, String>) -> Findings {
    let mut found = Findings::default();
    if files.len() > MAX_FILES {
        found.refuse(format!(
            "{} files; an app has at most {MAX_FILES}",
            files.len()
        ));
    }
    let total: usize = files.values().map(String::len).sum();
    if total > MAX_SOURCE_BYTES {
        found.refuse(format!(
            "{} KB of source; an app has at most {} KB",
            total / 1024,
            MAX_SOURCE_BYTES / 1024
        ));
    }
    if !files.contains_key(MAIN) {
        found.refuse(format!("{MAIN} is missing: it's where every app starts"));
    }
    let mut called = BTreeSet::new();
    let mut by_name = false;
    for (path, text) in files {
        if let Some(problem) = path_problem(path) {
            found.refuse(problem);
            continue;
        }
        if text.len() > MAX_FILE_BYTES {
            found.refuse(format!(
                "{path}: {} KB; a file has at most {} KB",
                text.len() / 1024,
                MAX_FILE_BYTES / 1024
            ));
            continue;
        }
        if path.ends_with(".css") {
            check_css(path, text, &mut found);
        } else {
            let scan = scan_script(text);
            for (line, rule) in &scan.forbidden {
                if *rule == Rule::Media && manifest.may(AppPermission::Media) {
                    continue;
                }
                found.refuse(format!("{path}:{line}: {}", rule.why()));
            }
            called.extend(scan.actions);
            by_name |= scan.by_name;
        }
    }
    for action in &called {
        match action.parse::<ActionName>() {
            Ok(name) if manifest.declares(&name) => {}
            _ => found.refuse(format!(
                "calls {action}, which the manifest doesn't declare; declare it or don't call it"
            )),
        }
    }
    let unused: Vec<&str> = manifest
        .actions
        .iter()
        .map(ActionName::as_str)
        .filter(|a| !called.contains(*a))
        .collect();
    if !unused.is_empty() && !by_name {
        found.note(format!(
            "declares {} but never calls {}; an app should declare only what it uses",
            unused.join(", "),
            if unused.len() == 1 { "it" } else { "them" }
        ));
    }
    if by_name {
        found.note(
            "calls an action by a name the check can't read; the manifest still limits it"
                .to_owned(),
        );
    }
    found
}

/// Something an app's own code may not do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    /// `eval`, `new Function`, timers given strings: code made from text.
    CodeFromText,
    /// `import()`: code fetched at run time.
    DynamicImport,
    /// Markup from strings: `innerHTML` and the like.
    Markup,
    /// Its own connections: `fetch`, WebSocket and the like.
    Network,
    /// Storage of its own: app data goes through actions (GA-6).
    Storage,
    /// Workers.
    Workers,
    /// WebRTC, for meeting apps only.
    Media,
}

impl Rule {
    fn why(self) -> &'static str {
        match self {
            Self::CodeFromText => "makes code from text (eval, new Function, a string for a timer)",
            Self::DynamicImport => "imports at run time; import what the app needs at the top",
            Self::Markup => {
                "builds markup from a string; use h() and show() from the SDK, which make text only"
            }
            Self::Network => {
                "opens its own connection; apps reach the system only through actions (bridge.call)"
            }
            Self::Storage => {
                "keeps data of its own on this computer; app data goes through actions (GA-6)"
            }
            Self::Workers => "starts a worker; workers aren't supported in apps people make yet",
            Self::Media => "opens a WebRTC connection; only a meeting app (permission media) may",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    Str(String),
    Punct(char),
}

/// Whether a `/` after `previous` starts a regular expression rather than
/// dividing: where an expression is expected.
fn starts_regex(previous: Option<&Token>) -> bool {
    match previous {
        None => true,
        Some(Token::Punct(c)) => "(,=:[!&|?{};+-*%<>~^".contains(*c),
        Some(Token::Ident(word)) => matches!(
            word.as_str(),
            "return"
                | "typeof"
                | "case"
                | "do"
                | "else"
                | "in"
                | "of"
                | "new"
                | "delete"
                | "void"
                | "throw"
                | "yield"
                | "await"
        ),
        Some(Token::Str(_)) => false,
    }
}

/// A script's tokens with their lines: identifiers, string values (template
/// literals whole, their `${}` parts included as text; regular expressions
/// as strings) and punctuation.
fn tokens(text: &str) -> Vec<(usize, Token)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<(usize, Token)> = Vec::new();
    let (mut i, mut line) = (0, 1);
    while i < chars.len() {
        let c = chars[i];
        match c {
            '/' if chars.get(i + 1) != Some(&'/')
                && chars.get(i + 1) != Some(&'*')
                && starts_regex(out.last().map(|(_, t)| t)) =>
            {
                // A regular expression: to its closing `/`, outside any
                // character class, then its flags.
                let mut value = String::new();
                let mut class = false;
                i += 1;
                while i < chars.len() && chars[i] != '\n' && (class || chars[i] != '/') {
                    match chars[i] {
                        '\\' if i + 1 < chars.len() => {
                            value.push(chars[i + 1]);
                            i += 1;
                        }
                        '[' => class = true,
                        ']' => class = false,
                        _ => {}
                    }
                    value.push(chars[i]);
                    i += 1;
                }
                i += 1;
                while i < chars.len() && chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                out.push((line, Token::Str(value)));
            }
            '\n' => {
                line += 1;
                i += 1;
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                    if chars[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i += 2;
            }
            '"' | '\'' | '`' => {
                let start = line;
                let mut value = String::new();
                i += 1;
                while i < chars.len() && chars[i] != c {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        value.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    if chars[i] == '\n' {
                        line += 1;
                    }
                    value.push(chars[i]);
                    i += 1;
                }
                i += 1;
                out.push((start, Token::Str(value)));
            }
            c if c.is_alphabetic() || c == '_' || c == '$' => {
                let mut word = String::new();
                while i < chars.len()
                    && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$')
                {
                    word.push(chars[i]);
                    i += 1;
                }
                out.push((line, Token::Ident(word)));
            }
            // A number reads as a word, so a `/` after it divides.
            c if c.is_ascii_digit() => {
                let mut number = String::new();
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                    number.push(chars[i]);
                    i += 1;
                }
                out.push((line, Token::Ident(number)));
            }
            c if c.is_whitespace() => i += 1,
            c => {
                out.push((line, Token::Punct(c)));
                i += 1;
            }
        }
    }
    out
}

#[derive(Debug, Default)]
struct Scan {
    /// (line, rule) for each thing the code may not do.
    forbidden: Vec<(usize, Rule)>,
    /// Action names called literally: `.call("mail.send", …)`.
    actions: BTreeSet<String>,
    /// Some call names its action with something other than a literal.
    by_name: bool,
}

fn is_action_name(s: &str) -> bool {
    s.contains('.')
        && s.split('.').all(|part| {
            part.starts_with(|c: char| c.is_ascii_lowercase())
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        })
}

fn scan_script(text: &str) -> Scan {
    let tokens = tokens(text);
    let mut scan = Scan::default();
    let ident = |i: usize| match tokens.get(i) {
        Some((_, Token::Ident(w))) => Some(w.as_str()),
        _ => None,
    };
    let punct = |i: usize, c: char| matches!(tokens.get(i), Some((_, Token::Punct(p))) if *p == c);
    for (i, (line, token)) in tokens.iter().enumerate() {
        let Token::Ident(word) = token else {
            continue;
        };
        let after_dot = i > 0 && punct(i - 1, '.');
        let opens = punct(i + 1, '(');
        let rule = match word.as_str() {
            "eval" if opens && !after_dot => Some(Rule::CodeFromText),
            "Function" if opens || (i > 0 && ident(i - 1) == Some("new")) => {
                Some(Rule::CodeFromText)
            }
            "setTimeout" | "setInterval"
                if opens && matches!(tokens.get(i + 2), Some((_, Token::Str(_)))) =>
            {
                Some(Rule::CodeFromText)
            }
            "import" if opens => Some(Rule::DynamicImport),
            "innerHTML"
            | "outerHTML"
            | "insertAdjacentHTML"
            | "srcdoc"
            | "setHTMLUnsafe"
            | "createContextualFragment"
                if after_dot =>
            {
                Some(Rule::Markup)
            }
            "write" | "writeln" if after_dot && i > 1 && ident(i - 2) == Some("document") => {
                Some(Rule::Markup)
            }
            "DOMParser" => Some(Rule::Markup),
            "fetch" if opens && !after_dot => Some(Rule::Network),
            "XMLHttpRequest" | "WebSocket" | "EventSource" | "WebTransport" => Some(Rule::Network),
            "sendBeacon" if after_dot => Some(Rule::Network),
            "localStorage" | "sessionStorage" | "indexedDB" => Some(Rule::Storage),
            "cookie" if after_dot && i > 1 && ident(i - 2) == Some("document") => {
                Some(Rule::Storage)
            }
            "caches" if !after_dot => Some(Rule::Storage),
            "Worker" | "SharedWorker" | "importScripts" => Some(Rule::Workers),
            "serviceWorker" if after_dot => Some(Rule::Workers),
            "RTCPeerConnection" => Some(Rule::Media),
            "call" | "read" if after_dot => {
                // `.call<T>(` or `.call(`: skip a type argument.
                let mut j = i + 1;
                if punct(j, '<') {
                    let mut depth = 0;
                    while j < tokens.len() {
                        if punct(j, '<') {
                            depth += 1;
                        } else if punct(j, '>') {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        j += 1;
                    }
                    j += 1;
                }
                if punct(j, '(') {
                    match tokens.get(j + 1) {
                        Some((_, Token::Str(name))) if is_action_name(name) => {
                            scan.actions.insert(name.clone());
                        }
                        Some((_, Token::Str(_))) => {}
                        // `fn.call(this, …)` reads the same: a note, never
                        // a refusal, since glassd limits calls anyway.
                        Some(_) => scan.by_name = true,
                        None => {}
                    }
                }
                None
            }
            _ => None,
        };
        if let Some(rule) = rule {
            scan.forbidden.push((*line, rule));
        }
    }
    scan
}

/// CSS: nothing from elsewhere. Imports of the app's own files and the
/// SDK's stylesheet are the bundler's to resolve.
fn check_css(path: &str, text: &str, found: &mut Findings) {
    let mut without_comments = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        without_comments.push_str(&rest[..start]);
        rest = rest[start + 2..]
            .find("*/")
            .map_or("", |end| &rest[start + 2 + end + 2..]);
    }
    without_comments.push_str(rest);
    let lower = without_comments.to_ascii_lowercase();
    for (n, line) in lower.lines().enumerate() {
        let remote = ["http:", "https:", "//", "data:"].iter().any(|scheme| {
            ["url(", "url(\"", "url('", "@import \"", "@import '"]
                .iter()
                .any(|open| line.contains(&format!("{open}{scheme}")))
        });
        if remote {
            found.refuse(format!(
                "{path}:{}: loads something from elsewhere; an app's styles are its own",
                n + 1
            ));
        }
        if line.contains("expression(") || line.contains("javascript:") {
            found.refuse(format!("{path}:{}: runs script from a style", n + 1));
        }
    }
}

/// The bundler's inputs (paths relative to the build folder) are the app's
/// own source and the SDK, nothing else: no library from elsewhere and no
/// file outside the app (GA-4, "only pre-approved libraries").
pub fn check_inputs<'a>(inputs: impl IntoIterator<Item = &'a str>) -> Findings {
    let mut found = Findings::default();
    for input in inputs {
        let inside = |root: &str| {
            input.strip_prefix(root).is_some_and(|rest| {
                !rest.is_empty() && rest.split('/').all(|p| !p.is_empty() && p != "..")
            })
        };
        if !(inside(crate::bundle::APP_DIR) || inside(crate::bundle::SDK_DIR)) {
            found.refuse(format!(
                "uses {input}; an app may use only its own files and the Glasshouse SDK"
            ));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(actions: &[&str]) -> AppManifest {
        serde_json::from_value(serde_json::json!({
            "id": "my-mail",
            "name": "My mail",
            "version": "1",
            "actions": actions,
            "labels": ["internal"],
        }))
        .unwrap()
    }

    fn source(main: &str) -> BTreeMap<String, String> {
        BTreeMap::from([(MAIN.to_owned(), main.to_owned())])
    }

    #[test]
    fn ga4_an_app_calls_only_what_it_declares() {
        let main = r#"
            const bridge = new Bridge();
            await bridge.read<Mail[]>("mail.list", {});
            await bridge.call("mail.send", { to });
        "#;
        let found = check_source(&manifest(&["mail.list", "mail.send"]), &source(main));
        assert!(found.passed(), "{found:?}");
        assert!(found.notes.is_empty(), "{found:?}");

        let found = check_source(&manifest(&["mail.list"]), &source(main));
        assert_eq!(
            found.refusals,
            ["calls mail.send, which the manifest doesn't declare; declare it or don't call it"]
        );
        let found = check_source(
            &manifest(&["mail.list", "mail.send", "files.read"]),
            &source(main),
        );
        assert!(found.passed());
        assert!(found.notes[0].contains("never calls it"), "{found:?}");
    }

    #[test]
    fn ga4_code_from_text_markup_and_connections_are_refused() {
        for (code, rule) in [
            ("eval(text)", "makes code from text"),
            ("new Function('a', body)", "makes code from text"),
            ("setTimeout(\"go()\", 5)", "makes code from text"),
            ("await import('./late.ts')", "imports at run time"),
            ("node.innerHTML = subject", "builds markup"),
            ("el.insertAdjacentHTML('beforeend', x)", "builds markup"),
            ("document.write(x)", "builds markup"),
            ("fetch('https://example.com')", "opens its own connection"),
            ("new WebSocket(url)", "opens its own connection"),
            ("navigator.sendBeacon(u, d)", "opens its own connection"),
            ("localStorage.setItem('k', v)", "keeps data of its own"),
            ("document.cookie = 'a=b'", "keeps data of its own"),
            ("new Worker('./w.js')", "starts a worker"),
            ("new RTCPeerConnection()", "WebRTC"),
            // A regular expression with a quote in it hides nothing.
            (
                "const q = /\"/; eval(x); const r = /\"/;",
                "makes code from text",
            ),
        ] {
            let found = check_source(&manifest(&["mail.list"]), &source(code));
            assert!(
                found.refusals.iter().any(|r| r.contains(rule)),
                "{code}: {found:?}"
            );
        }
    }

    #[test]
    fn comments_strings_and_lookalikes_pass() {
        let main = r#"
            // Never use innerHTML here; eval( is bad. fetch( too.
            /* document.write(x) */
            const tip = "Don't eval(anything) or fetch(it)";
            const timer = setTimeout(() => refresh(), 5);
            items.forEach((item) => show(item.text));
            handler.call(this, event);
            const meta = import.meta.url;
            const quotes = /["'`]/g;
            const half = total / 2 / count;
            const third = 3 / parts.length; eval;
        "#;
        let found = check_source(&manifest(&["mail.list"]), &source(main));
        assert!(found.passed(), "{found:?}");
        assert!(
            found.notes.iter().any(|n| n.contains("by a name")),
            "{found:?}"
        );
    }

    #[test]
    fn media_is_for_meeting_apps() {
        let code = "const pc = new RTCPeerConnection({});";
        let mut meeting = manifest(&["meet.join"]);
        meeting.permissions.push(AppPermission::Media);
        assert!(check_source(&meeting, &source(code)).passed());
    }

    #[test]
    fn files_live_under_src_as_typescript_or_css() {
        for bad in [
            "main.ts",
            "src/../etc/passwd.ts",
            "src/.hidden.ts",
            "src/page.html",
            "src//x.ts",
            "src/a b.ts",
            "/src/x.ts",
        ] {
            let mut files = source("");
            files.insert(bad.to_owned(), String::new());
            assert!(
                !check_source(&manifest(&["mail.list"]), &files).passed(),
                "{bad}"
            );
        }
        let mut files = source("");
        files.insert("src/lib/week.ts".into(), String::new());
        files.insert(
            STYLES.into(),
            "@import \"@glasshouse/sdk/base.css\";".into(),
        );
        assert!(check_source(&manifest(&["mail.list"]), &files).passed());
        assert!(!check_source(&manifest(&["mail.list"]), &BTreeMap::new()).passed());
    }

    #[test]
    fn styles_load_nothing_from_elsewhere() {
        for bad in [
            "a { background: url(https://x.example/i.png) }",
            "@import 'https://x.example/a.css';",
            "a { background: url(\"//x.example/i.png\") }",
            "a { width: expression(alert(1)) }",
        ] {
            let mut files = source("");
            files.insert(STYLES.into(), bad.into());
            assert!(
                !check_source(&manifest(&["mail.list"]), &files).passed(),
                "{bad}"
            );
        }
        let mut files = source("");
        files.insert(
            STYLES.into(),
            "/* url(https://x) */ @import \"../../sdk/src/base.css\"; a { color: red }".into(),
        );
        assert!(check_source(&manifest(&["mail.list"]), &files).passed());
    }

    #[test]
    fn ga4_only_the_apps_files_and_the_sdk_are_bundled() {
        assert!(check_inputs(["app/src/main.ts", "sdk/src/index.ts", "sdk/src/base.css"]).passed());
        for bad in [
            "../home/alice/notes.txt",
            "app/../../etc/passwd",
            "node_modules/left-pad/index.js",
            "/etc/passwd",
            "app/",
        ] {
            assert!(!check_inputs([bad]).passed(), "{bad}");
        }
    }

    /// Our own apps are what people start from (ADR 0032), so their source
    /// must pass the checks a remix of them gets, unchanged. Meet's worker
    /// waits for workers in apps people make.
    #[test]
    fn tl5_first_party_apps_pass_their_own_checks() {
        let apps = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
        for app in ["mail", "calendar", "chat", "settings", "store", "artifacts"] {
            let dir = apps.join(app);
            let manifest: AppManifest =
                serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap())
                    .unwrap();
            let files = crate::store::read_tree(&dir, "src").unwrap();
            let found = check_source(&manifest, &files);
            assert!(found.passed(), "{app}: {:?}", found.refusals);
        }
    }
}
