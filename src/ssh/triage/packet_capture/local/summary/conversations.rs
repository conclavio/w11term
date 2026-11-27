use super::ip::Addresses;
use serde::Serialize;
use std::{cmp::Reverse, collections::HashMap, net::IpAddr};

const MAX_TRACKED_PAIRS: usize = 4096;
const MAX_RANKED_PAIRS: usize = 10;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

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
