use super::{Event, Outcome, Raw, Trace, argument_values, prefix, slice};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

const MAX_LIFETIMES: usize = 150_000;
const MAX_STEPS: usize = 2_000_000;

type LifeId = usize;
type TableId = usize;
type Bounded<T = ()> = Result<T, ()>;

#[derive(Clone, Copy)]
enum Origin {
    Opened,
    Created,
    Accepted,
    Duplicate(LifeId),
    Inherited(LifeId),
    Split(LifeId),
    Observed,
}

#[derive(Clone, Copy)]
enum End {
    Closed,
    ClosedWithError,
    Invalid,
    ReplacedByDup,
    Reused,
    CloseOnExec,
    Exit,
}

#[derive(Clone, Copy)]
enum Change {
    Began(Origin),
    Used,
    Shared(u32),
    CopiedTo(LifeId, Option<u32>),
    CloseOnExec(bool),
    Ended(End),
}

#[derive(Clone, Copy)]
pub(in crate::app::syscall_view) struct Step {
    pub event: usize,
    change: Change,
}

impl Step {
    pub fn label(self) -> String {
        match self.change {
            Change::Began(Origin::Opened) => "Opened".into(),
            Change::Began(Origin::Created) => "Created".into(),
            Change::Began(Origin::Accepted) => "Accepted".into(),
            Change::Began(Origin::Duplicate(source)) => {
                format!("Duplicated from lifetime {}", source + 1)
            }
            Change::Began(Origin::Inherited(source)) => {
                format!("Inherited from lifetime {}", source + 1)
            }
            Change::Began(Origin::Split(source)) => format!(
                "Copied from lifetime {} when the descriptor table split",
                source + 1
            ),
            Change::Began(Origin::Observed) => "First observed, open not captured".into(),
            Change::Used => "Used".into(),
            Change::Shared(pid) => format!("Shared descriptor table with PID {pid}"),
            Change::CopiedTo(target, Some(pid)) => {
                format!("Copied to PID {pid}, lifetime {}", target + 1)
            }
            Change::CopiedTo(target, None) => format!("Copied to lifetime {}", target + 1),
            Change::CloseOnExec(true) => "Close-on-exec set".into(),
            Change::CloseOnExec(false) => "Close-on-exec cleared".into(),
            Change::Ended(end) => end.label().into(),
        }
    }
}

impl End {
    fn label(self) -> &'static str {
        match self {
            Self::Closed => "Closed",
            Self::ClosedWithError => "Closed with an error",
            Self::Invalid => "Invalid descriptor at close",
            Self::ReplacedByDup => "Replaced by duplication",
            Self::Reused => "Reused without a captured close",
            Self::CloseOnExec => "Closed on exec",
            Self::Exit => "Released at process exit",
        }
    }
}

pub(in crate::app::syscall_view) struct Lifetime {
    pub pid: Option<u32>,
    pub fd: u32,
    pub resource: Arc<str>,
    pub steps: Vec<Step>,
    family: LifeId,
    end: Option<End>,
}

impl Lifetime {
    pub fn status(&self) -> &'static str {
        self.end.map_or("No closing event captured", End::label)
    }
}

#[derive(Default)]
pub struct DescriptorReport {
    pub(in crate::app::syscall_view) lifetimes: Vec<Lifetime>,
    pub limited: bool,
}

