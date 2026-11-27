use super::{Collection, Sample, SampleStatus};
use serde::Serialize;
use std::rc::Rc;

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Resource {
    #[default]
    Cpu,
    Memory,
    Disk,
    Network,
    Kernel,
}

impl Resource {
    pub const ALL: [Self; 5] = [
        Self::Cpu,
        Self::Memory,
        Self::Disk,
        Self::Network,
        Self::Kernel,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Memory => "Memory",
            Self::Disk => "Disk",
            Self::Network => "Network",
            Self::Kernel => "Kernel",
        }
    }
}

#[derive(Serialize)]
pub struct Finding {
    pub resource: Resource,
    pub label: String,
    pub value: String,
    pub sample_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_line: Option<usize>,
}

pub(super) fn summarize(collection: &Collection) -> Vec<Finding> {
    let mut summary = Vec::new();
    for sample in &collection.samples {
        if !matches!(sample.status, SampleStatus::Ok | SampleStatus::OutputLimit) {
            let resource = match sample.id.as_str() {
                "dmesg" => Resource::Kernel,
                "free" => Resource::Memory,
                "iostat" => Resource::Disk,
                "sar-dev" | "sar-tcp" => Resource::Network,
                _ => Resource::Cpu,
            };
            let status = match sample.status {
                SampleStatus::Missing => "Utility missing",
                SampleStatus::PermissionDenied => "Permission denied",
                SampleStatus::TimedOut => "Timed out",
                SampleStatus::Incomplete => "Incomplete sample",
                _ => "Sample failed",
            };
            add(
                &mut summary,
                resource,
                sample,
                None,
                format!("{} coverage", sample.id),
                status,
            );
            if sample.id == "vmstat" {
                add(
                    &mut summary,
                    Resource::Memory,
                    sample,
                    None,
                    "Swap sampling",
                    status,
                );
            }
            continue;
        }
        match sample.id.as_str() {
            "vmstat" => vmstat(sample, &mut summary),
            "free" => memory(sample, &mut summary),
            "iostat" => disk(sample, &mut summary),
            "sar-dev" => interfaces(sample, &mut summary),
            "sar-tcp" => tcp(sample, &mut summary),
            "dmesg" => kernel(sample, &mut summary),
            _ => (),
        }
    }
    summary
}

fn add(
    summary: &mut Vec<Finding>,
    resource: Resource,
    sample: &Sample,
    stdout_line: Option<usize>,
    label: impl Into<String>,
    value: impl Into<String>,
) {
    summary.push(Finding {
        resource,
        label: label.into(),
        value: value.into(),
        sample_id: sample.id.clone(),
        stdout_line,
    });
}

fn lines(sample: &Sample) -> impl Iterator<Item = (usize, &str)> {
    sample
        .stdout
        .text
        .split_inclusive('\n')
        .enumerate()
        .filter_map(|(index, line)| {
            (!sample.stdout.at_byte_limit || line.ends_with('\n'))
                .then_some((index + 1, line.trim_end_matches(['\n', '\r'])))
        })
}

fn numeric(value: &str) -> Option<f64> {
    let value: f64 = value.parse().ok()?;
    (value.is_finite() && value >= 0.0).then_some(value)
}

enum Table {
    Vmstat,
    Device,
    Interface,
    Tcp,
}

struct Row<'a> {
    line: usize,
    columns: Rc<[&'a str]>,
    cells: Vec<&'a str>,
}

impl Row<'_> {
    fn value(&self, column: &str) -> Option<f64> {
        self.columns
            .iter()
            .position(|name| *name == column)
            .and_then(|index| numeric(self.cells[index]))
    }
}

fn clock_prefix<'a, 'text>(words: &'a [&'text str]) -> Option<&'a [&'text str]> {
    let (clock, rest) = words.split_first()?;
    if clock.len() != 8
        || !clock.as_bytes().iter().enumerate().all(|(index, c)| {
            if matches!(index, 2 | 5) {
                *c == b':'
            } else {
                c.is_ascii_digit()
            }
        })
    {
        return None;
    }
    Some(match rest {
        ["AM" | "PM", rest @ ..] => rest,
        _ => rest,
    })
}

