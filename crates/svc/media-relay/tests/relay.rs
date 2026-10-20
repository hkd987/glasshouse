//! The relay with str0m clients standing in for the Meet page (CF-21). Each
//! makes the page's offer (a data channel; audio and video sendonly, with a
//! second video line for its screen if asked; then one audio and one video
//! recvonly), is admitted through the private API like the console does,
//! and sends RTP whose payload says who sent it. Video payloads start with
//! VP8's payload descriptor and frame tag, as the real page's do, so the
//! relay can find key frames. Like the page, it offers more recvonly lines
//! over the data channel when the relay's `peers` line shows someone
//! without one.

#![allow(clippy::unwrap_used)]

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use glass_media_relay::{Config, Relay, SEATS};
use serde_json::{Value, json};
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::channel::ChannelId;
use str0m::format::{Codec, FormatParams};
use str0m::media::{Direction, Frequency, KeyframeRequestKind, MediaKind, Mid, Pt};
use str0m::net::{Protocol, Receive};
use str0m::rtp::RtpWrite;
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};

/// How long a test waits for something that should take well under a second.
const PATIENCE: Duration = Duration::from_secs(10);

struct Harness {
    // Dropped before the runtime it runs on.
    relay: Relay,
    _runtime: tokio::runtime::Runtime,
}

fn relay() -> Harness {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let config = Config {
        udp: "127.0.0.1:0".parse().unwrap(),
        public: "127.0.0.1".parse().unwrap(),
        api: "127.0.0.1:0".parse().unwrap(),
    };
    let relay = runtime.block_on(Relay::start(config)).unwrap();
    Harness {
        relay,
        _runtime: runtime,
    }
}

/// One request to the private API, as the console makes it.
fn http(api: SocketAddr, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let mut stream = TcpStream::connect(api).unwrap();
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: relay\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    let body = match body {
        "" => Value::Null,
        body => serde_json::from_str(body).unwrap(),
    };
    (status, body)
}

/// Payload type numbers, to check the relay maps them by codec.
#[derive(Clone, Copy)]
enum Numbering {
    /// str0m's defaults, every codec it knows (VP8 is 96, Opus 111).
    Default,
    /// VP8 and Opus only, numbered like no one else.
    Other,
}

/// VP8's payload descriptor (S=1, partition 0) and the frame tag's first
/// byte (P=0 on a key frame), then the text.
fn vp8(key: bool, text: &str) -> Vec<u8> {
    let mut out = vec![0x10, if key { 0x00 } else { 0x01 }];
    out.extend_from_slice(text.as_bytes());
    out
}

/// One RTP packet a page received.
#[derive(Debug)]
struct Received {
    mid: Option<Mid>,
    pt: Pt,
    /// `<page>:<audio|video|screen>:<n>`.
    text: String,
    /// A VP8 key frame's first packet.
    key: bool,
}

/// A stand-in for the Meet page.
struct Page {
    name: &'static str,
    rtc: Rtc,
    socket: UdpSocket,
    addr: SocketAddr,
    offer: String,
    pending: Option<SdpPendingOffer>,
    /// The recvonly lines of the pending offer.
    pending_slots: Vec<Mid>,
    channel: ChannelId,
    /// Its index in the meeting, once admitted.
    me: Option<u32>,
    /// The page's sendonly mids: audio, camera, and its screen if it has one.
    send: [Mid; 2],
    screen: Option<Mid>,
    /// Its recvonly mids.
    slots: Vec<Mid>,
    /// Opus's and VP8's payload types, once answered.
    pts: [Pt; 2],
    timeout: Instant,
    sending: bool,
    sharing: bool,
    /// Offers more slots when it lacks them, as the Meet page does.
    renegotiates: bool,
    /// Offers it sent over the data channel.
    offers: usize,
    sent: u64,
    last_sent: Option<Instant>,
    /// The camera's and the screen's next frame is a key frame.
    key_due: [bool; 2],
    connected: bool,
    channel_open: bool,
    closed: bool,
    received: Vec<Received>,
    keyframe_requests: Vec<Mid>,
    messages: Vec<Value>,
    inbox: VecDeque<Value>,
}

impl Page {
    fn new(name: &'static str, numbering: Numbering) -> Page {
        Page::with(name, numbering, false)
    }

