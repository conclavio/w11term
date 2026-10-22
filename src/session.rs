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
mod presentation;
use presentation::Presentation;

#[derive(Debug)]
pub enum Command {
    Observe(tokio::sync::oneshot::Sender<tokio::sync::broadcast::Receiver<Vec<u8>>>),
    Record {
        path: Option<std::path::PathBuf>,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    Authenticate(crate::options::Authentication),
    Events(tokio::sync::oneshot::Sender<crate::events::Events>),
    Frames(tokio::sync::oneshot::Sender<FrameStore>),
    Sftp(crate::sftp::Job),
    Key {
        key: KeyCode,
        modifiers: KeyModifiers,
        down: bool,
        repeat: bool,
    },
    Paste(String),
    Text(String),
    Raw(Vec<u8>, tokio::sync::oneshot::Sender<Result<(), String>>),
    Capture(tokio::sync::oneshot::Sender<(Snapshot, TerminalSize)>),
    Resize(TerminalSize),
    Scroll(isize),
    ScrollEdge(bool),
    Prompt(isize),
    Search {
        query: String,
        sensitive: bool,
    },
    SearchNavigate(isize),
    SearchClose,
    Wheel(isize, KeyModifiers),
    Mouse(MouseEvent),
    Focus(bool),
    ReloadDemo,
    Shutdown,
}

pub struct Session {
    pub prompts: crate::prompts::Prompts,
    pub events: crate::events::Events,
    pub player: Arc<std::sync::RwLock<crate::options::PlayerOptions>>,
    pub input: mpsc::UnboundedSender<Command>,
    pub frames: FrameStore,
    pub clipboard_enabled: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

struct Services {
    prompts: crate::prompts::Prompts,
    events: crate::events::Events,
    player: Arc<std::sync::RwLock<crate::options::PlayerOptions>>,
    notices: mpsc::Receiver<crate::sound::Notice>,
    live_frames: FrameStore,
}

impl Session {
    pub fn start(source: Source) -> Self {
        Self::start_sized(source, TerminalSize::default())
    }