fn rows(sample: &Sample, table: Table) -> Vec<Row<'_>> {
    let mut header = None::<Rc<[&str]>>;
    let mut result = Vec::new();
    for (line, text) in lines(sample) {
        let words: Vec<_> = text.split_ascii_whitespace().collect();
        let cells = if matches!(table, Table::Interface | Table::Tcp) {
            let Some(cells) = clock_prefix(&words) else {
                continue;
            };
            cells
        } else {
            &words
        };
        let Some((first, rest)) = cells.split_first() else {
            continue;
        };
        let is_header = match table {
            Table::Vmstat => *first == "r" && cells.contains(&"id"),
            Table::Device => matches!(*first, "Device" | "Device:"),
            Table::Interface => *first == "IFACE",
            Table::Tcp => cells.contains(&"retrans/s"),
        };
        if is_header {
            header = Some(Rc::from(cells));
            continue;
        }
        // sar interleaves TCP and ETCP tables with independent headers.
        if matches!(table, Table::Tcp) && numeric(first).is_none() {
            header = None;
        }
        let Some(columns) = header.as_ref() else {
            continue;
        };
        let numeric_cells = if matches!(table, Table::Device | Table::Interface) {
            if numeric(first).is_some() {
                continue;
            }
            rest
        } else {
            cells
        };
        if columns.len() == cells.len() && numeric_cells.iter().all(|cell| numeric(cell).is_some())
        {
            result.push(Row {
                line,
                columns: columns.clone(),
                cells: cells.to_vec(),
            });
        }
    }
    result
}

fn peak<'a, 'text>(
    rows: &'a [Row<'text>],
    metric: impl Fn(&Row<'text>) -> Option<f64>,
) -> Option<(&'a Row<'text>, f64)> {
    rows.iter()
        .filter_map(|row| Some((row, metric(row)?)))
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
}

fn vmstat(sample: &Sample, summary: &mut Vec<Finding>) {
    let rows = rows(sample, Table::Vmstat);
    // vmstat 1 5 starts with averages since boot.
    let live = rows.get(1..).unwrap_or_default();
    if let Some((row, value)) = peak(live, |row| {
        let value = row.value("us")? + row.value("sy")?;
        (value <= 100.0).then_some(value)
    }) {
        add(
            summary,
            Resource::Cpu,
            sample,
            Some(row.line),
            "Peak CPU busy",
            format!("{value:.0}% user + system"),
        );
    }
    if let Some((row, value)) = peak(live, |row| row.value("r")) {
        add(
            summary,
            Resource::Cpu,
            sample,
            Some(row.line),
            "Peak runnable tasks",
            format!("{value:.0}"),
        );
    }
    for (column, title) in [("si", "Peak swap in"), ("so", "Peak swap out")] {
        if let Some((row, value)) = peak(live, |row| row.value(column)) {
            add(
                summary,
                Resource::Memory,
                sample,
                Some(row.line),
                title,
                format!("{value:.0} KiB/s"),
            );
        }
    }
    if live.is_empty() {
        add(
            summary,
            Resource::Cpu,
            sample,
            None,
            "vmstat coverage",
            "No recognized interval rows",
        );
    }
}

fn memory(sample: &Sample, summary: &mut Vec<Finding>) {
    let mut lines = lines(sample).filter(|(_, line)| !line.trim().is_empty());
    let evidence = (|| {
        let (_, header) = lines.next()?;
        let header: Vec<_> = header.split_ascii_whitespace().collect();
        let total = header.iter().position(|name| *name == "total")?;
        let available = header.iter().position(|name| *name == "available")?;
        let (line, data) = lines.find(|(_, line)| line.trim_start().starts_with("Mem:"))?;
        let data: Vec<_> = data.split_ascii_whitespace().skip(1).collect();
        let total: u64 = data.get(total)?.parse().ok()?;
        let available: u64 = data.get(available)?.parse().ok()?;
        (total != 0 && available <= total).then_some((line, total, available))
    })();
    if let Some((line, total, available)) = evidence {
        add(
            summary,
            Resource::Memory,
            sample,
            Some(line),
            "Available memory",
            format!("{available} of {total} MiB"),
        );
    } else {
        add(
            summary,
            Resource::Memory,
            sample,
            None,
            "free coverage",
            "Available memory column unavailable",
        );
    }
}

