use super::{Comparison, SIDES, check_status, coverage_summary, display_output};
use std::io::{self, BufWriter, Write};

impl Comparison {
    pub fn write_report(&self, output: impl Write) -> io::Result<()> {
        let mut output = BufWriter::new(output);
        writeln!(output, "Linux performance triage comparison")?;
        for (side, label) in SIDES.into_iter().enumerate() {
            writeln!(
                output,
                "{label} · {} · {}",
                field(&self.source_name(side)),
                coverage_summary(&self.reports[side]),
            )?;
        }
        let endpoint = if self.same_endpoint() {
            "Same recorded SSH endpoint"
        } else {
            "Different recorded SSH endpoints"
        };
        writeln!(output, "{endpoint}")?;
        if self.changes.is_empty() {
            writeln!(
                output,
                "\nNo changes in check coverage or reported observations."
            )?;
            return output.flush();
        }

        writeln!(output, "\nChanges · {}", self.changes.len())?;
        for change in &self.changes {
            let id = &self.reports[0].target.probes[change.probe].id;
            writeln!(output, "\n{id} · {}", field(&self.title(change)))?;
            for (side, label) in SIDES.into_iter().enumerate() {
                write!(output, "{label} · {}", field(&self.value(change, side)))?;
                if let Some((line, _)) = self.observation_line(change, side) {
                    write!(output, " · {id} stdout line {line}")?;
                }
                writeln!(output)?;
            }
            if let Some(delta) = &change.delta {
                writeln!(output, "Delta · {delta}")?;
            }
        }

        writeln!(output, "\nSource output for changed checks")?;
        for probe in 0..self.reports[0].target.probes.len() {
            if !self.changes.iter().any(|change| change.probe == probe) {
                continue;
            }
            for (report, label) in self.reports.iter().zip(SIDES) {
                let recipe = &report.target.probes[probe];
                let sample = &report.collection.samples[probe];
                writeln!(
                    output,
                    "\n{label} · {} · {}",
                    recipe.id,
                    check_status(report, probe)
                )?;
                writeln!(output, "Command · {}", field(&recipe.command))?;
                writeln!(output, "Sampling · {}", field(&recipe.sampling))?;
                stream(&mut output, "stdout", &sample.stdout.text)?;
                stream(&mut output, "stderr", &sample.stderr.text)?;
            }
        }
        output.flush()
    }
}

fn stream(output: &mut impl Write, label: &str, text: &str) -> io::Result<()> {
    if text.is_empty() {
        return writeln!(output, "{label} · No output captured");
    }
    writeln!(output, "{label}")?;
    for (line, text) in text.lines().enumerate() {
        writeln!(output, "{} | {}", line + 1, field(text))?;
    }
    Ok(())
}

fn field(text: &str) -> String {
    display_output(text)
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}
