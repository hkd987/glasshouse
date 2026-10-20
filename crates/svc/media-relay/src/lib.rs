//! The meetings relay (CF-21, ADR 0030): a selective forwarding relay that
//! `glass-sfu` runs on our servers. It is never in the image.
//!
//! - **Rooms.** The admin console admits each participant over the private
//!   API (HTTP and JSON on loopback, see `api.rs`): it posts the page's
//!   WebRTC offer with the participant's index (and, optionally, the name
//!   the others see) and gets the relay's answer back. The answer carries
//!   ICE credentials for that participant only, so nobody else gets media.
//!   A room has [`SEATS`] seats, 25: the 26th is refused.
//! - **The page's offer.** A data channel; one `sendonly` audio m-line (the
//!   microphone); one `sendonly` video m-line (the camera) and optionally a
//!   second (the screen, sent only while shared); and any number of
//!   `recvonly` audio and video m-lines, in Opus and VP8. The `recvonly`
//!   lines are the page's *slots*: the relay sends each other participant's
//!   microphone, camera and shared screen on a free slot of that kind, and
//!   a slot freed by someone leaving carries the next person.
//! - **Media.** One UDP socket for everyone, ICE-lite, a host candidate on
//!   the configured public address. str0m runs in RTP mode: the relay reads
//!   RTP headers and RTCP and forwards each packet's payload unchanged to
//!   each other participant's slot for it. Frames are sealed end to end
//!   (SFrame); the relay can't decode them and never tries. It reads only
//!   VP8's payload descriptor and frame tag, which SFrame leaves clear, so a
//!   slot that changes hands starts on a key frame.
//! - **Data channel.** The page opens one, labelled `glass`. Each message is
//!   one JSON object ending in a newline. The relay sends:
//!   - `{"type":"peers","peers":[0,1,2],"sharing":[2],"names":{"1":"Priya"},
//!     "tracks":[{"mid":"5","participant":1,"kind":"audio","screen":false},
//!     {"mid":"6","participant":1,"kind":"video","screen":false},
//!     {"mid":"7","participant":2,"kind":"video","screen":true}]}`
//!     when the channel opens and whenever any of it changes: who is in the
//!     room, who shares a screen, the names the console gave (`names` only
//!     when there are some), and which of this page's slots carries whose
//!     microphone, camera or screen. A track without a slot isn't listed.
//!   - `{"type":"answer","sdp":"…"}` for each offer the page sends, then a
//!     `peers` line (always, even unchanged) naming its new slots; or
//!     `{"type":"refused","error":"…"}` if it can't answer, and the page
//!     rolls back.
//!
//!   The page sends:
//!   - `{"type":"offer","sdp":"…"}` with more `recvonly` m-lines when the
//!     last `peers` line shows a participant, or a shared screen, without a
//!     slot. One at a time: the next waits for the answer and the `peers`
//!     line after it.
//!   - `{"type":"screen","on":true}` when it starts sending its screen on
//!     its second video line, `{"type":"screen","on":false}` when it stops.
//!     Only then do the others get a slot for it.
//! - **Never recorded.** No code here writes a file: each crate's
//!   `clippy.toml` disallows the calls that could
//!   (`cf21_the_relay_never_writes_media`). Logs go to stderr.
//!
//! Not here yet (M2): simulcast and layer choice, and the active speaker.

mod api;
mod media;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use str0m::Candidate;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub use media::{CHANNEL, SEATS};

/// Where the relay listens.
#[derive(Debug, Clone)]
pub struct Config {
    /// The UDP socket for everyone's media.
    pub udp: SocketAddr,
    /// The address computers reach that socket on: the host candidate in
    /// every answer, with the socket's port.
    pub public: IpAddr,
    /// The private API for the admin console. Loopback only: it has no
    /// authentication yet.
    pub api: SocketAddr,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the private API listens on loopback only, it has no authentication yet (not {0})")]
    ApiNotLoopback(SocketAddr),
    #[error("{0} can't be the relay's public address: {1}")]
    Public(SocketAddr, String),
    #[error("listening for {what} on {addr}: {source}")]
    Bind {
        what: &'static str,
        addr: SocketAddr,
        source: std::io::Error,
    },
}

