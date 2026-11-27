use std::{collections::BTreeMap, num::NonZeroU32};
use w11term::ssh::TriageReport;

pub(super) struct Process {
    pub pid: NonZeroU32,
    pub command: String,
    pub peak_cpu: f64,
}

pub(super) fn busy_processes(report: &TriageReport) -> Vec<Process> {
    let Some(sample) = report
        .collection
        .samples
        .iter()
        .find(|sample| sample.id == "pidstat")
    else {
        return Vec::new();
    };
    let output = &sample.stdout;
    let complete_lines = if output.at_byte_limit && !output.text.ends_with('\n') {
        output
            .text
            .rsplit_once('\n')
            .map_or("", |(complete, _)| complete)
    } else {
        &output.text
    };
    parse(complete_lines)
}

fn parse(output: &str) -> Vec<Process> {
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
        let Some(pid) = fields
            .get(1)
            .and_then(|value| value.parse::<NonZeroU32>().ok())
            .filter(|pid| i32::try_from(pid.get()).is_ok())
        else {
            continue;
        };
        let Some(peak_cpu) = fields
            .get(cpu_index)
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value > 0.0)
        else {
            continue;
        };
        if fields.len() <= command_index
            || processes
                .get(&pid)
                .is_some_and(|row| row.peak_cpu >= peak_cpu)
        {
            continue;
        }
        processes.insert(
            pid,
            Process {
                pid,
                command: fields[command_index..].join(" "),
                peak_cpu,
            },
        );
    }
    let mut processes: Vec<_> = processes.into_values().collect();
    processes.sort_unstable_by(|left, right| {
        right
            .peak_cpu
            .total_cmp(&left.peak_cpu)
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
        let processes = parse(concat!(
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
        assert!((processes[0].peak_cpu - 110.0).abs() < f64::EPSILON);
        assert!((processes[1].peak_cpu - 3.0).abs() < f64::EPSILON);
    }
}
