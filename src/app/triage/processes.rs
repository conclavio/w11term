use std::{collections::BTreeMap, num::NonZeroU32};
use w11term::ssh::{TriageReport, TriageSampleStatus};

#[derive(Clone, Copy)]
enum Source {
    Interval,
    Snapshot,
}

pub(super) struct Process {
    pub pid: NonZeroU32,
    pub command: String,
    cpu: f64,
    source: Source,
}

impl Process {
    pub(super) fn cpu_label(&self) -> String {
        let measurement = match self.source {
            Source::Interval => "peak CPU",
            Source::Snapshot => "CPU in top snapshot",
        };
        format!("{:.1}% {measurement}", self.cpu)
    }
}

pub(super) fn busy_processes(report: &TriageReport) -> Vec<Process> {
    let processes = parse_pidstat(sample_text(report, "pidstat"));
    if processes.is_empty() {
        parse_top(sample_text(report, "top"))
    } else {
        processes
    }
}

fn sample_text<'a>(report: &'a TriageReport, id: &str) -> &'a str {
    let Some(sample) = report
        .collection
        .samples
        .iter()
        .find(|sample| sample.id == id)
    else {
        return "";
    };
    let may_end_mid_row = sample.stdout.at_byte_limit
        || matches!(
            sample.status,
            TriageSampleStatus::Incomplete
                | TriageSampleStatus::TimedOut
                | TriageSampleStatus::Failed
        );
    complete_lines(&sample.stdout.text, may_end_mid_row)
}

fn complete_lines(output: &str, may_end_mid_row: bool) -> &str {
    if may_end_mid_row && !output.ends_with('\n') {
        output
            .rsplit_once('\n')
            .map_or("", |(complete, _)| complete)
    } else {
        output
    }
}

fn parse_pidstat(output: &str) -> Vec<Process> {
    let mut layout = None;
    let mut processes = BTreeMap::<NonZeroU32, Process>::new();
    for line in output.lines() {
        let Some((clock, rest)) = line.trim_start().split_once(char::is_whitespace) else {
            continue;
        };
        if !clock.contains(':')
            || !clock
                .chars()
                .all(|character| character.is_ascii_digit() || character == ':')
        {
            // Average rows are not part of the peak interval measurement.
            continue;
        }
        let fields: Vec<_> = rest.split_ascii_whitespace().collect();
        let fields = if matches!(fields.first(), Some(&("AM" | "PM"))) {
            &fields[1..]
        } else {
            &fields
        };
        if fields.first() == Some(&"UID") && fields.get(1) == Some(&"PID") {
            layout = fields
                .iter()
                .position(|&field| field == "%CPU")
                .zip(fields.iter().position(|&field| field == "Command"));
            continue;
        }
        let Some((cpu_index, command_index)) = layout else {
            continue;
        };
        if let Some(command) = fields.get(command_index..) {
            record(
                &mut processes,
                fields.get(1).copied(),
                fields.get(cpu_index).copied(),
                command,
                Source::Interval,
            );
        }
    }
    rank(processes)
}

fn parse_top(output: &str) -> Vec<Process> {
    let mut lines = output
        .lines()
        .map(|line| line.split_ascii_whitespace().collect::<Vec<_>>());
    let Some((columns, pid, cpu, command)) = lines.find_map(|fields| {
        let index = |name: &str| fields.iter().position(|field| *field == name);
        Some((
            fields.len(),
            index("PID")?,
            index("%CPU")?,
            index("COMMAND")?,
        ))
    }) else {
        return Vec::new();
    };
    let mut processes = BTreeMap::new();
    for fields in lines.take_while(|fields| !fields.is_empty()) {
        let Some(extra) = fields.len().checked_sub(columns) else {
            continue;
        };
        // COMMAND can span multiple words before the next fixed-width field.
        let field = |index: usize| {
            fields
                .get(index + if index > command { extra } else { 0 })
                .copied()
        };
        record(
            &mut processes,
            field(pid),
            field(cpu),
            &fields[command..=command + extra],
            Source::Snapshot,
        );
    }
    rank(processes)
}

fn record(
    processes: &mut BTreeMap<NonZeroU32, Process>,
    pid: Option<&str>,
    cpu: Option<&str>,
    command: &[&str],
    source: Source,
) {
    let Some(pid) = pid
        .and_then(|value| value.parse::<NonZeroU32>().ok())
        .filter(|pid| i32::try_from(pid.get()).is_ok())
    else {
        return;
    };
    let Some(cpu) = cpu
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
    else {
        return;
    };
    if !command.is_empty() && processes.get(&pid).is_none_or(|process| cpu > process.cpu) {
        processes.insert(
            pid,
            Process {
                pid,
                command: command.join(" "),
                cpu,
                source,
            },
        );
    }
}

fn rank(processes: BTreeMap<NonZeroU32, Process>) -> Vec<Process> {
    let mut processes: Vec<_> = processes.into_values().collect();
    processes.sort_unstable_by(|left, right| {
        right
            .cpu
            .total_cmp(&left.cpu)
            .then(left.pid.cmp(&right.pid))
    });
    processes.truncate(5);
    processes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_peaks_by_pid_from_interval_rows_and_excludes_averages() {
        let processes = parse_pidstat(concat!(
            "Linux 6.8 x86_64 (8 CPU)\n",
            "06:30:00 AM UID PID %usr %system %guest %wait %CPU CPU Command\n",
            "06:30:01 AM 1000 42 60.0 20.0 0.0 0.0 80.0 3 two words\n",
            "06:30:01 AM 1000 80 3.0 0.0 0.0 0.0 3.0 3 helper\n",
            "06:30:02 AM 1000 42 90.0 20.0 0.0 0.0 110.0 3 two words\n",
            "06:30:02 AM 1000 99 0.0 0.0 0.0 0.0 0.0 3 idle\n",
            "Average: 1000 80 200.0 0.0 0.0 0.0 200.0 - helper\n",
        ));
        assert_eq!(processes.len(), 2);
        assert_eq!(processes[0].pid.get(), 42);
        assert_eq!(processes[0].command, "two words");
        assert_eq!(processes[0].cpu_label(), "110.0% peak CPU");
        assert_eq!(processes[1].cpu_label(), "3.0% peak CPU");
    }

    #[test]
    fn ranks_observed_top_rows_and_skips_a_truncated_command() {
        let output = concat!(
            "top - 09:40:00 up 13 days,  1 user\n",
            "%Cpu(s): 50.0 us, 50.0 id\n",
            "\n",
            "COMMAND USER %CPU PID %MEM\n",
            "two words root 120.3 42 1.0\n",
            "idle root 0.0 55 1.0\n",
            "bad root NaN 56 1.0\n",
            "too large root 200.0 4294967295 1.0\n",
            "helper test 9.1 80 0.1\n",
            "truncated test 500.0 33 0.1",
        );
        let processes = parse_top(complete_lines(output, true));
        assert_eq!(processes.len(), 2);
        assert_eq!(processes[0].pid.get(), 42);
        assert_eq!(processes[0].command, "two words");
        assert_eq!(processes[0].cpu_label(), "120.3% CPU in top snapshot");
        assert_eq!(processes[1].pid.get(), 80);
        assert_eq!(parse_top(complete_lines(output, false))[0].pid.get(), 33);
    }
}
