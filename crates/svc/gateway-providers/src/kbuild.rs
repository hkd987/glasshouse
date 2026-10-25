//! The kernel patch build service's client (ADR 0057, slice 2): the one
//! place a computer reaches it. The service holds the kernel's source and
//! the compiler; a computer sends it what an agent searches, reads and
//! wrote, and gets back text or a signed bundle.

use glass_managed::kbuild::{
    BuildRequest, BuildResult, SourceFound, SourceRead, SourceSearch, SourceText,
};

use crate::console::is_loopback;
use crate::http::{Http, Target};

/// The largest answer: a bundle that carries a rebuilt kernel.
const MAX_ANSWER: usize = 160 * 1024 * 1024;

#[derive(Debug)]
pub struct BuildClient {
    http: Http,
    base: String,
    token: String,
}

impl BuildClient {
    /// A client for the service at `url`, with `token`. Plain `http://` is
    /// refused unless `allow_insecure_http` is set and the host is this
    /// machine (development: the service is the host's, through a tunnel).
    pub fn new(url: &str, token: &str, allow_insecure_http: bool) -> Result<Self, String> {
        let base = url.trim_end_matches('/').to_owned();
        let target = Target::parse(&format!("{base}/"))
            .ok_or_else(|| format!("the build service's address {url:?} isn't an http(s) URL"))?;
        let http = if target.tls {
            Http::new()
        } else if allow_insecure_http && is_loopback(&target.host) {
            Http::loopback()
        } else {
            return Err(format!(
                "the build service's address {url:?} must use HTTPS (plain HTTP only to this \
                 machine, with allow_insecure_http, for development)"
            ));
        };
        if token.is_empty() || token.contains(['\r', '\n']) {
            return Err("the build service's token is empty or has a line break".into());
        }
        Ok(Self {
            http: http.with_max_reply(MAX_ANSWER),
            base,
            token: token.to_owned(),
        })
    }

    pub fn search(&self, request: &SourceSearch) -> Result<SourceFound, String> {
        self.post("/v1/source/search", request)
    }

    pub fn read(&self, request: &SourceRead) -> Result<SourceText, String> {
        self.post("/v1/source/read", request)
    }

    /// Builds a patch: the compiler's errors, or the bundle. A build of the
    /// kernel takes minutes.
    pub fn build(&self, request: &BuildRequest) -> Result<BuildResult, String> {
        self.post("/v1/build", request)
    }

    fn post<T: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<R, String> {
        let body = serde_json::to_vec(body).map_err(|e| e.to_string())?;
        let auth = format!("Bearer {}", self.token);
        let reply = self
            .http
            .post(
                &format!("{}{path}", self.base),
                &[
                    ("Content-Type", "application/json"),
                    ("Authorization", &auth),
                ],
                &body,
            )
            .map_err(|e| format!("the build service: {e}"))?;
        if reply.status != 200 {
            let why = serde_json::from_str::<serde_json::Value>(&reply.body)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_owned))
                .unwrap_or_else(|| format!("HTTP {}", reply.status));
            return Err(format!("the build service: {why}"));
        }
        serde_json::from_str(&reply.body)
            .map_err(|e| format!("the build service's answer doesn't parse: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::serve;

    #[test]
    fn adr0057_the_build_service_is_asked_with_its_token() {
        let (url, requests) = serve(
            200,
            r#"{"matches":[{"file":"sys/dev/a.c","line":3,"text":"x"}],"truncated":false}"#,
        );
        let client = BuildClient::new(&url, "dev-token", true).unwrap();
        let found = client
            .search(&SourceSearch {
                release: "15.1-RELEASE".into(),
                pattern: "suspend".into(),
                path: "dev".into(),
            })
            .unwrap();
        assert_eq!(found.matches[0].line, 3);
        let request = requests.recv().unwrap();
        assert!(
            request.head.contains("POST /v1/source/search"),
            "{}",
            request.head
        );
        assert!(request.head.contains("Authorization: Bearer dev-token"));

        let (url, _) = serve(403, r#"{"error":"that token isn't one of ours"}"#);
        let refused = BuildClient::new(&url, "x", true)
            .unwrap()
            .read(&SourceRead {
                release: "15.1-RELEASE".into(),
                path: "dev/a.c".into(),
                from: 1,
                lines: 10,
            })
            .unwrap_err();
        assert!(
            refused.contains("that token isn't one of ours"),
            "{refused}"
        );
        assert!(
            BuildClient::new("http://builds.example", "x", true).is_err(),
            "HTTPS off this machine"
        );
    }
}
