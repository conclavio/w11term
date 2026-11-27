//! Saved strace parsing, descriptor lifetimes and comparison for the viewer and CLI.

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::Read,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

mod comparison;
mod descriptors;
pub use comparison::{
    Comparison, Kind as ComparisonKind, Row as ComparisonRow, RowMatch as ComparisonMatch,
    TimingAnchor as ComparisonTimingAnchor,
};
pub use descriptors::{DescriptorReport, Lifetime, Step};

pub const MAX_QUERY_CHARS: usize = 256;

pub fn duration_delta_ms(working_ns: u64, failing_ns: u64) -> f64 {
    (i128::from(failing_ns) - i128::from(working_ns)) as f64 / 1_000_000.0
}

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_RECORDS: usize = 1_000_000;
const MAX_FILES: usize = 256;
const SLOW_NS: u64 = 10_000_000;
const CLOCK_DAY_NS: i64 = 24 * 60 * 60 * 1_000_000_000;
const CLOCK_HALF_DAY_NS: i64 = CLOCK_DAY_NS / 2;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Errors,
    Lifecycle,
    Slow,
    Incomplete,
}

impl Filter {
    pub const CHOICES: [(&'static str, Self); 5] = [
        ("All calls and events", Self::All),
        ("Errors and abnormal exits", Self::Errors),
        ("File and process events", Self::Lifecycle),
        ("Calls of at least 10 ms", Self::Slow),
        ("Incomplete or unrecognized", Self::Incomplete),
    ];
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    ZeroRead,
    Error,
    Signal,
    Exit,
    AbnormalExit,
    Incomplete,
    Other,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::Success => "OK",
            Self::ZeroRead => "Read 0 bytes",
            Self::Error => "Error",
            Self::Signal => "Signal",
            Self::Exit => "Exited",
            Self::AbnormalExit => "Abnormal exit",
            Self::Incomplete => "Incomplete",
            Self::Other => "Unrecognized",
        }
    }
}

#[derive(Clone, Copy)]
pub struct Raw {
    source: u16,
    pub line: u32,
    start: u32,
    len: u32,
}

impl Raw {
    /// The one-based position of this record's file in the trace's source list.
    pub fn source_number(self) -> usize {
        usize::from(self.source) + 1
    }
}

