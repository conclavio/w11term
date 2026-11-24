use super::*;
use anyhow::Context;
use std::{
    fs::File,
    io::{self, BufWriter, Seek, Write},
    path::{Path, PathBuf},
};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use w11term::events::{
    GroupRecordingEnd, GroupRecordingMode, GroupRecordingOutcome, GroupRecordingSummary,
    GroupTranscriptLine, GroupTranscriptRecording, GroupTranscriptStream,
};

pub(super) mod saved;

#[derive(Default)]
pub(super) enum State {
    #[default]
    Idle,
    Picking,
    Writing {
        path: PathBuf,
        worker: watch::Receiver<WriterStatus>,
        recording: Option<GroupTranscriptRecording>,
    },
    Failed(String),
}

pub(super) enum WriterStatus {
    Opening,
    Recording,
    Finished(Result<(), String>),
}

impl State {
    pub(super) fn start(
        path: PathBuf,
        transcript: &GroupTranscript,
        label: Arc<str>,
        names: BTreeMap<u64, String>,
        mode: GroupRecordingMode,
        open: impl FnOnce(&Path) -> anyhow::Result<File> + Send + 'static,
    ) -> Result<Self, String> {
        let (recording, stream) = transcript
            .start_recording(mode)
            .ok_or("This output already has a recorder")?;
        let (status, worker) = watch::channel(WriterStatus::Opening);
        let destination = path.clone();
        // Keep long-running writes off the bounded Reactor worker pool.
        std::thread::Builder::new()
            .name("w11term group output recorder".into())
            .spawn(move || {
                let result = open(&destination).and_then(|file| {
                    write_file(file, &destination, &label, &names, stream, || {
                        status.send_replace(WriterStatus::Recording);
                    })
                });
                status.send_replace(WriterStatus::Finished(
                    result.map_err(|error| format!("{error:#}")),
                ));
            })
            .map_err(|error| format!("Could not start the file writer · {error}"))?;
        Ok(Self::Writing {
            path,
            worker,
            recording: Some(recording),
        })
    }

    pub(super) fn poll(&mut self) -> Option<PathBuf> {
        let Self::Writing { path, worker, .. } = self else {
            return None;
        };
        let closed = worker.has_changed().is_err();
        let result = match &*worker.borrow() {
            WriterStatus::Finished(result) => Some(result.clone().map(|()| path.clone())),
            _ if closed => Some(Err("The file writer stopped unexpectedly".into())),
            _ => None,
        };
        match result? {
            Ok(path) => {
                *self = Self::Idle;
                Some(path)
            }
            Err(error) => {
                *self = Self::Failed(error);
                None
            }
        }
    }

    fn status_view(&self) -> View {
        let text = match self {
            Self::Idle => return View::empty(),
            Self::Picking => "Choosing an output file…".into(),
            Self::Writing {
                recording: None, ..
            } => "Finishing the output recording…".into(),
            Self::Writing { path, worker, .. }
                if matches!(*worker.borrow(), WriterStatus::Recording) =>
            {
                format!("Recording live output to {}", path.display())
            }
            Self::Writing { .. } => "Opening the output file…".into(),
            Self::Failed(error) => format!("Could not record live output · {error}"),
        };
        TextBlock::new()
            .text(text)
            .text_wrapping(TextWrapping::Wrap)
            .into()
    }
}

pub struct PreparedFile {
    path: PathBuf,
    file: File,
}

impl PreparedFile {
    pub(super) fn open(path: PathBuf) -> anyhow::Result<Self> {
        let mut file = create_file(&path)?;
        // If the approved scope changes before dispatch, leave a clear status
        // at the chosen destination. Only a dispatched run replaces it.
        file.write_all(b"[No command was sent]\r\n")
            .context("Preparing the output recording")?;
        Ok(Self { path, file })
    }

    pub(super) fn start(
        mut self,
        transcript: &GroupTranscript,
        label: Arc<str>,
        names: BTreeMap<u64, String>,
    ) -> Result<State, String> {
        State::start(
            self.path,
            transcript,
            label,
            names,
            GroupRecordingMode::UntilSettled,
            move |_| {
                self.file
                    .rewind()
                    .context("Starting the output recording")?;
                self.file
                    .set_len(0)
                    .context("Starting the output recording")?;
                Ok(self.file)
            },
        )
    }
}

pub enum Action {
    Pick,
    Picked(Result<Option<PathBuf>, String>),
    Stop,
    Saved(saved::Action),
}

pub(super) fn status_view(
    id: TabId,
    submission: &Submission,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    StackPanel::new().children((
        submission.recording.status_view(),
        submission.saved_recordings.view(id, submission.id, ctx),
    ))
}

fn ready(submission: &Submission) -> bool {
    submission
        .results
        .iter()
        .any(|delivery| matches!(delivery.status, DeliveryStatus::Sent))
        && submission.results.iter().all(|delivery| {
            !matches!(
                delivery.status,
                DeliveryStatus::Queued(_) | DeliveryStatus::Pending(..)
            )
        })
}

