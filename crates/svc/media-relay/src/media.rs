//! The media loop. One task owns the UDP socket, every room and every
//! participant's `Rtc`; the API hands it commands over a channel.
//!
//! str0m is Sans-IO: after every change to an `Rtc` (a datagram, a timeout,
//! a packet written, a keyframe asked for) the loop drains its output until
//! the next timeout. What one participant's output means for another (a
//! packet to forward, a keyframe to ask for) goes on a work queue, so each
//! `Rtc` is changed and drained one at a time.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer, SdpOffer};
use str0m::channel::ChannelId;
use str0m::crypto::CryptoProvider;
use str0m::format::PayloadParams;
use str0m::media::{Direction, KeyframeRequestKind, Media as MediaLine, MediaKind, Mid};
use str0m::net::{Protocol, Receive};
use str0m::rtp::{ExtensionValues, RtpPacket, RtpWrite, Ssrc};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, trace, warn};

/// Participants a room holds. M1 is two people; M2 makes it 25.
pub const SEATS: usize = 2;
/// The label of the data channel the page opens for the relay's messages.
pub const CHANNEL: &str = "glass";
/// The longest the loop waits when no participant needs a timeout sooner.
const IDLE: Duration = Duration::from_secs(1);
/// Larger than any datagram on a 1500-byte path.
const DATAGRAM: usize = 2048;

