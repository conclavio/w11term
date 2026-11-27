use super::{
    COMPARISON_LIMIT, Comparison, ComparisonRow, ViewState, delta, evidence_text, safe_text,
};
use std::{
    io::{self, BufWriter, Write},
    path::Path,
    sync::Arc,
};

const SIDES: [&str; 2] = ["Working", "Failing"];

pub(super) struct Snapshot {
    comparison: Arc<Comparison>,
    rows: Vec<(usize, usize)>,
    selected: Option<(usize, usize)>,
    query: Option<String>,
    timing: bool,
}

impl Snapshot {
    pub(super) fn capture(state: &ViewState) -> Option<Self> {
        let comparison = state.compare.clone()?;
        let rows = state
            .visible
            .iter()
            .map(|&index| {
                let sample = state
                    .match_sample(index)
                    .unwrap_or(comparison.rows()[index].sample);
                (index, sample)
            })
            .collect();
        Some(Self {
            comparison,
            rows,
            selected: state.selected.map(|index| (index, state.sample)),
            query: state.applied.as_ref().map(|filter| filter.query.clone()),
            timing: state.timing,
        })
    }

    pub(super) fn write_json(mut self, output: impl Write) -> io::Result<()> {
        if let Some((index, sample)) = self.selected
            && let Ok(offset) = self.rows.binary_search_by_key(&index, |&(index, _)| index)
        {
            self.rows[offset].1 = sample;
        }
        self.comparison.write_visible_json(
            self.query.as_deref().unwrap_or_default(),
            self.timing,
            &self.rows,
            output,
        )
    }

    pub(super) fn write(&self, output: impl Write) -> io::Result<()> {
        let mut output = BufWriter::new(output);
        let compare = &self.comparison;
        writeln!(output, "Syscall trace comparison")?;
        for (label, trace) in SIDES.into_iter().zip(compare.traces()) {
            let files = trace.file_count();
            let unit = if files == 1 { "file" } else { "files" };
            writeln!(
                output,
                "{label} · {} · {files} source {unit} · {} events",
                file_name(trace.first_path()),
                trace.events().len(),
            )?;
        }
        if let Some(query) = &self.query {
            writeln!(output, "Applied filter · {}", field(query))?;
        }
        writeln!(
            output,
            "Timing-only changes · {}",
            if self.timing { "Included" } else { "Hidden" }
        )?;
        writeln!(
            output,
            "Reported changes · {} of {}",
            self.rows.len(),
            compare.rows().len()
        )?;
        if compare.limited {
            writeln!(output, "{COMPARISON_LIMIT}")?;
        }
        let first = compare.first_divergence();
        for (offset, &(index, sample)) in self.rows.iter().enumerate() {
            let row = &compare.rows()[index];
            let divergence = if Some(index) == first {
                " · First divergence"
            } else {
                ""
            };
            let uncertain = if row.uncertain {
                " · Match uncertain"
            } else {
                ""
            };
            writeln!(
                output,
                "\n{}. {} · {} · {}{divergence}{uncertain}",
                offset + 1,
                row.kind.label(),
                field(&row.call),
                field(&row.resource),
            )?;
            writeln!(output, "   Executable · {}", field(&row.process))?;
            let [left, right] = row.counts();
            writeln!(
                output,
                "   Working {left} · Failing {right} · Count delta {:+}",
                right as i64 - left as i64,
            )?;
            if row.result_changes != 0 {
                let [before, after] = row
                    .result
                    .each_ref()
                    .map(|value| value.as_deref().unwrap_or("Unavailable"));
                writeln!(
                    output,
                    "   Representative result · {} → {} · {} changed",
                    field(before),
                    field(after),
                    row.result_changes,
                )?;
            }
            if let [Some(before), Some(after)] = row.durations {
                writeln!(
                    output,
                    "   Total call duration delta {:+.3} ms",
                    delta(before, after),
                )?;
            }
            for (side, label) in SIDES.into_iter().enumerate() {
                write_source(&mut output, compare, row, side, sample, label)?;
            }
        }
        if let Some((index, sample)) = self.selected
            && let Ok(offset) = self.rows.binary_search_by_key(&index, |&(index, _)| index)
        {
            let row = &compare.rows()[index];
            writeln!(
                output,
                "\nEvidence for change {} · pair {} of {}",
                offset + 1,
                sample + 1,
                row.evidence_count(),
            )?;
            for (side, label) in SIDES.into_iter().enumerate() {
                writeln!(
                    output,
                    "\n{label}\n{}",
                    evidence_text(compare, row, side, sample)
                )?;
            }
        }
        output.flush()
    }
}

fn write_source(
    mut output: impl Write,
    compare: &Comparison,
    row: &ComparisonRow,
    side: usize,
    sample: usize,
    label: &str,
) -> io::Result<()> {
    let Some(index) = compare.event(row, side, sample) else {
        return writeln!(output, "   {label} · No corresponding call");
    };
    let trace = &compare.traces()[side];
    let event = &trace.events()[index];
    let result = event
        .error
        .as_deref()
        .unwrap_or_else(|| event.outcome.label());
    write!(output, "   {label} · {} · ", field(result))?;
    for (offset, raw) in event.originals().enumerate() {
        if offset != 0 {
            write!(output, " + ")?;
        }
        write!(
            output,
            "{} line {}",
            file_name(trace.original(raw).0),
            raw.line
        )?;
    }
    writeln!(output)
}

fn file_name(path: &Path) -> String {
    field(
        &path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy(),
    )
}

fn field(text: &str) -> String {
    match text.char_indices().nth(512) {
        Some((end, _)) => format!("{}…", safe_text(&text[..end])),
        None => safe_text(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::syscall_view::{Trace, compare::ROWS};

    #[test]
    fn snapshot_writes_changes_from_every_page() {
        let directory = tempfile::tempdir().unwrap();
        let working = directory.path().join("working.strace");
        let failing = directory.path().join("failing.strace");
        let start = "5 execve(\"/bin/diag\", [\"diag\"], NULL) = 0\n";
        std::fs::write(&working, start).unwrap();
        let mut after = start.to_owned();
        for i in 0..ROWS + 5 {
            after.push_str(&format!("5 stat(\"/tmp/setting-{i}\", {{}}) = 0\n"));
        }
        std::fs::write(&failing, after).unwrap();
        let comparison = Comparison::build(
            Arc::new(Trace::open(vec![working]).unwrap()),
            Arc::new(Trace::open(vec![failing]).unwrap()),
        );
        let mut state = ViewState {
            compare: Some(Arc::new(comparison)),
            ..ViewState::default()
        };
        state.filter(false);
        assert_eq!(state.page_rows().len(), ROWS);
        let report = Snapshot::capture(&state).unwrap();
        let mut bytes = Vec::new();
        report.write(&mut bytes).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("Reported changes · 45 of 45"));
        assert!(text.contains("45. Added call · stat · \"/tmp/setting-44\""));
        assert!(text.contains("Failing · OK · failing.strace line 46"));
        assert!(text.contains("Evidence for change 1 · pair 1 of 1"));
    }
}