impl DescriptorReport {
    pub(super) fn comparison_bindings(&self) -> impl Iterator<Item = (usize, u32, usize)> + '_ {
        self.lifetimes.iter().flat_map(|life| {
            life.steps.iter().filter_map(move |step| {
                matches!(
                    step.change,
                    Change::Used | Change::Ended(End::Closed | End::ClosedWithError | End::Invalid)
                )
                .then_some((step.event, life.fd, life.family))
            })
        })
    }

    pub(super) fn comparison_roots(&self) -> impl Iterator<Item = (usize, usize, &str, bool)> + '_ {
        self.lifetimes
            .iter()
            .enumerate()
            .filter(|(id, life)| *id == life.family)
            .map(|(id, life)| {
                let start = life.steps[0];
                (
                    id,
                    start.event,
                    &*life.resource,
                    matches!(start.change, Change::Began(Origin::Observed)),
                )
            })
    }
    pub(in crate::app::syscall_view) fn search(&self, trace: &Trace, query: &str) -> Vec<usize> {
        let query = query.trim().to_lowercase();
        let pid = query
            .strip_prefix("pid ")
            .and_then(|pid| pid.trim().parse::<u32>().ok());
        let fd = query
            .strip_prefix("fd ")
            .and_then(|fd| fd.trim().parse::<u32>().ok());
        let family = query
            .strip_prefix("lifetime ")
            .and_then(|id| id.trim().parse::<usize>().ok())
            .and_then(|id| self.lifetimes.get(id.checked_sub(1)?))
            .map(|life| life.family);
        self.lifetimes
            .iter()
            .enumerate()
            .filter_map(|(id, life)| {
                (query.is_empty()
                    || fd == Some(life.fd)
                    || family == Some(life.family)
                    || pid.is_some_and(|pid| {
                        life.pid == Some(pid)
                            || life.steps.iter().any(|step| {
                                trace.events[step.event].pid == Some(pid)
                                    || matches!(step.change, Change::Shared(child) if child == pid)
                                    || matches!(step.change, Change::CopiedTo(_, Some(child)) if child == pid)
                            })
                    })
                    || pid.is_none()
                        && fd.is_none()
                        && family.is_none()
                        && life.resource.to_lowercase().contains(&query))
                .then_some(id)
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Task {
    source: Option<u16>,
    pid: Option<u32>,
}

#[derive(Clone, Copy)]
struct Fd<'a> {
    number: u32,
    label: Option<&'a str>,
}

impl Fd<'_> {
    fn plain(number: u32) -> Self {
        Self {
            number,
            label: None,
        }
    }
}

fn descriptor(value: &str) -> Option<Fd<'_>> {
    let value = value.trim_start();
    let end = value.bytes().take_while(u8::is_ascii_digit).count();
    let number = value[..end].parse().ok()?;
    let suffix = &value[end..];
    let label = suffix.strip_prefix('<').and_then(|inner| {
        let extent = inner.bytes().enumerate().find_map(|(index, byte)| {
            (byte == b'>' && (index == 0 || inner.as_bytes()[index - 1] != b'-')).then_some(index)
        })?;
        let label = &inner[..extent];
        (!label.is_empty() && label.len() <= 4096).then_some(label)
    });
    if suffix.starts_with(|c: char| !(c.is_whitespace() || c == '<')) {
        return None;
    }
    Some(Fd { number, label })
}

#[derive(Clone, Copy)]
struct Slot {
    life: LifeId,
    cloexec: Option<bool>,
}

#[derive(Default)]
struct Table {
    members: usize,
    slots: BTreeMap<u32, Slot>,
}

#[derive(Default)]
struct Builder {
    report: DescriptorReport,
    tables: Vec<Table>,
    tasks: HashMap<Task, TableId>,
    steps: usize,
}

impl Trace {
    pub(in crate::app::syscall_view) fn descriptor_report(&self) -> DescriptorReport {
        let mut builder = Builder::default();
        let mut order = self
            .events
            .iter()
            .enumerate()
            .map(|(index, event)| {
                let raw = if uses_completion_order(&event.name) {
                    event.last.unwrap_or(event.first)
                } else {
                    event.first
                };
                (index, raw)
            })
            .collect::<Vec<_>>();
        if self.merged_by_time {
            order.sort_by_cached_key(|&(index, raw)| {
                (self.stamp_of(raw).unwrap_or(u64::MAX), index)
            });
        } else {
            order.sort_by_key(|&(_, raw)| (raw.source, raw.start));
        }
        for (index, _) in order {
            let event = &self.events[index];
            let task = Task {
                source: (!self.merged_by_time || event.pid.is_none()).then_some(event.first.source),
                pid: event.pid,
            };
            if self.descriptor_event(&mut builder, task, index).is_err() {
                builder.report.limited = true;
                break;
            }
        }
        builder.report
    }

