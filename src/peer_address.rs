//! Conservative clearnet-only address policy, shared by gossip, DNS and cache loading.

use std::net::{IpAddr, Ipv6Addr, SocketAddr};

/// Normalize IPv4-mapped IPv6 before deduplication and destination checks.
pub(crate) fn normalize(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V6(v6) if v6.ip().to_ipv4_mapped().is_some() => SocketAddr::new(
            IpAddr::V4(v6.ip().to_ipv4_mapped().expect("mapped")),
            v6.port(),
        ),
        _ => addr,
    }
}

/// Allow only conservative public unicast destinations on mainnet's standard port.
pub(crate) fn discoverable(addr: SocketAddr) -> bool {
    if addr.port() != 8333 {
        return false;
    }
    if matches!(addr, SocketAddr::V6(v6) if v6.scope_id() != 0 || v6.flowinfo() != 0) {
        return false;
    }
    match normalize(addr).ip() {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 0 || b == 168 || (b == 88 && c == 99)))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            // Only native global unicast; exclude transition/tunnel, documentation,
            // benchmarking, ORCHID and other special-use 2001::/23 allocations.
            s[0] & 0xe000 == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

/// Prefix diversity heuristic, not proof of independent ownership or routing.
pub(crate) fn group(addr: SocketAddr) -> u64 {
    match normalize(addr).ip() {
        IpAddr::V4(ip) => {
            let b = ip.octets();
            u64::from(u16::from_be_bytes([b[0], b[1]]))
        }
        IpAddr::V6(ip) => {
            (1 << 32) | u64::from(u32::from(ip.segments()[0]) << 16 | u32::from(ip.segments()[1]))
        }
    }
}

/// Decode the legacy IPv6-shaped address field without invoking a vector decoder.
pub(crate) fn legacy(bytes: [u8; 16], port: u16) -> SocketAddr {
    normalize(SocketAddr::new(Ipv6Addr::from(bytes).into(), port))
}
