//! The relay with str0m clients standing in for the Meet page (CF-21). Each
//! makes the page's offer (a data channel, then audio and video sendonly,
//! then audio and video recvonly), is admitted through the private API like
//! the console does, and sends RTP whose payload says who sent it.

#![allow(clippy::unwrap_used)]

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use glass_media_relay::{Config, Relay};
use serde_json::{Value, json};
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
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

/// A stand-in for the Meet page.
struct Page {
    name: &'static str,
    rtc: Rtc,
    socket: UdpSocket,
    addr: SocketAddr,
    offer: String,
    pending: Option<SdpPendingOffer>,
    /// The page's sendonly mids, audio then video.
    send: [Mid; 2],
    /// The page's recvonly mids, audio then video.
    recv: [Mid; 2],
    /// Opus's and VP8's payload types, once answered.
    pts: [Pt; 2],
    timeout: Instant,
    sending: bool,
    sent: u64,
    last_sent: Option<Instant>,
    connected: bool,
    closed: bool,
    /// Every RTP packet received: on which mid, its payload type, its payload.
    received: Vec<(Option<Mid>, Pt, String)>,
    keyframe_requests: Vec<Mid>,
    messages: Vec<Value>,
}

impl Page {
    fn new(name: &'static str, numbering: Numbering) -> Page {
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
        change.add_channel("glass".into());
        let send = [
            change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None),
            change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None),
        ];
        let recv = [
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
            send,
            recv,
            pts: [0.into(), 0.into()],
            timeout: Instant::now(),
            sending: true,
            sent: 0,
            last_sent: None,
            connected: false,
            closed: false,
            received: Vec::new(),
            keyframe_requests: Vec::new(),
            messages: Vec::new(),
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

    /// Reads what has arrived, hands str0m the time, and sends a packet of
    /// audio and one of video every 20 ms once connected.
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
        let due = self
            .last_sent
            .is_none_or(|t| now - t >= Duration::from_millis(20));
        if self.connected && self.sending && due {
            self.last_sent = Some(now);
            self.sent += 1;
            for (i, kind) in ["audio", "video"].into_iter().enumerate() {
                let payload = format!("{}:{kind}:{}", self.name, self.sent).into_bytes();
                let time = (self.sent * if i == 0 { 960 } else { 1800 }) as u32;
                let mut api = self.rtc.direct_api();
                let stream = api.stream_tx_by_mid(self.send[i], None).unwrap();
                stream.write_rtp(
                    RtpWrite::new(self.pts[i], (1000 + self.sent).into(), time, now, payload)
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
            Event::ChannelData(data) => {
                let text = std::str::from_utf8(&data.data).unwrap();
                assert!(text.ends_with('\n'), "{text:?} is a line");
                self.messages.push(serde_json::from_str(text).unwrap());
            }
            Event::RtpPacket(packet) => {
                let mid = self
                    .rtc
                    .direct_api()
                    .stream_rx(&packet.header.ssrc)
                    .map(|s| s.mid());
                let payload = String::from_utf8(packet.payload.to_vec()).unwrap();
                self.received
                    .push((mid, packet.header.payload_type, payload));
            }
            Event::KeyframeRequest(request) => self.keyframe_requests.push(request.mid),
            _ => {}
        }
    }

    /// Packets of `kind` received from the page named `from`.
    fn from(&self, from: &str, kind: &str) -> usize {
        let prefix = format!("{from}:{kind}:");
        self.received
            .iter()
            .filter(|(_, _, p)| p.starts_with(&prefix))
            .count()
    }

    fn last_peers(&self) -> Option<&Value> {
        self.messages.iter().rev().find(|m| m["type"] == "peers")
    }
}

/// Admits `page` to `room` the way the console does, and hands it the answer.
fn admit(api: SocketAddr, room: &str, participant: u32, page: &mut Page) -> u16 {
    let body = json!({"participant": participant, "offer": page.offer});
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

    for (page, other) in [(&*a, "b"), (&*b, "a")] {
        for (mid, pt, payload) in &page.received {
            let kind = if payload.starts_with(&format!("{other}:audio:")) {
                0
            } else if payload.starts_with(&format!("{other}:video:")) {
                1
            } else {
                panic!("{} got {payload:?}", page.name);
            };
            // On the page's recvonly m-line of that kind, in the page's own
            // payload type for the codec, the payload unchanged.
            assert_eq!(*mid, Some(page.recv[kind]), "{payload}");
            assert_eq!(*pt, page.pts[kind], "{payload}");
        }
        assert_eq!(
            page.last_peers(),
            Some(&json!({"type": "peers", "peers": [0, 1]}))
        );
    }
    assert_ne!(
        a.pts, b.pts,
        "the two pages number their codecs differently"
    );

    assert!(elsewhere.connected && elsewhere.sent > 0);
    assert!(elsewhere.received.is_empty(), "{:?}", elsewhere.received);
    assert_eq!(
        elsewhere.last_peers(),
        Some(&json!({"type": "peers", "peers": [0]}))
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
    let mut api = b.rtc.direct_api();
    let stream = api.stream_rx_by_mid(b.recv[1], None).unwrap();
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
fn cf21_a_room_has_two_seats_and_the_console_frees_them() {
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

    let first = join("r", 0, &offer);
    assert_eq!(first.0, 200, "{}", first.1);
    assert_eq!(join("r", 1, &offer).0, 200);
    let (status, body) = join("r", 2, &offer);
    assert_eq!(status, 409);
    assert!(
        body["error"].as_str().unwrap().contains("2 participants"),
        "{body}"
    );
    // The same index again replaces that participant's connection.
    assert_eq!(join("r", 1, &offer).0, 200);
    assert_eq!(
        http(api, "GET", "/health", None),
        (200, json!({"rooms": 1, "participants": 2}))
    );

    assert_eq!(http(api, "DELETE", "/rooms/r/participants/1", None).0, 204);
    assert_eq!(http(api, "DELETE", "/rooms/r/participants/1", None).0, 404);
    assert_eq!(join("r", 2, &offer).0, 200);
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
    let both = json!({"type": "peers", "peers": [0, 1]});
    let mut pages = [&mut a, &mut b];
    assert!(run_until(&mut pages, PATIENCE, |p| {
        p.iter()
            .all(|p| p.last_peers() == Some(&both) && p.received.len() > 10)
    }));

    assert_eq!(
        http(api, "DELETE", "/rooms/meeting/participants/1", None).0,
        204
    );
    let alone = json!({"type": "peers", "peers": [0]});
    assert!(
        run_until(&mut pages, PATIENCE, |p| p[0].last_peers() == Some(&alone)
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