    fn stamp_of(&self, raw: Raw) -> Option<u64> {
        let input = String::from_utf8_lossy(slice(&self.sources, raw));
        prefix(&input, None).1.map(|stamp| stamp.ns)
    }

    fn descriptor_event(&self, builder: &mut Builder, task: Task, index: usize) -> Bounded {
        let event = &self.events[index];
        if self.sources.len() > 1 && !self.merged_by_time && is_spawn(&event.name) {
            return Ok(());
        }
        if matches!(event.outcome, Outcome::Exit | Outcome::AbnormalExit) {
            return builder.retire(task, index);
        }
        if !matches!(
            event.outcome,
            Outcome::Success | Outcome::ZeroRead | Outcome::Error
        ) {
            return Ok(());
        }
        self.with_call(event, |arguments, result| {
            builder.apply(task, index, event, arguments, result)
        })
        .unwrap_or(Ok(()))
    }
}

fn uses_completion_order(call: &str) -> bool {
    // Spawn must link the child before its first line, even when strace
    // prints that line between the parent's unfinished and resumed records.
    matches!(
        call,
        "open"
            | "openat"
            | "openat2"
            | "creat"
            | "socket"
            | "socketpair"
            | "pipe"
            | "pipe2"
            | "accept"
            | "accept4"
            | "dup"
            | "dup2"
            | "dup3"
            | "fcntl"
            | "fcntl64"
            | "close_range"
            | "unshare"
            | "execve"
            | "execveat"
    )
}

fn is_spawn(call: &str) -> bool {
    matches!(call, "fork" | "vfork" | "clone" | "clone3")
}

impl Builder {
    fn limits(&self, additional_lifetimes: usize, additional_steps: usize) -> Bounded {
        if self.report.lifetimes.len() + additional_lifetimes > MAX_LIFETIMES
            || self.steps + additional_steps > MAX_STEPS
        {
            Err(())
        } else {
            Ok(())
        }
    }

    fn new_table(&mut self, task: Task) -> TableId {
        let id = self.tables.len();
        self.tables.push(Table {
            members: 1,
            ..Table::default()
        });
        self.tasks.insert(task, id);
        id
    }

    fn table(&mut self, task: Task) -> TableId {
        self.tasks
            .get(&task)
            .copied()
            .unwrap_or_else(|| self.new_table(task))
    }

    fn step(&mut self, life: LifeId, event: usize, change: Change) -> Bounded {
        self.limits(0, 1)?;
        self.steps += 1;
        self.report.lifetimes[life]
            .steps
            .push(Step { event, change });
        Ok(())
    }

    fn begin(
        &mut self,
        task: Task,
        event: usize,
        fd: Fd<'_>,
        origin: Origin,
        name: Arc<str>,
        cloexec: Option<bool>,
    ) -> Bounded<LifeId> {
        let table = self.table(task);
        self.limits(
            1,
            1 + usize::from(self.tables[table].slots.contains_key(&fd.number)),
        )?;
        let reason = if matches!(origin, Origin::Duplicate(_)) {
            End::ReplacedByDup
        } else {
            End::Reused
        };
        self.close_slot(table, fd.number, event, reason)?;
        let id = self.report.lifetimes.len();
        let family = match origin {
            Origin::Duplicate(source) | Origin::Inherited(source) | Origin::Split(source) => {
                self.report.lifetimes[source].family
            }
            _ => id,
        };
        self.report.lifetimes.push(Lifetime {
            pid: task.pid,
            fd: fd.number,
            resource: name,
            family,
            end: None,
            steps: Vec::new(),
        });
        self.tables[table]
            .slots
            .insert(fd.number, Slot { life: id, cloexec });
        self.step(id, event, Change::Began(origin))?;
        Ok(id)
    }

    fn close_slot(&mut self, table: TableId, fd: u32, event: usize, end: End) -> Bounded {
        if let Some(slot) = self.tables[table].slots.remove(&fd) {
            self.step(slot.life, event, Change::Ended(end))?;
            self.report.lifetimes[slot.life].end = Some(end);
        }
        Ok(())
    }

