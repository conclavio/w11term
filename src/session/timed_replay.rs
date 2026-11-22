//! A bounded producer reads and verifies the capture away from the pane's
//! parser thread. The pane owns the playback clock and stays usable while idle.
use crate::recorder::{ReplayEvent, TimedReplay};
use anyhow::Result;
use std::{path::PathBuf, time::Duration};
use tokio::{sync::mpsc, time::Instant};
use wezterm_term::TerminalSize;

enum Record {
    Initial(TerminalSize),
    Timed(Duration, Option<ReplayEvent>),
}

pub(super) struct Playback {
    pub path: PathBuf,
    records: mpsc::Receiver<Result<Record>>,
    started: Option<Instant>,
    pending: Option<(Instant, Option<ReplayEvent>)>,
}

impl Playback {
    pub fn open(path: PathBuf) -> Result<Self> {
        let (sender, records) = mpsc::channel(1);
        let input = path.clone();
        std::thread::Builder::new()
            .name("vt-recording-reader".into())
            .spawn(move || {
                let read = || -> Result<()> {
                    let mut replay = TimedReplay::open(&input)?;
                    if sender
                        .blocking_send(Ok(Record::Initial(replay.initial_size())))
                        .is_err()
                    {
                        return Ok(());
                    }
                    while let Some((elapsed, event)) = replay.next_event()? {
                        if sender
                            .blocking_send(Ok(Record::Timed(elapsed, Some(event))))
                            .is_err()
                        {
                            return Ok(());
                        }
                    }
                    // Finishing a capture can occur during silence, even before its first output.
                    let _ = sender.blocking_send(Ok(Record::Timed(replay.duration(), None)));
                    Ok(())
                };
                if let Err(error) = read() {
                    let _ = sender.blocking_send(Err(error));
                }
            })?;
        Ok(Self {
            path,
            records,
            started: None,
            pending: None,
        })
    }

    /// Called in select! with input, shutdown, search and presentation timers.
    /// Keep the event while asleep so cancelling the wait cannot skip it.
    pub async fn next(&mut self) -> Option<Result<ReplayEvent>> {
        if self.pending.is_none() {
            let Some(record) = self.records.recv().await else {
                return Some(Err(anyhow::anyhow!(
                    "timed recording reader stopped unexpectedly"
                )));
            };
            match record {
                Ok(Record::Initial(size)) => {
                    self.started = Some(Instant::now());
                    return Some(Ok(ReplayEvent::Resize(size)));
                }
                Ok(Record::Timed(elapsed, event)) => {
                    let started = self
                        .started
                        .expect("the reader sends geometry before output");
                    let Some(deadline) = started.checked_add(elapsed) else {
                        return Some(Err(anyhow::anyhow!("recording timestamp is out of range")));
                    };
                    self.pending = Some((deadline, event));
                }
                Err(error) => return Some(Err(error)),
            }
        }
        let (deadline, _) = self.pending.as_ref()?;
        if *deadline > Instant::now() {
            tokio::time::sleep_until(*deadline).await;
        }
        self.pending.take().and_then(|(_, event)| event.map(Ok))
    }
}

#[cfg(test)]
mod tests;
