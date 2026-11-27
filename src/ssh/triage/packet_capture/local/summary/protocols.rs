use super::{Traffic, ip};
use serde::Serialize;
use std::{borrow::Cow, collections::BTreeMap};

#[derive(Default, Serialize)]
pub struct IpProtocols {
    pub by_number: BTreeMap<u8, Traffic>,
    pub unclassified_packets: u32,
    pub unresolved_protocol_packets: u32,
    pub fragmented_packets: u32,
    pub tcp_flags: TcpFlags,
}

#[derive(Clone, Copy, Default, Serialize)]
pub struct TcpFlags {
    pub decoded_headers: u32,
    pub syn_without_ack: u32,
    pub syn_ack: u32,
    pub rst: u32,
    pub fin: u32,
}

impl IpProtocols {
    pub fn tcp_packets(&self) -> u32 {
        self.by_number
            .get(&ip::TCP)
            .map_or(0, |traffic| traffic.packets)
    }

    pub(super) fn record(&mut self, packet: Option<ip::Packet>, captured: u32) {
        let Some(packet) = packet else {
            self.unclassified_packets += 1;
            return;
        };
        self.fragmented_packets += u32::from(packet.fragmented);
        let Some(protocol) = packet.protocol else {
            self.unresolved_protocol_packets += 1;
            return;
        };
        let total = self.by_number.entry(protocol).or_default();
        total.packets += 1;
        total.captured_bytes += u64::from(captured);
        if let Some(flags) = packet.tcp_flags {
            const SYN: u8 = 0x02;
            const ACK: u8 = 0x10;
            self.tcp_flags.decoded_headers += 1;
            self.tcp_flags.syn_without_ack += u32::from(flags & (SYN | ACK) == SYN);
            self.tcp_flags.syn_ack += u32::from(flags & (SYN | ACK) == SYN | ACK);
            self.tcp_flags.rst += u32::from(flags & 0x04 != 0);
            self.tcp_flags.fin += u32::from(flags & 0x01 != 0);
        }
    }
}

pub fn ip_protocol_label(number: u8) -> Cow<'static, str> {
    Cow::Borrowed(match number {
        1 => "ICMP",
        2 => "IGMP",
        4 => "IPv4 encapsulation",
        ip::TCP => "TCP",
        17 => "UDP",
        41 => "IPv6 encapsulation",
        47 => "GRE",
        50 => "ESP",
        51 => "AH",
        58 => "ICMPv6",
        59 => "No Next Header",
        89 => "OSPF",
        132 => "SCTP",
        _ => return Cow::Owned(format!("IP protocol {number}")),
    })
}