/// What the API asks of the loop. Each command gets one reply.
#[derive(Debug)]
pub(crate) enum Command {
    Join {
        room: String,
        participant: u32,
        offer: String,
        reply: oneshot::Sender<Result<SdpAnswer, JoinError>>,
    },
    Leave {
        room: String,
        participant: u32,
        reply: oneshot::Sender<bool>,
    },
    CloseRoom {
        room: String,
        reply: oneshot::Sender<bool>,
    },
    Health {
        reply: oneshot::Sender<Health>,
    },
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub(crate) struct Health {
    pub rooms: usize,
    pub participants: usize,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum JoinError {
    #[error("the room has its {SEATS} participants already")]
    Full,
    #[error("{0}")]
    Offer(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PeerId(u64);

/// A pair of the page's m-lines, one audio and one video.
#[derive(Debug, Clone, Copy)]
struct Mids {
    audio: Mid,
    video: Mid,
}

impl Mids {
    fn of(&self, kind: MediaKind) -> Mid {
        match kind {
            MediaKind::Audio => self.audio,
            MediaKind::Video => self.video,
        }
    }
}

/// One admitted participant: a page's connection to the relay.
struct Peer {
    room: String,
    participant: u32,
    rtc: Rtc,
    /// The page's `sendonly` m-lines: what the relay receives.
    incoming: Mids,
    /// The page's `recvonly` m-lines: what the relay sends it.
    outgoing: Mids,
    channel: Option<ChannelId>,
    connected: bool,
    /// Someone now watches this participant's video: ask for a keyframe
    /// with the next video packet (the receive stream may not exist yet).
    keyframe_due: bool,
    /// Outgoing audio, then video.
    continuity: [Continuity; 2],
    timeout: Instant,
}

/// What one participant's output asks of the others.
enum Work {
    /// A packet from `from`'s page, for everyone else in the room.
    Forward {
        from: PeerId,
        kind: MediaKind,
        codec: PayloadParams,
        packet: Box<RtpPacket>,
    },
    /// `viewer`'s page asked for a keyframe of the video it receives.
    Keyframe {
        viewer: PeerId,
        kind: KeyframeRequestKind,
    },
    /// `viewer`'s connection is up: it needs a keyframe to start from.
    Connected(PeerId),
    /// Tell this participant who is in the room.
    Peers(PeerId),
    /// The connection closed or timed out.
    Gone(PeerId),
}

pub(crate) struct Media {
    socket: UdpSocket,
    /// The host candidate in every answer; also where str0m is told
    /// datagrams arrive, whatever address the socket is bound to.
    host: Candidate,
    crypto: Arc<CryptoProvider>,
    /// Room id → participant index → peer.
    rooms: HashMap<String, BTreeMap<u32, PeerId>>,
    peers: HashMap<PeerId, Peer>,
    next_id: u64,
    work: VecDeque<Work>,
}

enum Wake {
    Command(Option<Command>),
    Datagram(std::io::Result<(usize, SocketAddr)>),
    Timeout,
}

impl Media {
    pub(crate) fn new(socket: UdpSocket, host: Candidate, crypto: Arc<CryptoProvider>) -> Self {
        Media {
            socket,
            host,
            crypto,
            rooms: HashMap::new(),
            peers: HashMap::new(),
            next_id: 0,
            work: VecDeque::new(),
        }
    }

    /// Runs until the API goes away.
    pub(crate) async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        let mut buf = vec![0u8; DATAGRAM];
        loop {
            let now = Instant::now();
            let deadline = self
                .peers
                .values()
                .map(|p| p.timeout)
                .fold(now + IDLE, Instant::min);
            let wake = tokio::select! {
                command = commands.recv() => Wake::Command(command),
                received = self.socket.recv_from(&mut buf) => Wake::Datagram(received),
                () = tokio::time::sleep_until(deadline.into()) => Wake::Timeout,
            };
            match wake {
                Wake::Command(None) => return,
                Wake::Command(Some(command)) => self.command(command),
                Wake::Datagram(Ok((n, source))) => self.datagram(&buf[..n], source),
                Wake::Datagram(Err(e)) => debug!("receiving media: {e}"),
                Wake::Timeout => {}
            }
            self.tick(Instant::now());
        }
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Join {
                room,
                participant,
                offer,
                reply,
            } => {
                let answer = self.join(room, participant, &offer);
                let _ = reply.send(answer);
            }
            Command::Leave {
                room,
                participant,
                reply,
            } => {
                let _ = reply.send(self.leave(&room, participant));
            }
            Command::CloseRoom { room, reply } => {
                let _ = reply.send(self.close_room(&room));
            }
            Command::Health { reply } => {
                let _ = reply.send(Health {
                    rooms: self.rooms.len(),
                    participants: self.peers.len(),
                });
            }
        }
    }

    /// Admits `participant` to `room` with the page's offer, and answers it.
    /// The same index again replaces the old connection.
    fn join(
        &mut self,
        room: String,
        participant: u32,
        offer: &str,
    ) -> Result<SdpAnswer, JoinError> {
        let seats = self.rooms.get(&room);
        let replacing = seats.and_then(|s| s.get(&participant)).copied();
        if replacing.is_none() && seats.is_some_and(|s| s.len() >= SEATS) {
            return Err(JoinError::Full);
        }
        let (rtc, answer, incoming, outgoing, timeout) = self.answer(offer)?;
        if let Some(old) = replacing
            && let Some(peer) = self.remove(old)
        {
            info!(room, participant, "replaced by a new connection");
            hang_up(&self.socket, peer);
        }
        let id = PeerId(self.next_id);
        self.next_id += 1;
        let others = self.others_in(&room, id);
        self.peers.insert(
            id,
            Peer {
                room: room.clone(),
                participant,
                rtc,
                incoming,
                outgoing,
                channel: None,
                connected: false,
                keyframe_due: !others.is_empty(),
                continuity: Default::default(),
                timeout,
            },
        );
        self.rooms
            .entry(room.clone())
            .or_default()
            .insert(participant, id);
        info!(room, participant, "joined");
        self.membership_changed(&room);
        self.process();
        Ok(answer)
    }

    /// A new `Rtc` that has accepted `offer`, the page's m-lines, and the
    /// answer. The offer must have a data channel and exactly one each of
    /// audio and video `sendonly` and `recvonly`, in Opus and VP8.
    fn answer(&self, offer: &str) -> Result<(Rtc, SdpAnswer, Mids, Mids, Instant), JoinError> {
        let bad = |why: String| JoinError::Offer(why);
        if !offer.lines().any(|l| l.starts_with("m=application")) {
            return Err(bad("the offer has no data channel".into()));
        }
        let parsed = SdpOffer::from_sdp_string(&without_unresolved_candidates(offer))
            .map_err(|e| bad(format!("the offer isn't SDP the relay reads: {e}")))?;
        let mut rtc = Rtc::builder()
            .set_crypto_provider(self.crypto.clone())
            .set_ice_lite(true)
            .set_rtp_mode(true)
            // VP8 and Opus only: the page's SFrame transform leaves
            // exactly VP8's payload header in the clear (ADR 0030).
            .clear_codecs()
            .enable_opus(true, false)
            .enable_vp8(true)
            .build(Instant::now());
        rtc.add_local_candidate(self.host.clone());
        let answer = rtc
            .sdp_api()
            .accept_offer(parsed)
            .map_err(|e| bad(format!("the relay can't answer this offer: {e}")))?;
        let timeout = loop {
            match rtc.poll_output() {
                Ok(Output::Timeout(t)) => break t,
                Ok(Output::Event(_)) => {}
                Ok(Output::Transmit(t)) => send(&self.socket, &t.contents, t.destination),
                Err(e) => return Err(bad(format!("the relay can't answer this offer: {e}"))),
            }
        };
        // str0m's MediaAdded waits for SRTP, so the mids come from the
        // offer and everything else about them from str0m.
        let media: Vec<&MediaLine> = offer
            .lines()
            .filter_map(|l| l.trim_end().strip_prefix("a=mid:"))
            .filter_map(|mid| rtc.media(mid.into()))
            .collect();
        let (incoming, outgoing) = layout(&media).map_err(bad)?;
        Ok((rtc, answer, incoming, outgoing, timeout))
    }

    fn leave(&mut self, room: &str, participant: u32) -> bool {
        let Some(id) = self
            .rooms
            .get(room)
            .and_then(|s| s.get(&participant))
            .copied()
        else {
            return false;
        };
        if let Some(peer) = self.remove(id) {
            hang_up(&self.socket, peer);
        }
        info!(room, participant, "left");
        self.membership_changed(room);
        self.process();
        true
    }

    fn close_room(&mut self, room: &str) -> bool {
        let Some(seats) = self.rooms.remove(room) else {
            return false;
        };
        for id in seats.values() {
            if let Some(peer) = self.peers.remove(id) {
                hang_up(&self.socket, peer);
            }
        }
        info!(room, participants = seats.len(), "room closed");
        true
    }

    /// A datagram goes to the participant whose connection accepts it: a
    /// STUN request by the ICE credentials from that participant's answer,
    /// anything else by the address ICE settled on. Nobody else's traffic
    /// reaches an `Rtc`.
    fn datagram(&mut self, data: &[u8], source: SocketAddr) {
        let Ok(contents) = data.try_into() else {
            return;
        };
        let input = Input::Receive(
            Instant::now(),
            Receive {
                proto: Protocol::Udp,
                source,
                destination: self.host.addr(),
                contents,
            },
        );
        let Some((&id, peer)) = self.peers.iter_mut().find(|(_, p)| p.rtc.accepts(&input)) else {
            trace!(%source, "dropped a datagram no admitted participant accepts");
            return;
        };
        if let Err(e) = peer.rtc.handle_input(input) {
            debug!(room = peer.room, participant = peer.participant, "{e}");
            peer.rtc.disconnect();
        }
        drain(&self.socket, id, peer, &mut self.work);
        self.process();
    }

    /// Hands each participant whose timeout is due the time.
    fn tick(&mut self, now: Instant) {
        for (&id, peer) in self.peers.iter_mut().filter(|(_, p)| p.timeout <= now) {
            if let Err(e) = peer.rtc.handle_input(Input::Timeout(now)) {
                debug!(room = peer.room, participant = peer.participant, "{e}");
                peer.rtc.disconnect();
            }
            drain(&self.socket, id, peer, &mut self.work);
        }
        self.process();
    }

    fn process(&mut self) {
        while let Some(work) = self.work.pop_front() {
            match work {
                Work::Forward {
                    from,
                    kind,
                    codec,
                    packet,
                } => self.forward(from, kind, codec, &packet),
                Work::Keyframe { viewer, kind } => {
                    for source in self.others(viewer) {
                        self.request_keyframe(source, kind);
                    }
                }
                Work::Connected(viewer) => {
                    for source in self.others(viewer) {
                        self.request_keyframe(source, KeyframeRequestKind::Pli);
                    }
                }
                Work::Peers(id) => self.send_peers(id),
                Work::Gone(id) => {
                    if let Some(peer) = self.remove(id) {
                        info!(
                            room = peer.room,
                            participant = peer.participant,
                            "left: the connection closed"
                        );
                        self.membership_changed(&peer.room);
                    }
                }
            }
        }
    }

    /// Writes `packet` to every other participant's receiving m-line of the
    /// same kind: the payload as it came, the payload type mapped by codec.
    fn forward(&mut self, from: PeerId, kind: MediaKind, codec: PayloadParams, packet: &RtpPacket) {
        let clock = codec.spec().clock_rate.get();
        for to in self.others(from) {
            let Some(peer) = self.peers.get_mut(&to) else {
                continue;
            };
            if !peer.connected {
                continue;
            }
            let mid = peer.outgoing.of(kind);
            let Some(pt) = peer.rtc.codec_config().match_params(codec).map(|p| p.pt()) else {
                continue;
            };
            if !peer
                .rtc
                .media(mid)
                .is_some_and(|m| m.remote_pts().contains(&pt))
            {
                continue;
            }
            let slot = usize::from(kind == MediaKind::Video);
            let (seq_no, time) = peer.continuity[slot].map(
                (from, packet.header.ssrc),
                *packet.seq_no,
                packet.header.timestamp,
                clock / 50,
            );
            let ext = &packet.header.ext_vals;
            let ext = ExtensionValues {
                audio_level: ext.audio_level,
                voice_activity: ext.voice_activity,
                video_orientation: ext.video_orientation,
                ..Default::default()
            };
            let mut api = peer.rtc.direct_api();
            let Some(stream) = api.stream_tx_by_mid(mid, None) else {
                continue;
            };
            stream.write_rtp(
                RtpWrite::new(
                    pt,
                    seq_no.into(),
                    time,
                    packet.timestamp,
                    packet.payload.clone(),
                )
                .marker(packet.header.marker)
                .ext_vals(ext)
                // The relay answers the viewer's NACKs from its own
                // cache; Opus doesn't use them.
                .nackable(kind == MediaKind::Video),
            );
            drain(&self.socket, to, peer, &mut self.work);
        }
    }

    /// Asks `source`'s page for a keyframe of its video now, or with its
    /// next video packet if it isn't sending yet.
    fn request_keyframe(&mut self, source: PeerId, kind: KeyframeRequestKind) {
        let Some(peer) = self.peers.get_mut(&source) else {
            return;
        };
        let mid = peer.incoming.video;
        let mut api = peer.rtc.direct_api();
        match api.stream_rx_by_mid(mid, None) {
            Some(stream) if peer.connected => {
                stream.request_keyframe(kind);
                drain(&self.socket, source, peer, &mut self.work);
            }
            _ => peer.keyframe_due = true,
        }
    }

    /// `{"type":"peers","peers":[…]}` on the data channel, once it's open.
    fn send_peers(&mut self, id: PeerId) {
        let Some(peer) = self.peers.get(&id) else {
            return;
        };
        let peers: Vec<u32> = self
            .rooms
            .get(&peer.room)
            .map(|seats| seats.keys().copied().collect())
            .unwrap_or_default();
        let Some(peer) = self.peers.get_mut(&id) else {
            return;
        };
        let Some(mut channel) = peer.channel.and_then(|c| peer.rtc.channel(c)) else {
            return;
        };
        let line = format!("{}\n", serde_json::json!({"type": "peers", "peers": peers}));
        match channel.write(false, line.as_bytes()) {
            Ok(true) => {}
            Ok(false) => warn!(
                room = peer.room,
                participant = peer.participant,
                "data channel full"
            ),
            Err(e) => warn!(
                room = peer.room,
                participant = peer.participant,
                "data channel: {e}"
            ),
        }
        drain(&self.socket, id, peer, &mut self.work);
    }

    fn membership_changed(&mut self, room: &str) {
        if let Some(seats) = self.rooms.get(room) {
            self.work.extend(seats.values().map(|&id| Work::Peers(id)));
        }
    }

    /// Everyone in `id`'s room but `id`.
    fn others(&self, id: PeerId) -> Vec<PeerId> {
        match self.peers.get(&id) {
            Some(peer) => self.others_in(&peer.room, id),
            None => Vec::new(),
        }
    }

    fn others_in(&self, room: &str, id: PeerId) -> Vec<PeerId> {
        self.rooms
            .get(room)
            .map(|seats| seats.values().copied().filter(|&p| p != id).collect())
            .unwrap_or_default()
    }

    /// Takes `id` out of its room and the loop.
    fn remove(&mut self, id: PeerId) -> Option<Peer> {
        let peer = self.peers.remove(&id)?;
        if let Some(seats) = self.rooms.get_mut(&peer.room) {
            if seats.get(&peer.participant) == Some(&id) {
                seats.remove(&peer.participant);
            }
            if seats.is_empty() {
                self.rooms.remove(&peer.room);
            }
        }
        Some(peer)
    }
}

impl Peer {
    fn event(&mut self, id: PeerId, event: Event, work: &mut VecDeque<Work>) {
        match event {
            Event::Connected => {
                self.connected = true;
                info!(
                    room = self.room,
                    participant = self.participant,
                    "connected"
                );
                work.push_back(Work::Connected(id));
            }
            Event::IceConnectionStateChange(IceConnectionState::Disconnected) | Event::Closed => {
                work.push_back(Work::Gone(id));
            }
            Event::ChannelOpen(channel, label) if label == CHANNEL => {
                self.channel = Some(channel);
                work.push_back(Work::Peers(id));
            }
            Event::ChannelClose(channel) if self.channel == Some(channel) => self.channel = None,
            Event::RtpPacket(packet) => self.received(id, packet, work),
            Event::KeyframeRequest(request) if request.mid == self.outgoing.video => {
                work.push_back(Work::Keyframe {
                    viewer: id,
                    kind: request.kind,
                });
            }
            _ => {}
        }
    }

    /// A packet from the page: forward it if it's on one of the page's
    /// sending m-lines in a codec the relay knows.
    fn received(&mut self, id: PeerId, packet: RtpPacket, work: &mut VecDeque<Work>) {
        let mut api = self.rtc.direct_api();
        let Some(stream) = api.stream_rx(&packet.header.ssrc) else {
            return;
        };
        let mid = stream.mid();
        let kind = if mid == self.incoming.audio {
            MediaKind::Audio
        } else if mid == self.incoming.video {
            MediaKind::Video
        } else {
            return;
        };
        if kind == MediaKind::Video && self.keyframe_due {
            // Without it a new viewer sees nothing until the sender's next
            // keyframe, which can be seconds away.
            stream.request_keyframe(KeyframeRequestKind::Pli);
            self.keyframe_due = false;
        }
        let pt = packet.header.payload_type;
        let Some(codec) = self.rtc.codec_config().find(|p| p.pt() == pt).copied() else {
            return;
        };
        work.push_back(Work::Forward {
            from: id,
            kind,
            codec,
            packet: Box::new(packet),
        });
    }
}

/// The page's receiving and sending m-lines, as the relay sees them: the
/// page's `sendonly` is the relay's `recvonly`, and the other way round.
fn layout(media: &[&MediaLine]) -> Result<(Mids, Mids), String> {
    if media.len() != 4 {
        return Err(format!(
            "the offer needs four media m-lines (audio and video, sendonly and recvonly), not {}",
            media.len()
        ));
    }
    let one = |kind: MediaKind, direction: Direction| -> Result<Mid, String> {
        let mut found = media
            .iter()
            .filter(|m| m.kind() == kind && m.direction() == direction);
        let (Some(found), None) = (found.next(), found.next()) else {
            let page = match direction {
                Direction::RecvOnly => "sendonly",
                _ => "recvonly",
            };
            return Err(format!(
                "the offer needs exactly one {kind:?} m-line that is {page}",
            ));
        };
        let codec = match kind {
            MediaKind::Audio => "Opus",
            MediaKind::Video => "VP8",
        };
        match found.remote_pts().is_empty() {
            false => Ok(found.mid()),
            true => Err(format!("the offer's {kind:?} m-line has no {codec}")),
        }
    };
    let incoming = Mids {
        audio: one(MediaKind::Audio, Direction::RecvOnly)?,
        video: one(MediaKind::Video, Direction::RecvOnly)?,
    };
    let outgoing = Mids {
        audio: one(MediaKind::Audio, Direction::SendOnly)?,
        video: one(MediaKind::Video, Direction::SendOnly)?,
    };
    Ok((incoming, outgoing))
}

/// Drains `peer`'s output until its next timeout, as str0m requires after
/// every change.
fn drain(socket: &UdpSocket, id: PeerId, peer: &mut Peer, work: &mut VecDeque<Work>) {
    loop {
        if !peer.rtc.is_alive() {
            work.push_back(Work::Gone(id));
            return;
        }
        match peer.rtc.poll_output() {
            Ok(Output::Timeout(t)) => {
                peer.timeout = t;
                return;
            }
            Ok(Output::Transmit(t)) => send(socket, &t.contents, t.destination),
            Ok(Output::Event(event)) => peer.event(id, event, work),
            Err(e) => {
                warn!(room = peer.room, participant = peer.participant, "{e}");
                peer.rtc.disconnect();
            }
        }
    }
}

/// Says goodbye (RTCP BYE, DTLS close_notify) and lets the connection go.
fn hang_up(socket: &UdpSocket, mut peer: Peer) {
    if peer.rtc.close().is_err() {
        return;
    }
    while peer.rtc.is_alive() {
        match peer.rtc.poll_output() {
            Ok(Output::Transmit(t)) => send(socket, &t.contents, t.destination),
            Ok(Output::Event(_)) => {}
            Ok(Output::Timeout(_)) | Err(_) => return,
        }
    }
}

/// UDP is lossy anyway: a full send buffer drops the datagram.
fn send(socket: &UdpSocket, contents: &[u8], destination: SocketAddr) {
    if let Err(e) = socket.try_send_to(contents, destination) {
        trace!(%destination, "sending media: {e}");
    }
}

/// The offer without candidates the relay can't parse, such as a browser's
/// mDNS host names (`<uuid>.local`). An ICE-lite relay doesn't need the
/// page's candidates: it learns the page's address from its checks.
fn without_unresolved_candidates(offer: &str) -> String {
    let mut out = String::with_capacity(offer.len());
    for line in offer.split_inclusive('\n') {
        let unresolved = line
            .strip_prefix("a=candidate:")
            .and_then(|c| c.split_whitespace().nth(4))
            .is_some_and(|addr| addr.parse::<std::net::IpAddr>().is_err());
        if !unresolved {
            out.push_str(line);
        }
    }
    out
}

/// Keeps an outgoing m-line's sequence numbers and timestamps continuous
/// when the participant feeding it changes (the same index joining again),
/// so the page sees one stream rather than a jump.
#[derive(Debug, Default)]
struct Continuity {
    source: Option<(PeerId, Ssrc)>,
    seq_offset: i64,
    time_offset: u32,
    /// The highest sequence number sent, and its timestamp.
    last: Option<(u64, u32)>,
}

impl Continuity {
    /// The outgoing sequence number and timestamp for a packet from
    /// `source`; `step` is how far the timestamp moves across a switch.
    fn map(&mut self, source: (PeerId, Ssrc), seq: u64, time: u32, step: u32) -> (u64, u32) {
        if self.source != Some(source) {
            if let Some((last_seq, last_time)) = self.last {
                self.seq_offset = (last_seq as i64 + 1) - seq as i64;
                self.time_offset = last_time.wrapping_add(step).wrapping_sub(time);
            }
            self.source = Some(source);
        }
        let seq = seq.saturating_add_signed(self.seq_offset);
        let time = time.wrapping_add(self.time_offset);
        if self.last.is_none_or(|(last, _)| seq > last) {
            self.last = Some((seq, time));
        }
        (seq, time)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_source_continues_the_old_ones_numbering() {
        let mut c = Continuity::default();
        let a = (PeerId(1), Ssrc::from(10));
        let b = (PeerId(2), Ssrc::from(20));
        // The first source passes through unchanged.
        assert_eq!(c.map(a, 1000, 5000, 960), (1000, 5000));
        assert_eq!(c.map(a, 1001, 5960, 960), (1001, 5960));
        // A late packet doesn't move the high-water mark.
        assert_eq!(c.map(a, 999, 4040, 960), (999, 4040));
        // The replacement starts right after, one step later.
        assert_eq!(c.map(b, 40_000, 7, 960), (1002, 6920));
        assert_eq!(c.map(b, 40_001, 967, 960), (1003, 7880));
        // Timestamps wrap like RTP's.
        let mut c = Continuity::default();
        c.map(a, 5, u32::MAX - 10, 960);
        assert_eq!(c.map(b, 7, 0, 960), (6, 949));
    }

    #[test]
    fn mdns_candidates_are_left_out_of_the_offer() {
        let offer = "v=0\r\n\
                     a=candidate:1 1 udp 2122260223 4b2e7c1a-1111-2222-3333-444455556666.local 54321 typ host\r\n\
                     a=candidate:2 1 udp 2122260223 192.0.2.7 54321 typ host\r\n\
                     a=mid:0\r\n";
        assert_eq!(
            without_unresolved_candidates(offer),
            "v=0\r\na=candidate:2 1 udp 2122260223 192.0.2.7 54321 typ host\r\na=mid:0\r\n"
        );
    }
}
