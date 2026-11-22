use crate::{
    demo,
    options::Source,
    ssh,
    terminal::{self, FrameStore, Snapshot, TransportInput},
};
use anyhow::Context as _;
use std::{
    sync::{Arc, atomic::AtomicBool},
    thread,
};
use tokio::sync::mpsc;
use wezterm_term::{KeyCode, KeyModifiers, MouseEvent, TerminalSize};
#[cfg(test)]
mod clear_scrollback_tests;
mod clipboard;
mod launch;
pub use clipboard::{ClipboardPaste, ClipboardReply, PendingPaste};
#[cfg(test)]
mod pane_search_tests;
mod presentation;
mod range_scroll;
pub use range_scroll::{RangeScroll, RangeScrollRequest};
#[cfg(test)]
mod synchronized_capture;
#[cfg(test)]
mod test_worker;
use launch::Phase;
use presentation::Presentation;

#[derive(Debug)]
pub struct ClaimedRestart(pub(crate) u64);

#[derive(Debug)]
pub enum Command {
    /// The caller claims this exited launch through Events::restart_exited.
    RestartExited(ClaimedRestart),
    /// Ordered with both native input and control-pipe writes on this pane.
    SetReadOnly(bool),
    /// Input method committed text has no corresponding physical key presses.
    Composed(String),
    Observe(tokio::sync::oneshot::Sender<tokio::sync::broadcast::Receiver<Vec<u8>>>),
    Record {
        path: Option<std::path::PathBuf>,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    Authenticate(crate::options::Authentication),
    Events(tokio::sync::oneshot::Sender<crate::events::Events>),
    Frames(tokio::sync::oneshot::Sender<FrameStore>),
    Inspect(bool),
    Sftp(crate::sftp::Job),
    Key {
        key: KeyCode,
        modifiers: KeyModifiers,
        down: bool,
        repeat: bool,
    },
    DetailedKey {
        key: KeyCode,
        details: terminal::KeyDetails,
        modifiers: KeyModifiers,
        down: bool,
        repeat: bool,
    },
    Paste(String),
    ClipboardPaste(ClipboardPaste, ClipboardReply),
    Text(String),
    Raw(Vec<u8>, tokio::sync::oneshot::Sender<Result<(), String>>),
    Capture(tokio::sync::oneshot::Sender<(Snapshot, TerminalSize)>),
    /// Capture immutable UTF-8 text on this worker. The caller owns all file I/O.
    ExportText(tokio::sync::oneshot::Sender<Result<String, &'static str>>),
    Resize(TerminalSize),
    Scroll(isize),
    /// Discard only the offscreen rows of the primary buffer on this worker.
    ClearScrollback,
    ScrollTo(isize),
    ScrollRange(RangeScrollRequest),
    CopyRange(
        crate::copy_mode::ScreenId,
        crate::interaction::Selection,
        tokio::sync::oneshot::Sender<Result<String, &'static str>>,
    ),
    ScrollEdge(bool),
    Prompt(isize),
    Search {
        query: String,
        sensitive: bool,
        regex: bool,
    },
    SearchNavigate(isize),
    SearchClose,
    SearchPreview {
        query: String,
        reply: tokio::sync::oneshot::Sender<crate::search::PaneSearchPreview>,
    },
    SearchReveal {
        query: String,
        anchor: crate::search::PaneSearchAnchor,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    Wheel(isize, KeyModifiers),
    Mouse(MouseEvent),
    Focus(bool),
    ReloadDemo,
    Shutdown,
}

fn contain_worker_failure(
    frames: &FrameStore,
    events: &crate::events::Events,
    work: impl FnOnce(),
) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).is_err() {
        events.close_restart();
        events.command_stream(false);
        events.set_progress(wezterm_term::Progress::None);
        // Never resume a possibly inconsistent terminal engine after unwinding.
        let mut frame = (*frames.load()).clone();
        frame.generation += 1;
        frame.status = "Terminal engine failed; close and reopen this tab to reconnect".into();
        frame.prompt = None;
        frames.publish(frame);
        events.push(
            "session-error",
            serde_json::json!({"reason":"terminal worker panicked","recoverable_by_reopen":true}),
        );
    }
}

pub struct Session {
    /// Process-local identity used by external clients across reconnects and layout changes.
    pub pane_id: u64,
    pub prompts: crate::prompts::Prompts,
    pub events: crate::events::Events,
    pub player: Arc<std::sync::RwLock<crate::options::PlayerOptions>>,
    pub input: mpsc::UnboundedSender<Command>,
    pub frames: FrameStore,
    pub clipboard_enabled: Arc<AtomicBool>,
    ssh_connection: ssh::ShareSlot,
    direct_ssh_shell: bool,
    durable: Option<ssh::DurableControl>,
    worker: Option<thread::JoinHandle<()>>,
}

struct Services {
    prompts: crate::prompts::Prompts,
    events: crate::events::Events,
    player: Arc<std::sync::RwLock<crate::options::PlayerOptions>>,
    notices: mpsc::Receiver<crate::sound::Notice>,
    live_frames: FrameStore,
    sharing: ssh::Sharing,
}

impl Session {
    pub fn restart_exited(&self, token: u64) -> anyhow::Result<()> {
        self.events.restart_exited(&self.input, token)
    }

    pub fn start(source: Source) -> Self {
        Self::start_sized(source, TerminalSize::default())
    }

