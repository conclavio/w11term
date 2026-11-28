use super::*;
use serde::Serializer;
use std::io::{self, Write};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SummarySort {
    #[default]
    Total,
    Slowest,
    Calls,
    Errors,
}

impl SummarySort {
    pub const CHOICES: [(&'static str, &'static str, Self); 4] = [
        ("Total recorded time", "total", Self::Total),
        ("Slowest call", "slowest", Self::Slowest),
        ("Call count", "calls", Self::Calls),
        ("Error count", "errors", Self::Errors),
    ];
}

pub struct Timing {
    pub calls: usize,
    pub total_ns: u128,
    pub max_ns: u64,
    pub slowest_event: usize,
}

pub struct CallStats {
    pub syscall: Arc<str>,
    pub calls: usize,
    pub errors: usize,
    pub incomplete_calls: usize,
    pub timing: Option<Timing>,
}

pub struct Summary {
    pub pid: Option<u32>,
    pub calls: usize,
    pub errors: usize,
    pub incomplete_calls: usize,
    pub other_events: usize,
    pub timed_calls: usize,
    pub total_ns: u128,
    pub rows: Vec<CallStats>,
}

impl Trace {
    pub fn summarize(&self, pid: Option<u32>) -> Summary {
        let mut summary = Summary {
            pid,
            calls: 0,
            errors: 0,
            incomplete_calls: 0,
            other_events: 0,
            timed_calls: 0,
            total_ns: 0,
            rows: Vec::new(),
        };
        let mut by_name = HashMap::new();
        for (index, event) in self.events.iter().enumerate() {
            if pid.is_some() && event.pid != pid {
                continue;
            }
            if !matches!(
                event.outcome,
                Outcome::Success | Outcome::ZeroRead | Outcome::Error | Outcome::Incomplete
            ) || !syscall_name(&event.name)
                || &*event.name == "Signal"
            {
                summary.other_events += 1;
                continue;
            }
            let row = by_name
                .entry(event.name.as_ref())
                .or_insert_with(|| CallStats {
                    syscall: event.name.clone(),
                    calls: 0,
                    errors: 0,
                    incomplete_calls: 0,
                    timing: None,
                });
            summary.calls += 1;
            row.calls += 1;
            if event.outcome == Outcome::Incomplete {
                row.incomplete_calls += 1;
                summary.incomplete_calls += 1;
                continue;
            }
            if event.outcome == Outcome::Error {
                row.errors += 1;
                summary.errors += 1;
            }
            if let Some(ns) = event.duration {
                let timing = row.timing.get_or_insert(Timing {
                    calls: 0,
                    total_ns: 0,
                    max_ns: ns,
                    slowest_event: index,
                });
                timing.calls += 1;
                timing.total_ns += u128::from(ns);
                if ns > timing.max_ns {
                    timing.max_ns = ns;
                    timing.slowest_event = index;
                }
                summary.timed_calls += 1;
                summary.total_ns += u128::from(ns);
            }
        }
        summary.rows = by_name.into_values().collect();
        summary
    }
}

impl Summary {
    pub fn order(&self, sort: SummarySort) -> Vec<usize> {
        let key = |row: &CallStats| match sort {
            SummarySort::Total => row.timing.as_ref().map(|time| time.total_ns),
            SummarySort::Slowest => row.timing.as_ref().map(|time| u128::from(time.max_ns)),
            SummarySort::Calls => Some(row.calls as u128),
            SummarySort::Errors => Some(row.errors as u128),
        };
        let mut order: Vec<_> = (0..self.rows.len()).collect();
        order.sort_unstable_by(|&a, &b| {
            let a = &self.rows[a];
            let b = &self.rows[b];
            key(b).cmp(&key(a)).then_with(|| a.syscall.cmp(&b.syscall))
        });
        order
    }

    pub fn write_json(
        &self,
        trace: &Trace,
        sort: SummarySort,
        output: &mut dyn Write,
    ) -> io::Result<()> {
        serde_json::to_writer(
            &mut *output,
            &Report {
                schema_version: 1,
                file_names: trace
                    .sources
                    .iter()
                    .map(|source| {
                        source
                            .path
                            .file_name()
                            .unwrap_or(source.path.as_os_str())
                            .to_string_lossy()
                    })
                    .collect(),
                pid: self.pid,
                sort,
                calls: self.calls,
                errors: self.errors,
                incomplete_calls: self.incomplete_calls,
                other_events: self.other_events,
                timed_calls: self.timed_calls,
                total_duration_ns: (self.timed_calls != 0).then(|| self.total_ns.to_string()),
                syscalls: Rows {
                    summary: self,
                    trace,
                    order: self.order(sort),
                },
            },
        )?;
        output.write_all(b"\n")
    }
}

#[derive(Serialize)]
struct Report<'a> {
    schema_version: u8,
    file_names: Vec<std::borrow::Cow<'a, str>>,
    pid: Option<u32>,
    sort: SummarySort,
    calls: usize,
    errors: usize,
    incomplete_calls: usize,
    other_events: usize,
    timed_calls: usize,
    total_duration_ns: Option<String>,
    syscalls: Rows<'a>,
}

struct Rows<'a> {
    summary: &'a Summary,
    trace: &'a Trace,
    order: Vec<usize>,
}

impl Serialize for Rows<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.order.iter().map(|&index| {
            let row = &self.summary.rows[index];
            let time = row.timing.as_ref();
            Row {
                syscall: &row.syscall,
                calls: row.calls,
                errors: row.errors,
                incomplete_calls: row.incomplete_calls,
                timed_calls: time.map_or(0, |time| time.calls),
                total_duration_ns: time.map(|time| time.total_ns.to_string()),
                max_duration_ns: time.map(|time| time.max_ns.to_string()),
                slowest: time.map(|time| {
                    let event = &self.trace.events[time.slowest_event];
                    Evidence {
                        pid: event.pid,
                        outcome: event.outcome,
                        errno: event.error.as_deref(),
                        sources: event
                            .originals()
                            .map(|raw| SourceLine {
                                file: raw.source_number(),
                                line: raw.line,
                            })
                            .collect(),
                    }
                }),
            }
        }))
    }
}

#[derive(Serialize)]
struct Row<'a> {
    syscall: &'a str,
    calls: usize,
    errors: usize,
    incomplete_calls: usize,
    timed_calls: usize,
    total_duration_ns: Option<String>,
    max_duration_ns: Option<String>,
    slowest: Option<Evidence<'a>>,
}

#[derive(Serialize)]
struct Evidence<'a> {
    pid: Option<u32>,
    outcome: Outcome,
    errno: Option<&'a str>,
    sources: Vec<SourceLine>,
}

#[derive(Serialize)]
struct SourceLine {
    file: usize,
    line: u32,
}
