use serde::Serialize;
use std::{
    cmp::Reverse,
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

pub(super) const PREFIX_BYTES: usize = 96;
const MAX_TRACKED_PAIRS: usize = 4096;
const MAX_RANKED_PAIRS: usize = 10;
const IPV4: u16 = 0x0800;
const IPV6: u16 = 0x86dd;

pub(super) type Addresses = (IpAddr, IpAddr);

#[derive(Default, Serialize)]
pub struct Conversations {
    pub tracked_pairs: usize,
    pub unclassified_packets: u32,
    pub untracked_packets: u32,
    pub top: Vec<Conversation>,
    #[serde(skip)]
    remaining: Vec<Conversation>,
}

impl Conversations {
    pub(in crate::ssh::triage::packet_capture::local) fn pairs(
        &self,
    ) -> impl Iterator<Item = &Conversation> {
        self.top.iter().chain(&self.remaining)
    }
}

#[derive(Serialize)]
pub struct Conversation {
    pub a: IpAddr,
    pub b: IpAddr,
    pub a_to_b: Traffic,
    pub b_to_a: Traffic,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Traffic {
    pub packets: u32,
    pub captured_bytes: u64,
}

impl Conversation {
    pub fn total(&self) -> Traffic {
        Traffic {
            packets: self.a_to_b.packets + self.b_to_a.packets,
            captured_bytes: self.a_to_b.captured_bytes + self.b_to_a.captured_bytes,
        }
    }
}

#[derive(Default)]
pub(super) struct Scan {
    pairs: HashMap<Addresses, [Traffic; 2]>,
    unclassified_packets: u32,
    untracked_packets: u32,
}

impl Scan {
    pub(super) fn record(&mut self, addresses: Option<Addresses>, captured: u32) {
        let Some((source, destination)) = addresses else {
            self.unclassified_packets += 1;
            return;
        };
        let (key, direction) = if source <= destination {
            ((source, destination), 0)
        } else {
            ((destination, source), 1)
        };
        let traffic = if self.pairs.len() < MAX_TRACKED_PAIRS {
            self.pairs.entry(key).or_default()
        } else if let Some(traffic) = self.pairs.get_mut(&key) {
            traffic
        } else {
            self.untracked_packets += 1;
            return;
        };
        traffic[direction].packets += 1;
        traffic[direction].captured_bytes += u64::from(captured);
    }

    pub(super) fn finish(self) -> Conversations {
        let tracked_pairs = self.pairs.len();
        let mut top: Vec<_> = self
            .pairs
            .into_iter()
            .map(|((a, b), [a_to_b, b_to_a])| Conversation {
                a,
                b,
                a_to_b,
                b_to_a,
            })
            .collect();
        top.sort_unstable_by_key(|pair| {
            let total = pair.total();
            (
                Reverse(total.captured_bytes),
                Reverse(total.packets),
                pair.a,
                pair.b,
            )
        });
        let remaining = top.split_off(top.len().min(MAX_RANKED_PAIRS));
        Conversations {
            tracked_pairs,
            unclassified_packets: self.unclassified_packets,
            untracked_packets: self.untracked_packets,
            top,
            remaining,
        }
    }
}

pub(super) fn decode(link_type: u16, prefix: &[u8]) -> Option<Addresses> {
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

fn decode_ip(protocol: u16, ip: &[u8]) -> Option<Addresses> {
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
            Some((a.into(), b.into()))
        }
        (IPV6, 6) => {
            let a = <[u8; 16]>::try_from(ip.get(8..24)?).ok()?;
            let b = <[u8; 16]>::try_from(ip.get(24..40)?).ok()?;
            Some((Ipv6Addr::from(a).into(), Ipv6Addr::from(b).into()))
        }
        _ => None,
    }
}

fn be16(bytes: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn ranks_exact_counts_in_both_directions_and_reports_the_tracking_limit() {
        let a = Ipv4Addr::UNSPECIFIED.into();
        let peer = |index: usize| Ipv4Addr::from(index as u32).into();
        let mut scan = Scan::default();
        for index in 1..=MAX_TRACKED_PAIRS {
            scan.record(Some((a, peer(index))), 32);
        }
        let b = peer(MAX_TRACKED_PAIRS);
        scan.record(Some((b, a)), 128);
        scan.record(Some((a, peer(MAX_TRACKED_PAIRS + 1))), 4096);
        scan.record(None, 512);
        let result = scan.finish();
        assert_eq!(result.tracked_pairs, MAX_TRACKED_PAIRS);
        assert_eq!(result.top.len(), MAX_RANKED_PAIRS);
        assert_eq!(result.untracked_packets, 1);
        assert_eq!(result.unclassified_packets, 1);
        let pair = &result.top[0];
        assert_eq!((pair.a, pair.b), (a, b));
        assert_eq!(pair.a_to_b.captured_bytes, 32);
        assert_eq!(pair.b_to_a.captured_bytes, 128);
        assert_eq!(pair.total().packets, 2);
        assert_eq!(result.top[1].b, peer(1));
    }
}