    pub fn start_sized(source: Source, size: TerminalSize) -> Self {
        Self::start_configured(source, size, true)
    }
    pub fn start_configured(source: Source, size: TerminalSize, allow_clipboard: bool) -> Self {
        Self::start_configured_with_connection(source, size, allow_clipboard, None)
    }

    /// Snapshot the authenticated connection of a ready SSH pane, for an
    /// explicit split of that pane. Ordinary profile opens are independent.
    pub fn ssh_connection(&self) -> Option<ssh::ConnectionShare> {
        self.ssh_connection.current()
    }
    /// Require the same authenticated transport generation as a pane snapshot.
    pub fn ssh_connection_at(&self, generation: u64) -> Option<ssh::ConnectionShare> {
        self.ssh_connection
            .generation()
            .and_then(|(current, link)| (current == generation).then_some(link))
    }
    /// The program launched in this remote PTY, independent of edits to its saved recipe.
    pub fn is_direct_ssh_shell(&self) -> bool {
        self.direct_ssh_shell && !self.player.read().unwrap().rustmush
    }

    pub fn profile_folders(
        &self,
        scope: crate::folder_grants::FolderScope,
    ) -> crate::folder_grants::PaneShares {
        crate::folder_grants::PaneShares::new(self.pane_id, self.ssh_connection.clone(), scope)
    }

    pub fn start_configured_with_connection(
        source: Source,
        size: TerminalSize,
        allow_clipboard: bool,
        connection: Option<ssh::ConnectionShare>,
    ) -> Self {
        Self::start_worker(source, size, allow_clipboard, connection, None, None)
    }

    pub fn start_workspace_ssh(
        options: crate::options::SshOptions,
        size: TerminalSize,
        allow_clipboard: bool,
        startup: crate::workspace::SshStartup,
    ) -> Self {
        Self::start_ssh_with_startup(options, size, allow_clipboard, startup, None)
    }

    pub fn start_ssh_with_startup(
        options: crate::options::SshOptions,
        size: TerminalSize,
        allow_clipboard: bool,
        startup: crate::workspace::SshStartup,
        connection: Option<ssh::ConnectionShare>,
    ) -> Self {
        Self::start_worker(
            Source::Ssh(options),
            size,
            allow_clipboard,
            connection,
            None,
            Some(startup),
        )
    }

    pub fn start_durable(
        source: Source,
        size: TerminalSize,
        allow_clipboard: bool,
        connection: Option<ssh::ConnectionShare>,
        saved: Option<crate::durable::DurableReference>,
    ) -> Self {
        Self::start_worker(
            source,
            size,
            allow_clipboard,
            connection,
            Some(ssh::DurableStart::Attach(saved)),
            None,
        )
    }

    /// Discover existing PTYs without creating a remote shell or a saved workspace reference.
    pub fn discover_durable(
        mut options: crate::options::SshOptions,
        size: TerminalSize,
        allow_clipboard: bool,
    ) -> Self {
        options.durable = true;
        Self::start_worker(
            Source::Ssh(options),
            size,
            allow_clipboard,
            None,
            Some(ssh::DurableStart::Discover),
            None,
        )
    }

    /// Explicit opt-in: hc connect can start the user's Herdr Core daemon.
    pub fn discover_herdr(
        mut options: crate::options::SshOptions,
        size: TerminalSize,
        allow_clipboard: bool,
    ) -> Self {
        options.durable = true;
        Self::start_worker(
            Source::Ssh(options),
            size,
            allow_clipboard,
            None,
            Some(ssh::DurableStart::DiscoverHerdr),
            None,
        )
    }

    pub fn durable_state(&self) -> Option<ssh::DurableState> {
        self.durable.as_ref().map(|control| control.state())
    }

    pub fn durable_action(&self, action: ssh::DurableAction) -> anyhow::Result<()> {
        self.durable
            .as_ref()
            .context("This pane does not manage a durable SSH shell")?
            .action(action)
    }

