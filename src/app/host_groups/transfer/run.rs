use super::*;
use std::sync::Mutex;
use tokio::{sync::mpsc, task::JoinSet};
use w11term::sftp::{
    DEFAULT_TIMEOUT, GroupDownloadFolder, Job, Operation, TransferCancel, TransferProgress,
};

const PARALLEL_HOSTS: usize = 4;
const CANCELLED_BEFORE_START: Outcome = Outcome::Cancelled {
    bytes: 0,
    partial: false,
};

#[derive(Clone)]
pub(super) enum Outcome {
    Queued,
    Active,
    Complete(u64),
    Cancelled { bytes: u64, partial: bool },
    Unavailable(&'static str),
    Failed(String),
}

impl Outcome {
    pub(super) fn pending(&self) -> bool {
        matches!(self, Self::Queued | Self::Active)
    }

    pub(super) fn caption(&self, direction: Direction, progress: TransferProgress) -> String {
        let (active, complete, cancelled) = match direction {
            Direction::Upload => ("Uploading", "Uploaded", "Cancelled before upload"),
            Direction::Download => ("Downloading", "Downloaded", "Cancelled before download"),
        };
        match self {
            Self::Queued => "Queued".into(),
            Self::Active => match progress.total {
                Some(total) => format!("{active} · {} of {total} bytes", progress.bytes),
                None if progress.bytes > 0 => format!("{active} · {} bytes", progress.bytes),
                None => "Opening SFTP…".into(),
            },
            Self::Complete(bytes) => format!("{complete} · {bytes} bytes"),
            Self::Cancelled {
                bytes,
                partial: true,
            } => {
                format!("Cancelled after {bytes} bytes · a partial file may remain")
            }
            Self::Cancelled {
                bytes,
                partial: false,
            } if direction == Direction::Download && *bytes > 0 => {
                format!("Cancelled · discarded {bytes} bytes")
            }
            Self::Cancelled { partial: false, .. } => cancelled.into(),
            Self::Unavailable(reason) => (*reason).into(),
            Self::Failed(error) => format!("{} failed · {error}", direction.name()),
        }
    }
}

pub(super) struct Task {
    pub index: usize,
    pub pane: u64,
    pub generation: u64,
    pub input: mpsc::UnboundedSender<Command>,
    pub progress: watch::Sender<TransferProgress>,
}

enum Prepared {
    Upload(GroupUploadFile),
    Download(GroupDownloadFolder),
}

impl Prepared {
    fn operation(&self, pane: u64, remote: &str) -> Operation {
        match self {
            Self::Upload(source) => Operation::PutGroupFile {
                source: source.clone(),
                remote: remote.to_owned(),
            },
            Self::Download(folder) => Operation::Get {
                remote: remote.to_owned(),
                local: folder.destination(pane, remote),
                overwrite: false,
                max_bytes: None,
            },
        }
    }
}

struct Progress {
    outcomes: Vec<Outcome>,
    folder: Option<PathBuf>,
}

#[derive(Clone)]
pub(super) struct Batch {
    progress: Arc<Mutex<Progress>>,
    pub cancel: TransferCancel,
}

impl Batch {
    pub(super) fn new(outcomes: Vec<Outcome>) -> Self {
        Self {
            progress: Arc::new(Mutex::new(Progress {
                outcomes,
                folder: None,
            })),
            cancel: TransferCancel::default(),
        }
    }

    pub(super) fn snapshot(&self) -> Vec<Outcome> {
        self.progress.lock().unwrap().outcomes.clone()
    }

    pub(super) fn folder(&self) -> Option<PathBuf> {
        self.progress.lock().unwrap().folder.clone()
    }

    pub(super) fn fail_pending(&self, message: &str) {
        for state in &mut self.progress.lock().unwrap().outcomes {
            if state.pending() {
                *state = Outcome::Failed(message.to_owned());
            }
        }
    }

