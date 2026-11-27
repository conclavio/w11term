use super::offline;
use anyhow::{Context, Result, bail, ensure};
use std::{io::Write, path::PathBuf, sync::Arc};
use w11term::{ssh::TriageReport, triage_comparison::Comparison};

pub(super) const COMPARE_VERB: &str = "compare-linux-triage";
pub(super) const COMPARE_HELP: &str =
    "w11term-cli compare-linux-triage BEFORE AFTER [--format json|text] [--output-file PATH]";

pub(super) fn compare_files(args: &[String], stdout: &mut dyn Write) -> Result<()> {
    let mut args = args.iter();
    let before = args.next().context("provide the BEFORE triage report")?;
    let after = args.next().context("provide the AFTER triage report")?;
    let mut format = None;
    let mut output_file = None;
    while let Some(flag) = args.next() {
        let target = match flag.as_str() {
            "--format" => &mut format,
            "--output-file" => &mut output_file,
            _ => bail!("unknown option {flag} for {COMPARE_VERB}"),
        };
        ensure!(target.is_none(), "specify {flag} only once");
        *target = Some(
            args.next()
                .with_context(|| format!("{flag} requires a value"))?
                .as_str(),
        );
    }
    let text = match format {
        None | Some("json") => false,
        Some("text") => true,
        Some(_) => bail!("--format requires json or text"),
    };
    let paths = [PathBuf::from(before), PathBuf::from(after)];
    let load = |side: usize, label: &str| {
        TriageReport::load(&paths[side])
            .with_context(|| {
                format!(
                    "Reading the {label} triage report {}",
                    paths[side].display()
                )
            })
            .map(Arc::new)
    };
    let reports = [load(0, "before")?, load(1, "after")?];
    let comparison = Comparison::new(reports, paths.map(Some));
    offline::write_report(stdout, output_file, |dest| {
        if text {
            comparison.write_report(dest)
        } else {
            comparison.write_json(dest)
        }
    })
}