    fn start_worker(
        source: Source,
        size: TerminalSize,
        allow_clipboard: bool,
        connection: Option<ssh::ConnectionShare>,
        saved: Option<ssh::DurableStart>,
        startup: Option<crate::workspace::SshStartup>,
    ) -> Self {
        static NEXT_PANE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let (commands, input) = mpsc::unbounded_channel();
        let prompts = crate::prompts::Prompts::default();
        let events = crate::events::Events::default();
        let player = Arc::new(std::sync::RwLock::new(match &source {
            Source::Ssh(ssh) => ssh.player.clone(),
            _ => Default::default(),
        }));
        let worker_player = player.clone();
        let (notice_tx, notice_rx) = mpsc::channel(64);
        let (writer, transport_input) = mpsc::unbounded_channel();
        let mut terminal =
            terminal::create_with_scrollback(size, writer.clone(), source.scrollback_lines());
        let clipboard_enabled = Arc::new(AtomicBool::new(allow_clipboard));
        let clipboard: Arc<dyn wezterm_term::Clipboard> =
            Arc::new(crate::clipboard::RemoteClipboard(clipboard_enabled.clone()));
        terminal.set_clipboard(&clipboard);
        terminal.set_notification_handler(Box::new(crate::sound::Alerts {
            events: events.clone(),
            notices: notice_tx,
            player: player.clone(),
        }));
        let frames = FrameStore::new(Snapshot::capture(
            &terminal,
            None,
            1,
            0,
            "Starting…".into(),
            0,
        ));
        let worker_frames = frames.clone();
        let live_frames = FrameStore::new((*frames.load()).clone());
        let worker_prompts = prompts.clone();
        let worker_events = events.clone();
        let ssh_connection = ssh::ShareSlot::default();
        let direct_ssh_shell = direct_ssh_shell(&source, saved.is_some(), startup.as_ref());
        let (durable, lifecycle) = saved
            .filter(|_| matches!(source, Source::Ssh(_)))
            .map(ssh::DurableControl::new)
            .unzip();
        let sharing = ssh::Sharing::new(
            ssh_connection.clone(),
            connection.filter(|_| matches!(source, Source::Ssh(_))),
            lifecycle,
            startup,
        );
        let worker = thread::Builder::new()
            .name("terminal-session".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("creating terminal runtime");
                let failure_frames = worker_frames.clone();
                let failure_events = worker_events.clone();
                contain_worker_failure(&failure_frames, &failure_events, || {
                    runtime.block_on(run(
                        source,
                        terminal,
                        input,
                        writer,
                        transport_input,
                        worker_frames,
                        Services {
                            prompts: worker_prompts,
                            events: worker_events,
                            player: worker_player,
                            notices: notice_rx,
                            live_frames,
                            sharing,
                        },
                    ))
                });
            })
            .expect("starting terminal worker");
        Self {
            pane_id: NEXT_PANE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            prompts,
            events,
            player,
            input: commands,
            frames,
            clipboard_enabled,
            ssh_connection,
            direct_ssh_shell,
            durable,
            worker: Some(worker),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.events.close_restart();
        self.events.command_stream(false);
        let _ = self.input.send(Command::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

async fn run(
    source: Source,
    mut terminal: wezterm_term::Terminal,
    mut commands: mpsc::UnboundedReceiver<Command>,
    mut writer: mpsc::UnboundedSender<TransportInput>,
    mut transport_input: mpsc::UnboundedReceiver<TransportInput>,
    frames: FrameStore,
    services: Services,
) {
    let Services {
        prompts,
        events,
        player,
        mut notices,
        live_frames,
        sharing,
    } = services;
    let mut size = terminal.get_size();
    let (output_tx, mut output) = mpsc::channel(32);
    let is_demo = matches!(source, Source::Demo);
    let is_ssh = matches!(source, Source::Ssh(_));
    let interactive = is_ssh || matches!(source, Source::Local(_));
    // A durable backend may reconstruct an old screen at any time. Its OSC
    // 133 bytes cannot be used to announce a newly completed command.
    let live_commands = sharing.durable.is_none() && can_watch_commands(&source);
    let restartable = can_restart(live_commands, sharing.startup.as_ref());
    let mut launch =
        launch::Launcher::new(source.clone(), sharing, prompts.clone(), events.clone());
    let mut launch_id = 1u64;
    let mut phase = Phase::Running;
    let mut queued_before_connection = 0usize;
    let mut show_progress = interactive;
    let endpoint = match &source {
        Source::Ssh(options) => format!("{}@{}:{}", options.user, options.host, options.port),
        _ => String::new(),
    };
    let assets = crate::assets::Store::default();
    let sounds = crate::sound::worker(player.clone(), assets.clone(), events.clone());
    let mut asset_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut pending_pack = crate::assets::PendingPack::default();
    let mut status;
    let mut total_bytes = 0u64;
    let mut scroll_offset = 0usize;
    let mut read_only = false;
    let mut last_mouse = MouseEvent {
        kind: wezterm_term::MouseEventKind::Move,
        x: 0,
        y: 0,
        x_pixel_offset: 0,
        y_pixel_offset: 0,
        button: wezterm_term::MouseButton::None,
        modifiers: KeyModifiers::NONE,
    };
    let mut demo_loaded = false;
    let mut replay = None;
    let mut transports = tokio::task::JoinSet::new();
    match source {
        Source::Local(_) | Source::Ssh(_) => {
            status = launch.start(&mut transports, size, transport_input, output_tx);
        }
        Source::Replay(path) => {
            match std::fs::read(&path) {
                Ok(bytes) => {
                    total_bytes = bytes.len() as u64;
                    replay = Some(bytes);
                    status = format!("Replay · {}", path.display());
                }
                Err(error) => status = format!("Replay: {error}"),
            }
            // Keep consuming engine replies; replay has no peer to answer them.
            transports.spawn(async move { while transport_input.recv().await.is_some() {} });
        }
        Source::Demo => {
            status = "Terminal lab · F5 reload · Shift+PageUp/PageDown scroll · Ctrl+Shift+V paste"
                .into();
            transports.spawn(async move { while transport_input.recv().await.is_some() {} });
        }
    };
    let mut presentation = Presentation::new(frames.clone(), live_frames.clone());
    if let Some(bytes) = replay {
        presentation.advance(&mut terminal, bytes, 0, &status, total_bytes);
    }
    let mut search = crate::search::Search::default();
    let mut search_origin = Arc::new(());
    let mut search_dirty = false;
    let mut search_refresh = tokio::time::Instant::now();
    let mut connection_generation = 0;
    let mut recorder: Option<crate::recorder::Recorder> = None;
    let mut inspecting = false;
    let clipboard_origin = Arc::new(());
    let (observed, _) = tokio::sync::broadcast::channel::<Vec<u8>>(32);
    loop {
        if let Some(version) = pending_pack.next(
            asset_task.is_some(),
            is_ssh && player.read().unwrap().sync_assets_enabled(),
            &assets.version(),
        ) {
            let input = writer.clone();
            let endpoint = endpoint.clone();
            let assets = assets.clone();
            let events = events.clone();
            asset_task = Some(tokio::spawn(async move {
                events.push(
                    "assets-syncing",
                    serde_json::json!({"pack_version":version}),
                );
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(1800),
                    crate::assets::sync(input, endpoint, version.clone(), assets),
                )
                .await;
                match result {
                    Ok(Ok(fetched)) => events.push(
                        "assets-ready",
                        serde_json::json!({"pack_version":version,"downloaded":fetched}),
                    ),
                    other => events.push(
                        "assets-error",
                        serde_json::json!({"pack_version":version,"error":format!("{other:?}")}),
                    ),
                }
            }));
        }
        presentation.publish(
            &terminal,
            scroll_offset,
            presentation::Metadata {
                status: &status,
                bytes: total_bytes,
                search: &search,
                prompt: prompts.current(),
                connection: connection_generation,
            },
        );
        if phase == Phase::PublishingExit {
            phase = Phase::Exited;
            if restartable {
                events.pane_exited(launch_id);
            }
        }
        if inspecting {
            frames.set_inspection(Some(terminal::TerminalModes::read(&terminal)));
        }
        let animation_deadline = presentation.animation_deadline(&terminal);
        tokio::select! {
            _ = async { if let Some(deadline)=animation_deadline {tokio::time::sleep_until(deadline).await;} }, if animation_deadline.is_some() => {
                presentation.dirty |= terminal.advance_kitty_animations(std::time::Instant::now());
            },
            _=tokio::time::sleep_until(presentation.deadline.unwrap_or_else(||tokio::time::Instant::now()+std::time::Duration::from_secs(86400))),if presentation.deadline.is_some()=>{presentation.expire(&mut terminal);},
            _=prompts.changed()=>{},
            Some(notice)=notices.recv()=>match notice {
                crate::sound::Notice::Play(name)=>{if sounds.try_send(name).is_err(){events.push("sound-error",serde_json::json!({"reason":"audio queue full"}));}},
                crate::sound::Notice::Pack(version)=> { pending_pack.announce(version); }
            },
            _ = async { if let Some(task)=asset_task.as_mut() { let _=task.await; } }, if asset_task.is_some() => { asset_task=None; },
            _ = tokio::time::sleep_until(search_refresh), if search.open && search_dirty => {
                search.refresh(&terminal, scroll_offset);
                search_dirty = false;
                presentation.dirty=true;
            }
            command = commands.recv() => {
                let queued_offline = queued_before_connection > 0;
                queued_before_connection = queued_before_connection.saturating_sub(1);
                let command = if phase != Phase::Running || queued_offline {
                    match command {
                        Some(Command::ClipboardPaste(_, reply)) => { let _ = reply.send(Err("No active terminal connection; clipboard text was not sent".into())); continue; }
                        Some(Command::Raw(_, reply)) => { let _ = reply.send(Err("No active terminal connection; input was not sent".into())); continue; }
                        Some(Command::Sftp(job)) => { let _ = job.reply.send(Err("No active terminal connection; SFTP operation was not started".into())); continue; }
                        Some(Command::Focus(focused)) => { terminal.set_focus_without_reporting(focused); continue; }
                        Some(Command::Key {..} | Command::DetailedKey {..} | Command::Composed(_) | Command::Text(_) | Command::Paste(_) | Command::Mouse(_)) => continue,
                        Some(Command::Wheel(lines, _)) => Some(Command::Scroll(lines)),
                        other => other,
                    }
                } else { command };
                let command = if read_only {
                    match command {
                        Some(Command::ClipboardPaste(_, reply)) => { let _ = reply.send(Err("Pane input is read-only".into())); continue; }
                        Some(Command::Raw(_, reply)) => {
                            let _ = reply.send(Err("Pane input is read-only".into()));
                            continue;
                        }
                        Some(Command::Key { .. } | Command::DetailedKey { .. } | Command::Mouse(_)
                            | Command::Text(_) | Command::Paste(_) | Command::Composed(_)) => continue,
                        Some(Command::Wheel(lines, _)) => Some(Command::Scroll(lines)),
                        other => other,
                    }
                } else { command };
                let mut changes_screen = matches!(&command,Some(Command::Resize(_) | Command::Key {..} | Command::DetailedKey {..} | Command::Paste(_) | Command::Text(_) | Command::Composed(_) | Command::ReloadDemo));
                let old_offset=scroll_offset;
                presentation.dirty|=matches!(&command,Some(Command::Scroll(_) | Command::ScrollEdge(_) | Command::Wheel(..) | Command::Prompt(_) | Command::Search{..} | Command::SearchNavigate(_) | Command::SearchClose | Command::ReloadDemo)) || (is_demo && changes_screen);
                let result = match command {
                    Some(Command::ClipboardPaste(request, reply)) => {
                        use clipboard::Decision;
                        if interactive && connection_generation == 0 {
                            let _ = reply.send(Err("The terminal is still connecting. Paste again when it is ready.".into()));
                            continue;
                        }
                        let action = request.evaluate(&clipboard_origin, connection_generation, terminal.bracketed_paste_enabled());
                        match action {
                            Err(error) => { let _ = reply.send(Err(error)); }
                            Ok(Decision::Confirm(pending)) => { let _ = reply.send(Ok(Some(pending))); }
                            Ok(Decision::Send { text, .. }) if text.is_empty() => { let _ = reply.send(Ok(None)); }
                            Ok(Decision::Send { text, raw }) if interactive => {
                                let bytes = if raw { text.into_bytes() } else {
                                    scroll_offset = 0;
                                    terminal.encode_paste(&text).into_bytes()
                                };
                                clipboard::write(&writer, connection_generation, bytes, reply);
                            }
                            Ok(Decision::Send { text, raw: false }) if is_demo => {
                                changes_screen = true;
                                scroll_offset = 0;
                                presentation.advance(&mut terminal, text.replace('\n', "\r\n"), 0, &status, total_bytes);
                                let _ = reply.send(Ok(None));
                            }
                            Ok(Decision::Send { .. }) => { let _ = reply.send(Err("Clipboard input requires an interactive terminal".into())); }
                        }
                        Ok(())
                    }
                    Some(Command::RestartExited(ClaimedRestart(token))) => {
                        if phase == Phase::Exited && restartable && token == launch_id {
                            launch_id += 1;
                            phase = Phase::Restarting { reset: true };
                            search_origin = Arc::new(());
                            terminal.reset_connection();
                            scroll_offset = 0;
                            last_mouse.button = wezterm_term::MouseButton::None;
                            let (tx, rx) = mpsc::unbounded_channel();
                            terminal::replace_transport(&mut terminal, tx.clone());
                            writer = tx;
                            let (output_tx, new_output) = mpsc::channel(32);
                            output = new_output;
                            status = launch.start(&mut transports, size, rx, output_tx);
                            presentation.dirty = true;
                            search.refresh(&terminal, scroll_offset);
                        }
                        Ok(())
                    }
                    Some(Command::Shutdown) | None => break,
                    Some(Command::SetReadOnly(enabled)) => {
                        read_only = enabled;
                        if enabled && matches!(last_mouse.button, wezterm_term::MouseButton::Left | wezterm_term::MouseButton::Middle | wezterm_term::MouseButton::Right)
                            && last_mouse.kind != wezterm_term::MouseEventKind::Release {
                            let release = MouseEvent { kind: wezterm_term::MouseEventKind::Release, ..last_mouse };
                            last_mouse.button = wezterm_term::MouseButton::None;
                            terminal.mouse_event(release)
                        } else { Ok(()) }
                    }
                    Some(Command::Observe(reply))=>{let _=reply.send(observed.subscribe());Ok(())},
                    Some(Command::Record{path,reply})=>{
                        let result=(||->anyhow::Result<serde_json::Value>{
                            if let Some(path)=path {anyhow::ensure!(recorder.is_none(),"pane is already recording");let started=crate::recorder::Recorder::start(&path,size)?;recorder=Some(started);events.push("recording-started",serde_json::json!({"path":path}));Ok(serde_json::json!({"recording":true,"path":path}))}
                            else {let stopped=recorder.take().context("pane is not recording")?;let bytes=stopped.bytes;let path=stopped.path.clone();stopped.finish()?;events.push("recording-stopped",serde_json::json!({"bytes":bytes}));Ok(serde_json::json!({"recording":false,"path":path,"bytes":bytes}))}
                        })();let _=reply.send(result.map_err(|e|format!("{e:#}")));Ok(())
                    },
                    Some(Command::Authenticate(auth))=>{launch.set_authentication(auth.clone());let _=writer.send(TransportInput::Authenticate(auth));Ok(())},
                    Some(Command::Events(reply))=>{let _=reply.send(events.clone());Ok(())},
                    Some(Command::Frames(reply))=>{let _=reply.send(live_frames.clone());Ok(())},
                    Some(Command::Inspect(enabled))=>{
                        inspecting=enabled;
                        if !enabled { frames.set_inspection(None); }
                        Ok(())
                    },
                    Some(Command::Sftp(job)) => {
                        if is_ssh {
                            if let Err(error) = writer.send(TransportInput::Sftp(job))
                                && let TransportInput::Sftp(job) = error.0 { let _ = job.reply.send(Err("SSH connection is closed".into())); }
                        } else { let _ = job.reply.send(Err("SFTP requires an SSH session".into())); }
                        Ok(())
                    }
                    Some(Command::Raw(bytes, reply)) => {
                        if interactive {
                            if let Err(error) = writer.send(TransportInput::Raw(bytes, reply))
                                && let TransportInput::Raw(_, reply) = error.0 { let _ = reply.send(Err("Terminal connection is closed".into())); }
                        } else { let _ = reply.send(Err("raw input requires an SSH or local shell".into())); }
                        Ok(())
                    }
                    Some(Command::Capture(reply)) => {
                        let _ = reply.send(presentation.capture());
                        Ok(())
                    }
                    Some(Command::ExportText(reply)) => {
                        if !reply.is_closed() {
                            let _ = reply.send(terminal::export_text(&terminal));
                        }
                        Ok(())
                    }
                    Some(Command::Key { key, modifiers, down,repeat }) => {
                        scroll_offset = 0;
                        if is_demo && down { demo_key(&mut terminal, key, modifiers); Ok(()) }
                        else { terminal::send_key_event(&mut terminal, &writer, key, modifiers, down,repeat) }
                    }
                    Some(Command::DetailedKey {key,details,modifiers,down,repeat}) => {
                        scroll_offset=0;
                        if is_demo && down { if let Some(text)=details.text { presentation.advance(&mut terminal,text,scroll_offset,&status,total_bytes); } else { demo_key(&mut terminal,key,modifiers); } Ok(()) }
                        else { terminal::send_key_event_detailed(&mut terminal,&writer,key,modifiers,down,repeat,&details) }
                    }
                    Some(Command::Paste(text)) => {
                        scroll_offset = 0;
                        if is_demo { presentation.advance(&mut terminal,text.replace('\n', "\r\n"),scroll_offset,&status,total_bytes); Ok(()) } else { terminal.send_paste(&text) }
                    }
                    Some(Command::Text(text)) => {
                        scroll_offset = 0;
                        let mut result = Ok(());
                        for c in text.chars() {
                            if is_demo { demo_key(&mut terminal, KeyCode::Char(c), KeyModifiers::NONE); }
                            else if let Err(error) = terminal::send_key(&mut terminal, &writer, KeyCode::Char(c), KeyModifiers::NONE, true) { result = Err(error); break; }
                        }
                        result
                    }
                    Some(Command::Composed(text)) => {
                        scroll_offset = 0;
                        if is_demo {
                            for c in text.chars() {demo_key(&mut terminal,KeyCode::Char(c),KeyModifiers::NONE);}
                            Ok(())
                        } else {
                            writer.send(TransportInput::Data(text.into_bytes())).map_err(|_|anyhow::anyhow!("terminal transport closed"))
                        }
                    }
                    Some(Command::Resize(new_size)) => {
                        if size != new_size || (is_demo && !demo_loaded) {
                            presentation.dirty=true;
                            size = new_size;
                            terminal.resize(size);
                            if !terminal.is_alt_screen_active() {scroll_offset=scroll_offset.min(crate::search::max_offset(&terminal));}
                            if is_demo && !demo_loaded {
                                let bytes = demo::bytes();
                                total_bytes += bytes.len() as u64;
                                presentation.advance(&mut terminal,bytes,scroll_offset,&status,total_bytes);
                                demo_loaded = true;
                            }
                            let _ = writer.send(TransportInput::Resize(size));
                        }
                        Ok(())
                    }
                    Some(Command::Scroll(lines)) => {
                        if !terminal.is_alt_screen_active() { scroll_offset = scroll_offset.saturating_add_signed(lines).min(crate::search::max_offset(&terminal)); }
                        Ok(())
                    }
                    Some(Command::ClearScrollback) => {
                        if !terminal.is_alt_screen_active() && crate::search::max_offset(&terminal) > 0 {
                            terminal.erase_scrollback();
                            scroll_offset = 0;
                            search.refresh(&terminal, scroll_offset);
                            search_dirty = false;
                            presentation.scrollback_cleared(terminal.screen().visible_row_to_stable_row(0));
                        }
                        Ok(())
                    }
                    Some(Command::CopyRange(id, selection, reply)) => {
                        let _ = reply.send(crate::copy_mode::selected_text(&terminal, connection_generation, id, selection));
                        Ok(())
                    }
                    Some(Command::ScrollRange(request)) => {
                        if request.available()
                            && let Some(offset) = request.rows.scroll_offset(&terminal, connection_generation, request.align_top)
                        {
                            let view = (offset != scroll_offset).then(||
                                presentation.scrolled_view(&terminal, offset, &search));
                            request.commit(|| {
                                if let Some(view) = view {
                                    scroll_offset = offset;
                                    presentation.publish_scroll(view);
                                }
                            });
                        }
                        // The viewport was already published before acknowledging UIA.
                        continue;
                    }
                    Some(Command::ScrollTo(top)) => {
                        if !terminal.is_alt_screen_active() {
                            scroll_offset = terminal.screen().visible_row_to_stable_row(0).saturating_sub(top).max(0) as usize;
                            scroll_offset = scroll_offset.min(crate::search::max_offset(&terminal));
                        }
                        Ok(())
                    }
                    Some(Command::ScrollEdge(oldest)) => { if !terminal.is_alt_screen_active() {scroll_offset = if oldest {crate::search::max_offset(&terminal)}else{0};} Ok(()) }
                    Some(Command::Prompt(direction)) => {scroll_offset = crate::search::prompt_offset(&mut terminal,scroll_offset,direction);Ok(())}
                    Some(Command::Search { query, sensitive, regex }) => {
                        scroll_offset = search.configure_with_mode(query, sensitive, regex, &terminal, scroll_offset);
                        search_dirty = false;
                        Ok(())
                    }
                    Some(Command::SearchNavigate(direction)) => {
                        if search_dirty {search.refresh(&terminal,scroll_offset);search_dirty=false;}
                        scroll_offset=search.navigate(direction,&terminal,scroll_offset);Ok(())
                    }
                    Some(Command::SearchClose) => {search.open=false;Ok(())}
                    Some(Command::SearchPreview { query, reply }) => {
                        let preview = crate::search::PaneSearchPreview::scan(&terminal, &search_origin, query, || reply.is_closed());
                        let _ = reply.send(preview);
                        Ok(())
                    }
                    Some(Command::SearchReveal { query, anchor, reply }) => {
                        let prepared = anchor.prepare_reveal(&terminal, &search_origin, query, scroll_offset, || reply.is_closed());
                        if let Some((next, offset)) = prepared {
                            if reply.send(true).is_ok() {
                                search = next;
                                search_dirty = false;
                                scroll_offset = offset;
                                presentation.dirty = true;
                            }
                        } else {
                            let _ = reply.send(false);
                        }
                        Ok(())
                    }
                    Some(Command::Wheel(lines, modifiers)) => {
                        terminal::scroll_wheel(&mut terminal,&mut scroll_offset,last_mouse,lines,modifiers,player.read().unwrap().alternate_scroll_enabled())
                    }
                    Some(Command::Mouse(event)) => { last_mouse = event; terminal.mouse_event(event) },
                    Some(Command::Focus(focused)) => { terminal.focus_changed(focused); Ok(()) }
                    Some(Command::ReloadDemo) => { if is_demo { presentation.advance(&mut terminal,demo::bytes(),scroll_offset,&status,total_bytes); } Ok(()) }
                };
                if let Err(error) = result { status = format!("Input: {error:#}"); }
                presentation.dirty|=scroll_offset!=old_offset;
                if changes_screen && search.open && !search_dirty {search_dirty=true;search_refresh=tokio::time::Instant::now()+std::time::Duration::from_millis(150);}
            }
            event = output.recv(), if interactive && phase != Phase::Exited => {
                let Some(event) = event else {
                    // All sender clones are gone and every final Data/Status was processed.
                    // A transient disconnect with a scheduled reconnect never closes this queue.
                    let joined = transports.join_next().await;
                    prompts.cancel();
                    events.command_stream(false);
                    events.set_progress(wezterm_term::Progress::None);
                    if let Some(task) = asset_task.take() { task.abort(); }
                    pending_pack = Default::default();
                    while let Ok(notice) = notices.try_recv() {
                        if let crate::sound::Notice::Play(name) = notice
                            && sounds.try_send(name).is_err() {
                            events.push("sound-error",serde_json::json!({"reason":"audio queue full"}));
                        }
                    }
                    if let Some(Err(error)) = joined { status = format!("Terminal transport failed: {error}"); }
                    // A peer that exits halfway through a synchronized update
                    // cannot deliver an end marker. Publish its final image
                    // before making the restart token visible.
                    if terminal.synchronized_output() { presentation.expire(&mut terminal); }
                    presentation.dirty = true;
                    phase = Phase::PublishingExit;
                    continue;
                };
                let anchor = (scroll_offset>0 && !terminal.is_alt_screen_active()).then(||terminal.screen().visible_row_to_stable_row(0)-scroll_offset as isize);
                // Preserve mode transitions and data order even when the first paint
                // and a connection event arrive together in a single snapshot batch.
                let ready = std::iter::from_fn(|| output.try_recv().ok());
                for event in std::iter::once(event).chain(ready).take(8) {
                    match event {
                        ssh::Event::HerdrViewer => {
                            show_progress = false;
                            events.set_progress(wezterm_term::Progress::None);
                            terminal::enter_herdr_viewer(&mut terminal, &writer, &events);
                        }
                        ssh::Event::Disconnected => {
                            if let Phase::Restarting { reset } = &mut phase { *reset = false; }
                            events.command_stream(false);
                            events.set_progress(wezterm_term::Progress::None);
                            show_progress = interactive;
                        }
                        ssh::Event::Data(bytes) => {
                            if observed.receiver_count() > 0 {
                                let _ = observed.send(bytes.clone());
                            }
                            record_output(&mut recorder, &bytes, &events);
                            total_bytes += bytes.len() as u64;
                            let before = terminal.get_progress();
                            presentation.advance(&mut terminal, bytes, scroll_offset, &status, total_bytes);
                            if show_progress && before != terminal.get_progress() {
                                events.set_progress(terminal.get_progress());
                            }
                        }
                        ssh::Event::Status(message) => {
                            log::info!("{message}");
                            status = message;
                        }
                        ssh::Event::SessionListUpdated(message) => {
                            status = message;
                            presentation.refresh_view = true;
                        }
                        ssh::Event::Connected(generation) => {
                            search_origin = Arc::new(());
                            if matches!(phase, Phase::Restarting { .. }) {
                                // Anything already waiting in this FIFO predates the ready frame.
                                // Apply local configuration, but never deliver its queued user input.
                                queued_before_connection = commands.len();
                            }
                            if generation > 1 && !matches!(phase, Phase::Restarting { reset: true }) {
                                terminal.reset_connection();
                                events.set_progress(wezterm_term::Progress::None);
                                scroll_offset = 0;
                            }
                            presentation.dirty = true;
                            connection_generation = generation;
                            phase = Phase::Running;
                            events.command_stream(live_commands);
                        }
                    }
                }
                if let Some(anchor)=anchor && !terminal.is_alt_screen_active() {scroll_offset=(terminal.screen().visible_row_to_stable_row(0)-anchor).max(0).min(crate::search::max_offset(&terminal) as isize) as usize;}
                if search.open && !search_dirty {search_dirty=true;search_refresh=tokio::time::Instant::now()+std::time::Duration::from_millis(150);}
            }
        }
    }
    events.command_stream(false);
    drop(output);
    transports.shutdown().await;
    if let Some(task) = asset_task {
        task.abort();
    }
    if let Some(recorder) = recorder {
        let _ = recorder.finish();
    }
}

fn can_watch_commands(source: &Source) -> bool {
    match source {
        Source::Ssh(options) => !options.durable,
        Source::Local(_) => true,
        Source::Replay(_) | Source::Demo => false,
    }
}

fn can_restart(live_shell: bool, startup: Option<&crate::workspace::SshStartup>) -> bool {
    live_shell && startup.is_none_or(|recipe| recipe.command.is_empty())
}

fn direct_ssh_shell(
    source: &Source,
    managed: bool,
    startup: Option<&crate::workspace::SshStartup>,
) -> bool {
    !managed
        && matches!(source, Source::Ssh(ssh) if !ssh.durable && !ssh.player.rustmush)
        && startup.is_none_or(|launch| launch.command.is_empty())
}

fn record_output(
    recorder: &mut Option<crate::recorder::Recorder>,
    bytes: &[u8],
    events: &crate::events::Events,
) {
    if let Some(active) = recorder
        && let Err(error) = active.write(bytes)
    {
        events.push(
            "recording-error",
            serde_json::json!({"error":error.to_string()}),
        );
        if let Some(stopped) = recorder.take() {
            let _ = stopped.finish();
        }
    }
}

fn demo_key(terminal: &mut wezterm_term::Terminal, key: KeyCode, modifiers: KeyModifiers) {
    match key {
        KeyCode::Char('l') if modifiers.contains(KeyModifiers::CTRL) => {
            terminal.advance_bytes(b"\x1b[2J\x1b[Hlab> ")
        }
        KeyCode::Char('\r') => terminal.advance_bytes(b"\r\nlab> "),
        KeyCode::Char('\u{8}') => terminal.advance_bytes(b"\x08 \x08"),
        KeyCode::Char(c) if !c.is_control() && !modifiers.contains(KeyModifiers::CTRL) => {
            let mut buffer = [0; 4];
            terminal.advance_bytes(c.encode_utf8(&mut buffer).as_bytes());
        }
        _ => {}
    }
}

pub fn benchmark() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut terminal = terminal::create(
        TerminalSize {
            rows: 40,
            cols: 120,
            ..TerminalSize::default()
        },
        tx,
    );
    let line = b"\x1b[38;2;126;204;250m2026-10-21\x1b[0m parser / scrollback / ANSI benchmark: 0123456789 abcdefghijklmnopqrstuvwxyz\r\n";
    let block: Vec<u8> = line.repeat(512);
    let started = std::time::Instant::now();
    for _ in 0..256 {
        terminal.advance_bytes(&block);
    }
    let bytes = block.len() * 256;
    let elapsed = started.elapsed();
    println!(
        "Engine-only replay: {:.1} MiB in {:.3}s ({:.1} MiB/s). Excludes SSH, snapshots, and rendering.",
        bytes as f64 / 1048576.0,
        elapsed.as_secs_f64(),
        bytes as f64 / 1048576.0 / elapsed.as_secs_f64()
    );
}

#[cfg(test)]
mod failure_tests {
    use super::*;
    #[test]
    fn directory_splits_require_an_interactive_direct_ssh_launch() {
        let mut ssh: crate::options::SshOptions = serde_json::from_value(
            serde_json::json!({"user":"user","host":"localhost","auth":"none"}),
        )
        .unwrap();
        let directory =
            crate::workspace::SshStartup::new(&ssh, "/work".into(), String::new()).unwrap();
        let command =
            crate::workspace::SshStartup::new(&ssh, "/work".into(), "tail -f log".into()).unwrap();
        let ordinary = Source::Ssh(ssh.clone());
        assert!(direct_ssh_shell(&ordinary, false, None));
        assert!(direct_ssh_shell(&ordinary, false, Some(&directory)));
        assert!(!direct_ssh_shell(&ordinary, false, Some(&command)));
        assert!(!direct_ssh_shell(&ordinary, true, None));
        assert!(!direct_ssh_shell(&Source::Demo, false, None));
        ssh.durable = true;
        assert!(!direct_ssh_shell(&Source::Ssh(ssh.clone()), false, None));
        ssh.durable = false;
        ssh.player.rustmush = true;
        assert!(!direct_ssh_shell(&Source::Ssh(ssh), false, None));
    }