    fn existing(
        &mut self,
        task: Task,
        event: usize,
        fd: Fd<'_>,
        infer: bool,
    ) -> Bounded<Option<LifeId>> {
        let table = self.table(task);
        if let Some(slot) = self.tables[table].slots.get(&fd.number) {
            let life = &mut self.report.lifetimes[slot.life];
            if &*life.resource == "Unknown resource"
                && let Some(label) = fd.label
            {
                life.resource = label.into();
            }
            return Ok(Some(slot.life));
        }
        if !infer {
            return Ok(None);
        }
        let resource = fd.label.unwrap_or("Unknown resource").into();
        self.begin(task, event, fd, Origin::Observed, resource, None)
            .map(Some)
    }

    fn used(
        &mut self,
        task: Task,
        event: usize,
        fd: Option<Fd<'_>>,
        infer: bool,
    ) -> Bounded<Option<LifeId>> {
        let Some(fd) = fd else { return Ok(None) };
        let Some(life) = self.existing(task, event, fd, infer)? else {
            return Ok(None);
        };
        self.step(life, event, Change::Used)?;
        Ok(Some(life))
    }

    fn flag(&mut self, table: TableId, fd: u32, event: usize, enabled: bool) -> Bounded {
        if let Some(slot) = self.tables[table].slots.get_mut(&fd) {
            slot.cloexec = Some(enabled);
            let life = slot.life;
            self.step(life, event, Change::CloseOnExec(enabled))?;
        }
        Ok(())
    }

    fn retire(&mut self, task: Task, event: usize) -> Bounded {
        if let Some(table) = self.tasks.remove(&task) {
            self.tables[table].members -= 1;
            if self.tables[table].members == 0 {
                for (_, slot) in std::mem::take(&mut self.tables[table].slots) {
                    self.step(slot.life, event, Change::Ended(End::Exit))?;
                    self.report.lifetimes[slot.life].end = Some(End::Exit);
                }
            }
        }
        Ok(())
    }

    fn copy_slots(&mut self, from: TableId, task: Task, event: usize, split: bool) -> Bounded {
        let slots = &self.tables[from].slots;
        self.limits(slots.len(), slots.len() * 2)?;
        let slots = slots
            .iter()
            .map(|(&fd, &slot)| (fd, slot))
            .collect::<Vec<_>>();
        for (fd, slot) in slots {
            let origin = if split {
                Origin::Split(slot.life)
            } else {
                Origin::Inherited(slot.life)
            };
            let resource = self.report.lifetimes[slot.life].resource.clone();
            let copied = self.begin(task, event, Fd::plain(fd), origin, resource, slot.cloexec)?;
            self.step(slot.life, event, Change::CopiedTo(copied, task.pid))?;
        }
        Ok(())
    }

    fn split(&mut self, task: Task, event: usize) -> Bounded<TableId> {
        let from = self.table(task);
        if self.tables[from].members <= 1 {
            return Ok(from);
        }
        self.tables[from].members -= 1;
        let to = self.new_table(task);
        self.copy_slots(from, task, event, true)?;
        Ok(to)
    }

    fn spawn(&mut self, task: Task, event: usize, pid: u32, share: bool) -> Bounded {
        if task.pid == Some(pid) || pid == 0 {
            return Ok(());
        }
        let child = Task {
            pid: Some(pid),
            ..task
        };
        // A PID already seen before a spawn has an uncertain relationship.
        if self.tasks.contains_key(&child) {
            return Ok(());
        }
        let from = self.table(task);
        if share {
            self.tables[from].members += 1;
            self.tasks.insert(child, from);
            let lives = self.tables[from]
                .slots
                .values()
                .map(|slot| slot.life)
                .collect::<Vec<_>>();
            for life in lives {
                self.step(life, event, Change::Shared(pid))?;
            }
        } else {
            self.new_table(child);
            self.copy_slots(from, child, event, false)?;
        }
        Ok(())
    }

    fn exec(&mut self, task: Task, event: usize) -> Bounded {
        if !self.tasks.contains_key(&task) {
            return Ok(());
        }
        let table = self.split(task, event)?;
        let closing = self.tables[table]
            .slots
            .iter()
            .filter_map(|(&fd, slot)| (slot.cloexec == Some(true)).then_some(fd))
            .collect::<Vec<_>>();
        for fd in closing {
            self.close_slot(table, fd, event, End::CloseOnExec)?;
        }
        Ok(())
    }

