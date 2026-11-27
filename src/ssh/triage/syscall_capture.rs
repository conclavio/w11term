use super::{
    ArtifactMessages, ConnectionShare, PendingChannel, Target, diagnostic_destination, quote,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::Duration,
};
use tokio::sync::watch;

const SCRIPT: &str = include_str!("syscall-capture.sh");
const REVIEW_DEADLINE: Duration = Duration::from_secs(8);
pub(crate) const CONTROL_SECONDS: u64 = 155;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyscallFilter {
    #[default]
    All,
    Files,
    Processes,
    Network,
}

impl SyscallFilter {
    fn strace(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Files => "%file,%desc,%process",
            Self::Processes => "%process,%signal",
            Self::Network => "%network,%process",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyscallOptions {
    pub program: String,
    pub arguments: Vec<String>,
    pub directory: String,
    pub seconds: u8,
    pub max_mib: u8,
    pub string_size: u16,
    pub filter: SyscallFilter,
}

impl SyscallOptions {
    pub const DEFAULT_SECONDS: u8 = 15;
    pub const DEFAULT_MAX_MIB: u8 = 16;
    pub const DEFAULT_STRING_SIZE: u16 = 128;

    pub fn validate(self) -> Result<Self> {
        ensure!(
            safe_path(&self.program),
            "Choose a Linux executable path or name up to 4096 bytes"
        );
        ensure!(
            self.directory.is_empty()
                || self.directory.starts_with('/') && safe_path(&self.directory),
            "Choose an absolute Linux working directory up to 4096 bytes"
        );
        ensure!(self.arguments.len() <= 64, "Use up to 64 arguments");
        let mut bytes = self.program.len() + self.directory.len();
        for argument in &self.arguments {
            ensure!(
                !argument.contains('\0'),
                "Linux arguments cannot contain NUL"
            );
            bytes += argument.len();
            ensure!(
                bytes <= 8192,
                "Command paths and arguments exceed 8192 bytes"
            );
        }
        ensure!(
            (1..=60).contains(&self.seconds),
            "Trace duration must be 1..=60 seconds"
        );
        ensure!(
            (1..=64).contains(&self.max_mib),
            "Trace output budget must be 1..=64 MiB"
        );
        ensure!(
            (16..=256).contains(&self.string_size),
            "Trace string length must be 16..=256 bytes"
        );
        Ok(self)
    }

    fn max_bytes(&self) -> u64 {
        u64::from(self.max_mib) * 1024 * 1024
    }
}

fn safe_path(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
}

struct Active {
    generation: u64,
    pane: u64,
    job: String,
    stop: SyscallCaptureStop,
}

#[derive(Clone)]
pub struct SyscallCaptureStop(watch::Sender<bool>);

impl Default for SyscallCaptureStop {
    fn default() -> Self {
        Self(watch::channel(false).0)
    }
}

impl SyscallCaptureStop {
    pub fn request(&self) {
        self.0.send_replace(true);
    }

    pub fn is_requested(&self) -> bool {
        *self.0.borrow()
    }

    fn check_before_launch(&self) -> Result<()> {
        ensure!(
            !self.is_requested(),
            "Syscall capture stopped before launching the command"
        );
        Ok(())
    }
}

#[derive(Default)]
pub(in crate::ssh) struct ActiveJobs(Mutex<Option<Active>>);

impl ActiveJobs {
    fn lock(&self) -> MutexGuard<'_, Option<Active>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn register(
        &self,
        generation: u64,
        pane: u64,
        job: &str,
        stop: SyscallCaptureStop,
    ) -> Result<(Registration<'_>, watch::Receiver<bool>)> {
        let mut active = self.lock();
        ensure!(
            active.is_none(),
            "A syscall capture is active on this SSH transport"
        );
        let requested = stop.0.subscribe();
        *active = Some(Active {
            generation,
            pane,
            job: job.into(),
            stop,
        });
        Ok((Registration(self), requested))
    }

    fn stop(&self, generation: u64, pane: u64, job: &str) -> Result<()> {
        let active = self.lock();
        let active = active
            .as_ref()
            .filter(|current| {
                current.job == job && current.pane == pane && current.generation == generation
            })
            .context("No matching running syscall capture on the selected pane")?;
        active.stop.request();
        Ok(())
    }
}

struct Registration<'a>(&'a ActiveJobs);

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        *self.0.lock() = None;
    }
}

#[derive(Clone, Serialize)]
pub struct Plan {
    connection_generation: u64,
    pub host: String,
    pub user: String,
    pub port: u16,
    pub host_key_fingerprint: String,
    pub program: String,
    pub directory: String,
    #[serde(skip)]
    identity: String,
    pub options: SyscallOptions,
    pub strace_arguments: Vec<String>,
    review_id: String,
}

