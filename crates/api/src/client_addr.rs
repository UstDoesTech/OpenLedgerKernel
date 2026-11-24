//! Which address a request came from, for the limits on sign-in.
//!
//! Behind a load balancer every connection comes from the load balancer, and the real address is
//! in `X-Forwarded-For`. That header is only believed when the connection itself comes from a
//! range the service was told to trust. From anywhere else it is ignored, so a caller cannot
//! choose its own address by sending one.
//!
//! The address is used to count and limit sign-in failures. It is never logged and never sent back.

use std::net::{IpAddr, Ipv4Addr};

/// The address used when a request carries no connection address at all, which only happens when
/// the router is called without a server (in tests). Everything without one shares it.
pub const UNKNOWN: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

/// The address a connection came from, as the server gives it to a request. It exists so that one
/// type serves both the plain and the TLS listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peer(pub std::net::SocketAddr);

impl<'a>
    axum::extract::connect_info::Connected<axum::serve::IncomingStream<'a, tokio::net::TcpListener>>
    for Peer
{
    fn connect_info(stream: axum::serve::IncomingStream<'a, tokio::net::TcpListener>) -> Self {
        Peer(*stream.remote_addr())
    }
}

impl<'a>
    axum::extract::connect_info::Connected<axum::serve::IncomingStream<'a, crate::tls::TlsListener>>
    for Peer
{
    fn connect_info(stream: axum::serve::IncomingStream<'a, crate::tls::TlsListener>) -> Self {
        Peer(*stream.remote_addr())
    }
}

/// The most entries read from the forwarded header, so a long one costs nothing.
const MAX_HOPS: usize = 16;

/// One address range, such as `10.11.192.0/20`, or one address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Range {
    network: IpAddr,
    bits: u8,
}

impl Range {
    fn parse(text: &str) -> Result<Self, String> {
        let (addr, bits) = match text.split_once('/') {
            Some((a, b)) => (a, Some(b)),
            None => (text, None),
        };
        let network: IpAddr = addr
            .trim()
            .parse()
            .map_err(|_| "not an address range".to_owned())?;
        let network = network.to_canonical();
        let widest = if network.is_ipv4() { 32 } else { 128 };
        let bits = match bits {
            Some(b) => b
                .trim()
                .parse::<u8>()
                .map_err(|_| "not an address range".to_owned())?,
            None => widest,
        };
        if bits > widest {
            return Err("not an address range".to_owned());
        }
        Ok(Range { network, bits })
    }

    fn contains(&self, ip: IpAddr) -> bool {
        match (self.network, ip.to_canonical()) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                same_prefix(n.to_bits().into(), a.to_bits().into(), self.bits, 32)
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => same_prefix(n.to_bits(), a.to_bits(), self.bits, 128),
            _ => false,
        }
    }
}

fn same_prefix(a: u128, b: u128, bits: u8, width: u32) -> bool {
    if bits == 0 {
        return true;
    }
    let shift = width - u32::from(bits);
    (a >> shift) == (b >> shift)
}

/// The ranges whose `X-Forwarded-For` is believed. None by default, so the header is never read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedProxies(Vec<Range>);

impl TrustedProxies {
    /// Nothing is trusted.
    pub fn none() -> Self {
        Self::default()
    }

