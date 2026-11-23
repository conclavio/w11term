use super::*;
use std::sync::Mutex;
use tokio::{sync::mpsc, task::JoinSet};
use w11term::sftp::{DEFAULT_TIMEOUT, Job, Operation, TransferCancel, TransferProgress};

const PARALLEL_HOSTS: usize = 4;

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

    pub(super) fn caption(&self, progress: TransferProgress) -> String {
        match self {
            Self::Queued => "Queued".into(),
            Self::Active => match progress.total {
                Some(total) => format!("Uploading · {} of {total} bytes", progress.bytes),
                None => "Opening SFTP…".into(),
            },
            Self::Complete(bytes) => format!("Uploaded · {bytes} bytes"),
            Self::Cancelled {
                bytes,
                partial: true,
            } => {
                format!("Cancelled after {bytes} bytes · a partial file may remain")
            }
            Self::Cancelled { partial: false, .. } => "Cancelled before upload".into(),
            Self::Unavailable(reason) => (*reason).into(),
            Self::Failed(error) => format!("Upload failed · {error}"),
        }
    }
}

pub(super) struct Task {
    pub index: usize,
    pub generation: u64,
    pub input: mpsc::UnboundedSender<Command>,
    pub progress: watch::Sender<TransferProgress>,
}

#[derive(Clone)]
pub(super) struct Batch {
    states: Arc<Mutex<Vec<Outcome>>>,
    pub cancel: TransferCancel,
}

impl Batch {
    pub(super) fn new(states: Vec<Outcome>) -> Self {
        Self {
            states: Arc::new(Mutex::new(states)),
            cancel: TransferCancel::default(),
        }
    }

    pub(super) fn snapshot(&self) -> Vec<Outcome> {
        self.states.lock().unwrap().clone()
    }

    pub(super) fn fail_pending(&self, message: &str) {
        for state in &mut *self.states.lock().unwrap() {
            if state.pending() {
                *state = Outcome::Failed(message.to_owned());
            }
        }
    }

    pub(super) fn upload(self, tasks: Vec<Task>, source: GroupUploadFile, remote: String) {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                self.fail_pending(&format!("Could not start the uploader · {error}"));
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
                active.spawn(batch.upload_one(task, source.clone(), remote.clone()));
            }
            while active.join_next().await.is_some() {}
        });
        self.fail_pending("The uploader stopped before the destination was confirmed.");
    }

    async fn upload_one(self, task: Task, source: GroupUploadFile, remote: String) {
        let outcome = if self.cancel.is_cancelled() {
            Outcome::Cancelled {
                bytes: 0,
                partial: false,
            }
        } else {
            self.states.lock().unwrap()[task.index] = Outcome::Active;
            self.send(task.input, task.generation, task.progress, source, remote)
                .await
        };
        self.states.lock().unwrap()[task.index] = outcome;
    }

    async fn send(
        &self,
        input: mpsc::UnboundedSender<Command>,
        generation: u64,
        progress: watch::Sender<TransferProgress>,
        source: GroupUploadFile,
        remote: String,
    ) -> Outcome {
        let (reply, receive) = oneshot::channel();
        let job = Job {
            expected_generation: Some(generation),
            operation: Operation::PutGroupFile { source, remote },
            deadline: tokio::time::Instant::now() + Duration::from_secs(DEFAULT_TIMEOUT),
            reply,
            progress: Some(progress),
            cancel: Some(self.cancel.clone()),
        };
        if input.send(Command::Sftp(job)).is_err() {
            return Outcome::Unavailable("The pane closed before upload.");
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
            Err(_) => Outcome::Failed("The SSH session closed before upload confirmation.".into()),
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
                generation: index as u64 + 1,
                input: input.clone(),
                progress: watch::channel(TransferProgress::default()).0,
            })
            .collect();
        drop(input);
        let batch = Batch::new(vec![Outcome::Queued; count]);
        let worker = batch.clone();
        let run = tokio::task::spawn_blocking(move || {
            worker.upload(tasks, source, "/incoming/file".into());
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