    fn sharer(name: &'static str) -> Page {
        Page::with(name, Numbering::Default, true)
    }

    fn with(name: &'static str, numbering: Numbering, screen: bool) -> Page {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let addr = socket.local_addr().unwrap();
        let mut config = Rtc::builder().set_rtp_mode(true);
        if let Numbering::Other = numbering {
            config = config.clear_codecs();
            let codecs = config.codec_config();
            codecs.add_config(
                100.into(),
                Some(101.into()),
                Codec::Vp8,
                Frequency::NINETY_KHZ,
                None,
                FormatParams::default(),
            );
            codecs.add_config(
                109.into(),
                None,
                Codec::Opus,
                Frequency::FORTY_EIGHT_KHZ,
                Some(2),
                FormatParams {
                    min_p_time: Some(10),
                    use_inband_fec: Some(true),
                    ..Default::default()
                },
            );
        }
        let mut rtc = config.build(Instant::now());
        rtc.add_local_candidate(Candidate::host(addr, "udp").unwrap());
        let mut change = rtc.sdp_api();
        let channel = change.add_channel("glass".into());
        let send = [
            change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None),
            change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None),
        ];
        let screen = screen
            .then(|| change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None));
        let slots = vec![
            change.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None),
            change.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None),
        ];
        let (offer, pending) = change.apply().unwrap();
        Page {
            name,
            rtc,
            socket,
            addr,
            offer: offer.to_sdp_string(),
            pending: Some(pending),
            pending_slots: Vec::new(),
            channel,
            me: None,
            send,
            screen,
            slots,
            pts: [0.into(), 0.into()],
            timeout: Instant::now(),
            sending: true,
            sharing: false,
            renegotiates: true,
            offers: 0,
            sent: 0,
            last_sent: None,
            key_due: [true, true],
            connected: false,
            channel_open: false,
            closed: false,
            received: Vec::new(),
            keyframe_requests: Vec::new(),
            messages: Vec::new(),
            inbox: VecDeque::new(),
        }
    }

    fn accept(&mut self, answer: &str) {
        let answer = SdpAnswer::from_sdp_string(answer).unwrap();
        let pending = self.pending.take().unwrap();
        self.rtc.sdp_api().accept_answer(pending, answer).unwrap();
        self.drain();
        let pt = |rtc: &Rtc, mid: Mid, codec: Codec| {
            let negotiated = rtc.media(mid).unwrap().remote_pts();
            rtc.codec_config()
                .find(|p| p.spec().codec == codec && negotiated.contains(&p.pt()))
                .unwrap()
                .pt()
        };
        self.pts = [
            pt(&self.rtc, self.send[0], Codec::Opus),
            pt(&self.rtc, self.send[1], Codec::Vp8),
        ];
    }

    /// Reads what has arrived, hands str0m the time, handles the relay's
    /// lines, and sends a packet of audio and one of video (and one of its
    /// screen while sharing) every 20 ms once connected.
    fn poll(&mut self) {
        let mut buf = [0u8; 2048];
        loop {
            let (n, source) = match self.socket.recv_from(&mut buf) {
                Ok(received) => received,
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(_) => continue,
            };
            let Ok(contents) = buf[..n].try_into() else {
                continue;
            };
            let input = Input::Receive(
                Instant::now(),
                Receive {
                    proto: Protocol::Udp,
                    source,
                    destination: self.addr,
                    contents,
                },
            );
            if self.rtc.accepts(&input) {
                self.rtc.handle_input(input).unwrap();
                self.drain();
            }
        }
        let now = Instant::now();
        if now >= self.timeout {
            self.rtc.handle_input(Input::Timeout(now)).unwrap();
            self.drain();
        }
        while let Some(message) = self.inbox.pop_front() {
            self.handle(message);
        }
        let due = self
            .last_sent
            .is_none_or(|t| now - t >= Duration::from_millis(20));
        if self.connected && self.sending && due {
            self.last_sent = Some(now);
            self.sent += 1;
            let n = self.sent;
            let mut lines = vec![(self.send[0], self.pts[0], 960, false, "audio")];
            lines.push((self.send[1], self.pts[1], 1800, true, "video"));
            if let (true, Some(screen)) = (self.sharing, self.screen) {
                lines.push((screen, self.pts[1], 1800, true, "screen"));
            }
            for (mid, pt, step, video, kind) in lines {
                let text = format!("{}:{kind}:{n}", self.name);
                let payload = if video {
                    let i = usize::from(kind == "screen");
                    let key = std::mem::take(&mut self.key_due[i]) || n.is_multiple_of(50);
                    vp8(key, &text)
                } else {
                    text.into_bytes()
                };
                let mut api = self.rtc.direct_api();
                let stream = api.stream_tx_by_mid(mid, None).unwrap();
                stream.write_rtp(
                    RtpWrite::new(pt, (1000 + n).into(), (n * step) as u32, now, payload)
                        .marker(true),
                );
                self.drain();
            }
        }
    }

    fn drain(&mut self) {
        loop {
            match self.rtc.poll_output().unwrap() {
                Output::Timeout(t) => {
                    self.timeout = t;
                    return;
                }
                Output::Transmit(t) => {
                    let _ = self.socket.send_to(&t.contents, t.destination);
                }
                Output::Event(event) => self.event(event),
            }
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Connected => self.connected = true,
            Event::Closed | Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                self.closed = true
            }
            Event::ChannelOpen(..) => self.channel_open = true,
            Event::ChannelData(data) => {
                let text = std::str::from_utf8(&data.data).unwrap();
                assert!(text.ends_with('\n'), "{text:?} is a line");
                let message: Value = serde_json::from_str(text).unwrap();
                self.messages.push(message.clone());
                self.inbox.push_back(message);
            }
            Event::RtpPacket(packet) => {
                let mid = self
                    .rtc
                    .direct_api()
                    .stream_rx(&packet.header.ssrc)
                    .map(|s| s.mid());
                let payload = &packet.payload;
                let (key, text) = match payload.first() {
                    Some(0x10) => (payload[1] & 0x01 == 0, &payload[2..]),
                    _ => (false, &payload[..]),
                };
                // Like the engine's decoder: video that starts without a
                // key frame (it came before the answer naming its line was
                // applied) asks for one.
                let video = payload.first() == Some(&0x10);
                if let Some(mid) = mid
                    && video
                    && !key
                    && !self.received.iter().any(|r| r.mid == Some(mid))
                {
                    let mut api = self.rtc.direct_api();
                    if let Some(stream) = api.stream_rx_by_mid(mid, None) {
                        stream.request_keyframe(KeyframeRequestKind::Pli);
                    }
                }
                self.received.push(Received {
                    mid,
                    pt: packet.header.payload_type,
                    text: String::from_utf8(text.to_vec()).unwrap(),
                    key,
                });
            }
            Event::KeyframeRequest(request) => {
                if request.mid == self.send[1] {
                    self.key_due[0] = true;
                } else if Some(request.mid) == self.screen {
                    self.key_due[1] = true;
                }
                self.keyframe_requests.push(request.mid);
            }
            _ => {}
        }
    }

    /// One line on the data channel.
    fn say(&mut self, message: Value) {
        let line = format!("{message}\n");
        let mut channel = self.rtc.channel(self.channel).unwrap();
        assert!(channel.write(false, line.as_bytes()).unwrap());
        self.drain();
    }

    fn handle(&mut self, message: Value) {
        match message["type"].as_str().unwrap() {
            "answer" => {
                let answer = SdpAnswer::from_sdp_string(message["sdp"].as_str().unwrap()).unwrap();
                let pending = self.pending.take().unwrap();
                self.rtc.sdp_api().accept_answer(pending, answer).unwrap();
                self.slots.append(&mut self.pending_slots);
                self.drain();
            }
            "refused" => panic!("{} was refused: {message}", self.name),
            "peers" => self.offer_slots(&message),
            other => panic!("{} got {other}", self.name),
        }
    }

    /// Offers a recvonly line for each track the `peers` line lists
    /// without one, unless an offer is out.
    fn offer_slots(&mut self, peers: &Value) {
        if !self.renegotiates || self.pending.is_some() {
            return;
        }
        let me = self.me.unwrap();
        let tracks = peers["tracks"].as_array().unwrap();
        let has = |p: u64, kind: &str, screen: bool| {
            tracks
                .iter()
                .any(|t| t["participant"] == p && t["kind"] == kind && t["screen"] == screen)
        };
        let mut lacking = Vec::new();
        for p in peers["peers"].as_array().unwrap() {
            let p = p.as_u64().unwrap();
            if p == u64::from(me) {
                continue;
            }
            if !has(p, "audio", false) {
                lacking.push(MediaKind::Audio);
            }
            if !has(p, "video", false) {
                lacking.push(MediaKind::Video);
            }
        }
        for p in peers["sharing"].as_array().unwrap() {
            let p = p.as_u64().unwrap();
            if p != u64::from(me) && !has(p, "video", true) {
                lacking.push(MediaKind::Video);
            }
        }
        if lacking.is_empty() {
            return;
        }
        let mut change = self.rtc.sdp_api();
        self.pending_slots = lacking
            .into_iter()
            .map(|kind| change.add_media(kind, Direction::RecvOnly, None, None, None))
            .collect();
        let (offer, pending) = change.apply().unwrap();
        self.pending = Some(pending);
        self.offers += 1;
        self.say(json!({"type": "offer", "sdp": offer.to_sdp_string()}));
    }

    fn share(&mut self, on: bool) {
        self.sharing = on;
        self.key_due[1] = true;
        self.say(json!({"type": "screen", "on": on}));
    }

    /// Packets of `kind` (audio, video or screen) received from the page
    /// named `from`.
    fn from(&self, from: &str, kind: &str) -> usize {
        let prefix = format!("{from}:{kind}:");
        self.received
            .iter()
            .filter(|r| r.text.starts_with(&prefix))
            .count()
    }

    fn last_peers(&self) -> Option<&Value> {
        self.messages.iter().rev().find(|m| m["type"] == "peers")
    }

    /// Who is in the room, by the last `peers` line.
    fn peers(&self) -> Option<Value> {
        self.last_peers().map(|m| m["peers"].clone())
    }

    /// The slot the last `peers` line says carries `participant`'s `kind`.
    fn slot_of(&self, participant: u32, kind: &str, screen: bool) -> Option<Mid> {
        self.last_peers()?["tracks"]
            .as_array()?
            .iter()
            .find(|t| t["participant"] == participant && t["kind"] == kind && t["screen"] == screen)
            .map(|t| t["mid"].as_str().unwrap().into())
    }
}

