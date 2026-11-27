use super::{Outcome, Trace, argument_values};
use std::{
    collections::{HashMap, VecDeque},
    ops::Range,
    sync::Arc,
};

const MAX_ROWS: usize = 20_000;
const LOOKAHEAD: usize = 48;
const TIME_FLOOR_NS: u64 = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app::syscall_view) enum Kind {
    Result,
    Count,
    Added,
    Missing,
    Timing,
    Unaligned,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Result => "Changed result",
            Self::Count => "Changed count",
            Self::Added => "Added call",
            Self::Missing => "Missing call",
            Self::Timing => "Changed timing",
            Self::Unaligned => "Uncertain match",
        }
    }
}

pub(in crate::app::syscall_view) struct Row {
    pub kind: Kind,
    pub call: Arc<str>,
    pub resource: Arc<str>,
    pub process: Arc<str>,
    pub spans: [Range<usize>; 2],
    pub sample: usize,
    pub result: [Option<Arc<str>>; 2],
    pub durations: [Option<u64>; 2],
    pub uncertain: bool,
    pub result_changes: usize,
}

impl Row {
    pub fn counts(&self) -> [usize; 2] {
        [self.spans[0].len(), self.spans[1].len()]
    }

    pub fn evidence_count(&self) -> usize {
        self.spans[0].len().max(self.spans[1].len())
    }
}

pub struct RowMatch {
    pub index: usize,
    pub sample: Option<usize>,
}

pub struct Comparison {
    pub(in crate::app::syscall_view) traces: [Arc<Trace>; 2],
    pub(in crate::app::syscall_view) rows: Vec<Row>,
    pub limited: bool,
    orders: [Vec<usize>; 2],
}

impl Comparison {
    pub(in crate::app::syscall_view) fn build(working: Arc<Trace>, failing: Arc<Trace>) -> Self {
        let traces = [working, failing];
        let processes = traces.each_ref().map(|trace| Processes::read(trace));
        let (pairs, identities) = pair_processes(&processes);
        let normalized =
            [0, 1].map(|side| Normalized::new(&traces[side], &processes[side], &identities[side]));
        let mut comparison = Self {
            traces: traces.clone(),
            rows: Vec::new(),
            limited: normalized.iter().any(|side| side.limited),
            orders: Default::default(),
        };
        for pair in pairs {
            let runs = [0, 1].map(|side| match pair.index[side] {
                Some(index) => normalized[side].runs(&processes[side].entries[index].events),
                None => Vec::new(),
            });
            let image = pair
                .index
                .iter()
                .enumerate()
                .find_map(|(side, index)| {
                    index.and_then(|index| processes[side].entries[index].image.clone())
                })
                .unwrap_or_else(|| "Executable unavailable".into());
            comparison.compare_process(&runs, image, pair.uncertain);
            if comparison.rows.len() == MAX_ROWS {
                comparison.limited = true;
                break;
            }
        }
        let traces = &comparison.traces;
        let orders = &comparison.orders;
        comparison.rows.sort_by_cached_key(|row| {
            let mut time = None;
            for side in 0..2 {
                let span = &row.spans[side];
                if let Some(&event) = orders[side]
                    .get(row_index(span, row.sample))
                    .filter(|_| !span.is_empty())
                    && let Some(elapsed) = traces[side].relative_start(event)
                {
                    time = Some(time.map_or(elapsed, |current: u64| current.min(elapsed)));
                }
            }
            time.unwrap_or(u64::MAX)
        });
        comparison
    }

    pub(in crate::app::syscall_view) fn event(
        &self,
        row: &Row,
        side: usize,
        sample: usize,
    ) -> Option<usize> {
        let span = &row.spans[side];
        (sample < span.len()).then(|| self.orders[side][span.start + sample])
    }

