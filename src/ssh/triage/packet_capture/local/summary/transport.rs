use super::{
    Traffic,
    conversations::{MAX_RANKED_PAIRS, PairTally, canonical_pair, total},
    ip,
};
use serde::Serialize;
use std::net::SocketAddr;

#[derive(Default, Serialize)]
pub struct TransportConversations {
    pub tracked_pairs: usize,
    pub packets_without_ports: u32,
    pub untracked_packets: u32,
    pub top: Vec<TransportConversation>,
    #[serde(skip)]
    remaining: Vec<TransportConversation>,
}

impl TransportConversations {
    pub fn pair(&self, index: usize) -> Option<&TransportConversation> {
        if index < self.top.len() {
            self.top.get(index)
        } else {
            self.remaining.get(index - self.top.len())
        }
    }

    pub(in crate::ssh::triage::packet_capture::local) fn pairs(
        &self,
    ) -> impl Iterator<Item = &TransportConversation> {
        self.top.iter().chain(&self.remaining)
    }
}

#[derive(Serialize)]
pub struct TransportConversation {
    pub protocol: u8,
    pub a: SocketAddr,
    pub b: SocketAddr,
    pub a_to_b: Traffic,
    pub b_to_a: Traffic,
}

impl TransportConversation {
    pub fn total(&self) -> Traffic {
        total(self.a_to_b, self.b_to_a)
    }
}

#[derive(Default)]
pub(super) struct Scan {
    pairs: PairTally<(u8, SocketAddr, SocketAddr)>,
    packets_without_ports: u32,
}

impl Scan {
    pub(super) fn record(&mut self, packet: Option<&ip::Packet>, captured: u32) {
        let Some(packet) = packet else { return };
        let Some(protocol @ (ip::TCP | ip::UDP)) = packet.protocol else {
            return;
        };
        let Some((source_port, destination_port)) = packet.ports else {
            self.packets_without_ports += 1;
            return;
        };
        let (source, destination) = packet.addresses;
        let (endpoints, direction) = canonical_pair(
            SocketAddr::new(source, source_port),
            SocketAddr::new(destination, destination_port),
        );
        self.pairs
            .record((protocol, endpoints.0, endpoints.1), direction, captured);
    }

    pub(super) fn finish(self) -> TransportConversations {
        let (ranked, untracked_packets) = self.pairs.into_ranked();
        let tracked_pairs = ranked.len();
        let mut top: Vec<_> = ranked
            .into_iter()
            .map(
                |((protocol, a, b), [a_to_b, b_to_a])| TransportConversation {
                    protocol,
                    a,
                    b,
                    a_to_b,
                    b_to_a,
                },
            )
            .collect();
        let remaining = top.split_off(tracked_pairs.min(MAX_RANKED_PAIRS));
        TransportConversations {
            tracked_pairs,
            packets_without_ports: self.packets_without_ports,
            untracked_packets,
            top,
            remaining,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_by_protocol_and_both_ports_and_counts_reverse_traffic_once() {
        let a = "2001:db8::1".parse().unwrap();
        let b = "2001:db8::2".parse().unwrap();
        let mut scan = Scan::default();
        let mut packet = ip::Packet {
            addresses: (a, b),
            protocol: Some(ip::TCP),
            tcp_flags: None,
            fragmented: false,
            ports: Some((49152, 443)),
        };
        scan.record(Some(&packet), 120);
        packet.addresses = (b, a);
        packet.ports = Some((443, 49152));
        scan.record(Some(&packet), 80);
        packet.protocol = Some(ip::UDP);
        scan.record(Some(&packet), 48);
        packet.ports = Some((443, 49153));
        scan.record(Some(&packet), 24);
        packet.ports = None;
        scan.record(Some(&packet), 56);
        packet.protocol = Some(58);
        scan.record(Some(&packet), 56);

        let summary = scan.finish();
        assert_eq!(summary.tracked_pairs, 3);
        assert_eq!(summary.packets_without_ports, 1);
        let tcp = &summary.top[0];
        assert_eq!(tcp.protocol, ip::TCP);
        assert_eq!(tcp.a.to_string(), "[2001:db8::1]:49152");
        assert_eq!(tcp.b.to_string(), "[2001:db8::2]:443");
        assert_eq!(tcp.a_to_b.captured_bytes, 120);
        assert_eq!(tcp.b_to_a.captured_bytes, 80);
        assert_eq!(summary.top[1].protocol, ip::UDP);
        assert_eq!(summary.top[1].a, tcp.a);
        assert_eq!(summary.top[2].a.port(), 49153);
    }
}