/// Admits `page` to `room` the way the console does, and hands it the answer.
fn admit(api: SocketAddr, room: &str, participant: u32, page: &mut Page) -> u16 {
    admit_as(api, room, participant, None, page)
}

fn admit_as(
    api: SocketAddr,
    room: &str,
    participant: u32,
    name: Option<&str>,
    page: &mut Page,
) -> u16 {
    let mut body = json!({"participant": participant, "offer": page.offer});
    if let Some(name) = name {
        body["name"] = name.into();
    }
    let (status, body) = http(
        api,
        "POST",
        &format!("/rooms/{room}/participants"),
        Some(body),
    );
    if status == 200 {
        let answer = body["answer"].as_str().unwrap();
        assert!(answer.contains("a=ice-lite"), "{answer}");
        assert!(answer.contains("a=candidate:"), "{answer}");
        page.accept(answer);
        page.me = Some(participant);
    }
    status
}

/// Drives the pages until `done` holds or `limit` passes; whether it held.
fn run_until(
    pages: &mut [&mut Page],
    limit: Duration,
    done: impl Fn(&[&mut Page]) -> bool,
) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        for page in pages.iter_mut() {
            page.poll();
        }
        if done(pages) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    false
}

fn run_for(pages: &mut [&mut Page], time: Duration) {
    run_until(pages, time, |_| false);
}

