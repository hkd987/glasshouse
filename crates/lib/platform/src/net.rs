//! Who is at the other end of a loopback TCP connection: the uid that owns
//! the peer's socket, as the kernel's socket table says (`sockstat`, ADR
//! 0007; on Linux development hosts, `/proc/net/tcp`).
//!
//! The app bridge serves only its own user's processes this way (ADR 0027):
//! a browser speaks HTTP over TCP, not over a Unix socket, so the kernel's
//! record of who owns the connecting socket stands in for a socket's
//! credentials. Agents can't connect at all: their jails have no network
//! (`ip4 = disable`).

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use crate::{Result, cmd};

/// The uid owning the socket at `peer` that is connected to `local`, both
/// IPv4 loopback addresses. `None` when no such socket is visible: one that
/// is already closed, or another user's where this process may not see
/// other users' sockets (`security.bsd.see_other_uids=0`). Callers refuse
/// on `None`.
pub fn loopback_peer_uid(local: SocketAddr, peer: SocketAddr) -> Result<Option<u32>> {
    let (SocketAddr::V4(local), SocketAddr::V4(peer)) = (local, peer) else {
        return Ok(None);
    };
    if !local.ip().is_loopback() || !peer.ip().is_loopback() {
        return Ok(None);
    }
    #[cfg(target_os = "freebsd")]
    {
        let port = peer.port().to_string();
        let out = cmd::run(
            "sockstat",
            &["-4", "-q", "-c", "-n", "-P", "tcp", "-p", &port],
        )?;
        Ok(sockstat_owner(&out, local, peer))
    }
    #[cfg(not(target_os = "freebsd"))]
    {
        let _ = cmd::run;
        let table = std::fs::read_to_string("/proc/net/tcp")
            .map_err(|e| crate::PlatformError::io("reading /proc/net/tcp", e))?;
        Ok(proc_net_tcp_owner(&table, local, peer))
    }
}

/// The owner of the socket whose own address is `peer` and whose remote is
/// `local`, from `sockstat -n` lines: `UID COMMAND PID FD PROTO LOCAL
/// FOREIGN`.
#[cfg_attr(not(target_os = "freebsd"), allow(dead_code))]
fn sockstat_owner(out: &str, local: SocketAddrV4, peer: SocketAddrV4) -> Option<u32> {
    out.lines().find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [uid, .., own, remote] = fields.as_slice() else {
            return None;
        };
        (own.parse::<SocketAddrV4>().ok()? == peer && remote.parse::<SocketAddrV4>().ok()? == local)
            .then(|| uid.parse().ok())
            .flatten()
    })
}

/// The same from Linux's `/proc/net/tcp`: addresses as hex `ADDR:PORT`
/// (the address in host byte order), the uid in the eighth column.
#[cfg_attr(target_os = "freebsd", allow(dead_code))]
fn proc_net_tcp_owner(table: &str, local: SocketAddrV4, peer: SocketAddrV4) -> Option<u32> {
    let parse = |field: &str| -> Option<SocketAddrV4> {
        let (addr, port) = field.split_once(':')?;
        let addr = u32::from_str_radix(addr, 16).ok()?;
        let port = u16::from_str_radix(port, 16).ok()?;
        Some(SocketAddrV4::new(Ipv4Addr::from(u32::from_be(addr)), port))
    };
    table.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (own, remote, uid) = (fields.get(1)?, fields.get(2)?, fields.get(7)?);
        (parse(own)? == peer && parse(remote)? == local)
            .then(|| uid.parse().ok())
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> SocketAddrV4 {
        s.parse().unwrap()
    }

    #[test]
    fn sockstat_names_the_owner_of_the_connecting_end() {
        let out = "1001 nc         5954  3 tcp4  127.0.0.1:52241       127.0.0.1:7300       \n\
                   1001 glass-app- 3542 10 tcp4  127.0.0.1:7300        127.0.0.1:52241      \n";
        let (bridge, client) = (v4("127.0.0.1:7300"), v4("127.0.0.1:52241"));
        assert_eq!(sockstat_owner(out, bridge, client), Some(1001));
        // The bridge's own end isn't the peer.
        assert_eq!(sockstat_owner(out, client, bridge), Some(1001));
        assert_eq!(sockstat_owner(out, bridge, v4("127.0.0.1:1")), None);
    }

    #[test]
    fn proc_net_tcp_names_the_owner_of_the_connecting_end() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
                     0: 0100007F:1C84 0100007F:CBE1 01 00000000:00000000 00:00000000 00000000  1000        0 1\n\
                     1: 0100007F:CBE1 0100007F:1C84 01 00000000:00000000 00:00000000 00000000  1001        0 2\n";
        let (bridge, client) = (v4("127.0.0.1:7300"), v4("127.0.0.1:52193"));
        assert_eq!(proc_net_tcp_owner(table, bridge, client), Some(1001));
        assert_eq!(proc_net_tcp_owner(table, client, bridge), Some(1000));
    }

    #[test]
    fn a_real_connection_belongs_to_this_process_owner() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_server, peer) = listener.accept().unwrap();
        assert_eq!(peer, client.local_addr().unwrap());
        let uid = loopback_peer_uid(listener.local_addr().unwrap(), peer).unwrap();
        assert_eq!(uid, Some(crate::users::effective_uid()));
    }
}
