//! The Glasshouse Agents API from a person's own computer (ADR 0066): what
//! the `glass` CLI and the desktop app call. HTTPS to the company's stack,
//! or plain HTTP to a front on this machine's loopback address in
//! development. Both sign in through the browser the same way ([`SignIn`],
//! ADR 0067, 0071).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use crate::http::Http;
use glass_gateway::ProviderError;

/// The front at `url`, called with `token`.
#[derive(Debug)]
pub struct AgentsClient {
    http: Http,
    url: String,
    token: String,
}

/// What the front answered: its status and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub body: String,
}

impl AgentsClient {
    /// `url` is `https://agents.example.com`, or `http://127.0.0.1:<port>`
    /// for a front on this machine; nothing else goes over plain HTTP.
    pub fn new(url: &str, token: &str) -> Result<Self, ProviderError> {
        let url = url.trim_end_matches('/');
        let http = if url.starts_with("https://") {
            Http::new()
        } else if url.starts_with("http://127.0.0.1:")
            || url.starts_with("http://localhost:")
            || url.starts_with("http://[::1]:")
        {
            Http::loopback()
        } else {
            return Err(ProviderError::Rejected(format!(
                "{url} isn't HTTPS: Glasshouse Agents is only reached over HTTPS"
            )));
        };
        Ok(Self {
            http: http.outwaiting_providers(),
            url: url.to_owned(),
            token: token.to_owned(),
        })
    }

    /// `GET /api/v1/<path>`.
    pub fn get(&self, path: &str) -> Result<Answer, ProviderError> {
        self.call("GET", path, b"")
    }

    /// `POST /api/v1/<path>` with a JSON body.
    pub fn post(&self, path: &str, body: &serde_json::Value) -> Result<Answer, ProviderError> {
        self.call("POST", path, body.to_string().as_bytes())
    }

    pub fn delete(&self, path: &str) -> Result<Answer, ProviderError> {
        self.call("DELETE", path, b"")
    }

    /// `<method> /api/v1/<path>` with `body` as it is (JSON when there is
    /// one): the desktop app passes its web app's calls on so.
    pub fn send(&self, method: &str, path: &str, body: &[u8]) -> Result<Answer, ProviderError> {
        if !matches!(method, "GET" | "POST" | "PUT" | "DELETE") {
            return Err(ProviderError::Rejected(format!(
                "{method} isn't a method the API takes"
            )));
        }
        self.call(method, path, body)
    }

    fn call(&self, method: &str, path: &str, body: &[u8]) -> Result<Answer, ProviderError> {
        let url = format!("{}/api/v1/{}", self.url, path.trim_start_matches('/'));
        let auth = format!("Bearer {}", self.token);
        let mut headers = vec![
            ("Authorization", auth.as_str()),
            ("Accept", "application/json"),
        ];
        if method == "POST" || method == "PUT" {
            headers.push(("Content-Type", "application/json"));
        }
        let reply = self.http.send(method, &url, &headers, body)?;
        Ok(Answer {
            status: reply.status,
            body: reply.body,
        })
    }
}

/// What a one-time code from the front's sign-in page starts with
/// (glassd's `token.code`).
const CODE_PREFIX: &str = "gac_";

/// Who signs in, as the front's page names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    /// The `glass` CLI.
    Glass,
    /// The desktop app (ADR 0071).
    App,
}

impl Client {
    /// As the page and the browser's last words say it.
    pub fn name(self) -> &'static str {
        match self {
            Client::Glass => "glass",
            Client::App => "the Glasshouse Agents app",
        }
    }
}

/// Why signing in didn't finish, said for the person.
#[derive(Debug, thiserror::Error)]
pub enum SignInError {
    #[error("{0}")]
    Address(String),
    #[error("waiting for the browser: {0}")]
    Listening(#[from] std::io::Error),
    #[error("the browser didn't come back within {0} minutes: sign in again")]
    TimedOut(u64),
    #[error("Glasshouse Agents didn't sign you in: {0}")]
    Refused(String),
}

/// Signing in through the browser (ADR 0067): this computer waits on its
/// loopback address while the person signs in at their organization's
/// sign-in and allows the client; the front sends a one-time code back
/// here, traded once for a token. The token never passes through the
/// browser.
#[derive(Debug)]
pub struct SignIn {
    listener: TcpListener,
    state: String,
    url: String,
    client: Client,
    page: String,
}

impl SignIn {
    /// Starts waiting. Open [`SignIn::page`] in the person's browser.
    pub fn start(url: &str, client: Client) -> Result<Self, SignInError> {
        let url = url.trim_end_matches('/').to_owned();
        AgentsClient::new(&url, "").map_err(|e| match e {
            ProviderError::Rejected(why) => SignInError::Address(why),
            other => SignInError::Address(other.to_string()),
        })?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let state = hex::encode(
            glass_crypto::random::<16>()
                .map_err(|e| SignInError::Listening(std::io::Error::other(e.to_string())))?,
        );
        let app = match client {
            Client::Glass => "",
            Client::App => "&app=desktop",
        };
        let page = format!(
            "{url}/login/cli?port={port}&state={state}&host={}{app}",
            computer_name()
        );
        Ok(Self {
            listener,
            state,
            url,
            client,
            page,
        })
    }