    fn range(&mut self, task: Task, event: usize, first: &str, last: &str, flags: &str) -> Bounded {
        let (Some(first), Some(last)) = (descriptor(first).map(|fd| fd.number), upper_bound(last))
        else {
            return Ok(());
        };
        if first > last || !self.tasks.contains_key(&task) {
            return Ok(());
        }
        let table = if has_flag(flags, "CLOSE_RANGE_UNSHARE") {
            self.split(task, event)?
        } else {
            self.table(task)
        };
        let affected = self.tables[table]
            .slots
            .range(first..=last)
            .map(|(&fd, _)| fd)
            .collect::<Vec<_>>();
        for fd in affected {
            if has_flag(flags, "CLOSE_RANGE_CLOEXEC") {
                self.flag(table, fd, event, true)?;
            } else {
                self.close_slot(table, fd, event, End::Closed)?;
            }
        }
        Ok(())
    }

    fn duplicate(
        &mut self,
        task: Task,
        event: usize,
        source: Option<Fd<'_>>,
        result: Option<Fd<'_>>,
        cloexec: bool,
    ) -> Bounded {
        let (Some(source), Some(result)) = (source, result) else {
            return Ok(());
        };
        let Some(from) = self.used(task, event, Some(source), true)? else {
            return Ok(());
        };
        if source.number != result.number {
            let resource = result
                .label
                .map(Arc::from)
                .unwrap_or_else(|| self.report.lifetimes[from].resource.clone());
            self.begin(
                task,
                event,
                result,
                Origin::Duplicate(from),
                resource,
                Some(cloexec),
            )?;
        }
        Ok(())
    }