/// Every packet `page` got came from someone in its room, on the slot the
/// relay said carries that person's track, in the page's own payload type.
/// Each video track got a key frame, and with `key_first` (slots the page
/// had before that track came) the first packet was one: the relay sends
/// nothing on a line that changes hands until a key frame starts.
fn check_received(page: &Page, others: &[(&str, u32)], key_first: bool) {
    let mut started = Vec::new();
    let mut keyed = Vec::new();
    for r in &page.received {
        let (sender, kind) = r
            .text
            .split_once(':')
            .map(|(s, rest)| (s, rest.split(':').next().unwrap()))
            .unwrap();
        let Some(&(_, participant)) = others.iter().find(|(name, _)| *name == sender) else {
            panic!(
                "{} got {:?} from someone not in its room",
                page.name, r.text
            );
        };
        let (kind, screen) = match kind {
            "audio" => ("audio", false),
            "video" => ("video", false),
            "screen" => ("video", true),
            other => panic!("{other}"),
        };
        let mid = r.mid.unwrap();
        assert!(
            page.slots.contains(&mid),
            "{} got {:?} off its slots",
            page.name,
            r.text
        );
        let said = page
            .messages
            .iter()
            .filter(|m| m["type"] == "peers")
            .any(|m| {
                m["tracks"].as_array().unwrap().iter().any(|t| {
                    t["mid"] == mid.to_string()
                        && t["participant"] == participant
                        && t["kind"] == kind
                        && t["screen"] == screen
                })
            });
        assert!(
            said,
            "{} got {:?} on {mid}, which the relay never said carries it",
            page.name, r.text
        );
        assert_eq!(r.pt, page.pts[usize::from(kind == "video")], "{:?}", r.text);
        if kind == "video" && !started.contains(&(sender, screen)) {
            assert!(
                r.key || !key_first,
                "{}'s first {kind} from {sender} isn't a key frame",
                page.name
            );
            started.push((sender, screen));
        }
        if kind == "video" && r.key && !keyed.contains(&(sender, screen)) {
            keyed.push((sender, screen));
        }
    }
    started.sort_unstable();
    keyed.sort_unstable();
    assert_eq!(started, keyed, "{}: video without a key frame", page.name);
}

