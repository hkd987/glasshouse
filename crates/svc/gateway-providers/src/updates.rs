//! Glasshouse's update server (ADR 0061): signed OS releases over HTTPS,
//! laid out as the update folder is (`keys.json`, `<channel>/release.json`
//! and the files beside it). A computer reads it directly, or its org's
//! console mirrors it for the fleet. Nothing read here is trusted for
//! arriving: glassd checks every signature, and every image against its
//! signed digest, as it does a folder's.

use std::io::Write;

use crate::http::{Ask, Http, Target};

/// A client for one update server.
#[derive(Debug)]
pub struct UpdateClient {
    http: Http,
    base: String,
}

impl UpdateClient {
    /// The server at `url`, over HTTPS. Plain `http://` only to this
    /// machine's loopback address, with `allow_loopback_http`, for
    /// development and tests.
    pub fn new(url: &str, allow_loopback_http: bool) -> Result<Self, String> {
        let base = url.trim_end_matches('/').to_owned();
        let target = Target::parse(&format!("{base}/"))
            .ok_or_else(|| format!("the update server {url:?} is not an http(s) URL"))?;
        let http = if target.tls {
            Http::new()
        } else if allow_loopback_http && crate::console::is_loopback(&target.host) {
            Http::loopback()
        } else {
            return Err(format!("the update server {url:?} must use HTTPS"));
        };
        Ok(Self {
            http: http.for_downloads(),
            base,
        })
    }

    /// Where it is, for people.
    pub fn url(&self) -> &str {
        &self.base
    }

    /// `path` under the server (`keys.json`, `pilot/release.json`), from
    /// byte `offset` on, onto `to`: at most `max` bytes, the number written.
    /// What was written before an error stays written.
    pub fn get(
        &self,
        path: &str,
        offset: u64,
        to: &mut dyn Write,
        max: u64,
    ) -> Result<u64, String> {
        let url = format!("{}/{path}", self.base);
        let got = self
            .http
            .download(
                &Ask {
                    method: "GET",
                    url: &url,
                    headers: &[],
                    body: &[],
                },
                offset,
                to,
                max,
            )
            .map_err(|e| format!("{path}: {e}"))?;
        match got.status {
            200..=299 => Ok(got.written),
            // Asked from its end: nothing more to come.
            416 if offset > 0 => Ok(0),
            404 => Err(format!("{path}: not on the update server")),
            status => Err(format!("{path}: the update server answered {status}")),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::http::fake;

    #[test]
    fn adr0061_only_https_but_to_this_machine() {
        assert!(UpdateClient::new("http://updates.example/", true).is_err());
        assert!(UpdateClient::new("http://127.0.0.1:9/", false).is_err());
        assert!(UpdateClient::new("http://127.0.0.1:9/", true).is_ok());
        assert!(UpdateClient::new("https://updates.example/glasshouse", false).is_ok());
    }

    /// Binary, by length, by chunks and to the connection's end; from
    /// where it stopped, whether the server takes ranges or not.
    #[test]
    fn adr0061_an_image_downloads_whole_and_goes_on_from_where_it_stopped() {
        let image: Vec<u8> = (0..=255u8).cycle().take(70_000).collect();
        let chunked = {
            let mut body = Vec::new();
            for part in image.chunks(30_000) {
                body.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
                body.extend_from_slice(part);
                body.extend_from_slice(b"\r\n");
            }
            body.extend_from_slice(b"0\r\n\r\n");
            body
        };
        let replies = vec![
            [
                format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", image.len()).into_bytes(),
                image.clone(),
            ]
            .concat(),
            [
                b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n".to_vec(),
                chunked,
            ]
            .concat(),
            [b"HTTP/1.1 200 OK\r\n\r\n".to_vec(), image.clone()].concat(),
            // From byte 50 000: one server honours the range, one doesn't.
            [
                format!(
                    "HTTP/1.1 206 Partial Content\r\ncontent-length: {}\r\n\r\n",
                    image.len() - 50_000
                )
                .into_bytes(),
                image[50_000..].to_vec(),
            ]
            .concat(),
            [
                format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", image.len()).into_bytes(),
                image.clone(),
            ]
            .concat(),
            b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n".to_vec(),
            // Longer than the signed size.
            [
                format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", image.len()).into_bytes(),
                image.clone(),
            ]
            .concat(),
        ];
        let (url, requests) = fake::serve_raw(replies);
        let client = UpdateClient::new(&url, true).unwrap();
        for _ in 0..3 {
            let mut got = Vec::new();
            assert_eq!(
                client.get("pilot/a.zfs", 0, &mut got, 1 << 20).unwrap(),
                70_000
            );
            assert_eq!(got, image);
        }
        for _ in 0..2 {
            let mut got = image[..50_000].to_vec();
            assert_eq!(
                client
                    .get("pilot/a.zfs", 50_000, &mut got, 1 << 20)
                    .unwrap(),
                20_000
            );
            assert_eq!(got, image);
        }
        let heads: Vec<String> = requests.try_iter().map(|r| r.head).collect();
        assert!(
            heads[0].starts_with("GET /pilot/a.zfs HTTP/1.1"),
            "{}",
            heads[0]
        );
        assert!(!heads[0].contains("Range"));
        assert!(heads[3].contains("Range: bytes=50000-"), "{}", heads[3]);
        let missing = client
            .get("pilot/b.zfs", 0, &mut Vec::new(), 10)
            .unwrap_err();
        assert!(missing.contains("not on the update server"), "{missing}");
        let mut some = Vec::new();
        let long = client.get("pilot/a.zfs", 0, &mut some, 60_000).unwrap_err();
        assert!(long.contains("longer than"), "{long}");
    }

    /// A connection that drops partway keeps what came, for the next try.
    #[test]
    fn adr0061_a_dropped_download_keeps_what_came() {
        let reply = [
            b"HTTP/1.1 200 OK\r\ncontent-length: 1000\r\n\r\n".to_vec(),
            vec![7u8; 400],
        ]
        .concat();
        let (url, _) = fake::serve_raw(vec![reply]);
        let client = UpdateClient::new(&url, true).unwrap();
        let mut got = Vec::new();
        let err = client.get("pilot/a.zfs", 0, &mut got, 1 << 20).unwrap_err();
        assert!(err.contains("closed before"), "{err}");
        assert_eq!(got.len(), 400);
    }
}
