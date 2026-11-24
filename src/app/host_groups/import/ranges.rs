use super::list::MAX_TARGETS;
use anyhow::{Context, Result, ensure};
use std::fmt::Write;

const INVALID: &str = "Use numeric ranges in the hostname, such as ops@rack<1-2>-web<01-03,07>.";

pub(super) struct Expansion<'a> {
    segments: Vec<Segment<'a>>,
    tail: &'a str,
}

struct Segment<'a> {
    prefix: &'a str,
    intervals: Vec<Interval>,
    count: u64,
}

struct Interval {
    start: u32,
    end: u32,
    width: usize,
}

impl Interval {
    fn parse(input: &str) -> Result<Self> {
        let (start, end) = input.split_once('-').unwrap_or((input, input));
        let padded = |number: &str| number.len() > 1 && number.starts_with('0');
        let width = if padded(start) || padded(end) {
            start.len().max(end.len())
        } else {
            0
        };
        let integer = |number: &str| -> Result<u32> {
            ensure!(
                !number.is_empty()
                    && number.len() <= 10
                    && number.bytes().all(|b| b.is_ascii_digit()),
                "{INVALID}"
            );
            number.parse().ok().context(INVALID)
        };
        Ok(Self {
            start: integer(start)?,
            end: integer(end)?,
            width,
        })
    }

    fn count(&self) -> u64 {
        u64::from(self.start.abs_diff(self.end)) + 1
    }

    fn write(&self, offset: u64, output: &mut String) {
        let start = u64::from(self.start);
        let number = if self.start <= self.end {
            start + offset
        } else {
            start - offset
        };
        write!(output, "{number:0width$}", width = self.width).expect("writing a String");
    }
}

impl Segment<'_> {
    fn write(&self, mut offset: u64, output: &mut String) {
        output.push_str(self.prefix);
        for interval in &self.intervals {
            let count = interval.count();
            if offset < count {
                interval.write(offset, output);
                return;
            }
            offset -= count;
        }
        unreachable!("numeric range offset is within the segment");
    }
}

impl<'a> Expansion<'a> {
    pub(super) fn parse(input: &'a str) -> Result<Option<Self>> {
        if !input.contains(['<', '>']) {
            return Ok(None);
        }
        let scheme = input.find("://").map_or(0, |position| position + 3);
        let host = input[scheme..]
            .rfind('@')
            .map_or(scheme, |position| scheme + position + 1);
        let host_end = input[host..]
            .find([':', '/', '[', ']'])
            .map_or(input.len(), |position| host + position);
        ensure!(
            !input[..host].contains(['<', '>']) && !input[host_end..].contains(['<', '>']),
            "{INVALID}"
        );
        let mut tail = input;
        let mut segments = Vec::new();
        while let Some((prefix, rest)) = tail.split_once('<') {
            ensure!(!prefix.contains('>'), "{INVALID}");
            let (bounds, suffix) = rest.split_once('>').context(INVALID)?;
            let intervals = bounds
                .split(',')
                .map(Interval::parse)
                .collect::<Result<Vec<_>>>()?;
            let count = intervals.iter().map(Interval::count).sum();
            segments.push(Segment {
                prefix,
                intervals,
                count,
            });
            tail = suffix;
        }
        ensure!(!tail.contains('>'), "{INVALID}");
        Ok(Some(Self { segments, tail }))
    }

    pub(super) fn expand(&self, remaining: usize) -> Result<impl Iterator<Item = String> + '_> {
        let count = self
            .segments
            .iter()
            .fold(1u64, |total, segment| total.saturating_mul(segment.count));
        ensure!(
            count <= remaining as u64,
            "A host list can expand to at most {MAX_TARGETS} entries."
        );
        Ok((0..count).map(move |index| {
            let mut address = String::new();
            // The rightmost group varies first. Each stride counts the combinations to its right.
            let mut stride = count;
            for segment in &self.segments {
                stride /= segment.count;
                segment.write((index / stride) % segment.count, &mut address);
            }
            address.push_str(self.tail);
            address
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combines_ordinals_in_written_order_and_keeps_width_per_interval() {
        let plan = Expansion::parse("ssh://ops@rack<2-1>-web<03,2-01,7>:2222/")
            .unwrap()
            .unwrap();
        let addresses = plan.expand(8).unwrap().collect::<Vec<_>>();
        assert_eq!(
            addresses,
            ["2-03", "2-02", "2-01", "2-7", "1-03", "1-02", "1-01", "1-7"].map(|pair| {
                let (rack, web) = pair.split_once('-').unwrap();
                format!("ssh://ops@rack{rack}-web{web}:2222/")
            })
        );
        assert!(plan.expand(7).is_err());
    }

    #[test]
    fn rejects_invalid_segments_and_ranges_outside_the_hostname() {
        for input in [
            "u@web<1,>",
            "u@web<2-1-0>",
            "u@web<,3>",
            "u@web<7- 9>",
            "u@web<1-3",
            "u@web<1-3>>",
            "u<1-3>@web",
            "u@<1-3>@web",
            "u@web:<1-3>",
            "u@web/<1-3>",
            "u@[::1<1-3>]",
            "u@web<4294967296>",
        ] {
            assert!(Expansion::parse(input).is_err(), "{input}");
        }
    }

    #[test]
    fn validates_the_cartesian_size_before_expanding() {
        let plan = Expansion::parse("u@rack<0-4294967295>-web<0-4294967295>")
            .unwrap()
            .unwrap();
        assert!(plan.expand(MAX_TARGETS).is_err());
        let long = "<01>".repeat(1_000);
        let plan = Expansion::parse(&long).unwrap().unwrap();
        assert_eq!(plan.expand(1).unwrap().next().unwrap(), "01".repeat(1_000));
    }
}
