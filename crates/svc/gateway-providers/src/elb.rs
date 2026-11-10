//! AWS's public keys for the claims an Application Load Balancer signs
//! (`x-amzn-oidc-data`, ADR 0067): one PEM per key id, at
//! `https://public-keys.auth.elb.<region>.amazonaws.com/<kid>`. An agent
//! host reaches it through the stack's egress relay, which allows that
//! host only.

use crate::http::{Http, Target};

/// The most a key's PEM takes.
const PEM_MAX: usize = 8 * 1024;

/// Where AWS publishes `region`'s load balancer keys.
pub fn keys_url(region: &str) -> String {
    format!("https://public-keys.auth.elb.{region}.amazonaws.com")
}

/// The PEM for key `kid` at `base`: AWS's, through `proxy` (the stack's
/// egress relay) when there is one, or a stand-in on this machine's
/// loopback address (deploy/local).
pub fn load_balancer_key(base: &str, kid: &str, proxy: Option<&str>) -> Result<String, String> {
    let fits = !kid.is_empty()
        && kid.len() <= 64
        && kid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !fits {
        return Err(format!("{kid:?} isn't a load balancer key id"));
    }
    let base = base.trim_end_matches('/');
    let target = Target::parse(&format!("{base}/"))
        .ok_or_else(|| format!("{base:?} isn't an http(s) URL"))?;
    let http = if target.tls {
        match proxy {
            Some(proxy) => {
                Http::via(proxy).ok_or_else(|| format!("{proxy:?} isn't an http:// proxy"))?
            }
            None => Http::new(),
        }
    } else if crate::console::is_loopback(&target.host) {
        Http::loopback()
    } else {
        return Err(format!("{base:?} must use HTTPS"));
    };
    let reply = http
        .quick()
        .send("GET", &format!("{base}/{kid}"), &[], b"")
        .map_err(|e| format!("couldn't get the load balancer's key {kid}: {e}"))?;
    if reply.status != 200 {
        return Err(format!(
            "the load balancer's key {kid}: HTTP {}",
            reply.status
        ));
    }
    if reply.body.len() > PEM_MAX || !reply.body.contains("BEGIN PUBLIC KEY") {
        return Err(format!("the load balancer's key {kid} isn't a public key"));
    }
    Ok(reply.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake;

    #[test]
    fn adr0067_a_key_is_fetched_by_its_id_and_nothing_else() {
        let pem = "-----BEGIN PUBLIC KEY-----\nMFkw...\n-----END PUBLIC KEY-----\n";
        let (url, got) = fake::serve(200, pem);
        assert_eq!(load_balancer_key(&url, "8f4c-01", None).unwrap(), pem);
        assert!(got.recv().unwrap().head.starts_with("GET /8f4c-01 "));
        for bad in ["", "../x", "a/b", "a?b"] {
            assert!(load_balancer_key(&url, bad, None).is_err(), "{bad}");
        }
        let (url, _got) = fake::serve(200, "<html>not a key</html>");
        assert!(
            load_balancer_key(&url, "8f4c-01", None)
                .unwrap_err()
                .contains("isn't a public key")
        );
        assert!(
            load_balancer_key("http://keys.example.com", "8f4c-01", None)
                .unwrap_err()
                .contains("HTTPS")
        );
        let relay = load_balancer_key(&keys_url("us-east-1"), "8f4c-01", Some("https://relay"));
        assert!(relay.unwrap_err().contains("isn't an http:// proxy"));
        assert_eq!(
            keys_url("eu-west-1"),
            "https://public-keys.auth.elb.eu-west-1.amazonaws.com"
        );
    }
}