#[derive(Clone, Copy)]
struct Stamp {
    ns: u64,
    basis: TimeBasis,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TimeBasis {
    Epoch,
    Clock,
    Delta,
    Relative,
}

enum RelativeClock {
    Start,
    Elapsed(u64),
    Unavailable,
}

impl RelativeClock {
    fn advance(&mut self, stamp: &mut Option<Stamp>, body: &str) {
        if let Some(Stamp {
            ns,
            basis: TimeBasis::Delta,
        }) = *stamp
        {
            let elapsed = match *self {
                Self::Start => Some(0),
                Self::Elapsed(elapsed) => elapsed.checked_add(ns),
                Self::Unavailable => None,
            };
            if let Some(ns) = elapsed {
                *self = Self::Elapsed(ns);
                *stamp = Some(Stamp {
                    ns,
                    basis: TimeBasis::Relative,
                });
            } else {
                *self = Self::Unavailable;
            }
        } else if stamp.is_some() || (!body.is_empty() && !body.starts_with("> ")) {
            // Untimed records can hide an interval. Blank lines and stack
            // frames are auxiliary output and carry no delta.
            *self = Self::Unavailable;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeOrder {
    File,
    Epoch,
    Clock { anchor: i64, earliest: i64 },
}

impl MergeOrder {
    fn sort_time(self, stamp: Stamp) -> Option<u64> {
        match self {
            Self::File => None,
            Self::Epoch => Some(stamp.ns),
            Self::Clock { anchor, earliest } if stamp.basis == TimeBasis::Clock => {
                u64::try_from(ClockMerge::offset(anchor, stamp.ns as i64) - earliest).ok()
            }
            Self::Clock { .. } => None,
        }
    }
}

#[derive(Default)]
struct ClockMerge {
    anchor: Option<i64>,
    earliest: i64,
    latest: i64,
    observed_rollover: bool,
}

impl ClockMerge {
    fn offset(anchor: i64, clock: i64) -> i64 {
        (clock - anchor + CLOCK_HALF_DAY_NS).rem_euclid(CLOCK_DAY_NS) - CLOCK_HALF_DAY_NS
    }

    fn observe(&mut self, stamp: Option<Stamp>, previous: &mut Option<(i64, i64)>) -> bool {
        let Some(Stamp {
            ns,
            basis: TimeBasis::Clock,
        }) = stamp
        else {
            return false;
        };
        let clock = ns as i64;
        let anchor = *self.anchor.get_or_insert(clock);
        let offset = Self::offset(anchor, clock);
        if let Some((last_clock, last_offset)) = *previous {
            if offset < last_offset {
                return false;
            }
            self.observed_rollover |= clock < last_clock;
        }
        *previous = Some((clock, offset));
        self.earliest = self.earliest.min(offset);
        self.latest = self.latest.max(offset);
        self.latest - self.earliest < CLOCK_HALF_DAY_NS
    }

    fn timeline(self) -> Option<MergeOrder> {
        let anchor = self.anchor?;
        // A short arc through midnight could also be two distant times on
        // the same day. Require a rollover in the records of one source.
        let crosses_midnight = anchor + self.earliest < 0 || anchor + self.latest >= CLOCK_DAY_NS;
        (!crosses_midnight || self.observed_rollover).then_some(MergeOrder::Clock {
            anchor,
            earliest: self.earliest,
        })
    }
}

pub struct Event {
    pub pid: Option<u32>,
    pub name: Arc<str>,
    pub resource: Arc<str>,
    pub outcome: Outcome,
    pub error: Option<Arc<str>>,
    pub later_success: bool,
    stamp: Option<Stamp>,
    pub duration: Option<u64>,
    first: Raw,
    last: Option<Raw>,
}

impl Event {
    pub fn originals(&self) -> impl Iterator<Item = Raw> {
        std::iter::once(self.first).chain(self.last)
    }

    fn matches(&self, filter: Filter) -> bool {
        match filter {
            Filter::All => true,
            Filter::Errors => matches!(self.outcome, Outcome::Error | Outcome::AbnormalExit),
            Filter::Lifecycle => {
                matches!(self.outcome, Outcome::Exit | Outcome::AbnormalExit)
                    || matches!(
                        &*self.name,
                        "open"
                            | "openat"
                            | "openat2"
                            | "creat"
                            | "close"
                            | "close_range"
                            | "dup"
                            | "dup2"
                            | "dup3"
                            | "fork"
                            | "vfork"
                            | "clone"
                            | "clone3"
                            | "execve"
                            | "execveat"
                            | "exit"
                            | "exit_group"
                            | "chdir"
                            | "fchdir"
                            | "pipe"
                            | "pipe2"
                            | "socket"
                            | "socketpair"
                            | "accept"
                            | "accept4"
                    )
            }
            Filter::Slow => self.duration.is_some_and(|ns| ns >= SLOW_NS),
            Filter::Incomplete => matches!(self.outcome, Outcome::Incomplete | Outcome::Other),
        }
    }

    fn like(&self, other: &Self) -> bool {
        self.pid == other.pid
            && self.first.source == other.first.source
            && self.name == other.name
            && self.resource == other.resource
            && self.outcome == other.outcome
            && self.error == other.error
            && self.later_success == other.later_success
            && (!self.resource.is_empty()
                || matches!(
                    &*self.name,
                    "getpid" | "getppid" | "gettid" | "getuid" | "getgid" | "geteuid" | "getegid"
                ))
            && self.matches(Filter::Slow) == other.matches(Filter::Slow)
            && matches!(
                self.outcome,
                Outcome::Success | Outcome::ZeroRead | Outcome::Error
            )
    }
}

pub struct Group {
    pub range: Range<usize>,
    pub duration: u64,
    pub timed: usize,
}

struct Source {
    path: PathBuf,
    pid: Option<u32>,
    bytes: Vec<u8>,
}

pub struct Trace {
    sources: Vec<Source>,
    events: Vec<Event>,
    groups: Vec<Group>,
    merge_order: MergeOrder,
    pub incomplete: usize,
    pub errors: usize,
}

impl Trace {
    pub fn open(paths: Vec<PathBuf>) -> Result<Self> {
        let hints = file_pids(&paths)?;
        let mut sources = Vec::new();
        let mut remaining = MAX_BYTES;
        for (path, pid) in paths.into_iter().zip(hints) {
            let file = File::open(&path).with_context(|| format!("Opening {}", path.display()))?;
            let metadata = file.metadata()?;
            ensure!(metadata.is_file(), "Choose regular trace files");
            ensure!(
                metadata.len() <= remaining as u64,
                "Trace selection exceeds 64 MiB"
            );
            let mut bytes = Vec::new();
            file.take(remaining as u64 + 1).read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= remaining, "Trace selection exceeds 64 MiB");
            remaining -= bytes.len();
            sources.push(Source { path, pid, bytes });
        }
        Self::parse(sources)
    }

    fn parse(sources: Vec<Source>) -> Result<Self> {
        let mut events: Vec<Event> = Vec::new();
        let mut pending = HashMap::<(u16, Option<u32>), usize>::new();
        let mut strings = HashSet::<Arc<str>>::new();
        let mut records = 0;
        let mut clock = (sources.len() > 1).then(ClockMerge::default);
        for (source, input) in sources.iter().enumerate() {
            let mut start = 0;
            let mut previous_clock = None;
            let mut relative = if sources.len() == 1 && input.pid.is_none() {
                RelativeClock::Start
            } else {
                RelativeClock::Unavailable
            };
            for (line, bytes) in input
                .bytes
                .split_inclusive(|&byte| byte == b'\n')
                .enumerate()
            {
                records += 1;
                ensure!(
                    records <= MAX_RECORDS,
                    "Trace selection exceeds {MAX_RECORDS} records"
                );
                let raw = Raw {
                    source: source as u16,
                    line: line as u32 + 1,
                    start: start as u32,
                    len: bytes.len() as u32,
                };
                start += bytes.len();
                let line = String::from_utf8_lossy(bytes);
                let (pid, mut stamp, body) = prefix(&line, input.pid);
                if let Some(merge) = &mut clock
                    && !merge.observe(stamp, &mut previous_clock)
                {
                    clock = None;
                }
                // A resumed record also advances the trace-wide clock, even
                // though the paired call keeps the time of its first record.
                relative.advance(&mut stamp, body);
                let key = (raw.source, pid);
                if let Some((name, rest)) = resumed(body)
                    && let Some(&index) = pending.get(&key)
                    && &*events[index].name == name
                {
                    pending.remove(&key);
                    let event = &mut events[index];
                    let first = String::from_utf8_lossy(slice(&sources, event.first));
                    let (_, _, first) = prefix(&first, input.pid);
                    let first = first
                        .strip_suffix("<unfinished ...>")
                        .unwrap_or(first)
                        .trim_end();
                    let combined = format!("{first}{rest}");
                    let parsed = classify(&combined);
                    update(event, parsed, &mut strings);
                    if !bytes.ends_with(b"\n") {
                        event.outcome = Outcome::Incomplete;
                    }
                    event.last = Some(raw);
                    continue;
                }
                let parsed = classify(body);
                if parsed.unfinished {
                    pending.insert(key, events.len());
                }
                let mut event = Event {
                    pid,
                    name: Arc::default(),
                    resource: Arc::default(),
                    outcome: Outcome::Other,
                    error: None,
                    later_success: false,
                    stamp,
                    duration: None,
                    first: raw,
                    last: None,
                };
                update(&mut event, parsed, &mut strings);
                if !bytes.ends_with(b"\n") && !matches!(event.outcome, Outcome::Other) {
                    event.outcome = Outcome::Incomplete;
                }
                events.push(event);
            }
        }
        ensure!(
            !events.is_empty(),
            "The selected files contain no trace records"
        );
        let epoch = sources.len() > 1
            && events.iter().all(|event| {
                event
                    .stamp
                    .is_some_and(|stamp| stamp.basis == TimeBasis::Epoch)
            });
        let merge_order = if epoch {
            MergeOrder::Epoch
        } else {
            clock
                .and_then(ClockMerge::timeline)
                .unwrap_or(MergeOrder::File)
        };
        if matches!(merge_order, MergeOrder::Clock { .. }) {
            for event in &mut events {
                let stamp = event.stamp.as_mut().expect("validated clock timestamp");
                stamp.ns = merge_order
                    .sort_time(*stamp)
                    .expect("validated clock timeline");
            }
        }
        if merge_order != MergeOrder::File {
            events.sort_by_key(|event| event.stamp.unwrap().ns);
        }
        mark_retries(&mut events);
        let groups = group(&events);
        let incomplete = events
            .iter()
            .filter(|event| event.matches(Filter::Incomplete))
            .count();
        let errors = events
            .iter()
            .filter(|event| event.matches(Filter::Errors))
            .count();
        Ok(Self {
            sources,
            events,
            groups,
            merge_order,
            incomplete,
            errors,
        })
    }

    pub fn first_path(&self) -> &Path {
        &self.sources[0].path
    }

    pub fn paths(&self) -> Vec<PathBuf> {
        self.sources
            .iter()
            .map(|source| source.path.clone())
            .collect()
    }

    pub fn file_count(&self) -> usize {
        self.sources.len()
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    pub fn merge_order(&self) -> MergeOrder {
        self.merge_order
    }

    fn merged_by_time(&self) -> bool {
        self.merge_order != MergeOrder::File
    }

    pub fn original(&self, raw: Raw) -> (&Path, &[u8]) {
        (
            &self.sources[usize::from(raw.source)].path,
            slice(&self.sources, raw),
        )
    }

    fn with_call<T>(&self, event: &Event, read: impl FnOnce(&str, &str) -> T) -> Option<T> {
        let line = String::from_utf8_lossy(slice(&self.sources, event.first));
        let (_, _, body) = prefix(&line, None);
        let paired = event.last.and_then(|raw| {
            let row = String::from_utf8_lossy(slice(&self.sources, raw));
            let (_, _, row) = prefix(&row, None);
            let (_, continuation) = resumed(row)?;
            Some(format!(
                "{}{continuation}",
                body.strip_suffix("<unfinished ...>")?.trim_end()
            ))
        });
        let (_, arguments) = paired.as_deref().unwrap_or(body).split_once('(')?;
        let (arguments, result) = call_end(arguments)?;
        Some(read(arguments, result.trim_start()))
    }

    pub fn relative_start(&self, index: usize) -> Option<u64> {
        if self.sources.len() > 1 && !self.merged_by_time() {
            return None;
        }
        let current = self.events.get(index)?.stamp?;
        if current.basis == TimeBasis::Relative {
            return Some(current.ns);
        }
        let start = self.events.first()?.stamp?;
        if start.basis == TimeBasis::Delta || current.basis != start.basis {
            return None;
        }
        current.ns.checked_sub(start.ns)
    }

    fn offset_from_event(&self, reference: usize, index: usize) -> Option<i128> {
        let reference_stamp = self.events.get(reference)?.stamp?;
        let current = self.events.get(index)?.stamp?;
        if current.basis != reference_stamp.basis {
            return None;
        }
        let (reference, current) = match current.basis {
            TimeBasis::Epoch | TimeBasis::Relative => (reference_stamp.ns, current.ns),
            // Use the existing run clock checks for time-of-day records.
            TimeBasis::Clock => (self.relative_start(reference)?, self.relative_start(index)?),
            TimeBasis::Delta => return None,
        };
        Some(i128::from(current) - i128::from(reference))
    }

    pub fn filtered(&self, filter: Filter, query: &str) -> Vec<usize> {
        let query = query.trim().to_lowercase();
        let pid = query.parse::<u32>().ok();
        self.groups
            .iter()
            .enumerate()
            .filter_map(|(index, group)| {
                let event = &self.events[group.range.start];
                (event.matches(filter)
                    && (query.is_empty()
                        || pid.is_some_and(|pid| event.pid == Some(pid))
                        || [
                            &*event.name,
                            &*event.resource,
                            event.error.as_deref().unwrap_or(""),
                        ]
                        .iter()
                        .any(|field| field.to_lowercase().contains(&query))))
                .then_some(index)
            })
            .collect()
    }
}

fn slice(sources: &[Source], raw: Raw) -> &[u8] {
    let start = raw.start as usize;
    &sources[usize::from(raw.source)].bytes[start..start + raw.len as usize]
}

fn pid_suffix(path: &Path) -> Option<(&str, u32)> {
    let (basename, suffix) = path.file_name()?.to_str()?.rsplit_once('.')?;
    let pid: u32 = suffix.parse().ok()?;
    (pid > 0 && !basename.is_empty()).then_some((basename, pid))
}

/// Validate the input file set and return each split trace's PID, if present.
pub fn file_pids(paths: &[PathBuf]) -> Result<Vec<Option<u32>>> {
    ensure!(
        (1..=MAX_FILES).contains(&paths.len()),
        "Choose one trace or up to {MAX_FILES} per-process files"
    );
    if let [path] = paths {
        return Ok(vec![pid_suffix(path).map(|(_, pid)| pid)]);
    }
    let (prefix, _) =
        pid_suffix(&paths[0]).context("Select one combined trace or matching NAME.PID files")?;
    let mut pids = HashSet::new();
    paths
        .iter()
        .map(|path| {
            let (name, pid) =
                pid_suffix(path).context("Each per-process trace must end in its numeric PID")?;
            ensure!(
                name == prefix && path.parent() == paths[0].parent() && pids.insert(pid),
                "Select distinct NAME.PID files from the same trace and folder"
            );
            Ok(Some(pid))
        })
        .collect()
}

fn prefix(line: &str, fallback: Option<u32>) -> (Option<u32>, Option<Stamp>, &str) {
    let input = line.trim_end();
    let input = input.trim_start().strip_prefix('\u{feff}').unwrap_or(input);
    let mut line = input.trim_start();
    let mut pid = fallback;
    let mut integral_delta = false;
    if let Some((number, rest)) = line
        .strip_prefix("[pid ")
        .and_then(|line| line.split_once(']'))
    {
        pid = number.trim().parse().ok().or(pid);
        line = rest.trim_start();
        integral_delta = true;
    } else if let Some((word, rest)) = line.split_once(char::is_whitespace)
        && word.len() < 9
        && let Ok(number) = word.parse::<u32>()
    {
        // Plain PIDs are printed at the left margin. Standalone integer
        // relative deltas use strace's six-character, space-padded field.
        let padding = &input[..input.len() - line.len()];
        if padding.is_empty()
            || padding.len() + word.len() != 6
            || !padding.bytes().all(|byte| byte == b' ')
        {
            pid = Some(number);
            line = rest.trim_start();
        }
        integral_delta = true;
    }
    if let Some((word, rest)) = line.split_once(char::is_whitespace)
        && let Some(stamp) = parse_stamp(word, integral_delta)
    {
        let body = rest.trim_start();
        let body = if stamp.basis == TimeBasis::Delta {
            body
        } else {
            body.strip_prefix("(+")
                .and_then(|text| text.split_once(')'))
                .filter(|(delta, _)| nanos(delta.trim()).is_some())
                .map_or(body, |(_, rest)| rest.trim_start())
        };
        return (pid, Some(stamp), body);
    }
    (pid, None, line)
}

fn parse_stamp(token: &str, integral_delta: bool) -> Option<Stamp> {
    if token.contains(':') {
        let (hours, rest) = token.split_once(':')?;
        let (minutes, seconds) = rest.split_once(':')?;
        let hours: u64 = hours.parse().ok()?;
        let minutes: u64 = minutes.parse().ok()?;
        let ns = nanos(seconds)?;
        if hours >= 24 || minutes >= 60 || ns >= 60_000_000_000 {
            return None;
        }
        return Some(Stamp {
            ns: (hours * 3600 + minutes * 60) * 1_000_000_000 + ns,
            basis: TimeBasis::Clock,
        });
    }
    let (seconds, fractional) = token.split_once('.').unwrap_or((token, ""));
    let epoch = seconds.len() >= 9;
    if !epoch && !integral_delta && fractional.is_empty() {
        return None;
    }
    Some(Stamp {
        ns: nanos(token)?,
        basis: if epoch {
            TimeBasis::Epoch
        } else {
            TimeBasis::Delta
        },
    })
}

fn nanos(value: &str) -> Option<u64> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let whole = whole.parse::<u64>().ok()?.checked_mul(1_000_000_000)?;
    if fraction.is_empty() {
        return Some(whole);
    }
    if fraction.len() > 9 || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    whole.checked_add(fraction.parse::<u64>().ok()? * 10u64.pow(9 - fraction.len() as u32))
}

fn resumed(body: &str) -> Option<(&str, &str)> {
    body.strip_prefix("<... ")?.split_once(" resumed>")
}

struct Parsed<'a> {
    name: &'a str,
    resource: String,
    outcome: Outcome,
    error: Option<&'a str>,
    duration: Option<u64>,
    unfinished: bool,
}

fn classify(body: &str) -> Parsed<'_> {
    let mut parsed = Parsed {
        name: "Unknown record",
        resource: String::new(),
        outcome: Outcome::Other,
        error: None,
        duration: None,
        unfinished: false,
    };
    if let Some((name, _)) = resumed(body).filter(|(name, _)| syscall_name(name)) {
        parsed.name = name;
        parsed.outcome = Outcome::Incomplete;
        return parsed;
    }
    if let Some(status) = body
        .strip_prefix("+++ ")
        .and_then(|body| body.strip_suffix(" +++"))
        .filter(|status| status.len() <= 128)
    {
        parsed.name = "Process exit";
        parsed.resource = status.into();
        parsed.outcome = if status == "exited with 0" {
            Outcome::Exit
        } else {
            Outcome::AbnormalExit
        };
        return parsed;
    }
    if let Some(signal) = body
        .strip_prefix("--- SIG")
        .and_then(|signal| signal.split_ascii_whitespace().next())
        .filter(|signal| signal.len() <= 32)
    {
        parsed.name = "Signal";
        parsed.resource = format!("SIG{signal}");
        parsed.outcome = Outcome::Signal;
        return parsed;
    }
    let Some((name, tail)) = body.split_once('(') else {
        return parsed;
    };
    if !syscall_name(name) {
        return parsed;
    }
    parsed.name = name;
    parsed.outcome = Outcome::Incomplete;
    if let Some(arguments) = tail.strip_suffix("<unfinished ...>") {
        parsed.unfinished = true;
        parsed.resource = resource(name, arguments.trim_end());
        return parsed;
    }
    let Some((arguments, result)) = call_end(tail) else {
        parsed.resource = resource(name, tail);
        return parsed;
    };
    parsed.resource = resource(name, arguments);
    let result = result.trim_start();
    if let Some((_, duration)) = result
        .rsplit_once(" <")
        .and_then(|(a, b)| Some((a, b.strip_suffix('>')?)))
    {
        parsed.duration = nanos(duration);
    }
    let mut words = result.split_ascii_whitespace();
    match words.next() {
        Some("-1") => {
            parsed.error = words.next().filter(|word| errno(word));
            parsed.outcome = if parsed.error.is_some() {
                Outcome::Error
            } else {
                Outcome::Success
            };
        }
        Some("?") | None => parsed.error = words.next().filter(|word| errno(word)),
        Some("0") if matches!(name, "read" | "pread64" | "readv" | "preadv" | "preadv2") => {
            parsed.outcome = Outcome::ZeroRead;
        }
        Some(_) => parsed.outcome = Outcome::Success,
    }
    parsed
}

fn syscall_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 80
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn errno(word: &str) -> bool {
    word.starts_with('E')
        && word.len() > 1
        && word
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

// Both call boundaries and argument separators lie outside C strings and
// nested strace values. A descriptor such as 3<TCP:[127.0.0.1:80->…]> is one value.
fn boundaries(value: &str) -> impl Iterator<Item = (usize, u8)> + '_ {
    let mut depth = 0u32;
    let mut quoted = false;
    let mut escaped = false;
    let mut descriptor = false;
    let mut previous = 0;
    value.bytes().enumerate().filter_map(move |(index, byte)| {
        let previous = std::mem::replace(&mut previous, byte);
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if descriptor {
            if byte == b'>' && previous != b'-' {
                descriptor = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'<' if previous.is_ascii_digit() => descriptor = true,
                b'(' | b'[' | b'{' => depth += 1,
                b',' | b')' if depth == 0 => return Some((index, byte)),
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                _ => (),
            }
        }
        None
    })
}

fn call_end(tail: &str) -> Option<(&str, &str)> {
    let (index, _) = boundaries(tail).find(|&(_, byte)| byte == b')')?;
    Some((
        &tail[..index],
        tail[index + 1..].trim_start().strip_prefix('=')?,
    ))
}

fn argument_values(arguments: &str) -> impl Iterator<Item = &str> {
    let mut start = 0;
    boundaries(arguments)
        .chain(std::iter::once((arguments.len(), b',')))
        .map(move |(index, _)| {
            let arg = arguments.get(start..index).unwrap_or("").trim();
            start = index.saturating_add(1);
            arg
        })
}

fn resource(name: &str, arguments: &str) -> String {
    let mut args = argument_values(arguments);
    let first = args.next().unwrap_or("");
    let second = args.next().unwrap_or("");
    let value = match name {
        "openat" | "openat2" | "newfstatat" | "fstatat64" | "faccessat" | "faccessat2"
        | "unlinkat" | "mkdirat" | "statx" | "execveat" | "readlinkat" | "mknodat" => {
            if second.starts_with("\"/") {
                second.to_owned()
            } else {
                format!("{first}, {second}")
            }
        }
        "rename" | "link" | "symlink" | "connect" | "bind" => format!("{first}, {second}"),
        "renameat" | "renameat2" | "linkat" => {
            format!(
                "{first}, {second} → {}, {}",
                args.next().unwrap_or(""),
                args.next().unwrap_or("")
            )
        }
        "read" | "write" | "readv" | "writev" | "pread64" | "pwrite64" | "preadv" | "pwritev"
        | "close" | "close_range" | "dup" | "dup2" | "dup3" | "fcntl" | "ioctl" | "fstat"
        | "fchdir" | "lseek" | "getdents" | "getdents64" | "recvfrom" | "recvmsg" | "sendto"
        | "sendmsg" | "accept" | "accept4" | "listen" | "shutdown" | "wait4" | "waitid" => {
            first.into()
        }
        _ if first.starts_with('"') => first.into(),
        _ => String::new(),
    };
    // Overlong arguments keep their original evidence, but cannot form groups.
    if value.len() <= 4096 {
        value
    } else {
        String::new()
    }
}

fn intern(value: &str, strings: &mut HashSet<Arc<str>>) -> Arc<str> {
    if let Some(existing) = strings.get(value) {
        return existing.clone();
    }
    let value: Arc<str> = value.into();
    strings.insert(value.clone());
    value
}

fn update(event: &mut Event, parsed: Parsed<'_>, strings: &mut HashSet<Arc<str>>) {
    event.name = intern(parsed.name, strings);
    event.resource = intern(&parsed.resource, strings);
    event.outcome = parsed.outcome;
    event.error = parsed.error.map(|error| intern(error, strings));
    event.duration = parsed.duration;
}

fn mark_retries(events: &mut [Event]) {
    let mut successes: HashMap<_, HashSet<_>> = HashMap::new();
    for event in events.iter_mut().rev() {
        let process = (event.first.source, event.pid);
        if matches!(event.outcome, Outcome::Exit | Outcome::AbnormalExit)
            || (event.outcome == Outcome::Success && matches!(&*event.name, "execve" | "execveat"))
        {
            successes.remove(&process);
            continue;
        }
        if !matches!(
            &*event.name,
            "open"
                | "openat"
                | "openat2"
                | "access"
                | "faccessat"
                | "faccessat2"
                | "stat"
                | "statx"
                | "newfstatat"
                | "lstat"
        ) || !event.resource.starts_with("\"/")
            || !event.resource.ends_with('"')
        {
            continue;
        }
        let key = (event.name.clone(), event.resource.clone());
        if event.outcome == Outcome::Success {
            successes.entry(process).or_default().insert(key);
        } else if matches!(event.error.as_deref(), Some("ENOENT" | "ENOTDIR")) {
            event.later_success = successes
                .get(&process)
                .is_some_and(|seen| seen.contains(&key));
        }
    }
}

fn group(events: &[Event]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for (index, event) in events.iter().enumerate() {
        if let Some(previous) = groups.last_mut()
            && event.like(&events[previous.range.start])
        {
            previous.range.end += 1;
            previous.duration = previous
                .duration
                .saturating_add(event.duration.unwrap_or_default());
            previous.timed += usize::from(event.duration.is_some());
        } else {
            groups.push(Group {
                range: index..index + 1,
                duration: event.duration.unwrap_or_default(),
                timed: usize::from(event.duration.is_some()),
            });
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Trace {
        Trace::parse(vec![Source {
            path: "fixture.strace".into(),
            pid: None,
            bytes: text.as_bytes().into(),
        }])
        .unwrap()
    }

    #[test]
    fn joins_resumed_calls_preserving_both_original_records_and_child_order() {
        let trace = parse(concat!(
            "30 1740000000.000000 read(4</tmp/data(4),copy>, \"first\", 5) = 5 <0.000001>\n",
            "30 1740000000.001000 read(4</tmp/data(4),copy>,  <unfinished ...>\n",
            "31 1740000000.001100 execve(\"/bin/cat\", [\"cat\"], 0x0) = 0 <0.000002>\n",
            "30 1740000000.002000 <... read resumed>\"second\", 6) = 6 <0.000003>\n",
            "30 1740000000.003000 read(4</tmp/other>, \"third\", 5) = 5 <0.001000>\n",
        ));
        assert_eq!(trace.events.len(), 4);
        assert_eq!(trace.groups.len(), 3);
        assert_eq!(trace.groups[0].range, 0..2);
        assert_eq!(trace.groups[0].duration, 4_000);
        assert_eq!(trace.relative_start(2), Some(1_100_000));
        let originals = trace.events[1].originals().collect::<Vec<_>>();
        assert_eq!((originals[0].line, originals[1].line), (2, 4));
        assert!(
            trace
                .original(originals[1])
                .1
                .starts_with(b"30 1740000000.002000 <... read resumed>")
        );
        assert_eq!(trace.events[2].pid, Some(31));
    }

    #[test]
    fn adds_relative_deltas_from_raw_records_even_when_a_call_is_resumed() {
        let trace = parse(concat!(
            "\n",
            "30 0.500000 read(4, <unfinished ...>\n",
            " > /tmp/fixture(main+0x1) [0x10]\n",
            "31 0.001000 close(5) = 0\n",
            "30 0.002000 <... read resumed>\"x\", 1) = 1 <0.010000>\n",
            "30 0.003000 close(4) = 0\n",
            "30 close(8) = 0\n",
            "30 0.004000 close(9) = 0\n",
        ));
        assert_eq!(trace.events[1].name.as_ref(), "read");
        assert_eq!(trace.events[1].last.unwrap().line, 5);
        assert_eq!(trace.events[1].duration, Some(10_000_000));
        assert_eq!(trace.relative_start(1), Some(0));
        assert_eq!(trace.relative_start(2), None);
        assert_eq!(trace.relative_start(3), Some(1_000_000));
        assert_eq!(trace.relative_start(4), Some(6_000_000));
        assert_eq!(trace.relative_start(6), None);
    }

    #[test]
    fn accepts_integer_precision_and_reads_absolute_times_with_relative_suffixes() {
        let relative = parse(concat!(
            "     0 open(\"/tmp/source\", O_RDONLY) = 3\n",
            "     2 close(3) = 0\n",
            "[pid 22]      1 close(4) = 0\n",
        ));
        assert_eq!(relative.events[0].pid, None);
        assert_eq!(relative.events[2].pid, Some(22));
        assert_eq!(relative.relative_start(1), Some(2_000_000_000));
        assert_eq!(relative.relative_start(2), Some(3_000_000_000));

        let absolute = parse(concat!(
            "1740000000 (+     0) open(\"/tmp/source\", O_RDONLY) = 3\n",
            "21 1740000001 (+     3.012345678) close(3) = 0\n",
        ));
        assert_eq!(absolute.events[0].pid, None);
        assert_eq!(absolute.events[0].name.as_ref(), "open");
        assert_eq!(absolute.events[1].name.as_ref(), "close");
        assert_eq!(absolute.relative_start(1), Some(1_000_000_000));
    }

    #[test]
    fn errors_include_later_success_evidence_and_incomplete_records_remain_visible() {
        let trace = parse(concat!(
            "[pid 90] 13:05:00.123000 openat(AT_FDCWD, \"/retry\", O_RDONLY) = -1 ENOENT (No such file or directory) <0.000001>\n",
            "[pid 90] 13:05:00.124000 openat(AT_FDCWD, \"/missing\", O_RDONLY) = -1 ENOENT (No such file or directory) <0.000002>\n",
            "[pid 90] 13:05:00.125000 openat(AT_FDCWD, \"/retry\", O_RDONLY) = 3</retry> <0.020003>\n",
            "[pid 90] 13:05:00.126000 read(3</retry>, \"a) = b\", 6) = 6 <0.000010>\n",
            "[pid 91] 13:05:00.127000 <... read resumed> \"lost\", 4) = 4 <0.000101>\n",
            "[pid 90] 13:05:00.128000 read(3</retry>,  <unfinished ...>\n",
            "[pid 90] 13:05:00.129000 --- SIGTERM {si_signo=SIGTERM} ---\n",
            "[pid 90] 13:05:00.130000 +++ killed by SIGTERM +++\n",
            "3 1740000000.000001 write(1, \"record cut at byte budget",
        ));
        assert!(trace.events[0].later_success);
        assert!(!trace.events[1].later_success);
        assert_eq!(trace.filtered(Filter::Errors, "enoent"), [0, 1]);
        assert_eq!(trace.filtered(Filter::Errors, ""), [0, 1, 7]);
        assert_eq!(trace.filtered(Filter::Slow, "90"), [2]);
        assert_eq!(trace.filtered(Filter::Incomplete, ""), [4, 5, 8]);
        assert_eq!(trace.incomplete, 3);
    }

    #[test]
    fn imports_matching_pid_files_and_merges_epoch_order() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("trace.501");
        let child = dir.path().join("trace.502");
        std::fs::write(&parent, "1740000000.010000 close(5) = 0\n").unwrap();
        std::fs::write(
            &child,
            "1740000000.001000 open(\"/tmp/test\", O_RDONLY) = 5\n",
        )
        .unwrap();
        let trace = Trace::open(vec![parent.clone(), child.clone()]).unwrap();
        assert_eq!(trace.merge_order, MergeOrder::Epoch);
        assert_eq!(trace.events[0].pid, Some(502));
        assert_eq!(trace.relative_start(1), Some(9_000_000));
        assert_eq!(trace.original(trace.events[0].first).0, child);
        std::fs::write(&child, "0.000020 close(5) = 0\n").unwrap();
        let trace = Trace::open(vec![parent.clone(), child]).unwrap();
        assert_eq!(trace.merge_order, MergeOrder::File);
        assert_eq!(trace.events[0].pid, Some(501));
        assert_eq!(trace.relative_start(1), None);
        assert!(Trace::open(vec![parent.clone(), parent]).is_err());
    }

    fn clock_files(first: &str, second: &str) -> Trace {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("clock.501");
        let child = dir.path().join("clock.502");
        std::fs::write(&parent, first).unwrap();
        std::fs::write(&child, second).unwrap();
        Trace::open(vec![parent, child]).unwrap()
    }

    #[test]
    fn merges_short_clock_traces_and_keeps_ties_in_input_order() {
        let trace = clock_files(
            "16:11:01.010000 close(3) = 0\n16:11:01.020000 close(4) = 0\n",
            "16:11:01.000000 open(\"/tmp/source\", O_RDONLY) = 3\n16:11:01.010000 close(3) = 0\n",
        );
        assert!(matches!(trace.merge_order, MergeOrder::Clock { .. }));
        let order: Vec<_> = trace.events.iter().map(|event| event.pid).collect();
        assert_eq!(order, [Some(502), Some(501), Some(502), Some(501)]);
        assert_eq!(trace.relative_start(1), Some(10_000_000));
        assert_eq!(trace.relative_start(2), Some(10_000_000));
        assert_eq!(trace.relative_start(3), Some(20_000_000));
        let (path, raw) = trace.original(trace.events[0].first);
        assert_eq!(path.file_name().unwrap(), "clock.502");
        assert!(raw.starts_with(b"16:11:01.000000 open"));
    }

    #[test]
    fn uses_a_resumed_record_to_prove_a_midnight_rollover() {
        let trace = clock_files(
            "00:00:00.010000 close(3) = 0\n",
            concat!(
                "23:59:59.980000 open(\"/tmp/source\", O_RDONLY) = 3\n",
                "23:59:59.990000 read(3, <unfinished ...>\n",
                "00:00:00.020000 <... read resumed>\"x\", 1) = 1\n",
            ),
        );
        assert!(matches!(trace.merge_order, MergeOrder::Clock { .. }));
        assert_eq!(trace.events.len(), 3);
        assert_eq!(trace.events[0].pid, Some(502));
        assert_eq!(trace.events[1].pid, Some(502));
        assert!(trace.events[1].last.is_some());
        assert_eq!(trace.events[2].pid, Some(501));
        assert_eq!(trace.relative_start(1), Some(10_000_000));
        assert_eq!(trace.relative_start(2), Some(30_000_000));
    }

    #[test]
    fn preserves_input_order_when_clock_dates_or_record_order_are_ambiguous() {
        for (first, second) in [
            ("00:00:00.010 close(3) = 0\n", "23:59:59.990 close(4) = 0\n"),
            ("18:50:00 close(3) = 0\n", "06:50:00 close(4) = 0\n"),
            (
                "16:11:02 close(3) = 0\n16:11:01 close(4) = 0\n",
                "16:11:03 close(5) = 0\n",
            ),
            (
                "16:11:02 close(3) = 0\n",
                "16:11:01 close(4) = 0\nclose(5) = 0\n",
            ),
        ] {
            let trace = clock_files(first, second);
            assert_eq!(trace.merge_order, MergeOrder::File);
            assert_eq!(trace.events[0].pid, Some(501));
            assert_eq!(trace.relative_start(1), None);
        }
    }

    #[test]
    fn relative_deltas_have_no_global_origin_in_per_process_files() {
        let input = "0.000000 open(\"/tmp/source\", O_RDONLY) = 3\n0.010000 close(3) = 0\n";
        let split = clock_files(input, "0.001000 close(4) = 0\n");
        assert_eq!(split.merge_order, MergeOrder::File);
        assert_eq!(split.relative_start(1), None);
        let one_process = Trace::parse(vec![Source {
            path: "relative.34".into(),
            pid: Some(34),
            bytes: input.as_bytes().into(),
        }])
        .unwrap();
        assert_eq!(one_process.events[0].pid, Some(34));
        assert_eq!(one_process.relative_start(1), None);
    }

    #[test]
    fn preserves_raw_non_utf8_and_a_truncated_record() {
        let payload = b"30 1740000000.000000 open(\"/tmp/\xff\", O_RDONLY)";
        let trace = Trace::parse(vec![Source {
            path: "cut".into(),
            pid: None,
            bytes: payload.into(),
        }])
        .unwrap();
        assert!(trace.events[0].outcome == Outcome::Incomplete);
        assert_eq!(trace.original(trace.events[0].first).1, payload);
    }
}