    pub(in crate::app::syscall_view) fn first_divergence(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.kind != Kind::Timing && !row.uncertain)
            .or_else(|| self.rows.iter().position(|row| row.kind != Kind::Timing))
    }

    pub(in crate::app::syscall_view) fn search(&self, query: &str) -> Vec<RowMatch> {
        let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let fields = [
                    row.call.as_ref(),
                    row.resource.as_ref(),
                    row.process.as_ref(),
                    row.kind.label(),
                ]
                .map(str::to_lowercase);
                let missing: Vec<_> = terms
                    .iter()
                    .filter(|term| !fields.iter().any(|field| field.contains(term.as_str())))
                    .collect();
                if missing.is_empty() {
                    return Some(RowMatch {
                        index,
                        sample: None,
                    });
                }
                let sample = (0..row.evidence_count()).find(|&sample| {
                    let errors = [0, 1].map(|side| {
                        self.event(row, side, sample)
                            .and_then(|event| self.traces[side].events[event].error.as_deref())
                            .map(str::to_lowercase)
                    });
                    missing.iter().all(|term| {
                        errors
                            .iter()
                            .flatten()
                            .any(|error| error.contains(term.as_str()))
                    })
                })?;
                Some(RowMatch {
                    index,
                    sample: Some(sample),
                })
            })
            .collect()
    }

    fn compare_process(&mut self, runs: &[Vec<Run>; 2], image: Arc<str>, uncertain: bool) {
        let positions = runs.each_ref().map(|runs| positions(runs));
        let mut at = [0, 0];
        let mut ambiguous_anchor = false;
        while at[0] < runs[0].len() || at[1] < runs[1].len() {
            if self.rows.len() == MAX_ROWS {
                break;
            }
            let before = runs[0].get(at[0]);
            let after = runs[1].get(at[1]);
            match (before, after) {
                (Some(left), Some(right)) if left.key == right.key => {
                    if let Some(difference) = paired_difference(left, right) {
                        self.store(
                            [before, after],
                            difference,
                            image.clone(),
                            uncertain || ambiguous_anchor,
                        );
                    }
                    ambiguous_anchor = false;
                    at[0] += 1;
                    at[1] += 1;
                }
                (Some(_), None) => {
                    self.store(
                        [before, None],
                        Difference::new(Kind::Missing),
                        image.clone(),
                        uncertain,
                    );
                    at[0] += 1;
                }
                (None, Some(_)) => {
                    self.store(
                        [None, after],
                        Difference::new(Kind::Added),
                        image.clone(),
                        uncertain,
                    );
                    at[1] += 1;
                }
                (Some(_), Some(_)) => {
                    if let Some(anchor) = next_anchor(runs, &positions, at) {
                        ambiguous_anchor = anchor.ambiguous;
                        for side in 0..2 {
                            while at[side] < anchor[side] && self.rows.len() < MAX_ROWS {
                                let mut found = [None; 2];
                                found[side] = runs[side].get(at[side]);
                                let kind = if side == 0 {
                                    Kind::Missing
                                } else {
                                    Kind::Added
                                };
                                self.store(
                                    found,
                                    Difference::new(kind),
                                    image.clone(),
                                    uncertain || anchor.ambiguous,
                                );
                                at[side] += 1;
                            }
                        }
                    } else {
                        self.store(
                            [before, after],
                            Difference::new(Kind::Unaligned),
                            image.clone(),
                            true,
                        );
                        at[0] += 1;
                        at[1] += 1;
                    }
                }
                (None, None) => break,
            }
        }
    }

    fn store(
        &mut self,
        runs: [Option<&Run>; 2],
        difference: Difference,
        process: Arc<str>,
        uncertain: bool,
    ) {
        let key = &runs.into_iter().flatten().next().unwrap().key;
        let (call, resource) = if let [Some(left), Some(right)] = runs
            && difference.kind == Kind::Unaligned
        {
            let left = &left.key;
            let right = &right.key;
            (
                format!("{} → {}", left.call, right.call).into(),
                format!("{} → {}", left.resource, right.resource).into(),
            )
        } else {
            (key.call.clone(), key.resource.clone())
        };
        let spans = [0, 1].map(|side| {
            let start = self.orders[side].len();
            if let Some(run) = runs[side] {
                self.orders[side].extend(run.calls.iter().map(|call| call.event));
            }
            start..self.orders[side].len()
        });
        let sample = difference.sample;
        let result = runs.map(|run| {
            run.and_then(|run| run.calls.get(sample))
                .map(|call| call.result.clone())
        });
        let durations = runs.map(|run| run.and_then(|run| run.duration));
        self.rows.push(Row {
            kind: difference.kind,
            call,
            resource,
            process,
            spans,
            sample,
            result,
            durations,
            uncertain: uncertain || runs.iter().flatten().any(|run| run.uncertain),
            result_changes: difference.changes,
        });
    }
}

