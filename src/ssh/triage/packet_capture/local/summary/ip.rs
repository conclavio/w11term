use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub(super) const PREFIX_BYTES: usize = 256;
const IPV4: u16 = 0x0800;
const IPV6: u16 = 0x86dd;
pub(super) const TCP: u8 = 6;
pub(super) const UDP: u8 = 17;
const MAX_IPV6_EXTENSIONS: usize = 16;

pub(super) type Addresses = (IpAddr, IpAddr);

pub(super) struct Packet {
    pub addresses: Addresses,
    pub protocol: Option<u8>,
    pub tcp_flags: Option<u8>,
    pub fragmented: bool,
    pub ports: Option<(u16, u16)>,
}

impl Packet {
    fn read_transport(&mut self, payload: &[u8]) {
        match self.protocol {
            Some(TCP) => {
                let Some(header) = payload.get(..20) else {
                    return;
                };
                let len = usize::from(header[12] >> 4) * 4;
                if len >= 20 && len <= payload.len() {
                    self.ports = Some(port_pair(header));
                    self.tcp_flags = Some(header[13]);
                }
            }
            Some(UDP) => {
                let Some(header) = payload.get(..8) else {
                    return;
                };
                if u16::from_be_bytes([header[4], header[5]]) >= 8 {
                    self.ports = Some(port_pair(header));
                }
            }
            _ => (),
        }
    }
}

pub(super) fn decode(link_type: u16, prefix: &[u8]) -> Option<Packet> {
    let (protocol, ip) = match link_type {
        1 => (be16(prefix.get(12..14)?)?, prefix.get(14..)?), // Ethernet
        113 => (be16(prefix.get(14..16)?)?, prefix.get(16..)?), // Linux cooked v1
        276 => (be16(prefix.get(..2)?)?, prefix.get(20..)?),  // Linux cooked v2
        0 | 108 => {
            let family: [u8; 4] = prefix.get(..4)?.try_into().ok()?;
            // LINKTYPE_NULL uses the capture host's byte order. A PCAPNG
            // section can contain frames imported from another host.
            let family = match family {
                [value, 0, 0, 0] if link_type == 0 => value,
                [0, 0, 0, value] => value,
                _ => return None,
            };
            let protocol = match family {
                2 => IPV4,
                24 | 28 | 30 => IPV6,
                _ => return None,
            };
            return decode_ip(protocol, prefix.get(4..)?);
        }
        101 => {
            let protocol = match prefix.first()? >> 4 {
                4 => IPV4,
                6 => IPV6,
                _ => return None,
            };
            return decode_ip(protocol, prefix);
        }
        228 => return decode_ip(IPV4, prefix),
        229 => return decode_ip(IPV6, prefix),
        _ => return None,
    };
    let (mut protocol, mut ip) = (protocol, ip);
    // 802.1Q and 802.1ad tags carry the inner EtherType after the tag control field.
    for _ in 0..4 {
        if !matches!(protocol, 0x8100 | 0x88a8) {
            break;
        }
        protocol = be16(ip.get(2..4)?)?;
        ip = ip.get(4..)?;
    }
    decode_ip(protocol, ip)
}

fn decode_ip(protocol: u16, ip: &[u8]) -> Option<Packet> {
    let version = ip.first()? >> 4;
    match (protocol, version) {
        (IPV4, 4) => {
            let header_length = usize::from(ip[0] & 0x0f) * 4;
            let total_length = usize::from(be16(ip.get(2..4)?)?);
            if header_length < 20 || header_length > ip.len() || total_length < header_length {
                return None;
            }
            let header: &[u8; 20] = ip[..20].try_into().ok()?;
            let a = Ipv4Addr::new(header[12], header[13], header[14], header[15]);
            let b = Ipv4Addr::new(header[16], header[17], header[18], header[19]);
            let flags_and_offset = u16::from_be_bytes([header[6], header[7]]);
            let protocol = header[9];
            let payload = &ip[header_length..total_length.min(ip.len())];
            let mut packet = Packet {
                addresses: (a.into(), b.into()),
                protocol: Some(protocol),
                tcp_flags: None,
                fragmented: flags_and_offset & 0x3fff != 0,
                ports: None,
            };
            if flags_and_offset & 0x1fff == 0 {
                packet.read_transport(payload);
            }
            Some(packet)
        }
        (IPV6, 6) => {
            let a = <[u8; 16]>::try_from(ip.get(8..24)?).ok()?;
            let b = <[u8; 16]>::try_from(ip.get(24..40)?).ok()?;
            let addresses = (Ipv6Addr::from(a).into(), Ipv6Addr::from(b).into());
            let size = usize::from(u16::from_be_bytes([ip[4], ip[5]]));
            Some(ipv6_packet(
                addresses,
                ip[6],
                &ip[40..(40 + size).min(ip.len())],
            ))
        }
        _ => None,
    }
}