/// A running relay. Dropping it stops it.
#[derive(Debug)]
pub struct Relay {
    media_addr: SocketAddr,
    api_addr: SocketAddr,
    media: JoinHandle<()>,
    api: JoinHandle<()>,
}

impl Relay {
    /// Binds both sockets and starts serving, on the current tokio runtime.
    pub async fn start(config: Config) -> Result<Relay, Error> {
        if !config.api.ip().is_loopback() {
            return Err(Error::ApiNotLoopback(config.api));
        }
        let socket = UdpSocket::bind(config.udp)
            .await
            .map_err(|source| Error::Bind {
                what: "media",
                addr: config.udp,
                source,
            })?;
        let bound = socket.local_addr().map_err(|source| Error::Bind {
            what: "media",
            addr: config.udp,
            source,
        })?;
        let media_addr = SocketAddr::new(config.public, bound.port());
        let host = Candidate::host(media_addr, "udp")
            .map_err(|e| Error::Public(media_addr, e.to_string()))?;
        let listener = TcpListener::bind(config.api)
            .await
            .map_err(|source| Error::Bind {
                what: "the API",
                addr: config.api,
                source,
            })?;
        let api_addr = listener.local_addr().map_err(|source| Error::Bind {
            what: "the API",
            addr: config.api,
            source,
        })?;
        // DTLS and SRTP on OpenSSL, like the rest of the system.
        let crypto = Arc::new(str0m::crypto::from_feature_flags());
        let (commands, inbox) = mpsc::channel(64);
        let media = tokio::spawn(media::Media::new(socket, host, crypto).run(inbox));
        let api = tokio::spawn(api::serve(listener, commands));
        tracing::info!(media = %media_addr, socket = %bound, api = %api_addr, "relay listening");
        Ok(Relay {
            media_addr,
            api_addr,
            media,
            api,
        })
    }

    /// The media address computers connect to (the host candidate).
    pub fn media_addr(&self) -> SocketAddr {
        self.media_addr
    }

    /// The private API's address.
    pub fn api_addr(&self) -> SocketAddr {
        self.api_addr
    }

    /// Resolves if the relay stops on its own, which it shouldn't.
    pub async fn stopped(&mut self) {
        tokio::select! {
            _ = &mut self.media => tracing::error!("the media loop stopped"),
            _ = &mut self.api => tracing::error!("the API stopped"),
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.media.abort();
        self.api.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CF-21: the relay never writes media, or anything else, to disk.
    /// clippy enforces it with each crate's `clippy.toml` (a crate's file
    /// replaces the workspace's, so each repeats the workspace settings).
    #[test]
    fn cf21_the_relay_never_writes_media() {
        let configs = [
            include_str!("../clippy.toml"),
            include_str!("../../../bin/glass-sfu/clippy.toml"),
        ];
        let workspace = include_str!("../../../../clippy.toml");
        for config in configs {
            for path in [
                "std::fs::write",
                "std::fs::File::create",
                "std::fs::OpenOptions::open",
                "tokio::fs::write",
                "tokio::fs::File::create",
                "tokio::fs::OpenOptions::open",
            ] {
                assert!(config.contains(&format!("\"{path}\"")), "{path} is allowed");
            }
            for line in workspace.lines().filter(|l| !l.trim().is_empty()) {
                assert!(config.contains(line), "the workspace's `{line}` is missing");
            }
        }
    }

    #[tokio::test]
    async fn the_api_listens_on_loopback_only() {
        let config = Config {
            udp: "127.0.0.1:0".parse().unwrap(),
            public: "127.0.0.1".parse().unwrap(),
            api: "0.0.0.0:0".parse().unwrap(),
        };
        assert!(matches!(
            Relay::start(config).await,
            Err(Error::ApiNotLoopback(_))
        ));
        let config = Config {
            udp: "127.0.0.1:0".parse().unwrap(),
            public: "0.0.0.0".parse().unwrap(),
            api: "127.0.0.1:0".parse().unwrap(),
        };
        assert!(matches!(Relay::start(config).await, Err(Error::Public(..))));
    }
}