#[derive(Serialize)]
pub struct Capture {
    pub path: PathBuf,
    pub bytes: u64,
    sha256: String,
    pub stop_reason: &'static str,
    pub command_exit_status: Option<u8>,
}

impl Target {
    pub fn preview_syscall_capture_blocking(mut self, options: SyscallOptions) -> Result<Plan> {
        crate::ssh::NETWORK.block_on(self.preview_syscall_capture(options))
    }

    pub fn save_syscall_capture_blocking(
        self,
        pane: u64,
        plan: Plan,
        path: PathBuf,
        stop: SyscallCaptureStop,
    ) -> Result<Capture> {
        ensure!(
            self.generation == plan.connection_generation,
            "The reviewed SSH connection changed. Review again."
        );
        let job = format!("{:032x}", rand::random::<u128>());
        crate::ssh::NETWORK.block_on(self.capture_with_stop(
            pane,
            plan.options,
            &plan.review_id,
            &job,
            path,
            stop,
        ))
    }

    pub(crate) async fn preview_syscall_capture(
        &mut self,
        options: SyscallOptions,
    ) -> Result<Plan> {
        let options = options.validate()?;
        let (reply, _) = self
            .run_diagnostic(
                "syscall capture review",
                command(&options, None),
                REVIEW_DEADLINE,
                16 * 1024,
            )
            .await?;
        let line = std::str::from_utf8(&reply)?
            .strip_suffix('\n')
            .context("Incomplete syscall capture review")?;
        let fields: Vec<_> = line.split('\t').collect();
        let ["w11term-syscall-plan-v1", directory, program, identity] = fields.as_slice() else {
            bail!("Invalid syscall capture review");
        };
        ensure!(
            [directory, program]
                .iter()
                .all(|value| value.starts_with('/') && safe_path(value))
                && !identity.is_empty()
                && identity.len() <= 160
                && !identity.chars().any(char::is_control),
            "Invalid executable or directory metadata"
        );
        let (host, user, port, fingerprint) = self.link.verified_endpoint();
        let mut arguments = vec![
            "-f".into(),
            "-I".into(),
            "1".into(),
            "--kill-on-exit".into(),
            "-ttt".into(),
            "-T".into(),
            "-yy".into(),
            "-s".into(),
            options.string_size.to_string(),
            "-e".into(),
            format!("trace={}", options.filter.strace()),
            "-o".into(),
            "<private trace stream>".into(),
            "--".into(),
            (*program).into(),
        ];
        arguments.extend(options.arguments.iter().cloned());
        let mut plan = Plan {
            connection_generation: self.generation,
            host: host.into(),
            user: user.into(),
            port,
            host_key_fingerprint: fingerprint.into(),
            program: (*program).into(),
            directory: (*directory).into(),
            identity: (*identity).into(),
            options,
            strace_arguments: arguments,
            review_id: String::new(),
        };
        let mut digest = Sha256::new();
        digest.update(serde_json::to_vec(&plan)?);
        digest.update(plan.identity.as_bytes());
        plan.review_id = crate::sftp::digest_hex(digest);
        Ok(plan)
    }

    pub(crate) async fn capture_syscalls(
        self,
        pane: u64,
        options: SyscallOptions,
        review_id: &str,
        job: &str,
        path: PathBuf,
    ) -> Result<Capture> {
        self.capture_with_stop(
            pane,
            options,
            review_id,
            job,
            path,
            SyscallCaptureStop::default(),
        )
        .await
    }

    async fn capture_with_stop(
        mut self,
        pane: u64,
        options: SyscallOptions,
        review_id: &str,
        job: &str,
        path: PathBuf,
        stop: SyscallCaptureStop,
    ) -> Result<Capture> {
        stop.check_before_launch()?;
        let job = validate_job(job)?;
        let review_id = crate::sftp::normalize_sha256(review_id)?;
        let parent = diagnostic_destination(&path, "syscall capture")?.to_owned();
        let plan = self.preview_syscall_capture(options).await?;
        ensure!(
            plan.review_id == review_id,
            "The reviewed command or capture settings changed. Review again."
        );
        let _busy = self.link.try_lock_diagnostic()?;
        let deadline = Duration::from_secs(u64::from(plan.options.seconds) + 72);
        let transfer = tokio::time::timeout(
            deadline,
            capture_and_download(&self.link, &plan, pane, job, &parent, stop),
        );
        let (file, mut capture) = tokio::select! {
            biased;
            _ = self.lease.changed() => bail!("The reviewed SSH pane disconnected during syscall capture"),
            response = transfer => response.context("Syscall capture or SFTP transfer exceeded its deadline")??,
        };
        capture.path = path.clone();
        tokio::task::spawn_blocking(move || -> Result<_> {
            file.persist_noclobber(path)
                .map_err(|error| error.error)
                .context("Saving the syscall trace")?;
            Ok(capture)
        })
        .await?
    }