    /// The front's page that asks the person to allow this client.
    pub fn page(&self) -> &str {
        &self.page
    }

    /// Waits up to `wait` for the browser to bring the code, then trades
    /// it for a token.
    pub fn finish(self, wait: Duration) -> Result<String, SignInError> {
        let code = self.wait_for_code(wait)?;
        let exchange =
            AgentsClient::new(&self.url, &code).map_err(|e| SignInError::Address(e.to_string()))?;
        let answer = exchange
            .post("tokens", &serde_json::json!({}))
            .map_err(|e| SignInError::Refused(e.to_string()))?;
        let body: serde_json::Value = serde_json::from_str(&answer.body).unwrap_or_default();
        if !(200..300).contains(&answer.status) {
            let said = body["error"]
                .as_str()
                .unwrap_or("it answered with an error");
            return Err(SignInError::Refused(said.to_owned()));
        }
        body["token"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| SignInError::Refused("it answered without a token".into()))
    }

    /// The code the browser brings to this computer's loopback address.
    /// Anything else that calls there is told so, and waited past.
    fn wait_for_code(&self, wait: Duration) -> Result<String, SignInError> {
        self.listener.set_nonblocking(true)?;
        let started = Instant::now();
        loop {
            if started.elapsed() > wait {
                return Err(SignInError::TimedOut(wait.as_secs().div_ceil(60).max(1)));
            }
            let mut stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            stream.set_nonblocking(false)?;
            let request = request_head(&mut stream);
            let request = String::from_utf8_lossy(&request);
            let target = request
                .lines()
                .next()
                .and_then(|line| line.strip_prefix("GET "))
                .and_then(|rest| rest.split(' ').next())
                .unwrap_or_default();
            let field = |name: &str| {
                target
                    .split_once('?')
                    .map(|(_, q)| q)
                    .unwrap_or_default()
                    .split('&')
                    .filter_map(|pair| pair.split_once('='))
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_owned())
            };
            let ours =
                target.starts_with("/callback?") && field("state").as_deref() == Some(&self.state);
            let code = field("code").filter(|c| ours && c.starts_with(CODE_PREFIX));
            let said = if code.is_some() {
                format!(
                    "Signed in. You can close this tab and go back to {}.",
                    self.client.name()
                )
            } else {
                format!(
                    "That isn't the sign-in {} is waiting for.",
                    self.client.name()
                )
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{said}",
                said.len()
            );
            if let Some(code) = code {
                return Ok(code);
            }
        }
    }
}

/// How long a caller of the sign-in's port has to send its request.
const REQUEST_WITHIN: Duration = Duration::from_secs(10);
/// The most of a request that is read: its first line is all that's used.
const REQUEST_MOST: usize = 8192;

/// A request's head (to its blank line), as far as it comes in
/// [`REQUEST_WITHIN`] and [`REQUEST_MOST`].
///
/// One `read` gives what has arrived so far, which needn't be the whole
/// request: a browser's usually is, and a request that came in two pieces
/// was read as its first piece, found not to be the sign-in's, answered
/// "That isn't the sign-in…" and closed with the rest still on its way.
/// The person then had to sign in again.
fn request_head(stream: &mut std::net::TcpStream) -> Vec<u8> {
    let started = Instant::now();
    let mut request = Vec::new();
    let mut chunk = [0u8; 2048];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < REQUEST_MOST {
        let Some(left) = REQUEST_WITHIN
            .checked_sub(started.elapsed())
            .filter(|left| !left.is_zero())
        else {
            break;
        };
        if stream.set_read_timeout(Some(left)).is_err() {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => request.extend_from_slice(&chunk[..n]),
        }
    }
    request
}