fn ipv6_packet(addresses: Addresses, mut next: u8, mut payload: &[u8]) -> Packet {
    let mut packet = Packet {
        addresses,
        protocol: None,
        tcp_flags: None,
        fragmented: false,
        ports: None,
    };
    for depth in 0..=MAX_IPV6_EXTENSIONS {
        if !ipv6_extension(next) {
            packet.protocol = Some(next);
            packet.read_transport(payload);
            break;
        }
        if depth == MAX_IPV6_EXTENSIONS {
            break;
        }
        let Some(&length_field) = payload.get(1) else {
            break;
        };
        let len = match next {
            0 | 43 | 60 => (usize::from(length_field) + 1) * 8,
            44 => 8,
            51 if length_field >= 2 && length_field.is_multiple_of(2) => {
                (usize::from(length_field) + 2) * 4
            }
            _ => break,
        };
        let Some(header) = payload.get(..len) else {
            break;
        };
        if next == 44 {
            let offset_and_flags = u16::from_be_bytes([header[2], header[3]]);
            let later_fragment = offset_and_flags & 0xfff8 != 0;
            packet.fragmented |= later_fragment || offset_and_flags & 1 != 0;
            if later_fragment {
                // The bytes after a later Fragment header can start inside an
                // upper-layer header or payload. Only the header type is known.
                packet.protocol = (!ipv6_extension(header[0])).then_some(header[0]);
                break;
            }
        }
        next = header[0];
        payload = &payload[len..];
    }
    packet
}

fn ipv6_extension(number: u8) -> bool {
    // ESP is opaque and counted as its own protocol. Experimental values
    // can be extensions, so they cannot identify a terminal protocol here.
    matches!(number, 0 | 43 | 44 | 51 | 60 | 135 | 139 | 140 | 253 | 254)
}

fn port_pair(header: &[u8]) -> (u16, u16) {
    (
        u16::from_be_bytes([header[0], header[1]]),
        u16::from_be_bytes([header[2], header[3]]),
    )
}