pub(super) fn names<'a>(targets: impl Iterator<Item = &'a Target>) -> BTreeMap<u64, String> {
    targets
        .map(|target| (target.pane, target.name.replace(['\r', '\n'], " ")))
        .collect()
}

pub(super) fn button(id: TabId, submission: &Submission, ctx: &ViewContext<TerminalApp>) -> View {
    let run = submission.id;
    match &submission.recording {
        State::Writing { recording, .. } => Button::new()
            .is_enabled(recording.is_some())
            .on_click(ctx.callback(move |_| {
                Message::HostGroup(super::Action::RecordOutput(id, run, Action::Stop))
            }))
            .content("Stop recording"),
        state => Button::new()
            .is_enabled(!matches!(state, State::Picking) && ready(submission))
            .on_click(ctx.callback(move |_| {
                Message::HostGroup(super::Action::RecordOutput(id, run, Action::Pick))
            }))
            .content("Record output…"),
    }
}

fn write_line(
    file: &mut impl Write,
    names: &BTreeMap<u64, String>,
    line: GroupTranscriptLine,
) -> io::Result<()> {
    if let Some(host) = names.get(&line.pane) {
        let suffix = if line.shortened {
            " [line shortened]"
        } else {
            ""
        };
        writeln!(file, "{host} · {}{suffix}\r", line.text)?;
    }
    Ok(())
}

fn write_outcomes(
    file: &mut impl Write,
    names: &BTreeMap<u64, String>,
    summary: GroupRecordingSummary,
) -> io::Result<()> {
    if !summary.hosts.is_empty() {
        writeln!(file, "[Host outcomes]\r")?;
    }
    for (pane, outcome) in summary.hosts {
        let Some(host) = names.get(&pane) else {
            continue;
        };
        let status = match outcome {
            GroupRecordingOutcome::AwaitingDelivery => "Awaiting delivery".into(),
            GroupRecordingOutcome::DeliveryUnconfirmed => "Delivery unconfirmed".into(),
            GroupRecordingOutcome::Rejected(reason) => format!("Not sent · {reason}"),
            GroupRecordingOutcome::Sent(None) => "Sent to SSH channel · result unavailable".into(),
            GroupRecordingOutcome::Sent(Some(execution)) => {
                format!("Sent to SSH channel · {}", execution_caption(&execution))
            }
        };
        writeln!(file, "{host} · {status}\r")?;
    }
    let ending = match summary.end {
        GroupRecordingEnd::Settled => "[Group run settled]",
        GroupRecordingEnd::Stopped => "[Recording stopped]",
    };
    writeln!(file, "{ending}\r")
}

fn write_output(
    file: &mut impl Write,
    label: &str,
    names: &BTreeMap<u64, String>,
    mut stream: GroupTranscriptStream,
    opened: impl FnOnce(),
) -> io::Result<()> {
    writeln!(file, "Submitted · {label}\r")?;
    if stream.history.omitted > 0 {
        writeln!(
            file,
            "[{} earlier output lines were no longer in the live view]\r",
            stream.history.omitted
        )?;
    }
    for line in stream.history.lines {
        write_line(file, names, line)?;
    }
    file.flush()?;
    opened();
    loop {
        let next = match stream.incoming.try_recv() {
            Ok(line) => Ok(line),
            Err(TryRecvError::Empty) => {
                file.flush()?;
                stream.incoming.blocking_recv()
            }
            Err(TryRecvError::Lagged(count)) => Err(RecvError::Lagged(count)),
            Err(TryRecvError::Closed) => Err(RecvError::Closed),
        };
        match next {
            Ok(line) => write_line(file, names, line)?,
            Err(RecvError::Lagged(count)) => {
                writeln!(file, "[Recording writer skipped {count} output lines]\r")?;
            }
            Err(RecvError::Closed) => break,
        }
    }
    let summary = stream.ended.try_recv().unwrap_or(GroupRecordingSummary {
        end: GroupRecordingEnd::Stopped,
        hosts: Vec::new(),
    });
    write_outcomes(file, names, summary)?;
    file.flush()
}

fn create_file(path: &Path) -> anyhow::Result<File> {
    File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("Creating a new file at {}", path.display()))
}

fn write_file(
    file: File,
    path: &Path,
    label: &str,
    names: &BTreeMap<u64, String>,
    stream: GroupTranscriptStream,
    opened: impl FnOnce(),
) -> anyhow::Result<()> {
    let mut file = BufWriter::new(file);
    write_output(&mut file, label, names, stream, opened)
        .with_context(|| format!("Writing {}. The file may be incomplete", path.display()))?;
    file.get_ref()
        .sync_all()
        .with_context(|| format!("Saving {}", path.display()))?;
    Ok(())
}

impl TerminalApp {
    fn output_recording_submission(&mut self, id: TabId, run: u64) -> Option<&mut Submission> {
        let Page::HostGroup(page) = &mut self.tabs.get_mut(id)?.page else {
            return None;
        };
        page.submissions
            .iter_mut()
            .find(|submission| submission.id == run)
    }