    fn apply(
        &mut self,
        task: Task,
        index: usize,
        event: &Event,
        arguments: &str,
        result: &str,
    ) -> Bounded {
        let mut args = argument_values(arguments);
        let a = args.next().unwrap_or("");
        let b = args.next().unwrap_or("");
        let c = args.next().unwrap_or("");
        let d = args.next().unwrap_or("");
        let first = descriptor(a);
        let okay = matches!(event.outcome, Outcome::Success | Outcome::ZeroRead);
        let infer = okay;
        let returned = if okay { descriptor(result) } else { None };
        match &*event.name {
            "fork" | "vfork" | "clone" | "clone3" => {
                if let Some(child) = returned {
                    let flags = if matches!(&*event.name, "fork" | "vfork") {
                        Some(false)
                    } else {
                        clone_shares_files(arguments)
                    };
                    if let Some(share) = flags {
                        self.spawn(task, index, child.number, share)?;
                    }
                }
            }
            "execve" | "execveat" if okay => self.exec(task, index)?,
            "unshare" if okay && has_flag(a, "CLONE_FILES") => {
                if self.tasks.contains_key(&task) {
                    self.split(task, index)?;
                }
            }
            "open" | "creat" | "openat" | "openat2" => {
                let at = matches!(&*event.name, "openat" | "openat2");
                if at {
                    self.used(task, index, first, infer)?;
                }
                if let Some(fd) = returned {
                    let path = if at { b } else { a };
                    let resource = fd
                        .label
                        .or_else(|| path.strip_prefix('"')?.strip_suffix('"'))
                        .unwrap_or("File");
                    let flags = if at { c } else { b };
                    let cloexec = event.name.as_ref() != "creat" && has_flag(flags, "O_CLOEXEC");
                    let resource = if resource.len() <= 4096 {
                        resource
                    } else {
                        "(Long path, see original call)"
                    };
                    self.begin(
                        task,
                        index,
                        fd,
                        Origin::Opened,
                        resource.into(),
                        Some(cloexec),
                    )?;
                }
            }
            "socket" | "accept" | "accept4" => {
                let accepting = &*event.name != "socket";
                if accepting {
                    self.used(task, index, first, infer)?;
                }
                if let Some(fd) = returned {
                    let flags = match &*event.name {
                        "socket" => b,
                        "accept4" => d,
                        _ => "",
                    };
                    let origin = if accepting {
                        Origin::Accepted
                    } else {
                        Origin::Created
                    };
                    self.begin(
                        task,
                        index,
                        fd,
                        origin,
                        fd.label.unwrap_or("Socket").into(),
                        Some(has_flag(flags, "SOCK_CLOEXEC")),
                    )?;
                }
            }
            "pipe" | "pipe2" | "socketpair" if okay => {
                let socket = &*event.name == "socketpair";
                let (pair, flags, default_label, flag) = if socket {
                    (d, b, "Socket pair", "SOCK_CLOEXEC")
                } else {
                    (
                        a,
                        if &*event.name == "pipe2" { b } else { "" },
                        "Pipe",
                        "O_CLOEXEC",
                    )
                };
                if let Some(pair) = pair
                    .strip_prefix('[')
                    .and_then(|value| value.strip_suffix(']'))
                {
                    for fd in argument_values(pair).filter_map(descriptor).take(2) {
                        self.begin(
                            task,
                            index,
                            fd,
                            Origin::Created,
                            fd.label.unwrap_or(default_label).into(),
                            Some(has_flag(flags, flag)),
                        )?;
                    }
                }
            }
            "dup" | "dup2" | "dup3" if okay => {
                self.duplicate(
                    task,
                    index,
                    first,
                    returned,
                    &*event.name == "dup3" && has_flag(c, "O_CLOEXEC"),
                )?;
            }
            "fcntl" | "fcntl64" if okay && matches!(b, "F_DUPFD" | "F_DUPFD_CLOEXEC") => {
                self.duplicate(task, index, first, returned, b == "F_DUPFD_CLOEXEC")?;
            }
            "fcntl" | "fcntl64" if okay && matches!(b, "F_SETFD" | "F_GETFD") => {
                if let Some(fd) = first {
                    self.used(task, index, Some(fd), true)?;
                    let table = self.table(task);
                    let flags = if b == "F_SETFD" { c } else { result };
                    self.flag(table, fd.number, index, fd_cloexec(flags))?;
                }
            }
            "close" => {
                if let Some(fd) = first {
                    let known_error = matches!(
                        event.error.as_deref(),
                        Some("EINTR" | "ENOSPC" | "EIO" | "EDQUOT")
                    );
                    if okay || known_error {
                        self.existing(task, index, fd, okay || fd.label.is_some())?;
                        let table = self.table(task);
                        self.close_slot(
                            table,
                            fd.number,
                            index,
                            if okay {
                                End::Closed
                            } else {
                                End::ClosedWithError
                            },
                        )?;
                    } else if event.error.as_deref() == Some("EBADF") {
                        let table = self.table(task);
                        self.close_slot(table, fd.number, index, End::Invalid)?;
                    } else {
                        self.used(task, index, Some(fd), false)?;
                    }
                }
            }
            "close_range" if okay => self.range(task, index, a, b, c)?,
            "sendfile" | "sendfile64" | "tee" => {
                self.used(task, index, first, infer)?;
                self.used(task, index, descriptor(b), infer)?;
            }
            "splice" | "copy_file_range" => {
                self.used(task, index, first, infer)?;
                self.used(task, index, descriptor(c), infer)?;
            }
            "mmap" | "mmap2" => {
                self.used(task, index, args.next().and_then(descriptor), infer)?;
            }
            syscall if descriptor_input(syscall) => {
                self.used(task, index, first, infer)?;
            }
            _ => (),
        }
        Ok(())
    }
}