    /// A comma-separated list of ranges, such as `10.11.192.0/20,10.11.208.0/20`. An empty text
    /// trusts nothing. A range that is not one is a mistake, and so is `/0`, which would trust
    /// everyone.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut ranges = Vec::new();
        for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let range = Range::parse(part)?;
            if range.bits == 0 {
                return Err("a trusted range may not be the whole internet".to_owned());
            }
            ranges.push(range);
        }
        Ok(TrustedProxies(ranges))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn trusts(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|r| r.contains(ip))
    }

    /// The address the request came from. `peer` is the connection's address, `forwarded` the
    /// values of every `X-Forwarded-For` header.
    ///
    /// When the connection is trusted, the entries are read from the right. Each proxy adds the
    /// address it saw to the end, so the right-most that is not itself trusted is the first
    /// address no trusted proxy vouched for, and anything to its left may have been made up. If
    /// the header cannot be read, the proxy's own address is used, so the caller is counted
    /// together with everyone else behind it, never as a stranger.
    pub fn client(&self, peer: IpAddr, forwarded: &[&str]) -> IpAddr {
        if !self.trusts(peer) {
            return peer;
        }
        let hops: Vec<&str> = forwarded
            .iter()
            .flat_map(|v| v.split(','))
            .map(str::trim)
            .collect();
        for hop in hops.iter().rev().take(MAX_HOPS) {
            match hop.parse::<IpAddr>() {
                Ok(ip) if self.trusts(ip) => continue,
                Ok(ip) => return ip.to_canonical(),
                Err(_) => return peer,
            }
        }
        peer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    fn proxies() -> TrustedProxies {
        TrustedProxies::parse("10.11.192.0/20, 10.11.208.0/20").unwrap()
    }

    #[test]
    fn nothing_is_trusted_unless_told() {
        let none = TrustedProxies::none();
        assert_eq!(
            none.client(ip("10.11.192.5"), &["203.0.113.9"]),
            ip("10.11.192.5")
        );
        assert!(TrustedProxies::parse("").unwrap().is_empty());
    }

    #[test]
    fn a_connection_from_outside_the_trusted_ranges_cannot_choose_its_address() {
        assert_eq!(
            proxies().client(ip("198.51.100.7"), &["203.0.113.9"]),
            ip("198.51.100.7")
        );
    }

    #[test]
    fn a_trusted_connection_gives_the_address_it_saw() {
        assert_eq!(
            proxies().client(ip("10.11.192.5"), &["203.0.113.9"]),
            ip("203.0.113.9")
        );
        // IPv6 clients and several headers.
        assert_eq!(
            proxies().client(ip("10.11.208.1"), &["10.11.208.9", "2001:db8::1"]),
            ip("2001:db8::1")
        );
    }

    #[test]
    fn what_a_caller_put_to_the_left_is_not_believed() {
        // The caller sent 1.2.3.4. The load balancer added the address it saw, to the right.
        assert_eq!(
            proxies().client(ip("10.11.192.5"), &["1.2.3.4, 203.0.113.9"]),
            ip("203.0.113.9")
        );
        assert_eq!(
            proxies().client(ip("10.11.192.5"), &["1.2.3.4", "203.0.113.9"]),
            ip("203.0.113.9")
        );
        // Another trusted hop in the chain is stepped over.
        assert_eq!(
            proxies().client(ip("10.11.192.5"), &["203.0.113.9, 10.11.208.7"]),
            ip("203.0.113.9")
        );
    }

    #[test]
    fn a_header_that_cannot_be_read_or_is_missing_counts_as_the_proxy() {
        assert_eq!(proxies().client(ip("10.11.192.5"), &[]), ip("10.11.192.5"));
        assert_eq!(
            proxies().client(ip("10.11.192.5"), &["not an address"]),
            ip("10.11.192.5")
        );
        assert_eq!(
            proxies().client(ip("10.11.192.5"), &["10.11.208.7"]),
            ip("10.11.192.5")
        );
    }

    #[test]
    fn ranges_are_read_strictly() {
        assert!(TrustedProxies::parse("10.0.0.0/33").is_err());
        assert!(TrustedProxies::parse("banana").is_err());
        assert!(TrustedProxies::parse("10.0.0.0/x").is_err());
        assert!(TrustedProxies::parse("0.0.0.0/0").is_err());
        assert!(TrustedProxies::parse("::/0").is_err());
        assert!(
            TrustedProxies::parse("10.0.0.1")
                .unwrap()
                .trusts(ip("10.0.0.1"))
        );
        assert!(
            !TrustedProxies::parse("10.0.0.1")
                .unwrap()
                .trusts(ip("10.0.0.2"))
        );
        assert!(
            TrustedProxies::parse("2001:db8::/32")
                .unwrap()
                .trusts(ip("2001:db8:1::1"))
        );
        // An IPv4 address that arrives mapped into IPv6 is read as IPv4.
        assert!(
            TrustedProxies::parse("10.0.0.0/8")
                .unwrap()
                .trusts(ip("::ffff:10.1.2.3"))
        );
    }
}
