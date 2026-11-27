use super::{super::sample_share, FilteredComparison};
use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::{
    borrow::Cow,
    io::{self, BufWriter, Write},
};

impl FilteredComparison {
    pub fn write_json(&self, output: impl Write) -> io::Result<()> {
        let comparison = &self.comparison;
        let source = |side: usize| Source {
            file_name: comparison.paths[side]
                .file_name()
                .map(|name| name.to_string_lossy()),
            total_samples: comparison.totals[side],
        };
        let report = Report {
            schema_version: 1,
            before: source(0),
            after: source(1),
            filter: &self.filter,
            total_changed_stacks: comparison.changes.len(),
            changes: Changes(self),
        };
        let mut output = BufWriter::new(output);
        serde_json::to_writer(&mut output, &report).map_err(io::Error::other)?;
        output.write_all(b"\n")?;
        output.flush()
    }
}

#[derive(Serialize)]
struct Source<'a> {
    file_name: Option<Cow<'a, str>>,
    total_samples: u64,
}

#[derive(Serialize)]
struct Report<'a> {
    schema_version: u8,
    before: Source<'a>,
    after: Source<'a>,
    filter: &'a str,
    total_changed_stacks: usize,
    changes: Changes<'a>,
}

struct Changes<'a>(&'a FilteredComparison);

impl Serialize for Changes<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let snapshot = self.0;
        let comparison = &snapshot.comparison;
        let mut rows = serializer.serialize_seq(Some(snapshot.indices.len()))?;
        for &index in snapshot.indices.iter() {
            let change = &comparison.changes[index];
            let (profile, frames) = change.stack_frames(comparison);
            let [before_samples, after_samples] = change.counts;
            rows.serialize_element(&StackChange {
                stack: frames.into_iter().map(|id| profile.symbol(id)).collect(),
                before_samples,
                after_samples,
                before_percent: sample_share(before_samples, comparison.totals[0]),
                after_percent: sample_share(after_samples, comparison.totals[1]),
                delta_percentage_points: comparison.delta_points(change),
            })?;
        }
        rows.end()
    }
}

#[derive(Serialize)]
struct StackChange<'a> {
    stack: Vec<&'a str>,
    before_samples: u64,
    after_samples: u64,
    before_percent: f64,
    after_percent: f64,
    delta_percentage_points: f64,
}
