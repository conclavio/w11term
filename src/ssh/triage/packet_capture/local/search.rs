use super::ConversationEndpoints;
use std::net::{IpAddr, SocketAddr};

pub struct ConversationQuery(Vec<Term>);

enum Term {
    Protocol(u8),
    Address(IpAddr),
    Network(IpAddr, u32),
    Endpoint(SocketAddr),
    Port(u16),
    AddressPrefix(String),
}

impl ConversationQuery {
    pub fn new(text: &str) -> Self {
        Self(text.split_whitespace().map(Term::parse).collect())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn matches(&self, endpoints: ConversationEndpoints) -> bool {
        let (addresses, transport) = match endpoints {
            ConversationEndpoints::Ip { a, b } => ([a, b], None),
            ConversationEndpoints::Transport { protocol, a, b } => {
                ([a.ip(), b.ip()], Some((protocol, [a, b])))
            }
        };
        self.0.iter().all(|term| match term {
            Term::Protocol(number) => transport.is_some_and(|(protocol, _)| protocol == *number),
            Term::Address(host) => addresses.contains(host),
            Term::Network(network, prefix) => addresses
                .into_iter()
                .any(|address| network_contains(*network, *prefix, address)),
            Term::Endpoint(endpoint) => {
                transport.is_some_and(|(_, sockets)| sockets.contains(endpoint))
            }
            Term::Port(port) => transport
                .is_some_and(|(_, sockets)| sockets.iter().any(|socket| socket.port() == *port)),
            Term::AddressPrefix(prefix) => addresses
                .iter()
                .any(|address| address.to_string().starts_with(prefix)),
        })
    }
}

impl Term {
    fn parse(text: &str) -> Self {
        if text.eq_ignore_ascii_case("tcp") {
            return Self::Protocol(6);
        }
        if text.eq_ignore_ascii_case("udp") {
            return Self::Protocol(17);
        }
        let host = text
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(text);
        if let Ok(address) = host.parse() {
            return Self::Address(address);
        }
        if let Ok(endpoint) = text.parse() {
            return Self::Endpoint(endpoint);
        }
        if let Some((address, length)) = text.split_once('/')
            && let (Ok(network), Ok(prefix)) = (address.parse::<IpAddr>(), length.parse::<u32>())
            && prefix <= if network.is_ipv4() { 32 } else { 128 }
        {
            return Self::Network(network, prefix);
        }
        if let Ok(port) = text.strip_prefix(':').unwrap_or(text).parse() {
            return Self::Port(port);
        }
        Self::AddressPrefix(host.to_ascii_lowercase())
    }
}

fn network_contains(network: IpAddr, prefix: u32, address: IpAddr) -> bool {
    match (network, address) {
        (IpAddr::V4(network), IpAddr::V4(address)) => {
            (u32::from(network) ^ u32::from(address)).leading_zeros() >= prefix
        }
        (IpAddr::V6(network), IpAddr::V6(address)) => {
            (u128::from(network) ^ u128::from(address)).leading_zeros() >= prefix
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_exact_hosts_ports_protocols_and_socket_endpoints() {
        let tcp = ConversationEndpoints::Transport {
            protocol: 6,
            a: "192.0.2.10:443".parse().unwrap(),
            b: "192.0.2.20:49152".parse().unwrap(),
        };
        for query in [
            "",
            "  TCP 443 ",
            "192.0.2.",
            "192.0.2.20/24 :443",
            "192.0.2.10:443",
        ] {
            assert!(ConversationQuery::new(query).matches(tcp), "{query}");
        }
        for query in [
            "udp",
            "44",
            "192.0.2.1",
            "192.0.2.20:443",
            "192.0.2.20/32 192.0.2.1",
            "192.0.2.0/33",
            "198.51.100.0/24",
        ] {
            assert!(!ConversationQuery::new(query).matches(tcp), "{query}");
        }
    }

    #[test]
    fn handles_ipv6_canonicalization_and_ip_only_conversations() {
        let a = "2001:db8::a".parse().unwrap();
        let b = "2001:db9::1".parse().unwrap();
        let ip = ConversationEndpoints::Ip { a, b };
        let udp = ConversationEndpoints::Transport {
            protocol: 17,
            a: SocketAddr::new(a, 53),
            b: SocketAddr::new(b, 64300),
        };
        for query in [
            "2001:0DB8:0000::A",
            "[2001:db8::a]",
            "2001:DB8::/32",
            "2001:db",
        ] {
            assert!(ConversationQuery::new(query).matches(ip), "{query}");
            assert!(ConversationQuery::new(query).matches(udp), "{query}");
        }
        assert!(ConversationQuery::new("udp [2001:0DB8::A]:53").matches(udp));
        assert!(!ConversationQuery::new("[2001:db9::1]:53").matches(udp));
        assert!(!ConversationQuery::new("2001:db8::/129").matches(udp));
        assert!(!ConversationQuery::new("0.0.0.0/0").matches(udp));
        assert!(!ConversationQuery::new("UDP 53").matches(ip));
    }
}