#[test]
fn cf21_the_relay_forwards_only_to_participants_the_console_admitted() {
    let harness = relay();
    let api = harness.relay.api_addr();
    let mut a = Page::new("a", Numbering::Default);
    let mut b = Page::new("b", Numbering::Other);
    let mut elsewhere = Page::new("elsewhere", Numbering::Default);
    // Knows the relay's address and the page's offer, but was never
    // admitted: its answer, with made-up ICE credentials, is its own.
    let mut stranger = Page::new("stranger", Numbering::Default);
    let mut fake = Rtc::builder().set_ice_lite(true).build(Instant::now());
    fake.add_local_candidate(Candidate::host(harness.relay.media_addr(), "udp").unwrap());
    let made_up = fake
        .sdp_api()
        .accept_offer(SdpOffer::from_sdp_string(&stranger.offer).unwrap())
        .unwrap();
    stranger.accept(&made_up.to_sdp_string());
    stranger.me = Some(0);

    assert_eq!(admit(api, "meeting-1", 0, &mut a), 200);
    assert_eq!(admit(api, "meeting-1", 1, &mut b), 200);
    assert_eq!(admit(api, "meeting-2", 0, &mut elsewhere), 200);

    let mut pages = [&mut a, &mut b, &mut elsewhere, &mut stranger];
    let flowing = run_until(&mut pages, PATIENCE, |p| {
        ["audio", "video"]
            .iter()
            .all(|kind| p[0].from("b", kind) >= 25 && p[1].from("a", kind) >= 25)
    });
    // A little longer, for anything that shouldn't arrive.
    run_for(&mut pages, Duration::from_millis(500));
    let [a, b, elsewhere, stranger] = pages;
    assert!(
        flowing,
        "a got {} packets, b {}",
        a.received.len(),
        b.received.len()
    );

    check_received(a, &[("b", 1)], true);
    check_received(b, &[("a", 0)], true);
    for page in [&*a, &*b] {
        assert_eq!(page.peers(), Some(json!([0, 1])));
        // Two people fit the lines the page offered first.
        assert_eq!(page.offers, 0);
    }
    assert_ne!(
        a.pts, b.pts,
        "the two pages number their codecs differently"
    );

    assert!(elsewhere.connected && elsewhere.sent > 0);
    assert!(elsewhere.received.is_empty(), "{:?}", elsewhere.received);
    assert_eq!(
        elsewhere.last_peers(),
        Some(&json!({"type": "peers", "peers": [0], "sharing": [], "tracks": []}))
    );

    assert!(!stranger.connected);
    assert!(stranger.received.is_empty() && stranger.messages.is_empty());
}

