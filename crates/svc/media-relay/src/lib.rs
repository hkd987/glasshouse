//! The meetings relay (CF-21, ADR 0030): a selective forwarding relay that
//! `glass-sfu` runs on our servers. It is never in the image.
//!
//! - **Rooms.** The admin console admits each participant over the private
//!   API (HTTP and JSON on loopback, see `api.rs`): it posts the page's
//!   WebRTC offer with the participant's index and gets the relay's answer
//!   back. The answer carries ICE credentials for that participant only, so
//!   nobody else gets media. M1 has two seats a room ([`SEATS`]).
//! - **Media.** One UDP socket for everyone, ICE-lite, a host candidate on
//!   the configured public address. str0m runs in RTP mode: the relay reads
//!   RTP headers and RTCP and forwards each packet's payload unchanged to
//!   the other participant's receiving m-line. Frames are sealed end to end
//!   (SFrame); the relay can't decode them and never tries.
//! - **Data channel.** The page opens one, labelled `glass`. The relay sends
//!   one JSON object a message, ending in a newline:
//!   `{"type":"peers","peers":[0,1]}` (the participants in the room) when
//!   it opens and whenever that changes.
//! - **Never recorded.** No code here writes a file: each crate's
//!   `clippy.toml` disallows the calls that could
//!   (`cf21_the_relay_never_writes_media`). Logs go to stderr.
//!
//! Not here yet (M2): simulcast and layer choice, the active speaker, 25
//! seats, renegotiation over the data channel.

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
