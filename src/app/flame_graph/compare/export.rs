use super::{Comparison, ViewState, plot, profile::symbol_query};
use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::{
    borrow::Cow,
    io::{self, BufWriter, Write},
    sync::Arc,
};

pub(in crate::app::flame_graph) struct Snapshot {
    comparison: Arc<Comparison>,
    rows: Arc<[usize]>,
    filter: String,
}

impl Snapshot {
    pub(in crate::app::flame_graph) fn capture(state: &ViewState) -> Self {
        Self {
            comparison: state.comparison.clone(),
            rows: state.visible.clone(),
            filter: symbol_query(&state.applied).to_owned(),
        }
    }

    pub(in crate::app::flame_graph) fn write(&self, output: impl Write) -> io::Result<()> {
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

struct Changes<'a>(&'a Snapshot);

impl Serialize for Changes<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let snapshot = self.0;
        let comparison = &snapshot.comparison;
        let mut rows = serializer.serialize_seq(Some(snapshot.rows.len()))?;
        for &index in snapshot.rows.iter() {
            let change = &comparison.changes[index];
            let (profile, frames) = change.stack_frames(comparison);
            let [before_samples, after_samples] = change.counts;
            rows.serialize_element(&StackChange {
                stack: frames.into_iter().map(|id| profile.symbol(id)).collect(),
                before_samples,
                after_samples,
                before_percent: plot::share(before_samples, comparison.totals[0]),
                after_percent: plot::share(after_samples, comparison.totals[1]),
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

#[cfg(test)]
mod tests {
    use super::super::{PAGE_SIZE, Profile};
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn snapshot_exports_every_applied_match_and_keeps_original_symbols() {
        let stack_count = PAGE_SIZE + 2;
        let mut before: String = (0..stack_count)
            .map(|id| format!("parent;leaf-{id}\u{202e}symbol 1\n"))
            .collect();
        before.push_str("outside-filter 2\n");
        let mut state = ViewState::new(
            Arc::new(Profile::parse(&before).unwrap()),
            Arc::new(Profile::parse("new-stack 7").unwrap()),
            ["folder/before.folded".into(), "folder/after.folded".into()],
        );
        state.filtered("PARENT".into(), state.comparison.filter("PARENT"));
        state.page(true);
        state.query = "typed but never applied".into();
        let snapshot = Snapshot::capture(&state);
        state.filtered(String::new(), state.comparison.filter(""));

        let mut bytes = Vec::new();
        snapshot.write(&mut bytes).unwrap();
        let report: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(report["schema_version"], 1);
        assert_eq!(report["filter"], "PARENT");
        assert_eq!(
            report["before"],
            json!({
                "file_name": "before.folded",
                "total_samples": stack_count + 2,
            })
        );
        assert_eq!(
            report["after"],
            json!({
                "file_name": "after.folded",
                "total_samples": 7,
            })
        );
        assert_eq!(report["total_changed_stacks"], stack_count + 2);
        let rows = report["changes"].as_array().unwrap();
        assert_eq!(rows.len(), stack_count);
        assert_eq!(rows[0]["stack"], json!(["parent", "leaf-0\u{202e}symbol"]));
        for row in rows {
            assert_eq!(row["before_samples"], 1);
            assert_eq!(row["after_samples"], 0);
            assert_eq!(row["after_percent"].as_f64().unwrap(), 0.0);
            let before_percent = row["before_percent"].as_f64().unwrap();
            let delta = row["delta_percentage_points"].as_f64().unwrap();
            assert!((before_percent + delta).abs() < 1.0e-12);
        }
    }
}