fn row_index(span: &Range<usize>, sample: usize) -> usize {
    span.start + sample.min(span.len().saturating_sub(1))
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Pid {
    source: Option<u16>,
    number: Option<u32>,
}

struct Process {
    parent: Option<usize>,
    image: Option<Arc<str>>,
    events: Vec<usize>,
}

#[derive(Clone, Copy, Default)]
struct Links {
    argument: Option<usize>,
    result: Option<usize>,
}

struct Processes {
    entries: Vec<Process>,
    by_event: Vec<usize>,
    links: Vec<Links>,
}

impl Processes {
    fn read(trace: &Trace) -> Self {
        let mut processes = Self {
            entries: Vec::new(),
            by_event: Vec::with_capacity(trace.events.len()),
            links: Vec::with_capacity(trace.events.len()),
        };
        let mut active = HashMap::new();
        let mut known = HashMap::new();
        let shared_order = trace.file_count() == 1 || trace.merged_by_time();
        for (index, event) in trace.events.iter().enumerate() {
            let pid = Pid {
                source: (!trace.merged_by_time() || event.pid.is_none())
                    .then_some(event.first.source),
                number: event.pid,
            };
            let current = *active.entry(pid).or_insert_with(|| processes.push(None));
            known.insert(pid, current);
            processes.by_event.push(current);
            let entry = &mut processes.entries[current];
            entry.events.push(index);
            if entry.image.is_none() && matches!(&*event.name, "execve" | "execveat") {
                entry.image = trace
                    .with_call(event, |arguments, _| {
                        let path = argument_values(arguments)
                            .nth(usize::from(&*event.name == "execveat"))?;
                        let path = path.strip_prefix('"')?.strip_suffix('"')?;
                        (path.len() <= 4096).then(|| Arc::from(path))
                    })
                    .flatten();
            }
            let mut links = Links::default();
            if matches!(
                &*event.name,
                "fork"
                    | "vfork"
                    | "clone"
                    | "clone3"
                    | "getpid"
                    | "gettid"
                    | "getppid"
                    | "wait4"
                    | "waitpid"
                    | "kill"
                    | "tkill"
                    | "tgkill"
                    | "pidfd_open"
            ) && (event.outcome == Outcome::Success || event.outcome == Outcome::Error)
            {
                trace.with_call(event, |arguments, value| {
                    let key = |number| Pid {
                        number: Some(number),
                        ..pid
                    };
                    let spawn = matches!(&*event.name, "fork" | "vfork" | "clone" | "clone3");
                    if spawn
                        && shared_order
                        && let Some(child) = positive(value)
                        && Some(child) != pid.number
                    {
                        let child = key(child);
                        let entry = *active
                            .entry(child)
                            .or_insert_with(|| processes.push(Some(current)));
                        known.insert(child, entry);
                        links.result = Some(entry);
                    } else {
                        if matches!(
                            &*event.name,
                            "wait4" | "waitpid" | "kill" | "tkill" | "tgkill" | "pidfd_open"
                        ) {
                            links.argument = argument_values(arguments)
                                .next()
                                .and_then(positive)
                                .and_then(|pid| known.get(&key(pid)).copied());
                        }
                        if matches!(
                            &*event.name,
                            "getpid" | "gettid" | "getppid" | "wait4" | "waitpid"
                        ) {
                            links.result =
                                positive(value).and_then(|pid| known.get(&key(pid)).copied());
                        }
                    }
                });
            }
            processes.links.push(links);
            if matches!(event.outcome, Outcome::Exit | Outcome::AbnormalExit) {
                active.remove(&pid);
            }
        }
        // Parents are discovered before children. Share the executable only
        // when the child never published an exec request of its own.
        for index in 0..processes.entries.len() {
            if processes.entries[index].image.is_none()
                && let Some(parent) = processes.entries[index].parent
            {
                processes.entries[index].image = processes.entries[parent].image.clone();
            }
        }
        processes
    }

    fn push(&mut self, parent: Option<usize>) -> usize {
        let index = self.entries.len();
        self.entries.push(Process {
            parent,
            image: None,
            events: Vec::new(),
        });
        index
    }

    fn children(&self) -> HashMap<Option<usize>, Vec<usize>> {
        let mut children = HashMap::<_, Vec<_>>::new();
        for (index, process) in self.entries.iter().enumerate() {
            if !process.events.is_empty() {
                children.entry(process.parent).or_default().push(index);
            }
        }
        children
    }
}

#[derive(Clone, Copy)]
struct Pair {
    index: [Option<usize>; 2],
    uncertain: bool,
}

fn pair_processes(processes: &[Processes; 2]) -> (Vec<Pair>, [Vec<usize>; 2]) {
    let trees = processes.each_ref().map(Processes::children);
    let mut pairs = Vec::new();
    let mut identities = processes.each_ref().map(|side| vec![0; side.entries.len()]);
    let mut queue = VecDeque::from([Pair {
        index: [None; 2],
        uncertain: false,
    }]);
    let mut first = true;
    while let Some(parent) = queue.pop_front() {
        let children = [0, 1].map(|side| {
            if first || parent.index[side].is_some() {
                trees[side]
                    .get(&parent.index[side])
                    .map(Vec::as_slice)
                    .unwrap_or_default()
            } else {
                &[]
            }
        });
        first = false;
        let mut right = HashMap::<Option<&str>, VecDeque<usize>>::new();
        let mut ambiguous = HashMap::<Option<&str>, [usize; 2]>::new();
        for side in 0..2 {
            for &index in children[side] {
                let image = processes[side].entries[index].image.as_deref();
                ambiguous.entry(image).or_default()[side] += 1;
                if side == 1 {
                    right.entry(image).or_default().push_back(index);
                }
            }
        }
        let mut matched = Vec::new();
        for &index in children[0] {
            let image = processes[0].entries[index].image.as_deref();
            let counterpart = right.get_mut(&image).and_then(VecDeque::pop_front);
            let uncertain = parent.uncertain
                || image.is_none()
                || ambiguous[&image].into_iter().any(|count| count > 1);
            matched.push(Pair {
                index: [Some(index), counterpart],
                uncertain,
            });
        }
        let mut leftover = right.into_values().flatten().collect::<Vec<_>>();
        leftover.sort_unstable();
        for index in leftover {
            matched.push(Pair {
                index: [None, Some(index)],
                uncertain: parent.uncertain || processes[1].entries[index].image.is_none(),
            });
        }
        for pair in matched {
            let joint = pairs.len() + 1;
            for (side, index) in pair.index.into_iter().enumerate() {
                if let Some(index) = index {
                    identities[side][index] = joint;
                }
            }
            pairs.push(pair);
            queue.push_back(pair);
        }
    }
    // A spawned child that never printed a line still has a distinguishable
    // raw PID, but no matched process identity is available.
    (pairs, identities)
}

fn positive(value: &str) -> Option<u32> {
    let value: u32 = value.split_ascii_whitespace().next()?.parse().ok()?;
    (value > 0).then_some(value)
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    call: Arc<str>,
    resource: Arc<str>,
}

struct Call {
    event: usize,
    result: Arc<str>,
    duration: Option<u64>,
}

struct Run {
    key: Key,
    calls: Vec<Call>,
    duration: Option<u64>,
    uncertain: bool,
}

struct Normalized<'a> {
    trace: &'a Trace,
    processes: &'a Processes,
    identities: &'a [usize],
    bindings: HashMap<(usize, u32), usize>,
    descriptors: HashMap<usize, (Arc<str>, bool)>,
    limited: bool,
}

