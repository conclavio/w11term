use super::{ArtifactMessages, Channel, client};
use anyhow::{Context, Result, ensure};
use std::sync::{Mutex, MutexGuard};
use tokio::sync::watch;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Packets,
    Syscalls,
    CpuCommands,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Packets => "packet capture",
            Self::Syscalls => "syscall capture",
            Self::CpuCommands => "CPU command capture",
        }
    }
}

#[derive(Clone)]
pub struct CaptureStop(watch::Sender<bool>);

impl Default for CaptureStop {
    fn default() -> Self {
        Self(watch::channel(false).0)
    }
}

impl CaptureStop {
    pub fn request(&self) {
        self.0.send_replace(true);
    }

    pub fn is_requested(&self) -> bool {
        *self.0.borrow()
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<bool> {
        self.0.subscribe()
    }

    pub(super) async fn ready(
        &self,
        messages: &mut ArtifactMessages,
        channel: &mut Channel<client::Msg>,
    ) -> Result<String> {
        let mut requested = self.subscribe();
        tokio::select! {
            biased;
            line = messages.line(channel) => line,
            _ = async { let _ = requested.wait_for(|&stop| stop).await; } => {
                channel.data(b"stop\n".as_slice()).await?;
                messages.line(channel).await
            }
        }
    }
}

struct Active {
    kind: Kind,
    generation: u64,
    pane: u64,
    job: String,
    stop: CaptureStop,
}

#[derive(Default)]
pub(in crate::ssh) struct ActiveJobs(Mutex<Option<Active>>);

impl ActiveJobs {
    fn lock(&self) -> MutexGuard<'_, Option<Active>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn register(
        &self,
        kind: Kind,
        generation: u64,
        pane: u64,
        job: &str,
        stop: &CaptureStop,
    ) -> Result<Registration<'_>> {
        let mut active = self.lock();
        ensure!(
            active.is_none(),
            "A diagnostic capture is active on this SSH transport"
        );
        *active = Some(Active {
            kind,
            generation,
            pane,
            job: job.into(),
            stop: stop.clone(),
        });
        Ok(Registration(self))
    }

    pub(super) fn stop(&self, kind: Kind, generation: u64, pane: u64, job: &str) -> Result<()> {
        let active = self.lock();
        let active = active
            .as_ref()
            .filter(|active| {
                active.kind == kind
                    && active.generation == generation
                    && active.pane == pane
                    && active.job == job
            })
            .with_context(|| {
                format!("No matching running {} on the selected pane", kind.label())
            })?;
        active.stop.request();
        Ok(())
    }
}

pub(super) struct Registration<'a>(&'a ActiveJobs);

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        *self.0.lock() = None;
    }
}

pub(super) fn validate_job(job: &str) -> Result<&str> {
    ensure!(
        job.len() == 32
            && job
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "Use a 32-character lowercase hexadecimal --job-id"
    );
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_matches_the_exact_kind_connection_pane_and_job() {
        let jobs = ActiveJobs::default();
        let stop = CaptureStop::default();
        let registration = jobs.register(Kind::Packets, 1, 2, "job", &stop).unwrap();
        assert!(jobs.stop(Kind::Syscalls, 1, 2, "job").is_err());
        assert!(jobs.stop(Kind::Packets, 2, 2, "job").is_err());
        assert!(jobs.stop(Kind::Packets, 1, 3, "job").is_err());
        assert!(jobs.stop(Kind::Packets, 1, 2, "old").is_err());
        assert!(!stop.is_requested());
        jobs.stop(Kind::Packets, 1, 2, "job").unwrap();
        assert!(*stop.0.subscribe().borrow());
        drop(registration);
        assert!(jobs.stop(Kind::Packets, 1, 2, "job").is_err());
    }
}
