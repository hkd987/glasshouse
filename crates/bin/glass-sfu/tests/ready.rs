//! The one stdout line test harnesses wait for, and the API behind it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::{Command, Stdio};

#[test]
fn cf21_the_relay_says_where_it_listens_when_ready() {
    let mut relay = Command::new(env!("CARGO_BIN_EXE_glass-sfu"))
        .args(["--udp", "127.0.0.1:0", "--api", "127.0.0.1:0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(relay.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let fields: Vec<&str> = line.trim_end().split(' ').collect();
    let ["glass-sfu", "ready", udp, api] = fields.as_slice() else {
        panic!("{line:?}");
    };
    let udp: SocketAddr = udp.strip_prefix("udp=").unwrap().parse().unwrap();
    let api: SocketAddr = api.strip_prefix("api=").unwrap().parse().unwrap();
    assert!(udp.ip().is_loopback() && udp.port() != 0, "{line}");

    let mut stream = TcpStream::connect(api).unwrap();
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: relay\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
    for field in [r#""rooms":0"#, r#""participants":0"#] {
        assert!(response.contains(field), "{response}");
    }

    relay.kill().unwrap();
    relay.wait().unwrap();
    // Nothing else on stdout.
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "");
}