pub(super) fn descriptor_input(syscall: &str) -> bool {
    matches!(
        syscall,
        "read"
            | "write"
            | "readv"
            | "writev"
            | "pread64"
            | "pwrite64"
            | "preadv"
            | "pwritev"
            | "preadv2"
            | "pwritev2"
            | "lseek"
            | "getdents"
            | "getdents64"
            | "fstat"
            | "fstat64"
            | "ftruncate"
            | "ftruncate64"
            | "fallocate"
            | "fchdir"
            | "fchmod"
            | "fchown"
            | "fsync"
            | "fdatasync"
            | "fcntl"
            | "fcntl64"
            | "flock"
            | "ioctl"
            | "connect"
            | "bind"
            | "listen"
            | "shutdown"
            | "getsockname"
            | "getpeername"
            | "setsockopt"
            | "getsockopt"
            | "recvfrom"
            | "recvmsg"
            | "recvmmsg"
            | "sendto"
            | "sendmsg"
            | "sendmmsg"
            | "faccessat"
            | "faccessat2"
            | "newfstatat"
            | "fstatat64"
            | "statx"
            | "unlinkat"
            | "mkdirat"
            | "readlinkat"
            | "vmsplice"
    )
}

fn upper_bound(value: &str) -> Option<u32> {
    match value {
        "-1" | "-1U" | "~0U" | "UINT_MAX" => Some(u32::MAX),
        value => value.parse().ok(),
    }
}

fn has_flag(value: &str, flag: &str) -> bool {
    value
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|word| word == flag)
}

fn fd_cloexec(flags: &str) -> bool {
    has_flag(flags, "FD_CLOEXEC")
        || flags.split_ascii_whitespace().next().is_some_and(|first| {
            let value = first.strip_prefix("0x").map_or_else(
                || first.parse::<u64>().ok(),
                |hex| u64::from_str_radix(hex, 16).ok(),
            );
            value.is_some_and(|value| value & 1 != 0)
        })
}

