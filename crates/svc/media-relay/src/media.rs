//! The media loop. One task owns the UDP socket, every room and every
//! participant's `Rtc`; the API hands it commands over a channel.
//!
//! str0m is Sans-IO: after every change to an `Rtc` (a datagram, a timeout,
//! a packet written, a keyframe asked for) the loop drains its output until
//! the next timeout. What one participant's output means for another (a
//! packet to forward, a keyframe to ask for, a line on the data channel)
//! goes on a work queue, so each `Rtc` is changed and drained one at a time.
//!
//! Each page receives on a pool of its own `recvonly` m-lines, its *slots*.
//! The relay gives each other participant's microphone and camera, and any
//! screen being shared, a free slot of that kind, and tells the page which
//! is which in its `peers` line. When the page has too few, it offers more
//! over the data channel and the relay answers (see `lib.rs`). A slot whose
//! participant leaves is free for the next one.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
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

/// Participants a room holds: meetings are up to 25 people (ADR 0030).
pub const SEATS: usize = 25;
/// The label of the data channel the page opens for the relay's messages.
pub const CHANNEL: &str = "glass";
/// The longest the loop waits when no participant needs a timeout sooner.
const IDLE: Duration = Duration::from_secs(1);
/// Larger than any datagram on a 1500-byte path.
const DATAGRAM: usize = 2048;
/// The longest display name the relay keeps, in characters.
const NAME_MAX: usize = 64;
/// How often the relay asks again for a key frame a slot still waits for.
/// The engine's encoder ignores requests within 300 ms of the last, so a
/// second viewer's request can be lost; and a slot that waits forwards
/// nothing, so that viewer's decoder never asks itself.
const KEYFRAME_RETRY: Duration = Duration::from_millis(500);
/// How often the loop looks for slots still waiting for a key frame.
const KEYFRAME_CHECK: Duration = Duration::from_millis(100);

/// What the API asks of the loop. Each command gets one reply.
#[derive(Debug)]
pub(crate) enum Command {
    Join {
        room: String,
        participant: u32,
        name: Option<String>,
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

/// What a participant sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Track {
    Microphone,
    Camera,
    Screen,
}

impl Track {
    fn kind(self) -> MediaKind {
        match self {
            Track::Microphone => MediaKind::Audio,
            Track::Camera | Track::Screen => MediaKind::Video,
        }
    }
}

/// The page's `sendonly` m-lines: what the relay receives.
#[derive(Debug, Clone, Copy)]
struct Incoming {
    microphone: Mid,
    camera: Mid,
    /// The page's second video m-line, if it offered one.
    screen: Option<Mid>,
}

impl Incoming {
    fn mid(&self, track: Track) -> Option<Mid> {
        match track {
            Track::Microphone => Some(self.microphone),
            Track::Camera => Some(self.camera),
            Track::Screen => self.screen,
        }
    }

    fn track(&self, mid: Mid) -> Option<Track> {
        [Track::Microphone, Track::Camera, Track::Screen]
            .into_iter()
            .find(|&t| self.mid(t) == Some(mid))
    }