    pub(super) fn handle_group_output_recording(
        &mut self,
        id: TabId,
        run: u64,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        match action {
            Action::Pick => self.pick_group_output_recording(id, run, ctx),
            Action::Picked(result) => {
                let Some(submission) = self
                    .output_recording_submission(id, run)
                    .filter(|submission| matches!(submission.recording, State::Picking))
                else {
                    return;
                };
                match result {
                    Ok(Some(path)) => self.start_group_output_writer(id, run, path),
                    Ok(None) => submission.recording = State::Idle,
                    Err(error) => submission.recording = State::Failed(error),
                }
            }
            Action::Stop if self.tabs.active_id() == Some(id) => {
                if let Some(submission) = self.output_recording_submission(id, run)
                    && let State::Writing { recording, .. } = &mut submission.recording
                {
                    *recording = None;
                }
            }
            Action::Stop => {}
            Action::Saved(action) => self.handle_saved_recording(id, run, action, ctx),
        }
    }

    fn pick_group_output_recording(&mut self, id: TabId, run: u64, ctx: &ComponentContext<Self>) {
        if self.host_group_mut(id).is_none_or(|page| {
            page.viewing != run
                || page
                    .live_output
                    .as_ref()
                    .is_none_or(|output| output.submission != run)
        }) {
            return;
        }
        let owner = self.keyboard.hwnd().map(|owner| owner.0 as usize);
        let Some(submission) = self
            .output_recording_submission(id, run)
            .filter(|submission| {
                matches!(submission.recording, State::Idle | State::Failed(_)) && ready(submission)
            })
        else {
            return;
        };
        let Some(owner) = owner else {
            submission.recording = State::Failed("The window is unavailable".into());
            return;
        };
        submission.recording = State::Picking;
        let reply = move |result| {
            Message::HostGroup(super::Action::RecordOutput(id, run, Action::Picked(result)))
        };
        ctx.spawn_background_with_rejection(
            move |cancel| {
                if cancel.is_cancelled() {
                    return reply(Ok(None));
                }
                let selected = file_picker::pick(
                    owner,
                    file_picker::Purpose::GroupLiveRecording(format!("ssh-group-output-{run}.txt")),
                );
                reply(
                    selected
                        .map(|file| file.map(|file| file.path))
                        .map_err(|error| format!("{error:#}")),
                )
            },
            reply(Err("File picker unavailable".into())),
        );
    }

    fn start_group_output_writer(&mut self, id: TabId, run: u64, path: PathBuf) {
        let Some(submission) = self.output_recording_submission(id, run) else {
            return;
        };
        let targets = submission.results.iter().map(|delivery| &delivery.target);
        submission.recording = State::start(
            path,
            &submission.transcript,
            submission.content.label().into(),
            names(targets),
            GroupRecordingMode::Manual,
            create_file,
        )
        .unwrap_or_else(State::Failed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast;
    use w11term::events::GroupTranscriptSnapshot;

    #[test]
    fn writes_host_order_gaps_and_a_stop_marker_and_preserves_existing_files() {
        let line = |id, pane, text, shortened| GroupTranscriptLine {
            id,
            pane,
            text: Arc::from(text),
            shortened,
        };
        let (sender, incoming) = broadcast::channel(2);
        for entry in [
            line(5, 1, "discarded", false),
            line(6, 3, "unacknowledged host", false),
            line(7, 2, "尾", true),
        ] {
            sender.send(entry).unwrap();
        }
        drop(sender);
        let (finish, ended) = oneshot::channel();
        finish
            .send(GroupRecordingSummary {
                end: GroupRecordingEnd::Stopped,
                hosts: vec![
                    (1, GroupRecordingOutcome::DeliveryUnconfirmed),
                    (
                        2,
                        GroupRecordingOutcome::Sent(Some(GroupExecution::PromptReturned)),
                    ),
                ],
            })
            .unwrap();
        let stream = GroupTranscriptStream {
            history: GroupTranscriptSnapshot {
                omitted: 2,
                lines: [
                    line(3, 2, "west first", false),
                    line(4, 1, "east next", false),
                ]
                .into(),
            },
            incoming,
            ended,
        };
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("output.txt");
        let names = [(1, "East".into()), (2, "West".into())].into();
        write_file(
            create_file(&path).unwrap(),
            &path,
            "show",
            &names,
            stream,
            || {},
        )
        .unwrap();
        let recorded = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            recorded,
            concat!(
                "Submitted · show\r\n[2 earlier output lines were no longer in the live view]\r\n",
                "West · west first\r\nEast · east next\r\n[Recording writer skipped 1 output lines]\r\n",
                "West · 尾 [line shortened]\r\n[Host outcomes]\r\nEast · Delivery unconfirmed\r\n",
                "West · Sent to SSH channel · Prompt returned · exit status unavailable\r\n[Recording stopped]\r\n",
            ),
        );
        assert!(PreparedFile::open(path.clone()).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), recorded);
    }
}