    pub(super) fn run(self, tasks: Vec<Task>, local: Local, remote: String) {
        if self.cancel.is_cancelled() {
            let mut progress = self.progress.lock().unwrap();
            for task in tasks {
                progress.outcomes[task.index] = CANCELLED_BEFORE_START;
            }
            return;
        }
        let prepared = match local {
            Local::Upload(source) => Prepared::Upload(source),
            Local::Download(parent) => match GroupDownloadFolder::create(&parent) {
                Ok(folder) => {
                    self.progress.lock().unwrap().folder = Some(folder.path().to_owned());
                    Prepared::Download(folder)
                }
                Err(error) => {
                    self.fail_pending(&format!("{error:#}"));
                    return;
                }
            },
        };
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                self.fail_pending(&format!("Could not start the transfer · {error}"));
                return;
            }
        };
        runtime.block_on(async {
            let mut active = JoinSet::new();
            for task in tasks {
                if active.len() >= PARALLEL_HOSTS {
                    active.join_next().await;
                }
                let batch = self.clone();
                let operation = prepared.operation(task.pane, &remote);
                active.spawn(batch.run_one(task, operation));
            }
            while active.join_next().await.is_some() {}
        });
        self.fail_pending("The transfer stopped before the destination was confirmed.");
    }

    async fn run_one(self, task: Task, operation: Operation) {
        let index = task.index;
        let outcome = if self.cancel.is_cancelled() {
            CANCELLED_BEFORE_START
        } else {
            self.progress.lock().unwrap().outcomes[index] = Outcome::Active;
            self.send(task, operation).await
        };
        self.progress.lock().unwrap().outcomes[index] = outcome;
    }

    async fn send(&self, task: Task, operation: Operation) -> Outcome {
        let (reply, receive) = oneshot::channel();
        let job = Job {
            expected_generation: Some(task.generation),
            operation,
            deadline: tokio::time::Instant::now() + Duration::from_secs(DEFAULT_TIMEOUT),
            reply,
            progress: Some(task.progress),
            cancel: Some(self.cancel.clone()),
        };
        if task.input.send(Command::Sftp(job)).is_err() {
            return Outcome::Unavailable("The pane closed before transfer.");
        }
        match receive.await {
            Ok(Ok(reply)) => {
                let Some(bytes) = reply["bytes_transferred"].as_u64() else {
                    return Outcome::Failed(
                        "The SFTP worker returned no transfer byte count.".into(),
                    );
                };
                if reply["cancelled"].as_bool() == Some(true) {
                    Outcome::Cancelled {
                        bytes,
                        partial: reply["remote_may_be_partial"].as_bool() == Some(true),
                    }
                } else {
                    Outcome::Complete(bytes)
                }
            }
            Ok(Err(error)) => Outcome::Failed(error),
            Err(_) => {
                Outcome::Failed("The SSH session closed before transfer confirmation.".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelling_stops_queued_uploads_and_cancels_in_flight_jobs() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source");
        std::fs::write(&path, b"file").unwrap();
        let source = GroupUploadFile::open(path).unwrap();
        let count = PARALLEL_HOSTS + 1;
        let (input, mut commands) = mpsc::unbounded_channel();
        let tasks = (0..count)
            .map(|index| Task {
                index,
                pane: index as u64 + 10,
                generation: index as u64 + 1,
                input: input.clone(),
                progress: watch::channel(TransferProgress::default()).0,
            })
            .collect();
        drop(input);
        let batch = Batch::new(vec![Outcome::Queued; count]);
        let worker = batch.clone();
        let run = tokio::task::spawn_blocking(move || {
            worker.run(tasks, Local::Upload(source), "/incoming/file".into());
        });
        let mut active = Vec::new();
        for _ in 0..PARALLEL_HOSTS {
            let Command::Sftp(job) = tokio::time::timeout(Duration::from_secs(5), commands.recv())
                .await
                .unwrap()
                .unwrap()
            else {
                panic!("expected a group SFTP job");
            };
            assert!(
                matches!(&job.operation, Operation::PutGroupFile { remote, .. } if remote == "/incoming/file")
            );
            active.push(job);
        }
        let mut generations: Vec<_> = active
            .iter()
            .map(|job| job.expected_generation.unwrap())
            .collect();
        generations.sort_unstable();
        assert_eq!(generations, (1..=PARALLEL_HOSTS as u64).collect::<Vec<_>>());
        assert!(matches!(batch.snapshot()[PARALLEL_HOSTS], Outcome::Queued));
        batch.cancel.cancel();
        for job in active {
            assert!(job.cancel.unwrap().is_cancelled());
            job.reply
                .send(Ok(
                    serde_json::json!({"bytes_transferred": 0, "cancelled": true}),
                ))
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .unwrap()
            .unwrap();
        assert!(commands.recv().await.is_none());
        assert!(batch.snapshot().iter().all(|result| matches!(
            result,
            Outcome::Cancelled {
                bytes: 0,
                partial: false
            }
        )));
    }
}