    fn has(&self, mid: Mid) -> bool {
        self.track(mid).is_some()
    }
}

/// One of the page's `recvonly` m-lines, and whose track the relay sends
/// on it.
#[derive(Debug)]
struct Slot {
    mid: Mid,
    kind: MediaKind,
    /// The participant and track it carries; none while it's free.
    source: Option<(u32, Track)>,
    /// What it carried last, so someone who comes back gets their line.
    last: Option<(u32, Track)>,
    /// The connection whose packets went out on it last. A participant
    /// joining again is a new connection, with a new keyframe to wait for.
    fed_by: Option<PeerId>,
    /// Video: nothing goes out until a keyframe starts, so the page's
    /// decoder never gets frames it can't use.
    waiting: bool,
    continuity: Continuity,
}

impl Slot {
    fn new(mid: Mid, kind: MediaKind) -> Slot {
        Slot {
            mid,
            kind,
            source: None,
            last: None,
            fed_by: None,
            waiting: true,
            continuity: Continuity::default(),
        }
    }
}

/// One admitted participant: a page's connection to the relay.
struct Peer {
    room: String,
    participant: u32,
    /// What others see under this participant's video, if the console
    /// said.
    name: Option<String>,
    rtc: Rtc,
    incoming: Incoming,
    slots: Vec<Slot>,
    /// The page said it shares its screen.
    sharing: bool,
    channel: Option<ChannelId>,
    connected: bool,
    /// The camera and the screen: someone now watches it, so ask for a
    /// keyframe with its next packet (its receive stream may not exist
    /// yet).
    keyframe_due: [bool; 2],
    /// When the relay last asked for a key frame of the camera and of the
    /// screen.
    asked: [Option<Instant>; 2],
    /// The last `peers` line sent, so an unchanged one isn't sent again.
    told: Option<String>,
    timeout: Instant,
}

/// What one participant's output asks of the others.
enum Work {
    /// A packet from `from`'s page, for everyone else in the room.
    Forward {
        from: PeerId,
        track: Track,
        codec: PayloadParams,
        packet: Box<RtpPacket>,
    },
    /// `viewer`'s page asked for a keyframe of the video on `mid`.
    Keyframe {
        viewer: PeerId,
        mid: Mid,
        kind: KeyframeRequestKind,
    },
    /// `viewer`'s connection is up: it needs keyframes to start from.
    Connected(PeerId),
    /// The data channel opened: tell the page who is in the room.
    ChannelOpen(PeerId),
    /// A message from the page on the data channel.
    Message(PeerId, Vec<u8>),
    /// The connection closed or timed out.
    Gone(PeerId),
}

/// What a page sends on the data channel, one JSON object a line.
#[derive(Debug, serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum FromPage {
    /// More `recvonly` m-lines, or any other change to its connection.
    Offer { sdp: String },
    /// It started or stopped sending its screen on its second video line.
    Screen { on: bool },
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
    /// When the loop last looked for slots waiting for a key frame.
    checked: Instant,
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
            checked: Instant::now(),
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
                name,
                offer,
                reply,
            } => {
                let answer = self.join(room, participant, name, &offer);
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
        name: Option<String>,
        offer: &str,
    ) -> Result<SdpAnswer, JoinError> {
        let seats = self.rooms.get(&room);
        let replacing = seats.and_then(|s| s.get(&participant)).copied();
        if replacing.is_none() && seats.is_some_and(|s| s.len() >= SEATS) {
            return Err(JoinError::Full);
        }
        let (rtc, answer, incoming, slots, timeout) = self.answer(offer)?;
        if let Some(old) = replacing
            && let Some(peer) = self.remove(old)
        {
            info!(room, participant, "replaced by a new connection");
            hang_up(&self.socket, peer);
        }
        let id = PeerId(self.next_id);
        self.next_id += 1;
        self.peers.insert(
            id,
            Peer {
                room: room.clone(),
                participant,
                name: name.as_deref().and_then(display_name),
                rtc,
                incoming,
                slots,
                sharing: false,
                channel: None,
                connected: false,
                keyframe_due: [false; 2],
                asked: [None; 2],
                told: None,
                timeout,
            },
        );
        self.rooms
            .entry(room.clone())
            .or_default()
            .insert(participant, id);
        info!(room, participant, "joined");
        self.changed(&room);
        self.process();
        Ok(answer)
    }

    /// A new `Rtc` that has accepted `offer`, the page's m-lines, and the
    /// answer. The offer must have a data channel, one audio and one or two
    /// video `sendonly` m-lines, in Opus and VP8, and any number of
    /// `recvonly` ones.
    fn answer(
        &self,
        offer: &str,
    ) -> Result<(Rtc, SdpAnswer, Incoming, Vec<Slot>, Instant), JoinError> {
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
        let media = media_of(&rtc, offer);
        let (incoming, slots) = layout(&media).map_err(bad)?;
        Ok((rtc, answer, incoming, slots, timeout))
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
        self.changed(room);
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
        if now.saturating_duration_since(self.checked) >= KEYFRAME_CHECK {
            self.checked = now;
            self.ask_again(now);
        }
        self.process();
    }

    /// Asks again for each key frame a connected viewer's slot still waits
    /// for, at most every [`KEYFRAME_RETRY`] per sender and track.
    fn ask_again(&mut self, now: Instant) {
        let mut waiting: Vec<(String, u32, Track)> = Vec::new();
        for peer in self.peers.values().filter(|p| p.connected) {
            for slot in peer.slots.iter().filter(|s| s.waiting) {
                if let Some((participant, track)) = slot.source
                    && track.kind() == MediaKind::Video
                {
                    let wanted = (peer.room.clone(), participant, track);
                    if !waiting.contains(&wanted) {
                        waiting.push(wanted);
                    }
                }
            }
        }
        for (room, participant, track) in waiting {
            let asked = self
                .rooms
                .get(&room)
                .and_then(|seats| seats.get(&participant))
                .and_then(|id| self.peers.get(id))
                .and_then(|p| p.asked[due_index(track)?]);
            if asked.is_none_or(|t| now.saturating_duration_since(t) >= KEYFRAME_RETRY) {
                self.request_keyframe(&room, participant, track, KeyframeRequestKind::Pli);
            }
        }
    }

    fn process(&mut self) {
        while let Some(work) = self.work.pop_front() {
            match work {
                Work::Forward {
                    from,
                    track,
                    codec,
                    packet,
                } => self.forward(from, track, codec, &packet),
                Work::Keyframe { viewer, mid, kind } => {
                    let Some(peer) = self.peers.get(&viewer) else {
                        continue;
                    };
                    let room = peer.room.clone();
                    let source = peer
                        .slots
                        .iter()
                        .find(|s| s.mid == mid)
                        .and_then(|s| s.source);
                    if let Some((participant, track)) = source {
                        self.request_keyframe(&room, participant, track, kind);
                    }
                }
                Work::Connected(viewer) => {
                    let Some(peer) = self.peers.get(&viewer) else {
                        continue;
                    };
                    let room = peer.room.clone();
                    let watched: Vec<(u32, Track)> = peer
                        .slots
                        .iter()
                        .filter(|s| s.kind == MediaKind::Video)
                        .filter_map(|s| s.source)
                        .collect();
                    for (participant, track) in watched {
                        self.request_keyframe(&room, participant, track, KeyframeRequestKind::Pli);
                    }
                }
                Work::ChannelOpen(id) => {
                    self.assign(id);
                    self.tell(id, true);
                }
                Work::Message(id, data) => self.message(id, &data),
                Work::Gone(id) => {
                    if let Some(peer) = self.remove(id) {
                        info!(
                            room = peer.room,
                            participant = peer.participant,
                            "left: the connection closed"
                        );
                        self.changed(&peer.room);
                    }
                }
            }
        }
    }

    /// Writes `packet` to every other participant's slot for that track:
    /// the payload as it came, the payload type mapped by codec.
    fn forward(&mut self, from: PeerId, track: Track, codec: PayloadParams, packet: &RtpPacket) {
        let Some(participant) = self.peers.get(&from).map(|p| p.participant) else {
            return;
        };
        let kind = track.kind();
        let clock = codec.spec().clock_rate.get();
        for to in self.others(from) {
            let Some(peer) = self.peers.get_mut(&to) else {
                continue;
            };
            if !peer.connected {
                continue;
            }
            let Some(slot) = peer
                .slots
                .iter_mut()
                .find(|s| s.source == Some((participant, track)))
            else {
                continue;
            };
            if kind == MediaKind::Video {
                if slot.fed_by != Some(from) {
                    slot.fed_by = Some(from);
                    slot.waiting = true;
                }
                if slot.waiting {
                    if !vp8_starts_keyframe(&packet.payload) {
                        continue;
                    }
                    slot.waiting = false;
                }
            }
            let mid = slot.mid;
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
            let (seq_no, time) = slot.continuity.map(
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

    /// Asks `participant`'s page for a keyframe of `track` now, or with its
    /// next packet of it if it isn't sending yet.
    fn request_keyframe(
        &mut self,
        room: &str,
        participant: u32,
        track: Track,
        kind: KeyframeRequestKind,
    ) {
        let Some(&source) = self.rooms.get(room).and_then(|s| s.get(&participant)) else {
            return;
        };
        let Some(peer) = self.peers.get_mut(&source) else {
            return;
        };
        let (Some(mid), Some(due)) = (peer.incoming.mid(track), due_index(track)) else {
            return;
        };
        peer.asked[due] = Some(Instant::now());
        let mut api = peer.rtc.direct_api();
        match api.stream_rx_by_mid(mid, None) {
            Some(stream) if peer.connected => {
                stream.request_keyframe(kind);
                drain(&self.socket, source, peer, &mut self.work);
            }
            _ => peer.keyframe_due[due] = true,
        }
    }

    /// Gives `viewer` a slot for each track it should get (everyone else's
    /// microphone and camera, and screens being shared) from its free
    /// ones, and frees the slots of tracks that have gone.
    fn assign(&mut self, viewer: PeerId) {
        let Some(peer) = self.peers.get(&viewer) else {
            return;
        };
        let room = peer.room.clone();
        let wanted: Vec<(u32, Track)> = self
            .rooms
            .get(&room)
            .into_iter()
            .flatten()
            .filter(|&(_, &id)| id != viewer)
            .filter_map(|(&participant, id)| Some((participant, self.peers.get(id)?)))
            .flat_map(|(participant, other)| {
                let screen = other.sharing && other.incoming.screen.is_some();
                [Track::Microphone, Track::Camera]
                    .into_iter()
                    .chain(screen.then_some(Track::Screen))
                    .map(move |track| (participant, track))
            })
            .collect();
        let Some(peer) = self.peers.get_mut(&viewer) else {
            return;
        };
        for slot in &mut peer.slots {
            if slot.source.is_some_and(|s| !wanted.contains(&s)) {
                slot.source = None;
            }
        }
        let mut watch = Vec::new();
        for want in wanted {
            if peer.slots.iter().any(|s| s.source == Some(want)) {
                continue;
            }
            let kind = want.1.kind();
            let free = |s: &Slot| s.source.is_none() && s.kind == kind;
            let Some(slot) = peer
                .slots
                .iter()
                .position(|s| free(s) && s.last == Some(want))
                .or_else(|| peer.slots.iter().position(free))
            else {
                continue;
            };
            let slot = &mut peer.slots[slot];
            slot.source = Some(want);
            slot.last = Some(want);
            slot.fed_by = None;
            slot.waiting = true;
            if kind == MediaKind::Video && peer.connected {
                watch.push(want);
            }
        }
        for (participant, track) in watch {
            self.request_keyframe(&room, participant, track, KeyframeRequestKind::Pli);
        }
    }

    /// Everyone in `room` gets the slots and the `peers` line for who is
    /// there now.
    fn changed(&mut self, room: &str) {
        let ids: Vec<PeerId> = self
            .rooms
            .get(room)
            .map(|seats| seats.values().copied().collect())
            .unwrap_or_default();
        for id in ids {
            self.assign(id);
            self.tell(id, false);
        }
    }

    /// The `peers` line on the data channel, once it's open: who is in the
    /// room, their names, who shares a screen, and which of this page's
    /// slots carries what. `again` sends it even if it hasn't changed.
    fn tell(&mut self, id: PeerId, again: bool) {
        let Some(peer) = self.peers.get(&id) else {
            return;
        };
        let seats: Vec<&Peer> = self
            .rooms
            .get(&peer.room)
            .into_iter()
            .flat_map(|seats| seats.values())
            .filter_map(|id| self.peers.get(id))
            .collect();
        let names: serde_json::Map<String, serde_json::Value> = seats
            .iter()
            .filter_map(|p| Some((p.participant.to_string(), json!(p.name.as_deref()?))))
            .collect();
        let tracks: Vec<serde_json::Value> = peer
            .slots
            .iter()
            .filter_map(|s| {
                let (participant, track) = s.source?;
                Some(json!({
                    "mid": s.mid.to_string(),
                    "participant": participant,
                    "kind": match s.kind {
                        MediaKind::Audio => "audio",
                        MediaKind::Video => "video",
                    },
                    "screen": track == Track::Screen,
                }))
            })
            .collect();
        let mut line = json!({
            "type": "peers",
            "peers": seats.iter().map(|p| p.participant).collect::<Vec<_>>(),
            "sharing": seats.iter().filter(|p| p.sharing).map(|p| p.participant).collect::<Vec<_>>(),
            "tracks": tracks,
        });
        if !names.is_empty() {
            line["names"] = names.into();
        }
        let line = format!("{line}\n");
        let Some(peer) = self.peers.get_mut(&id) else {
            return;
        };
        if !again && peer.told.as_deref() == Some(line.as_str()) {
            return;
        }
        if write_line(peer, &line) {
            peer.told = Some(line);
        }
        drain(&self.socket, id, peer, &mut self.work);
    }

    /// A message from `id`'s page: lines of JSON.
    fn message(&mut self, id: PeerId, data: &[u8]) {
        let Ok(text) = std::str::from_utf8(data) else {
            return;
        };
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            match serde_json::from_str::<FromPage>(line) {
                Ok(FromPage::Offer { sdp }) => self.renegotiate(id, &sdp),
                Ok(FromPage::Screen { on }) => self.screen(id, on),
                Err(e) => debug!("a line from the page the relay doesn't read: {e}"),
            }
        }
    }

    /// Answers the page's new offer on the data channel. New `recvonly`
    /// m-lines become free slots; then the page hears what each carries.
    fn renegotiate(&mut self, id: PeerId, sdp: &str) {
        let Some(peer) = self.peers.get_mut(&id) else {
            return;
        };
        let answered = SdpOffer::from_sdp_string(&without_unresolved_candidates(sdp))
            .map_err(|e| format!("the offer isn't SDP the relay reads: {e}"))
            .and_then(|offer| {
                peer.rtc
                    .sdp_api()
                    .accept_offer(offer)
                    .map_err(|e| format!("the relay can't answer this offer: {e}"))
            });
        let line = match answered {
            Ok(answer) => {
                let added: Vec<Slot> = media_of(&peer.rtc, sdp)
                    .into_iter()
                    .filter(|m| m.direction() == Direction::SendOnly)
                    .filter(|m| !m.remote_pts().is_empty())
                    .filter(|m| !peer.incoming.has(m.mid()))
                    .filter(|m| !peer.slots.iter().any(|s| s.mid == m.mid()))
                    .map(|m| Slot::new(m.mid(), m.kind()))
                    .collect();
                debug!(
                    room = peer.room,
                    participant = peer.participant,
                    added = added.len(),
                    "renegotiated"
                );
                peer.slots.extend(added);
                json!({"type": "answer", "sdp": answer.to_sdp_string()})
            }
            Err(why) => {
                warn!(room = peer.room, participant = peer.participant, "{why}");
                json!({"type": "refused", "error": why})
            }
        };
        if !write_line(peer, &format!("{line}\n")) {
            warn!(
                room = peer.room,
                participant = peer.participant,
                "the answer didn't fit the data channel"
            );
        }
        drain(&self.socket, id, peer, &mut self.work);
        self.assign(id);
        // Always, so the page knows its new lines are taken or free.
        self.tell(id, true);
    }

    /// The page started or stopped sharing its screen.
    fn screen(&mut self, id: PeerId, on: bool) {
        let Some(peer) = self.peers.get_mut(&id) else {
            return;
        };
        if peer.incoming.screen.is_none() || peer.sharing == on {
            return;
        }
        peer.sharing = on;
        info!(
            room = peer.room,
            participant = peer.participant,
            on,
            "screen sharing"
        );
        let room = peer.room.clone();
        self.changed(&room);
    }

    /// Everyone in `id`'s room but `id`.
    fn others(&self, id: PeerId) -> Vec<PeerId> {
        match self.peers.get(&id) {
            Some(peer) => self
                .rooms
                .get(&peer.room)
                .map(|seats| seats.values().copied().filter(|&p| p != id).collect())
                .unwrap_or_default(),
            None => Vec::new(),
        }
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
                work.push_back(Work::ChannelOpen(id));
            }
            Event::ChannelData(data) if self.channel == Some(data.id) && !data.binary => {
                work.push_back(Work::Message(id, data.data));
            }
            Event::ChannelClose(channel) if self.channel == Some(channel) => self.channel = None,
            Event::RtpPacket(packet) => self.received(id, packet, work),
            Event::KeyframeRequest(request) => {
                work.push_back(Work::Keyframe {
                    viewer: id,
                    mid: request.mid,
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
        let Some(track) = self.incoming.track(stream.mid()) else {
            return;
        };
        if let Some(due) = due_index(track)
            && self.keyframe_due[due]
        {
            // Without it a new viewer sees nothing until the sender's next
            // keyframe, which can be seconds away.
            stream.request_keyframe(KeyframeRequestKind::Pli);
            self.keyframe_due[due] = false;
        }
        let pt = packet.header.payload_type;
        let Some(codec) = self.rtc.codec_config().find(|p| p.pt() == pt).copied() else {
            return;
        };
        work.push_back(Work::Forward {
            from: id,
            track,
            codec,
            packet: Box::new(packet),
        });
    }
}

/// Where a video track's `keyframe_due` flag is.
fn due_index(track: Track) -> Option<usize> {
    match track {
        Track::Microphone => None,
        Track::Camera => Some(0),
        Track::Screen => Some(1),
    }
}

/// The m-lines `sdp` names, as `rtc` now has them, in the SDP's order.
fn media_of<'a>(rtc: &'a Rtc, sdp: &str) -> Vec<&'a MediaLine> {
    sdp.lines()
        .filter_map(|l| l.trim_end().strip_prefix("a=mid:"))
        .filter_map(|mid| rtc.media(mid.into()))
        .collect()
}

/// The page's sending m-lines and its receiving ones, as the relay sees
/// them: the page's `sendonly` is the relay's `recvonly`, and the other
/// way round. The page sends one audio line (its microphone) and one or
/// two video lines (its camera, then its screen); it receives on any
/// number of lines.
fn layout(media: &[&MediaLine]) -> Result<(Incoming, Vec<Slot>), String> {
    let codec = |m: &MediaLine| match m.kind() {
        MediaKind::Audio => "Opus",
        MediaKind::Video => "VP8",
    };
    let mut audio = Vec::new();
    let mut video = Vec::new();
    let mut slots = Vec::new();
    for m in media {
        if m.remote_pts().is_empty() {
            return Err(format!(
                "the offer's {:?} m-line has no {}",
                m.kind(),
                codec(m)
            ));
        }
        match (m.direction(), m.kind()) {
            (Direction::RecvOnly, MediaKind::Audio) => audio.push(m.mid()),
            (Direction::RecvOnly, MediaKind::Video) => video.push(m.mid()),
            (Direction::SendOnly, kind) => slots.push(Slot::new(m.mid(), kind)),
            _ => return Err("every m-line of the offer is sendonly or recvonly".into()),
        }
    }
    let ([microphone], [camera, screen @ ..]) = (audio.as_slice(), video.as_slice()) else {
        return Err(
            "the offer needs exactly one Audio m-line that is sendonly and one Video m-line that is sendonly"
                .into(),
        );
    };
    let screen = match screen {
        [] => None,
        [screen] => Some(*screen),
        _ => return Err("the offer may send two Video m-lines, its camera and its screen".into()),
    };
    let incoming = Incoming {
        microphone: *microphone,
        camera: *camera,
        screen,
    };
    Ok((incoming, slots))
}

/// Whether an RTP payload in VP8 (RFC 7741) starts a key frame: the start
/// of partition 0, with the frame tag's P bit clear. Those bytes stay in
/// the clear under SFrame for the packetizer (ADR 0030); the rest of the
/// frame, sealed, is never read.
fn vp8_starts_keyframe(payload: &[u8]) -> bool {
    let Some(&first) = payload.first() else {
        return false;
    };
    let start = first & 0x10 != 0 && first & 0x07 == 0;
    if !start {
        return false;
    }
    let mut at = 1;
    if first & 0x80 != 0 {
        let Some(&x) = payload.get(at) else {
            return false;
        };
        at += 1;
        if x & 0x80 != 0 {
            // The picture id: one byte, or two with its M bit.
            let long = payload.get(at).is_some_and(|b| b & 0x80 != 0);
            at += if long { 2 } else { 1 };
        }
        if x & 0x40 != 0 {
            at += 1;
        }
        if x & 0x30 != 0 {
            at += 1;
        }
    }
    payload.get(at).is_some_and(|tag| tag & 0x01 == 0)
}

/// A display name as the relay keeps it: printable, trimmed, at most
/// [`NAME_MAX`] characters. None if nothing is left.
fn display_name(name: &str) -> Option<String> {
    let name: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(NAME_MAX)
        .collect();
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// Writes one line on the page's data channel; whether it was taken.
fn write_line(peer: &mut Peer, line: &str) -> bool {
    let Some(mut channel) = peer.channel.and_then(|c| peer.rtc.channel(c)) else {
        return false;
    };
    match channel.write(false, line.as_bytes()) {
        Ok(true) => true,
        Ok(false) => {
            warn!(
                room = peer.room,
                participant = peer.participant,
                "data channel full"
            );
            false
        }
        Err(e) => {
            warn!(
                room = peer.room,
                participant = peer.participant,
                "data channel: {e}"
            );
            false
        }
    }
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
/// when the participant feeding it changes (someone new in a free slot, or
/// the same index joining again), so the page sees one stream rather than
/// a jump.
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

    /// The relay reads VP8's payload descriptor and frame tag, which stay
    /// clear under SFrame, to know where a key frame starts; nothing else.
    #[test]
    fn cf21_a_vp8_key_frame_start_is_found_in_the_clear_header() {
        // S=1, PID=0, then the frame tag with P=0: a key frame.
        assert!(vp8_starts_keyframe(&[0x10, 0x00, 0xaa]));
        // P=1: an interframe.
        assert!(!vp8_starts_keyframe(&[0x10, 0x01, 0xaa]));
        // Not the start of a partition, or not partition 0.
        assert!(!vp8_starts_keyframe(&[0x00, 0x00]));
        assert!(!vp8_starts_keyframe(&[0x11, 0x00]));
        // Chrome's: X with I (a 15-bit picture id), L and T/K.
        assert!(vp8_starts_keyframe(&[
            0x90, 0xe0, 0x81, 0x23, 0x05, 0x40, 0x10
        ]));
        assert!(!vp8_starts_keyframe(&[
            0x90, 0xe0, 0x81, 0x23, 0x05, 0x40, 0x11
        ]));
        // A 7-bit picture id only.
        assert!(vp8_starts_keyframe(&[0x90, 0x80, 0x12, 0x9c]));
        // Cut short.
        assert!(!vp8_starts_keyframe(&[]));
        assert!(!vp8_starts_keyframe(&[0x90, 0xe0, 0x81]));
    }

    #[test]
    fn display_names_are_plain_and_short() {
        assert_eq!(display_name("  Priya Raman "), Some("Priya Raman".into()));
        assert_eq!(display_name("a\u{0}b\nc"), Some("abc".into()));
        assert_eq!(display_name(" \t "), None);
        assert_eq!(display_name(&"x".repeat(100)).unwrap().len(), NAME_MAX);
    }
}