    #[test]
    fn command_watch_accepts_live_shells_and_excludes_reconstructed_streams() {
        let mut ssh: crate::options::SshOptions = serde_json::from_value(
            serde_json::json!({"user":"user","host":"localhost","auth":"none"}),
        )
        .unwrap();
        assert!(can_watch_commands(&Source::Ssh(ssh.clone())));
        let directory =
            crate::workspace::SshStartup::new(&ssh, "/work".into(), String::new()).unwrap();
        let command =
            crate::workspace::SshStartup::new(&ssh, "/work".into(), "build".into()).unwrap();
        assert!(can_restart(true, None) && can_restart(true, Some(&directory)));
        assert!(!can_restart(true, Some(&command)) && !can_restart(false, None));
        ssh.durable = true;
        assert!(!can_watch_commands(&Source::Ssh(ssh)));
        assert!(can_watch_commands(&Source::Local(
            crate::options::LocalOptions {
                executable: "pwsh".into(),
                args: Vec::new(),
                cwd: None,
                powershell_integration: false,
                scrollback_lines: Default::default(),
            }
        )));
        assert!(!can_watch_commands(&Source::Demo));
        assert!(!can_watch_commands(&Source::Replay("history.vt".into())));
    }

    #[test]
    fn panic_publishes_a_visible_failure_and_preserves_last_committed_pixels() {
        let (writer, _) = mpsc::unbounded_channel();
        let terminal = terminal::create(Default::default(), writer);
        let frames = FrameStore::new(Snapshot::capture(&terminal, None, 1, 0, "ready".into(), 0));
        let events = crate::events::Events::default();
        let before = frames.load();
        contain_worker_failure(&frames, &events, || panic!("injected engine failure"));
        let after = frames.load();
        assert_eq!(events.since(0).events[0].kind, "session-error");
        assert!(after.status.contains("Terminal engine failed"));
        assert_eq!(after.generation, before.generation + 1);
        assert_eq!(after.rows, before.rows);
    }
}