#[test]
fn cf21_a_new_viewer_gets_a_keyframe_from_the_sender() {
    let harness = relay();
    let api = harness.relay.api_addr();
    let mut a = Page::new("a", Numbering::Default);
    let mut b = Page::new("b", Numbering::Default);
    assert_eq!(admit(api, "meeting", 0, &mut a), 200);
    assert!(run_until(&mut [&mut a], PATIENCE, |p| p[0].sent >= 25));
    assert!(a.keyframe_requests.is_empty(), "nobody watches yet");

    // The second participant joins: the relay asks for a keyframe of a's
    // video, so b doesn't wait for one.
    assert_eq!(admit(api, "meeting", 1, &mut b), 200);
    let video = a.send[1];
    let mut pages = [&mut a, &mut b];
    assert!(
        run_until(&mut pages, PATIENCE, |p| p[0]
            .keyframe_requests
            .contains(&video)),
        "a got no keyframe request"
    );
    assert!(run_until(&mut pages, PATIENCE, |p| p[1].from("a", "video") > 0));

    // b's own requests (a PLI from its decoder) reach a too.
    let [a, b] = pages;
    let before = a.keyframe_requests.len();
    let slot = b.slot_of(0, "video", false).unwrap();
    let mut api = b.rtc.direct_api();
    let stream = api.stream_rx_by_mid(slot, None).unwrap();
    stream.request_keyframe(KeyframeRequestKind::Pli);
    b.drain();
    let mut pages = [a, b];
    assert!(
        run_until(&mut pages, PATIENCE, |p| p[0].keyframe_requests.len()
            > before),
        "b's request didn't reach a"
    );
    assert!(pages[0].keyframe_requests.iter().all(|mid| *mid == video));
}

#[test]
fn cf21_three_people_each_get_the_other_twos_audio_and_video() {
    let harness = relay();
    let api = harness.relay.api_addr();
    let mut a = Page::new("a", Numbering::Default);
    let mut b = Page::new("b", Numbering::Other);
    let mut c = Page::new("c", Numbering::Default);
    assert_eq!(admit_as(api, "standup", 0, Some("Ana"), &mut a), 200);
    assert_eq!(admit(api, "standup", 1, &mut b), 200);
    // The third joins later: a and b each offer more lines over the data
    // channel, and the relay gives them c's tracks.
    assert!(run_until(&mut [&mut a, &mut b], PATIENCE, |p| p[0]
        .from("b", "video")
        > 5));
    assert_eq!(admit_as(api, "standup", 2, Some(" Chidi\n"), &mut c), 200);

    let mut pages = [&mut a, &mut b, &mut c];
    let everyone = [("a", 0), ("b", 1), ("c", 2)];
    let flowing = run_until(&mut pages, PATIENCE, |p| {
        p.iter().zip(everyone).all(|(page, (me, _))| {
            everyone
                .iter()
                .filter(|(other, _)| *other != me)
                .all(|(other, _)| {
                    page.from(other, "audio") >= 25 && page.from(other, "video") >= 25
                })
        })
    });
    let [a, b, c] = pages;
    assert!(
        flowing,
        "a got {} packets, b {}, c {}",
        a.received.len(),
        b.received.len(),
        c.received.len()
    );
    for (page, me) in [(&*a, "a"), (&*b, "b"), (&*c, "c")] {
        let others: Vec<(&str, u32)> = everyone.iter().copied().filter(|(n, _)| *n != me).collect();
        check_received(page, &others, false);
        let peers = page.last_peers().unwrap();
        assert_eq!(peers["peers"], json!([0, 1, 2]));
        assert_eq!(peers["names"], json!({"0": "Ana", "2": "Chidi"}));
        // Four slots in use, each a different line.
        let tracks = peers["tracks"].as_array().unwrap();
        assert_eq!(tracks.len(), 4, "{peers}");
        let mut mids: Vec<&str> = tracks.iter().map(|t| t["mid"].as_str().unwrap()).collect();
        mids.sort_unstable();
        mids.dedup();
        assert_eq!(mids.len(), 4);
    }
    // a and b were there first: one offer each for c; c came with room
    // for one person and offered for the second.
    assert_eq!([a.offers, b.offers, c.offers], [1, 1, 1]);
}

