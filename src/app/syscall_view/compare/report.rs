use super::{
    COMPARISON_LIMIT, Comparison, ComparisonRow, ViewState, delta, evidence_text, safe_text,
};
use crate::app::openrouter_assistance::clip;
use std::{
    fmt::Write as _,
    io::{self, BufWriter, Write},
    path::Path,
    sync::Arc,
};

const SIDES: [&str; 2] = ["Working", "Failing"];
const REVIEW_OTHER_ROWS: usize = 12;

impl ViewState {
    pub(super) fn openrouter_preview(&self) -> Option<String> {
        let selected = self.selected?;
        let (comparison, row) = self.current()?;
        let first = comparison.first_divergence();
        let mut preview = String::from("Syscall trace comparison\n");
        for (label, trace) in SIDES.into_iter().zip(comparison.traces()) {
            let _ = writeln!(
                preview,
                "{label} · {} source files · {} calls",
                trace.file_count(),
                trace.events().len(),
            );
        }
        let scope = if self.applied.is_some() {
            "match the applied filter"
        } else {
            "are shown"
        };
        let timing = if self.timing { "included" } else { "hidden" };
        let _ = writeln!(
            preview,
            "{} of {} change rows {scope}. Timing-only changes are {timing}.",
            self.visible.len(),
            comparison.rows().len(),
        );
        if comparison.limited {
            let _ = writeln!(preview, "{COMPARISON_LIMIT}");
        }
        preview.push_str("\nSelected change\n");
        review_row(&mut preview, row, first == Some(selected));
        let _ = writeln!(
            preview,
            "Evidence pair {} of {}",
            self.sample + 1,
            row.evidence_count(),
        );
        for (side, label) in SIDES.into_iter().enumerate() {
            let Some(index) = comparison.event(row, side, self.sample) else {
                let _ = writeln!(preview, "{label} · No corresponding call");
                continue;
            };
            let trace = &comparison.traces()[side];
            let event = &trace.events()[index];
            let outcome = event
                .error
                .as_deref()
                .unwrap_or_else(|| event.outcome.label());
            let _ = write!(preview, "{label} · {}", review_field(outcome));
            for raw in event.originals() {
                let _ = write!(
                    preview,
                    " · Source {} line {}",
                    raw.source_number(),
                    raw.line
                );
            }
            if let Some(elapsed) = trace.relative_start(index) {
                let _ = write!(
                    preview,
                    " · +{:.3} ms from run start",
                    elapsed as f64 / 1_000_000.0
                );
            }
            if let Some(duration) = event.duration {
                let _ = write!(
                    preview,
                    " · {:.3} ms duration",
                    duration as f64 / 1_000_000.0
                );
            }
            if event.later_success {
                preview.push_str(" · Later success on the same resource");
            }
            preview.push('\n');
        }
        if self.visible.len() > 1 {
            preview.push_str("\nOther matching changes\n");
            for &index in self
                .visible
                .iter()
                .filter(|&&index| index != selected)
                .take(REVIEW_OTHER_ROWS)
            {
                review_row(
                    &mut preview,
                    &comparison.rows()[index],
                    first == Some(index),
                );
            }
            let omitted = self.visible.len().saturating_sub(REVIEW_OTHER_ROWS + 1);
            if omitted != 0 {
                let _ = writeln!(preview, "{omitted} additional matching changes omitted.");
            }
        }
        Some(preview)
    }
}

fn review_row(output: &mut String, row: &ComparisonRow, first_divergence: bool) {
    let _ = write!(output, "{} · {}", row.kind.label(), review_field(&row.call),);
    if !row.resource.is_empty() {
        let _ = write!(output, " · Resource {}", review_field(&row.resource));
    }
    if !row.process.is_empty() {
        let _ = write!(output, " · Executable {}", review_field(&row.process));
    }
    let [working, failing] = row.counts();
    let _ = write!(output, " · Working {working}, failing {failing}");
    if let [Some(before), Some(after)] = row.durations {
        let _ = write!(
            output,
            " · Total duration delta {:+.3} ms",
            delta(before, after)
        );
    }
    if first_divergence {
        output.push_str(" · First observed divergence");
    }
    if row.uncertain {
        output.push_str(" · Uncertain alignment");
    }
    output.push('\n');
}

fn review_field(text: &str) -> String {
    let safe = safe_text(text);
    let clipped = clip(&safe, 192);
    if clipped.len() < safe.len() {
        format!("{clipped}…")
    } else {
        safe
    }
}

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

        let preview = state.openrouter_preview().unwrap();
        assert!(preview.len() < w11term::openrouter::MAX_INPUT);
        assert!(preview.contains("45 of 45 change rows are shown"));
        assert!(preview.contains("32 additional matching changes omitted"));
        assert!(preview.contains("Failing · OK · Source 1 line 2"));
        assert!(!preview.contains("working.strace"));
        assert!(!preview.contains("failing.strace"));
        assert!(!preview.contains("/tmp/setting-44"));
        assert!(!preview.contains("stat("));
    }
}