impl<'a> Normalized<'a> {
    fn new(trace: &'a Trace, processes: &'a Processes, identities: &'a [usize]) -> Self {
        let report = trace.descriptor_report();
        let bindings = report
            .comparison_bindings()
            .map(|(event, fd, family)| ((event, fd), family))
            .collect();
        let mut ordinal = HashMap::<(usize, Arc<str>), usize>::new();
        let descriptors = report
            .comparison_roots()
            .map(|(id, event, label, observed)| {
                let stable: Arc<str> = stable_descriptor(label).into();
                let owner = identities[processes.by_event[event]];
                let occurrence = ordinal.entry((owner, stable.clone())).or_default();
                *occurrence += 1;
                let key = if *occurrence == 1 {
                    stable
                } else {
                    format!("{stable} (open {})", *occurrence).into()
                };
                (id, (key, observed))
            })
            .collect();
        Self {
            trace,
            processes,
            identities,
            bindings,
            descriptors,
            limited: report.limited,
        }
    }

    fn runs(&self, events: &[usize]) -> Vec<Run> {
        let mut runs: Vec<Run> = Vec::new();
        for &index in events {
            let (key, call, uncertain) = self.call(index);
            if let Some(previous) = runs.last_mut().filter(|run| run.key == key) {
                previous.duration = previous
                    .duration
                    .zip(call.duration)
                    .map(|(a, b)| a.saturating_add(b));
                previous.uncertain |= uncertain;
                previous.calls.push(call);
            } else {
                runs.push(Run {
                    key,
                    duration: call.duration,
                    calls: vec![call],
                    uncertain,
                });
            }
        }
        runs
    }