fn disk(sample: &Sample, summary: &mut Vec<Finding>) {
    let rows = rows(sample, Table::Device);
    if let Some((row, value)) = peak(&rows, |row| row.value("%util")) {
        add(
            summary,
            Resource::Disk,
            sample,
            Some(row.line),
            "Peak device busy time",
            format!("{value:.1}% on {}", row.cells[0]),
        );
    }
    if let Some((row, value)) = peak(&rows, |row| {
        row.value("aqu-sz").or_else(|| row.value("avgqu-sz"))
    }) {
        add(
            summary,
            Resource::Disk,
            sample,
            Some(row.line),
            "Peak average disk queue",
            format!("{value:.2} on {}", row.cells[0]),
        );
    }
    if let Some((row, value)) = peak(&rows, |row| {
        ["await", "r_await", "w_await"]
            .into_iter()
            .filter_map(|name| row.value(name))
            .reduce(f64::max)
    }) {
        add(
            summary,
            Resource::Disk,
            sample,
            Some(row.line),
            "Peak average I/O latency",
            format!("{value:.2} ms on {}", row.cells[0]),
        );
    }
    if rows.is_empty() {
        add(
            summary,
            Resource::Disk,
            sample,
            None,
            "iostat coverage",
            "No recognized device rows",
        );
    }
}

fn interfaces(sample: &Sample, summary: &mut Vec<Finding>) {
    let rows = rows(sample, Table::Interface);
    let traffic = |row: &Row<'_>| {
        let value = row.value("rxkB/s")? + row.value("txkB/s")?;
        value.is_finite().then_some(value)
    };
    if let Some((row, value)) = peak(&rows, traffic) {
        add(
            summary,
            Resource::Network,
            sample,
            Some(row.line),
            "Peak interface traffic",
            format!("{value:.1} KiB/s RX + TX on {}", row.cells[0]),
        );
    } else {
        add(
            summary,
            Resource::Network,
            sample,
            None,
            "sar device coverage",
            "No recognized interface rows",
        );
    }
}

fn tcp(sample: &Sample, summary: &mut Vec<Finding>) {
    let rows = rows(sample, Table::Tcp);
    for (column, label) in [
        ("retrans/s", "Peak TCP retransmits"),
        ("isegerr/s", "Peak TCP input errors"),
    ] {
        if let Some((row, value)) = peak(&rows, |row| row.value(column)) {
            add(
                summary,
                Resource::Network,
                sample,
                Some(row.line),
                label,
                format!("{value:.2}/s"),
            );
        }
    }
    if rows.is_empty() {
        add(
            summary,
            Resource::Network,
            sample,
            None,
            "sar TCP coverage",
            "No recognized TCP error rows",
        );
    }
}