#[test]
fn cf21_someone_who_leaves_frees_their_lines_for_the_next() {
    let harness = relay();
    let api = harness.relay.api_addr();
    let mut a = Page::new("a", Numbering::Default);
    let mut b = Page::new("b", Numbering::Default);
    let mut c = Page::new("c", Numbering::Default);
    assert_eq!(admit(api, "r", 0, &mut a), 200);
    assert_eq!(admit(api, "r", 1, &mut b), 200);
    assert_eq!(admit(api, "r", 2, &mut c), 200);
    let mut pages = [&mut a, &mut b, &mut c];
    assert!(run_until(&mut pages, PATIENCE, |p| {
        p[0].from("c", "video") > 10 && p[1].from("c", "video") > 10 && p[2].from("a", "video") > 10
    }));
    let [a, b, c] = pages;
    let c_on_a = a.slot_of(2, "video", false).unwrap();
    let offers = a.offers;

    assert_eq!(http(api, "DELETE", "/rooms/r/participants/2", None).0, 204);
    let mut pages = [&mut *a, &mut *b, &mut *c];
    assert!(run_until(&mut pages, PATIENCE, |p| p[0].peers()
        == Some(json!([0, 1]))
        && p[2].closed));
    assert_eq!(
        pages[0].slot_of(2, "video", false),
        None,
        "c's line is free"
    );

    // d takes c's lines on a: no new offer, and d's video starts on a key
    // frame even though the line carried c's a moment ago.
    let mut d = Page::new("d", Numbering::Default);
    assert_eq!(admit(api, "r", 3, &mut d), 200);
    let mut pages = [&mut *a, &mut *b, &mut d];
    assert!(run_until(&mut pages, PATIENCE, |p| {
        p[0].from("d", "video") > 10 && p[1].from("d", "video") > 10 && p[2].from("a", "video") > 10
    }));
    assert_eq!(a.slot_of(3, "video", false), Some(c_on_a));
    assert_eq!(a.offers, offers);
    let first = a.received.iter().find(|r| r.text.starts_with("d:video:"));
    assert!(
        first.unwrap().key,
        "d's video on c's old line starts on a key frame"
    );
    check_received(a, &[("b", 1), ("c", 2), ("d", 3)], false);
    check_received(&d, &[("a", 0), ("b", 1)], false);
}

#[test]
fn cf21_a_shared_screen_reaches_everyone_marked_as_a_screen() {
    let harness = relay();
    let api = harness.relay.api_addr();
    let mut a = Page::sharer("a");
    let mut b = Page::new("b", Numbering::Other);
    let mut c = Page::new("c", Numbering::Default);
    assert_eq!(admit(api, "review", 0, &mut a), 200);
    assert_eq!(admit(api, "review", 1, &mut b), 200);
    assert_eq!(admit(api, "review", 2, &mut c), 200);
    let mut pages = [&mut a, &mut b, &mut c];
    assert!(run_until(&mut pages, PATIENCE, |p| {
        p[1].from("a", "video") > 10 && p[2].from("a", "video") > 10 && p[0].from("c", "video") > 10
    }));
    let [a, b, c] = pages;
    assert_eq!(b.from("a", "screen"), 0, "nothing shared yet");

    a.share(true);
    let mut pages = [&mut *a, &mut *b, &mut *c];
    assert!(
        run_until(&mut pages, PATIENCE, |p| {
            p[1].from("a", "screen") > 25 && p[2].from("a", "screen") > 25
        }),
        "the screen didn't reach b and c"
    );
    for page in [&*b, &*c] {
        let peers = page.last_peers().unwrap();
        assert_eq!(peers["sharing"], json!([0]));
        let screen = page.slot_of(0, "video", true).unwrap();
        assert_ne!(Some(screen), page.slot_of(0, "video", false));
        check_received(page, &[("a", 0), ("b", 1), ("c", 2)], false);
    }
    assert_eq!(a.last_peers().unwrap()["sharing"], json!([0]));
    assert_eq!(a.from("b", "screen"), 0);

    a.share(false);
    let mut pages = [&mut *a, &mut *b, &mut *c];
    assert!(run_until(&mut pages, PATIENCE, |p| {
        p[1].slot_of(0, "video", true).is_none()
            && p[2].last_peers().unwrap()["sharing"] == json!([])
    }));
    let seen = pages[1].from("a", "screen");
    run_for(&mut pages, Duration::from_millis(300));
    assert!(pages[1].from("a", "screen") <= seen + 2);
    assert!(pages[1].from("a", "video") > 0);
}