    fn call(&self, index: usize) -> (Key, Call, bool) {
        let event = &self.trace.events[index];
        if matches!(event.outcome, Outcome::Exit | Outcome::AbnormalExit) {
            return (
                Key {
                    call: event.name.clone(),
                    resource: Arc::default(),
                },
                Call {
                    event: index,
                    result: event.resource.clone(),
                    duration: None,
                },
                false,
            );
        }
        let mut resource = event.resource.clone();
        let mut uncertain = matches!(event.outcome, Outcome::Incomplete | Outcome::Other);
        let result = self
            .trace
            .with_call(event, |arguments, result| {
                let mut args = argument_values(arguments);
                let first = args.next().unwrap_or("");
                let second = args.next().unwrap_or("");
                let link = self.processes.links[index];
                match &*event.name {
                    "wait4" | "waitpid" | "kill" | "tkill" | "tgkill" | "pidfd_open" => {
                        if positive(first).is_some() {
                            resource = self.logical_pid(link.argument, &mut uncertain);
                        } else if !first.is_empty() {
                            resource = first.into();
                        }
                    }
                    "openat" | "openat2" | "faccessat" | "faccessat2" | "newfstatat" | "statx"
                    | "unlinkat" | "mkdirat" | "readlinkat" => {
                        if !second.starts_with("\"/") && fd_number(first).is_some() {
                            let directory = self.logical_fd(index, first, &mut uncertain);
                            resource = format!("{directory}, {second}").into();
                        }
                    }
                    "sendfile" | "sendfile64" | "tee" => {
                        let output = self.logical_fd(index, first, &mut uncertain);
                        let input = self.logical_fd(index, second, &mut uncertain);
                        resource = format!("{output}, {input}").into();
                    }
                    "splice" | "copy_file_range" => {
                        let input = self.logical_fd(index, first, &mut uncertain);
                        let output =
                            self.logical_fd(index, args.next().unwrap_or(""), &mut uncertain);
                        resource = format!("{input}, {output}").into();
                    }
                    "fcntl" | "fcntl64" if fd_number(first).is_some() => {
                        let fd = self.logical_fd(index, first, &mut uncertain);
                        resource = format!("{fd}, {second}").into();
                    }
                    "connect" | "bind" if fd_number(first).is_some() => {
                        let fd = self.logical_fd(index, first, &mut uncertain);
                        resource = format!("{fd}, {second}").into();
                    }
                    "close_range" => {
                        resource = "Descriptor range".into();
                        uncertain = true;
                    }
                    name if (super::descriptors::descriptor_input(name)
                        || matches!(
                            name,
                            "accept" | "accept4" | "close" | "dup" | "dup2" | "dup3"
                        ))
                        && fd_number(first).is_some() =>
                    {
                        resource = self.logical_fd(index, first, &mut uncertain);
                    }
                    _ => (),
                }
                if let Some(error) = &event.error {
                    return error.clone();
                }
                let token = result
                    .split_ascii_whitespace()
                    .next()
                    .unwrap_or("unavailable");
                if token == "?" || event.outcome == Outcome::Incomplete {
                    return "Incomplete".into();
                }
                let returned_fd = matches!(
                    &*event.name,
                    "open"
                        | "openat"
                        | "openat2"
                        | "creat"
                        | "socket"
                        | "accept"
                        | "accept4"
                        | "dup"
                        | "dup2"
                        | "dup3"
                        | "pidfd_open"
                ) || matches!(&*event.name, "fcntl" | "fcntl64")
                    && matches!(second, "F_DUPFD" | "F_DUPFD_CLOEXEC");
                if returned_fd && fd_number(token).is_some() {
                    return "Descriptor returned".into();
                }
                if matches!(
                    &*event.name,
                    "fork"
                        | "vfork"
                        | "clone"
                        | "clone3"
                        | "wait4"
                        | "waitpid"
                        | "getpid"
                        | "gettid"
                        | "getppid"
                ) && positive(token).is_some()
                {
                    return self.logical_pid(link.result, &mut uncertain);
                }
                if matches!(&*event.name, "mmap" | "mmap2" | "mremap" | "brk" | "shmat")
                    && token.starts_with("0x")
                {
                    return "Address returned".into();
                }
                token.chars().take(128).collect::<String>().into()
            })
            .unwrap_or_else(|| {
                event
                    .error
                    .clone()
                    .unwrap_or_else(|| event.outcome.label().into())
            });
        (
            Key {
                call: event.name.clone(),
                resource,
            },
            Call {
                event: index,
                result,
                duration: event.duration,
            },
            uncertain,
        )
    }