fn be16(bytes: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(link_type: u16, prefix: &[u8]) -> Option<Addresses> {
        super::decode(link_type, prefix).map(|packet| packet.addresses)
    }

    #[test]
    fn classifies_outer_ip_headers_and_checks_link_type_and_truncation() {
        let v4 = [
            0x45, 0, 0, 60, 0, 0, 0, 3, 64, 17, 0, 0, 10, 0, 0, 1, 10, 0, 0, 2,
        ];
        let addresses = ("10.0.0.1".parse().unwrap(), "10.0.0.2".parse().unwrap());
        let mut ethernet = vec![0; 12];
        ethernet.extend([0x88, 0xa8, 0, 1, 0x81, 0, 0, 2, 8, 0]);
        ethernet.extend(v4);
        assert_eq!(decode(1, &ethernet), Some(addresses));
        assert_eq!(decode(1, &ethernet[..ethernet.len() - 1]), None);

        let mut cooked = vec![0; 14];
        cooked.extend([8, 0]);
        cooked.extend(v4);
        assert_eq!(decode(113, &cooked), Some(addresses));
        assert_eq!(decode(101, &v4), Some(addresses));
        assert_eq!(decode(228, &v4), Some(addresses));
        assert_eq!(decode(229, &v4), None);
        assert_eq!(decode(127, &ethernet), None);
        let mut bad_length = v4;
        bad_length[3] = 19;
        assert_eq!(decode(101, &bad_length), None);

        let mut v6 = [0; 40];
        v6[0] = 0x60;
        v6[8..24].copy_from_slice(&Ipv6Addr::LOCALHOST.octets());
        let peer = "2001:db8::2".parse::<Ipv6Addr>().unwrap();
        v6[24..40].copy_from_slice(&peer.octets());
        let addresses = (Ipv6Addr::LOCALHOST.into(), peer.into());
        let mut cooked = vec![0; 20];
        cooked[..2].copy_from_slice(&IPV6.to_be_bytes());
        cooked.extend(v6);
        assert_eq!(decode(276, &cooked), Some(addresses));
        assert_eq!(decode(229, &v6), Some(addresses));
        for family in [24_u32, 28, 30] {
            let mut loopback = family.to_le_bytes().to_vec();
            loopback.extend(v6);
            assert_eq!(decode(0, &loopback), Some(addresses));
            assert_eq!(decode(108, &loopback), None);
            loopback[..4].copy_from_slice(&family.to_be_bytes());
            assert_eq!(decode(0, &loopback), Some(addresses));
            assert_eq!(decode(108, &loopback), Some(addresses));
        }
    }

    #[test]
    fn tcp_flags_respect_declared_lengths_options_and_ipv4_fragment_offsets() {
        let mut packet = [0; 48];
        packet[0] = 0x46; // IPv4 options precede TCP.
        packet[3] = 48;
        packet[9] = TCP;
        packet[24..28].copy_from_slice(&[0xc0, 0x00, 0x01, 0xbb]);
        packet[36] = 0x60;
        packet[37] = 0x12;
        assert_eq!(super::decode(101, &packet).unwrap().tcp_flags, Some(0x12));
        assert_eq!(
            super::decode(101, &packet).unwrap().ports,
            Some((49152, 443))
        );
        assert_eq!(super::decode(101, &packet[..47]).unwrap().tcp_flags, None);
        packet[3] = 44;
        assert_eq!(super::decode(101, &packet).unwrap().tcp_flags, None);
        packet[3] = 48;
        packet[6] = 0x20; // First fragment with more fragments following.
        assert!(super::decode(101, &packet).unwrap().fragmented);
        assert_eq!(super::decode(101, &packet).unwrap().tcp_flags, Some(0x12));
        packet[7] = 1;
        let later = super::decode(101, &packet).unwrap();
        assert_eq!(later.protocol, Some(TCP));
        assert_eq!(later.tcp_flags, None);
        assert_eq!(later.ports, None);
    }

    #[test]
    fn reads_udp_ports_after_ipv6_extensions_only_when_the_udp_header_is_available() {
        let mut packet = [0; 56];
        packet[0] = 0x60;
        packet[5] = 24; // The UDP payload itself was not captured.
        packet[6] = 60;
        packet[40] = UDP;
        packet[48..56].copy_from_slice(&[0x00, 0x00, 0x14, 0xe9, 0x00, 0x10, 0x00, 0x00]);
        let decoded = super::decode(101, &packet).unwrap();
        assert_eq!(decoded.protocol, Some(UDP));
        assert_eq!(decoded.ports, Some((0, 5353)));
        assert_eq!(decoded.tcp_flags, None);
        assert_eq!(super::decode(101, &packet[..55]).unwrap().ports, None);
        packet[53] = 7;
        assert_eq!(super::decode(101, &packet).unwrap().ports, None);
        packet[53] = 16;
        packet[6] = 44;
        packet[43] = 8;
        let fragment = super::decode(101, &packet).unwrap();
        assert_eq!(fragment.protocol, Some(UDP));
        assert!(fragment.fragmented);
        assert_eq!(fragment.ports, None);
    }

    #[test]
    fn walks_ipv6_extensions_but_never_parses_a_later_fragment_as_tcp() {
        let mut packet = [0; 100];
        packet[0] = 0x60;
        packet[5] = 60;
        packet[6] = 0;
        packet[40] = 60;
        packet[48] = 51;
        packet[56] = TCP;
        packet[57] = 4; // 24-byte Authentication Header.
        packet[92] = 0x50;
        packet[93] = 0x04;
        let decoded = super::decode(101, &packet).unwrap();
        assert_eq!(decoded.protocol, Some(TCP));
        assert_eq!(decoded.tcp_flags, Some(0x04));
        assert_eq!(super::decode(101, &packet[..79]).unwrap().protocol, None);

        packet[48] = 44;
        packet[56] = TCP;
        packet[59] = 8; // One unit of fragment offset.
        let later = super::decode(101, &packet).unwrap();
        assert_eq!(later.protocol, Some(TCP));
        assert_eq!(later.tcp_flags, None);
        assert!(later.fragmented);
        packet[56] = 60;
        assert_eq!(super::decode(101, &packet).unwrap().protocol, None);
        packet[56] = 50;
        assert_eq!(super::decode(101, &packet).unwrap().protocol, Some(50));

        packet[6] = 135; // Unsupported extension.
        assert_eq!(super::decode(101, &packet).unwrap().protocol, None);
    }
}