#[test]
fn cf21_a_room_has_twenty_five_seats_and_the_console_frees_them() {
    assert_eq!(SEATS, 25);
    let harness = relay();
    let api = harness.relay.api_addr();
    let offer = Page::new("p", Numbering::Default).offer;
    let join = |room: &str, participant: u32, offer: &str| {
        let body = json!({"participant": participant, "offer": offer});
        http(
            api,
            "POST",
            &format!("/rooms/{room}/participants"),
            Some(body),
        )
    };

    for participant in 0..25 {
        let (status, body) = join("r", participant, &offer);
        assert_eq!(status, 200, "{participant}: {body}");
    }
    let (status, body) = join("r", 25, &offer);
    assert_eq!(status, 409, "the 26th");
    assert!(
        body["error"].as_str().unwrap().contains("25 participants"),
        "{body}"
    );
    // The same index again replaces that participant's connection.
    assert_eq!(join("r", 24, &offer).0, 200);
    assert_eq!(
        http(api, "GET", "/health", None),
        (200, json!({"rooms": 1, "participants": 25}))
    );

    assert_eq!(http(api, "DELETE", "/rooms/r/participants/24", None).0, 204);
    assert_eq!(http(api, "DELETE", "/rooms/r/participants/24", None).0, 404);
    assert_eq!(join("r", 25, &offer).0, 200);
    assert_eq!(http(api, "DELETE", "/rooms/r", None), (204, Value::Null));
    assert_eq!(
        http(api, "GET", "/health", None),
        (200, json!({"rooms": 0, "participants": 0}))
    );
    assert_eq!(http(api, "DELETE", "/rooms/r", None).0, 404);
    assert_eq!(join("r", 7, &offer).0, 200, "a closed room starts over");

    // Offers the relay won't answer.
    let no_channel: String = offer
        .split_inclusive("\r\n")
        .scan(false, |in_application, line| {
            if line.starts_with("m=") {
                *in_application = line.starts_with("m=application");
            }
            Some(if *in_application { "" } else { line })
        })
        .collect();
    let (status, body) = join("s", 0, &no_channel);
    assert_eq!(status, 400, "{body}");
    assert_eq!(join("s", 0, "hello").0, 400);
    let (status, _) = http(
        api,
        "POST",
        "/rooms/s/participants",
        Some(json!({"offer": offer})),
    );
    assert_eq!(status, 400);
}

#[test]
fn cf21_leaving_tells_the_others_and_closing_the_room_disconnects_everyone() {
    let harness = relay();
    let api = harness.relay.api_addr();
    let mut a = Page::new("a", Numbering::Default);
    let mut b = Page::new("b", Numbering::Default);
    assert_eq!(admit(api, "meeting", 0, &mut a), 200);
    assert_eq!(admit(api, "meeting", 1, &mut b), 200);
    let mut pages = [&mut a, &mut b];
    assert!(run_until(&mut pages, PATIENCE, |p| {
        p.iter()
            .all(|p| p.peers() == Some(json!([0, 1])) && p.received.len() > 10)
    }));

    assert_eq!(
        http(api, "DELETE", "/rooms/meeting/participants/1", None).0,
        204
    );
    assert!(
        run_until(&mut pages, PATIENCE, |p| p[0].peers() == Some(json!([0]))
            && p[1].closed),
        "a wasn't told, or b wasn't disconnected"
    );
    let [a, b] = pages;
    let heard = a.from("b", "audio");
    let mut pages = [a, b];
    run_for(&mut pages, Duration::from_millis(300));
    assert!(
        pages[0].from("b", "audio") <= heard + 2,
        "b's media still arrives"
    );

    assert_eq!(http(api, "DELETE", "/rooms/meeting", None).0, 204);
    assert!(
        run_until(&mut pages, PATIENCE, |p| p[0].closed),
        "a wasn't disconnected"
    );
}