fn kernel(sample: &Sample, summary: &mut Vec<Finding>) {
    let mut count = 0;
    let mut first = None;
    let mut messages = 0;
    for (line, message) in lines(sample).filter(|(_, message)| !message.is_empty()) {
        messages += 1;
        let lower = message.to_ascii_lowercase();
        if lower.contains("out of memory")
            || lower.contains("oom-kill")
            || lower.contains("i/o error")
        {
            first.get_or_insert(line);
            count += 1;
        }
    }
    if count > 0 {
        add(
            summary,
            Resource::Kernel,
            sample,
            first,
            "OOM or I/O error mentions",
            format!("{count} in {messages} captured lines"),
        );
    } else {
        add(
            summary,
            Resource::Kernel,
            sample,
            None,
            "Kernel log",
            format!("{messages} lines captured"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh::triage::Output;

    fn sample(id: &str, text: &str) -> Sample {
        Sample {
            id: id.into(),
            started_unix_ms: None,
            finished_unix_ms: None,
            exit_status: Some(0),
            status: SampleStatus::Ok,
            stdout: Output {
                text: text.into(),
                at_byte_limit: false,
                base64: None,
            },
            stderr: Output {
                text: String::new(),
                at_byte_limit: false,
                base64: None,
            },
        }
    }

    fn collect(samples: Vec<Sample>) -> Vec<Finding> {
        summarize(&Collection {
            started_unix_ms: 0,
            finished_unix_ms: 0,
            supervisor_status: 0,
            stopped_by_user: false,
            samples,
        })
    }

    fn finding<'a>(findings: &'a [Finding], label: &str) -> &'a Finding {
        findings
            .iter()
            .find(|finding| finding.label == label)
            .unwrap_or_else(|| panic!("Missing {label}"))
    }

    #[test]
    fn findings_use_live_rows_and_keep_the_exact_source_location() {
        let findings = collect(vec![
            sample(
                "vmstat",
                concat!(
                    "procs -----memory---- --swap-- -----cpu-----\n",
                    " r b si so us sy id wa st\n",
                    "99 1 42 42 95 4 1 0 0\n",
                    " 8 1  0  3 59 5 30 5 1\n",
                    " 3 0  2  0 40 2 50 8 0\n",
                ),
            ),
            sample(
                "free",
                " total used free shared buff/cache available\nMem: 2048 1400 40 0 608 320\nSwap: 0 0 0\n",
            ),
            sample(
                "iostat",
                concat!(
                    "Device r/s r_await w_await aqu-sz %util\n",
                    "sda 8 2 4 0.50 20.0\n",
                    "\nDevice: r/s await avgqu-sz %util\n",
                    "sdb 9 13.42 2.34 98\n",
                ),
            ),
            sample(
                "sar-dev",
                concat!(
                    "11:11:11 AM IFACE rxkB/s txkB/s %ifutil\n",
                    "11:11:12 AM eth0 33.00 1.25 0.1\n",
                    "11:11:12 AM lo 0.05 0.05 0.0\n",
                    "Average: eth0 100.00 200.00 10\n",
                ),
            ),
            sample(
                "sar-tcp",
                concat!(
                    "11:11:12 atmptf/s estres/s retrans/s isegerr/s orsts/s\n",
                    "11:11:13 0 0 4.50 1.00 0\n",
                    "11:11:13 active/s passive/s iseg/s oseg/s extra/s\n",
                    "11:11:14 15 0 800 900 0\n",
                    "Average: 0 0 1.0 0.2 0\n",
                ),
            ),
            sample(
                "dmesg",
                "[Mon] system started\n[Mon] Out of memory: Killed process\n[Mon] Buffer I/O error\n",
            ),
        ]);
        for (label, value, id, line) in [
            ("Peak CPU busy", "64% user + system", "vmstat", 4),
            ("Peak runnable tasks", "8", "vmstat", 4),
            ("Peak swap in", "2 KiB/s", "vmstat", 5),
            ("Available memory", "320 of 2048 MiB", "free", 2),
            ("Peak device busy time", "98.0% on sdb", "iostat", 5),
            ("Peak average I/O latency", "13.42 ms on sdb", "iostat", 5),
            (
                "Peak interface traffic",
                "34.2 KiB/s RX + TX on eth0",
                "sar-dev",
                2,
            ),
            ("Peak TCP retransmits", "4.50/s", "sar-tcp", 2),
            (
                "OOM or I/O error mentions",
                "2 in 3 captured lines",
                "dmesg",
                2,
            ),
        ] {
            let result = finding(&findings, label);
            assert_eq!(result.value, value, "{label}");
            assert_eq!(result.sample_id, id);
            assert_eq!(result.stdout_line, Some(line));
        }
    }

    #[test]
    fn findings_disclose_unavailable_checks_and_skip_a_truncated_final_row() {
        let mut vmstat = sample(
            "vmstat",
            "r b us sy id\n99 0 99 1 0\n3 0 40 10 50\n8 0 80 10 1",
        );
        vmstat.stdout.at_byte_limit = true;
        let mut log = sample("dmesg", "");
        log.status = SampleStatus::PermissionDenied;
        let findings = collect(vec![
            vmstat,
            log,
            sample("free", "total used free\nMem: 100 80 20\n"),
        ]);
        assert_eq!(
            finding(&findings, "Peak CPU busy").value,
            "50% user + system"
        );
        assert_eq!(
            finding(&findings, "dmesg coverage").value,
            "Permission denied"
        );
        assert_eq!(finding(&findings, "dmesg coverage").stdout_line, None);
        assert_eq!(
            finding(&findings, "free coverage").value,
            "Available memory column unavailable"
        );
    }
}