    fn logical_pid(&self, process: Option<usize>, uncertain: &mut bool) -> Arc<str> {
        if let Some(id) = process
            .map(|index| self.identities[index])
            .filter(|&id| id > 0)
        {
            format!("Process {id}").into()
        } else {
            *uncertain = true;
            "Unresolved process ID".into()
        }
    }

    fn logical_fd(&self, event: usize, value: &str, uncertain: &mut bool) -> Arc<str> {
        if let Some(binding) = fd_number(value).and_then(|fd| self.bindings.get(&(event, fd)))
            && let Some((label, observed)) = self.descriptors.get(binding)
        {
            *uncertain |= observed;
            return label.clone();
        }
        *uncertain = true;
        match value
            .split_once('<')
            .and_then(|(_, label)| label.strip_suffix('>'))
        {
            Some(label) if label.len() <= 4096 => stable_descriptor(label).into(),
            _ => "Unresolved descriptor".into(),
        }
    }
}

fn fd_number(value: &str) -> Option<u32> {
    value
        .split_once('<')
        .map_or(value, |(number, _)| number)
        .parse()
        .ok()
}

fn stable_descriptor(label: &str) -> &str {
    if label.starts_with("pipe:[") {
        "Pipe"
    } else if label.starts_with("socket:[") {
        "Socket"
    } else if let Some((kind, _)) = label.split_once(":[")
        && matches!(kind, "TCP" | "TCPv6" | "UDP" | "UDPv6" | "UNIX" | "NETLINK")
    {
        kind
    } else {
        label
    }
}

type Positions<'a> = HashMap<&'a Key, Vec<usize>>;

fn positions(runs: &[Run]) -> Positions<'_> {
    let mut positions = Positions::new();
    for (index, run) in runs.iter().enumerate() {
        positions.entry(&run.key).or_default().push(index);
    }
    positions
}

struct Anchor {
    index: [usize; 2],
    ambiguous: bool,
}

impl std::ops::Index<usize> for Anchor {
    type Output = usize;
    fn index(&self, side: usize) -> &usize {
        &self.index[side]
    }
}

fn next_anchor(
    runs: &[Vec<Run>; 2],
    positions: &[Positions<'_>; 2],
    at: [usize; 2],
) -> Option<Anchor> {
    let mut best: Option<(usize, [usize; 2], bool)> = None;
    for side in 0..2 {
        let other = 1 - side;
        for offset in 0..LOOKAHEAD {
            if best.as_ref().is_some_and(|(score, _, _)| offset > *score) {
                break;
            }
            let index = at[side] + offset;
            let Some(candidate) = runs[side].get(index) else {
                break;
            };
            let Some(locations) = positions[other].get(&candidate.key) else {
                continue;
            };
            let location = locations.partition_point(|&position| position < at[other]);
            let Some(&match_at) = locations.get(location) else {
                continue;
            };
            // A one-sided insertion or omission may be arbitrarily far from
            // its anchor. A two-sided gap uses bounded recovery.
            if offset > 0 && match_at - at[other] >= LOOKAHEAD {
                continue;
            }
            let score = offset + match_at - at[other];
            let mut pair = at;
            pair[side] = index;
            pair[other] = match_at;
            let repeated = locations
                .get(location + 1)
                .is_some_and(|&next| next - match_at <= LOOKAHEAD);
            if let Some((old_score, old_pair, ambiguous)) = &mut best {
                if score == *old_score && pair != *old_pair {
                    *ambiguous = true;
                }
                if score >= *old_score {
                    continue;
                }
            }
            best = Some((score, pair, repeated));
        }
    }
    best.map(|(_, index, ambiguous)| Anchor { index, ambiguous })
}

struct Difference {
    kind: Kind,
    sample: usize,
    changes: usize,
}

impl Difference {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            sample: 0,
            changes: 0,
        }
    }
}