/// This computer's name, as the front's page shows it: letters, digits,
/// `-`, `.` and `_` only.
pub fn computer_name() -> String {
    let named = std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/proc/sys/kernel/hostname").ok())
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        })
        .unwrap_or_default();
    let name: String = named
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "-._".contains(*c))
        .take(64)
        .collect();
    if name.is_empty() {
        "this-computer".into()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::serve_each;

    #[test]
    fn adr0066_the_cli_calls_the_front_with_its_token() {
        let (url, requests) = serve_each(vec![(200, r#"[]"#), (200, r#"{"task":"t"}"#)]);
        let client = AgentsClient::new(&url, "secret-token").unwrap();
        let got = client.get("agents").unwrap();
        assert_eq!(
            got,
            Answer {
                status: 200,
                body: "[]".into()
            }
        );
        let asked = client
            .post(
                "agents/reconciler/ask",
                &serde_json::json!({"text": "Match March"}),
            )
            .unwrap();
        assert_eq!(asked.body, r#"{"task":"t"}"#);

        let first = requests.recv().unwrap();
        assert!(
            first.head.starts_with("GET /api/v1/agents HTTP/1.1"),
            "{}",
            first.head
        );
        assert!(first.head.contains("Authorization: Bearer secret-token"));
        let second = requests.recv().unwrap();
        assert!(
            second
                .head
                .starts_with("POST /api/v1/agents/reconciler/ask")
        );
        assert!(second.head.contains("Content-Type: application/json"));
        assert_eq!(second.body, r#"{"text":"Match March"}"#);
    }

    /// The browser's part: the front's redirect to this computer.
    fn browser_comes_back(page: &str, code: &str, state: Option<&str>) -> String {
        let field = |name: &str| {
            page.split_once('?')
                .unwrap()
                .1
                .split('&')
                .find_map(|p| p.strip_prefix(&format!("{name}=")))
                .unwrap()
                .to_owned()
        };
        let port = field("port");
        let state = state.map(str::to_owned).unwrap_or_else(|| field("state"));
        let mut stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        write!(
            stream,
            "GET /callback?code={code}&state={state} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"
        )
        .unwrap();
        let mut said = String::new();
        stream.read_to_string(&mut said).unwrap();
        said
    }

    #[test]
    fn adr0071_the_app_signs_in_as_glass_does_and_the_page_names_it() {
        let (url, requests) = serve_each(vec![(200, r#"{"token":"gat_kept"}"#)]);
        let sign_in = SignIn::start(&url, Client::App).unwrap();
        let page = sign_in.page().to_owned();
        assert!(
            page.starts_with(&format!("{url}/login/cli?port=")),
            "{page}"
        );
        assert!(page.ends_with("&app=desktop"), "{page}");
        let waiting = std::thread::spawn(move || sign_in.finish(Duration::from_secs(10)));
        // Another call to the port, or the wrong sign-in's state, is waited past.
        let other = browser_comes_back(&page, "gac_other", Some("0000000000000000"));
        assert!(
            other.ends_with("That isn't the sign-in the Glasshouse Agents app is waiting for.")
        );
        // The browser's request needn't arrive at once: one that comes in
        // two pieces is the same request. (Read as its first piece, it was
        // "not the sign-in" and the connection closed under the second.)
        let said = {
            let port = page
                .split_once("port=")
                .and_then(|(_, rest)| rest.split('&').next())
                .unwrap();
            let state = page
                .split_once("state=")
                .and_then(|(_, rest)| rest.split('&').next())
                .unwrap();
            let mut stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
            stream.write_all(b"GET /callback?code=gac_once").unwrap();
            stream.flush().unwrap();
            std::thread::sleep(Duration::from_millis(400));
            stream
                .write_all(format!("&state={state} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
                .unwrap();
            let mut said = String::new();
            stream.read_to_string(&mut said).unwrap();
            said
        };
        assert!(
            said.ends_with("go back to the Glasshouse Agents app."),
            "{said}"
        );
        assert_eq!(waiting.join().unwrap().unwrap(), "gat_kept");
        // The code, traded once as the bearer.
        let exchange = requests.recv().unwrap();
        assert!(exchange.head.starts_with("POST /api/v1/tokens"));
        assert!(exchange.head.contains("Authorization: Bearer gac_once"));
    }

    #[test]
    fn adr0067_glass_signs_in_and_hears_a_refusal_in_words() {
        let (url, _requests) = serve_each(vec![(401, r#"{"error":"that code was used"}"#)]);
        let sign_in = SignIn::start(&url, Client::Glass).unwrap();
        let page = sign_in.page().to_owned();
        assert!(!page.contains("app="), "{page}");
        let waiting = std::thread::spawn(move || sign_in.finish(Duration::from_secs(10)));
        browser_comes_back(&page, "gac_used", None);
        let refused = waiting.join().unwrap().unwrap_err();
        assert_eq!(
            refused.to_string(),
            "Glasshouse Agents didn't sign you in: that code was used"
        );
        assert!(matches!(
            SignIn::start("http://agents.example.com", Client::App),
            Err(SignInError::Address(_))
        ));
        let quiet = SignIn::start(&url, Client::Glass).unwrap();
        assert!(matches!(
            quiet.finish(Duration::from_millis(300)),
            Err(SignInError::TimedOut(1))
        ));
    }

    #[test]
    fn adr0066_only_https_leaves_this_machine() {
        assert!(AgentsClient::new("http://agents.example.com", "t").is_err());
        assert!(AgentsClient::new("https://agents.example.com", "t").is_ok());
        assert!(AgentsClient::new("http://127.0.0.1:7500", "t").is_ok());
    }
}