    pub(crate) fn stop_syscall_capture(&self, pane: u64, job: &str) -> Result<()> {
        self.link
            .syscall_capture()
            .stop(self.generation, pane, validate_job(job)?)
    }
}

fn validate_job(job: &str) -> Result<&str> {
    ensure!(
        job.len() == 32
            && job
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "Use a 32-character lowercase hexadecimal --job-id"
    );
    Ok(job)
}

fn command(options: &SyscallOptions, capture: Option<(&Plan, &str)>) -> String {
    let (mode, directory, program, identity, job) = match capture {
        Some((plan, job)) => (
            "capture",
            plan.directory.as_str(),
            plan.program.as_str(),
            plan.identity.as_str(),
            job,
        ),
        None => ("inspect", "", "", "", ""),
    };
    let mut command = format!(
        "/bin/bash -c {} w11term-syscall-capture {mode} {} {} {} {} {} {} {} {} {} {}",
        quote(SCRIPT),
        options.seconds,
        options.max_bytes(),
        options.string_size,
        quote(options.filter.strace()),
        quote(&options.directory),
        quote(&options.program),
        quote(directory),
        quote(program),
        quote(identity),
        quote(job),
    );
    for argument in &options.arguments {
        command.push(' ');
        command.push_str(&quote(argument));
    }
    command
}

async fn capture_and_download(
    link: &ConnectionShare,
    plan: &Plan,
    pane: u64,
    job: &str,
    parent: &Path,
    stop: SyscallCaptureStop,
) -> Result<(tempfile::NamedTempFile, Capture)> {
    stop.check_before_launch()?;
    let (registration, mut requested) =
        link.syscall_capture()
            .register(plan.connection_generation, pane, job, stop.clone())?;
    let handle = link.handle();
    let mut guard = PendingChannel(Some(
        handle
            .channel_open_session()
            .await
            .context("Opening syscall trace channel")?,
    ));
    let channel = guard.0.as_mut().unwrap();
    stop.check_before_launch()?;
    channel
        .exec(true, command(&plan.options, Some((plan, job))))
        .await?;
    let mut messages = ArtifactMessages::new("Syscall capture");
    let line = tokio::select! {
        line = messages.line(channel) => line?,
        _ = async { let _ = requested.wait_for(|&stop| stop).await; } => {
            channel.data(b"stop\n".as_slice()).await?;
            messages.line(channel).await?
        }
    };
    drop(registration);
    let fields: Vec<_> = line.split('\t').collect();
    let ["w11term-syscall-ready-v1", reason, code, size, hash] = fields.as_slice() else {
        bail!("Invalid syscall capture completion record");
    };
    let bytes: u64 = size
        .trim()
        .parse()
        .context("Reading syscall capture size")?;
    ensure!(
        (1..=plan.options.max_bytes()).contains(&bytes),
        "Remote syscall capture exceeded its reviewed byte budget"
    );
    let (stop_reason, command_exit_status) = match (*reason, *code) {
        ("exit", code) => ("command-exited", Some(code.parse::<u8>()?)),
        ("bytes", "-") if bytes == plan.options.max_bytes() => ("byte-budget", None),
        ("stopped", "-") => ("stopped", None),
        ("time", "-") => ("time-limit", None),
        _ => bail!("Unknown syscall capture outcome"),
    };
    let sha256 = crate::sftp::normalize_sha256(hash)?;
    let remote = format!("/tmp/w11term-strace-{job}/trace.strace");
    let file =
        crate::sftp::stage_diagnostic_download(&handle, &remote, parent, bytes, &sha256).await?;
    channel.data(b"verified\n".as_slice()).await?;
    channel.eof().await?;
    ensure!(
        messages.line(channel).await? == "w11term-syscall-complete-v1",
        "The syscall capture cleanup was not acknowledged"
    );
    messages.finish(channel).await?;
    Ok((
        file,
        Capture {
            path: PathBuf::new(),
            bytes,
            sha256,
            stop_reason,
            command_exit_status,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stop_requested_before_the_worker_subscribes_stays_requested() {
        let jobs = ActiveJobs::default();
        let stop = SyscallCaptureStop::default();
        stop.request();
        assert!(stop.check_before_launch().is_err());

        let (_registration, mut receiver) = jobs.register(1, 2, "fixture", stop).unwrap();
        let requested = tokio::time::timeout(
            Duration::from_secs(1),
            receiver.wait_for(|&requested| requested),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(*requested);
    }
}