fn paired_difference(left: &Run, right: &Run) -> Option<Difference> {
    let mut result = None;
    let mut changes = 0;
    let mut timing = None;
    for (index, (left, right)) in left.calls.iter().zip(&right.calls).enumerate() {
        if left.result != right.result {
            result.get_or_insert(index);
            changes += 1;
        }
        if let Some((a, b)) = left.duration.zip(right.duration)
            && a.abs_diff(b) >= TIME_FLOOR_NS
            && a.abs_diff(b) >= a.max(b) / 4
        {
            timing.get_or_insert(index);
        }
    }
    if let Some(sample) = result {
        return Some(Difference {
            kind: Kind::Result,
            sample,
            changes,
        });
    }
    if left.calls.len() != right.calls.len() {
        Some(Difference {
            kind: Kind::Count,
            sample: left.calls.len().min(right.calls.len()),
            changes: 0,
        })
    } else {
        timing.map(|sample| Difference {
            kind: Kind::Timing,
            sample,
            changes: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(text: &str) -> Arc<Trace> {
        Arc::new(
            Trace::parse(vec![super::super::Source {
                path: "comparison.strace".into(),
                pid: None,
                bytes: text.as_bytes().to_vec(),
            }])
            .unwrap(),
        )
    }

    #[test]
    fn lines_up_children_by_executable_and_normalizes_pid_fd_and_wall_clock() {
        let working = trace(concat!(
            "51 1700000000.000000 execve(\"/usr/bin/tester\", [\"tester\"], NULL) = 0 <0.000001>\n",
            "51 1700000000.001000 openat(AT_FDCWD, \"/tmp/input\", O_RDONLY) = 3</tmp/input> <0.000001>\n",
            "51 1700000000.002000 clone(flags=SIGCHLD) = 52 <0.000001>\n",
            "52 1700000000.003000 execve(\"/usr/bin/helper\", [\"helper\"], NULL) = 0 <0.000001>\n",
            "52 1700000000.004000 getpid() = 52 <0.000001>\n",
            "52 1700000000.005000 read(3</tmp/input>, \"a\", 1) = 1 <0.000001>\n",
            "52 1700000000.006000 close(3</tmp/input>) = 0 <0.000001>\n",
            "52 1700000000.007000 +++ exited with 0 +++\n",
            "51 1700000000.008000 wait4(52, NULL, 0, NULL) = 52 <0.000001>\n",
        ));
        let failing = trace(concat!(
            "91 1800000000.000000 execve(\"/usr/bin/tester\", [\"tester\"], NULL) = 0 <0.000001>\n",
            "91 1800000000.001000 openat(AT_FDCWD, \"/tmp/input\", O_RDONLY) = 73</tmp/input> <0.000001>\n",
            "91 1800000000.002000 clone(flags=SIGCHLD <unfinished ...>\n",
            "99 1800000000.003000 execve(\"/usr/bin/helper\", [\"helper\"], NULL) = 0 <0.000001>\n",
            "91 1800000000.003020 <... clone resumed>) = 99 <0.000001>\n",
            "99 1800000000.004000 getpid() = 99 <0.000001>\n",
            "99 1800000000.005000 read(73</tmp/input>, \"a\", 1) = 1 <0.000001>\n",
            "99 1800000000.006000 close(73</tmp/input>) = 0 <0.000001>\n",
            "99 1800000000.007000 +++ exited with 0 +++\n",
            "91 1800000000.008000 wait4(99, NULL, 0, NULL) = 99 <0.000001>\n",
        ));
        let result = Comparison::build(working, failing);
        assert!(
            result.rows.is_empty(),
            "unexpected differences: {:?}",
            result
                .rows
                .iter()
                .map(|row| (&row.call, row.kind))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn exposes_the_first_failed_open_and_the_count_delta_after_fd_reuse() {
        let working = trace(concat!(
            "21 1700000000.000000 execve(\"/bin/diag\", [\"diag\"], NULL) = 0 <0.000001>\n",
            "21 1700000000.001000 openat(AT_FDCWD, \"/tmp/settings\", O_RDONLY) = 3</tmp/settings> <0.000001>\n",
            "21 1700000000.002000 close(3</tmp/settings>) = 0 <0.000001>\n",
            "21 1700000000.003000 openat(AT_FDCWD, \"/tmp/data\", O_RDONLY) = 3</tmp/data> <0.000001>\n",
            "21 1700000000.004000 read(3</tmp/data>, \"a\", 1) = 1 <0.000001>\n",
            "21 1700000000.005000 read(3</tmp/data>, \"b\", 1) = 1 <0.000001>\n",
            "21 1700000000.006000 close(3</tmp/data>) = 0 <0.000001>\n",
        ));
        let failing = trace(concat!(
            "77 1800000000.000000 execve(\"/bin/diag\", [\"diag\"], NULL) = 0 <0.000001>\n",
            "77 1800000000.001000 openat(AT_FDCWD, \"/tmp/settings\", O_RDONLY) = -1 ENOENT (No such file or directory) <0.010001>\n",
            "77 1800000000.003000 openat(AT_FDCWD, \"/tmp/data\", O_RDONLY) = 8</tmp/data> <0.000001>\n",
            "77 1800000000.004000 read(8</tmp/data>, \"a\", 1) = 1 <0.000001>\n",
            "77 1800000000.006000 close(8</tmp/data>) = 0 <0.000001>\n",
        ));
        let result = Comparison::build(working, failing);
        assert_eq!(result.rows.len(), 3);
        let first = &result.rows[0];
        assert_eq!(first.kind, Kind::Result);
        assert_eq!(&*first.resource, "\"/tmp/settings\"");
        assert_eq!(first.result[1].as_deref(), Some("ENOENT"));
        assert_eq!(first.durations, [Some(1_000), Some(10_001_000)]);
        assert_eq!(result.event(first, 0, first.sample), Some(1));
        assert_eq!(result.event(first, 1, first.sample), Some(1));
        assert_eq!(result.traces[1].relative_start(1), Some(1_000_000));
        assert_eq!(result.rows[1].kind, Kind::Missing);
        let reads = &result.rows[2];
        assert_eq!(
            (reads.kind, reads.counts(), reads.sample),
            (Kind::Count, [2, 1], 1)
        );
        assert_eq!(&*reads.resource, "/tmp/data");
        assert_eq!(result.event(reads, 0, 1), Some(5));
        assert_eq!(result.event(reads, 1, 1), None);
    }

    #[test]
    fn searches_normalized_changes_and_jumps_to_matching_error_evidence() {
        let working = trace(concat!(
            "21 execve(\"/bin/diag\", [\"diag\"], NULL) = 0\n",
            "21 access(\"/tmp/settings\", R_OK) = 0\n",
            "21 access(\"/tmp/settings\", R_OK) = 0\n",
        ));
        let failing = trace(concat!(
            "87 execve(\"/bin/diag\", [\"diag\"], NULL) = 0\n",
            "87 access(\"/tmp/settings\", R_OK) = -1 ENOENT (No such file or directory)\n",
            "87 access(\"/tmp/settings\", R_OK) = -1 EACCES (Permission denied)\n",
        ));
        let comparison = Comparison::build(working, failing);
        let matches = |query| {
            comparison
                .search(query)
                .into_iter()
                .map(|row| (row.index, row.sample))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            matches("/BIN/DIAG access /tmp/settings changed result"),
            [(0, None)]
        );
        assert_eq!(matches("access enoent"), [(0, Some(0))]);
        assert_eq!(matches("access eacces"), [(0, Some(1))]);
        assert!(matches("access eacces /different/path").is_empty());
    }

    #[test]
    fn recovers_an_anchor_after_a_long_insertion_and_keeps_timing_changes() {
        let before = "89 execve(\"/bin/r\", [\"r\"], NULL) = 0 <0.000001>\n89 clock_nanosleep(0, 0, 0x0, NULL) = 0 <0.002000>\n";
        let mut after = String::from("176 execve(\"/bin/r\", [\"r\"], NULL) = 0 <0.000001>\n");
        for i in 0..100 {
            after.push_str(&format!("176 stat(\"/probe/{i}\", {{}}) = 0 <0.000001>\n"));
        }
        after.push_str("176 clock_nanosleep(0, 0, 0x0, NULL) = 0 <0.005000>\n");
        let result = Comparison::build(trace(before), trace(&after));
        assert_eq!(result.rows.len(), 101);
        assert!(result.rows[..100].iter().all(|row| row.kind == Kind::Added));
        assert_eq!(result.rows[100].kind, Kind::Timing);
        assert_eq!(
            result.rows[100].durations,
            [Some(2_000_000), Some(5_000_000)]
        );
    }

    #[test]
    fn exposes_both_originals_when_two_calls_cannot_be_aligned() {
        let left = trace(
            "5 execve(\"/bin/r\", [\"r\"], NULL) = 0\n5 access(\"/old\", R_OK) = -1 ENOENT (No such file or directory)\n",
        );
        let right = trace("8 execve(\"/bin/r\", [\"r\"], NULL) = 0\n8 chdir(\"/new\") = 0\n");
        let comparison = Comparison::build(left, right);
        let [row] = comparison.rows.as_slice() else {
            panic!()
        };
        assert!(row.uncertain && row.kind == Kind::Unaligned);
        assert_eq!(&*row.call, "access → chdir");
        assert_eq!(comparison.event(row, 0, 0), Some(1));
        assert_eq!(comparison.event(row, 1, 0), Some(1));
    }
}