fn clone_shares_files(arguments: &str) -> Option<bool> {
    let flags = arguments
        .split_once("flags=")?
        .1
        .split([',', '}'])
        .next()?
        .trim();
    if let Some(hex) = flags.strip_prefix("0x") {
        return Some(u64::from_str_radix(hex, 16).ok()? & 0x400 != 0);
    }
    if let Ok(number) = flags.parse::<u64>() {
        return Some(number & 0x400 != 0);
    }
    (!flags.is_empty()
        && flags
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_|".contains(&byte)))
    .then(|| has_flag(flags, "CLONE_FILES"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> Trace {
        Trace::parse(vec![super::super::Source {
            path: "fixture.strace".into(),
            pid: None,
            bytes: text.as_bytes().into(),
        }])
        .unwrap()
    }

    #[test]
    fn reuse_separates_files_and_other_closes_keep_their_evidence() {
        let trace = parsed(concat!(
            "80 openat(AT_FDCWD, \"/old\", O_RDONLY) = 3</old>\n",
            "80 read(3</old>, \"a\", 1) = 1\n",
            "80 close(3</old>) = -1 EIO (Input/output error)\n",
            "80 openat(AT_FDCWD, \"/new\", O_WRONLY) = 3</new>\n",
            "80 write(3</new>, \"b\", 1) = 1\n",
            "80 close(100) = -1 EBADF (Bad file descriptor)\n",
            "80 close(3</new>) = 0\n",
        ));
        let report = trace.descriptor_report();
        assert_eq!(report.search(&trace, "FD 3"), [0, 1]);
        let [old, new] = report.lifetimes.as_slice() else {
            panic!()
        };
        assert_eq!(
            (&*old.resource, old.status()),
            ("/old", "Closed with an error")
        );
        assert_eq!((&*new.resource, new.status()), ("/new", "Closed"));
        assert_ne!(old.family, new.family);
        assert_eq!(
            old.steps.iter().map(|step| step.event).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(
            new.steps.iter().map(|step| step.event).collect::<Vec<_>>(),
            [3, 4, 6]
        );
    }

    #[test]
    fn links_a_shared_table_before_a_resumed_clone_and_splits_on_exec() {
        let trace = parsed(concat!(
            "100 openat(AT_FDCWD, \"/initial\", O_RDONLY) = 3</initial>\n",
            "100 dup3(3</initial>, 8, O_CLOEXEC) = 8</initial>\n",
            "100 clone(child_stack=NULL, flags=CLONE_FILES|SIGCHLD <unfinished ...>\n",
            "101 close(3</initial>) = 0\n",
            "100 <... clone resumed>) = 101\n",
            "100 openat(AT_FDCWD, \"/next\", O_RDWR) = 3</next>\n",
            "101 execve(\"/usr/bin/writer\", [\"writer\"], NULL) = 0\n",
            "101 write(3</next>, \"x\", 1) = 1\n",
            "101 +++ exited with 0 +++\n",
            "100 write(8</initial>, \"y\", 1) = -1 EBADF (Bad file descriptor)\n",
            "100 close(8</initial>) = 0\n",
            "100 close(3</next>) = 0\n",
        ));
        let report = trace.descriptor_report();
        let [first, alias, next, next_in_child, closed_in_child] = report.lifetimes.as_slice()
        else {
            panic!()
        };
        assert_eq!(
            first.steps.last().map(|step| trace.events[step.event].pid),
            Some(Some(101))
        );
        assert_eq!(first.status(), "Closed");
        assert_eq!(alias.status(), "Closed");
        assert_eq!(next.status(), "Closed");
        assert_eq!(next_in_child.status(), "Released at process exit");
        assert_eq!(closed_in_child.status(), "Closed on exec");
        assert_eq!(closed_in_child.family, alias.family);
        assert_eq!(next_in_child.family, next.family);
        assert_eq!(report.search(&trace, "lifetime 1"), [0, 1, 4]);
        assert_eq!(report.search(&trace, "PID 101"), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn copies_a_forked_table_and_applies_close_range_and_flag_changes() {
        let trace = parsed(concat!(
            "200 pipe2([3<pipe:[77]>, 4<pipe:[77]>], O_CLOEXEC) = 0\n",
            "200 socket(AF_INET, SOCK_STREAM|SOCK_CLOEXEC, IPPROTO_TCP) = 5<TCP:[42]>\n",
            "200 fork() = 201\n",
            "201 fcntl(3<pipe:[77]>, F_SETFD, 0) = 0\n",
            "201 close_range(4, 4294967295, CLOSE_RANGE_CLOEXEC) = 0\n",
            "201 execve(\"/bin/cat\", [\"cat\"], NULL) = 0\n",
            "201 read(3<pipe:[77]>, \"\", 1) = 0\n",
            "201 +++ exited with 0 +++\n",
            "200 fcntl(5<TCP:[42]>, F_DUPFD_CLOEXEC, 9) = 9<TCP:[42]>\n",
            "200 close_range(3, ~0U, 0) = 0\n",
        ));
        let report = trace.descriptor_report();
        assert_eq!(report.lifetimes.len(), 7);
        assert_eq!(report.search(&trace, "pipe:[77]"), [0, 1, 3, 4]);
        assert_eq!(report.search(&trace, "lifetime 3"), [2, 5, 6]);
        assert_eq!(
            report
                .lifetimes
                .iter()
                .map(Lifetime::status)
                .collect::<Vec<_>>(),
            [
                "Closed",
                "Closed",
                "Closed",
                "Released at process exit",
                "Closed on exec",
                "Closed on exec",
                "Closed",
            ]
        );
        assert_eq!(
            report.lifetimes[3].steps[0].label(),
            "Inherited from lifetime 1"
        );
        assert!(
            report.lifetimes[3]
                .steps
                .iter()
                .any(|step| step.label() == "Close-on-exec cleared")
        );
    }

    #[test]
    fn leaves_a_mid_trace_resource_incomplete_and_honors_implicit_dup_close() {
        let trace = parsed(concat!(
            "80 read(4</inherited>, \"a\", 1) = 1\n",
            "80 openat(AT_FDCWD, \"/replace\", O_RDWR) = 7</replace>\n",
            "80 dup2(4</inherited>, 7</replace>) = 7</inherited>\n",
            "80 write(7</inherited>, \"b\", 1) = 1\n",
        ));
        let report = trace.descriptor_report();
        let [unknown, replaced, alias] = report.lifetimes.as_slice() else {
            panic!()
        };
        assert_eq!(
            unknown.steps[0].label(),
            "First observed, open not captured"
        );
        assert_eq!(replaced.status(), "Replaced by duplication");
        assert_eq!(alias.status(), "No closing event captured");
        assert_eq!(alias.family, unknown.family);
    }
}