    pub fn start_sized(source: Source, size: TerminalSize) -> Self {
        Self::start_configured(source, size, true)
    }
    pub fn start_configured(source: Source, size: TerminalSize, allow_clipboard: bool) -> Self {
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
        let mut terminal = terminal::create(size, writer.clone());
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
        let worker = thread::Builder::new()
            .name("terminal-session".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("creating terminal runtime");
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
                    },
                ));
            })
            .expect("starting terminal worker");
        Self {
            prompts,
            events,
            player,
            input: commands,
            frames,
            clipboard_enabled,
            worker: Some(worker),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
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
    writer: mpsc::UnboundedSender<TransportInput>,
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
    } = services;
    let mut size = terminal.get_size();
    let (output_tx, mut output) = mpsc::channel(32);
    let is_demo = matches!(source, Source::Demo);
    let is_ssh = matches!(source, Source::Ssh(_));
    let endpoint = match &source {
        Source::Ssh(options) => format!("{}@{}:{}", options.user, options.host, options.port),
        _ => String::new(),
    };
    let assets = crate::assets::Store::default();
    let sounds = crate::sound::worker(player.clone(), assets.clone(), events.clone());
    let mut asset_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut status;
    let mut total_bytes = 0u64;
    let mut scroll_offset = 0usize;
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
    let transport = match source {
        Source::Ssh(options) => {
            status = format!(
                "Connecting to {}@{}:{}…",
                options.user, options.host, options.port
            );
            let prompts = prompts.clone();
            let events = events.clone();
            Some(tokio::spawn(async move {
                let result = ssh::run_configured(
                    options,
                    size,
                    transport_input,
                    output_tx.clone(),
                    Some(prompts),
                    events,
                )
                .await;
                let message = match result {
                    Ok(()) => "Disconnected".into(),
                    Err(error) => format!("SSH: {error:#}"),
                };
                let _ = output_tx.send(ssh::Event::Status(message)).await;
            }))
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
            Some(tokio::spawn(async move {
                while transport_input.recv().await.is_some() {}
            }))
        }
        Source::Demo => {
            status = "Terminal lab · F5 reload · Shift+PageUp/PageDown scroll · Ctrl+Shift+V paste"
                .into();
            Some(tokio::spawn(async move {
                while transport_input.recv().await.is_some() {}
            }))
        }
    };
    let mut presentation = Presentation::new(frames.clone(), live_frames.clone());
    if let Some(bytes) = replay {
        presentation.advance(&mut terminal, bytes, 0, &status, total_bytes);
    }
    let mut search = crate::search::Search::default();
    let mut search_dirty = false;
    let mut search_refresh = tokio::time::Instant::now();
    let mut connection_generation = 0;
    let mut recorder: Option<crate::recorder::Recorder> = None;
    let (observed, _) = tokio::sync::broadcast::channel::<Vec<u8>>(32);
    loop {
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
        tokio::select! {
            _=tokio::time::sleep_until(presentation.deadline.unwrap_or_else(||tokio::time::Instant::now()+std::time::Duration::from_secs(86400))),if presentation.deadline.is_some()=>{presentation.expire(&mut terminal);},
            _=prompts.changed()=>{},
            Some(notice)=notices.recv()=>match notice {
                crate::sound::Notice::Play(name)=>{if sounds.try_send(name).is_err(){events.push("sound-error",serde_json::json!({"reason":"audio queue full"}));}},
                crate::sound::Notice::Pack(version)=>{
                    if is_ssh&&player.read().unwrap().sync_assets&&assets.version()!=version&&asset_task.as_ref().is_none_or(|t|t.is_finished()) {
                        let input=writer.clone();let endpoint=endpoint.clone();let assets=assets.clone();let events=events.clone();
                        asset_task=Some(tokio::spawn(async move{
                            events.push("assets-syncing",serde_json::json!({"pack_version":version}));
                            let result=tokio::time::timeout(std::time::Duration::from_secs(180),crate::assets::sync(input,endpoint,version.clone(),assets)).await;
                            match result {Ok(Ok(fetched))=>events.push("assets-ready",serde_json::json!({"pack_version":version,"downloaded":fetched})),other=>events.push("assets-error",serde_json::json!({"pack_version":version,"error":format!("{other:?}")}))}
                        }));
                    }
                }
            },
            _ = tokio::time::sleep_until(search_refresh), if search.open && search_dirty => {
                search.refresh(&terminal, scroll_offset);
                search_dirty = false;
                presentation.dirty=true;
            }
            command = commands.recv() => {
                let changes_screen = matches!(&command,Some(Command::Resize(_) | Command::Key {..} | Command::Paste(_) | Command::Text(_) | Command::ReloadDemo));
                let old_offset=scroll_offset;
                presentation.dirty|=matches!(&command,Some(Command::Scroll(_) | Command::ScrollEdge(_) | Command::Wheel(..) | Command::Prompt(_) | Command::Search{..} | Command::SearchNavigate(_) | Command::SearchClose | Command::ReloadDemo)) || (is_demo && changes_screen);
                let result = match command {
                    Some(Command::Shutdown) | None => break,
                    Some(Command::Observe(reply))=>{let _=reply.send(observed.subscribe());Ok(())},
                    Some(Command::Record{path,reply})=>{
                        let result=(||->anyhow::Result<serde_json::Value>{
                            if let Some(path)=path {anyhow::ensure!(recorder.is_none(),"pane is already recording");let started=crate::recorder::Recorder::start(&path,size)?;recorder=Some(started);events.push("recording-started",serde_json::json!({"path":path}));Ok(serde_json::json!({"recording":true,"path":path}))}
                            else {let stopped=recorder.take().context("pane is not recording")?;let bytes=stopped.bytes;let path=stopped.path.clone();stopped.finish()?;events.push("recording-stopped",serde_json::json!({"bytes":bytes}));Ok(serde_json::json!({"recording":false,"path":path,"bytes":bytes}))}
                        })();let _=reply.send(result.map_err(|e|format!("{e:#}")));Ok(())
                    },
                    Some(Command::Authenticate(auth))=>{let _=writer.send(TransportInput::Authenticate(auth));Ok(())},
                    Some(Command::Events(reply))=>{let _=reply.send(events.clone());Ok(())},
                    Some(Command::Frames(reply))=>{let _=reply.send(live_frames.clone());Ok(())},
                    Some(Command::Sftp(job)) => {
                        if is_ssh {
                            if let Err(error) = writer.send(TransportInput::Sftp(job))
                                && let TransportInput::Sftp(job) = error.0 { let _ = job.reply.send(Err("SSH connection is closed".into())); }
                        } else { let _ = job.reply.send(Err("SFTP requires an SSH session".into())); }
                        Ok(())
                    }
                    Some(Command::Raw(bytes, reply)) => {
                        if is_ssh {
                            if let Err(error) = writer.send(TransportInput::Raw(bytes, reply))
                                && let TransportInput::Raw(_, reply) = error.0 { let _ = reply.send(Err("SSH connection is closed".into())); }
                        } else { let _ = reply.send(Err("raw input requires an SSH session".into())); }
                        Ok(())
                    }
                    Some(Command::Capture(reply)) => {
                        let _ = reply.send(presentation.capture());
                        Ok(())
                    }
                    Some(Command::Key { key, modifiers, down,repeat }) => {
                        scroll_offset = 0;
                        if is_demo && down { demo_key(&mut terminal, key, modifiers); Ok(()) }
                        else { terminal::send_key_event(&mut terminal, &writer, key, modifiers, down,repeat) }
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
                    Some(Command::ScrollEdge(oldest)) => { if !terminal.is_alt_screen_active() {scroll_offset = if oldest {crate::search::max_offset(&terminal)}else{0};} Ok(()) }
                    Some(Command::Prompt(direction)) => {scroll_offset = crate::search::prompt_offset(&mut terminal,scroll_offset,direction);Ok(())}
                    Some(Command::Search {query,sensitive}) => {
                        if query.len()<=4096 {scroll_offset=search.configure(query,sensitive,&terminal,scroll_offset);search_dirty=false;} Ok(())
                    }
                    Some(Command::SearchNavigate(direction)) => {
                        if search_dirty {search.refresh(&terminal,scroll_offset);search_dirty=false;}
                        scroll_offset=search.navigate(direction,&terminal,scroll_offset);Ok(())
                    }
                    Some(Command::SearchClose) => {search.open=false;Ok(())}
                    Some(Command::Wheel(lines, modifiers)) => {
                        terminal::scroll_wheel(&mut terminal,&mut scroll_offset,last_mouse,lines,modifiers)
                    }
                    Some(Command::Mouse(event)) => { last_mouse = event; terminal.mouse_event(event) },
                    Some(Command::Focus(focused)) => { terminal.focus_changed(focused); Ok(()) }
                    Some(Command::ReloadDemo) => { if is_demo { presentation.advance(&mut terminal,demo::bytes(),scroll_offset,&status,total_bytes); } Ok(()) }
                };
                if let Err(error) = result { status = format!("Input: {error:#}"); }
                presentation.dirty|=scroll_offset!=old_offset;
                if changes_screen && search.open && !search_dirty {search_dirty=true;search_refresh=tokio::time::Instant::now()+std::time::Duration::from_millis(150);}
            }
            Some(event) = output.recv() => {
                let anchor = (scroll_offset>0 && !terminal.is_alt_screen_active()).then(||terminal.screen().visible_row_to_stable_row(0)-scroll_offset as isize);
                match event {
                    ssh::Event::Data(bytes) => {if observed.receiver_count()>0{let _=observed.send(bytes.clone());}record_output(&mut recorder,&bytes,&events);total_bytes += bytes.len() as u64; presentation.advance(&mut terminal,bytes,scroll_offset,&status,total_bytes); }
                    ssh::Event::Status(message) => { log::info!("{message}"); status = message; }
                    ssh::Event::Connected(generation)=>{if generation>1{terminal.reset_connection();scroll_offset=0;}presentation.dirty=true;connection_generation=generation;},
                }
                // Coalesce a bounded amount of already-arrived output per snapshot.
                for _ in 0..7 {
                    match output.try_recv() {
                        Ok(ssh::Event::Data(bytes)) => {if observed.receiver_count()>0{let _=observed.send(bytes.clone());}record_output(&mut recorder,&bytes,&events);total_bytes += bytes.len() as u64; presentation.advance(&mut terminal,bytes,scroll_offset,&status,total_bytes); }
                        Ok(ssh::Event::Status(message)) => status = message,
                        Ok(ssh::Event::Connected(generation))=>{if generation>1{terminal.reset_connection();scroll_offset=0;}presentation.dirty=true;connection_generation=generation;},
                        Err(_) => break,
                    }
                }
                if let Some(anchor)=anchor && !terminal.is_alt_screen_active() {scroll_offset=(terminal.screen().visible_row_to_stable_row(0)-anchor).max(0).min(crate::search::max_offset(&terminal) as isize) as usize;}
                if search.open && !search_dirty {search_dirty=true;search_refresh=tokio::time::Instant::now()+std::time::Duration::from_millis(150);}
            }
        }
    }
    if let Some(task) = transport {
        task.abort();
    }
    if let Some(task) = asset_task {
        task.abort();
    }
    if let Some(recorder) = recorder {
        let _ = recorder.finish();
    }
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
