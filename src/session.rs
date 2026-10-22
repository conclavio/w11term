use crate::{
    demo,
    options::Source,
    ssh,
    terminal::{self, FrameStore, Snapshot, TransportInput},
};
use std::{
    sync::{Arc, atomic::AtomicBool},
    thread,
};
use tokio::sync::mpsc;
use wezterm_term::{KeyCode, KeyModifiers, MouseEvent, TerminalSize};

#[derive(Debug)]
pub enum Command {
    Sftp(crate::sftp::Job),
    Key {
        key: KeyCode,
        modifiers: KeyModifiers,
        down: bool,
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
    pub input: mpsc::UnboundedSender<Command>,
    pub frames: FrameStore,
    pub clipboard_enabled: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
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
        let (writer, transport_input) = mpsc::unbounded_channel();
        let mut terminal = terminal::create(size, writer.clone());
        let clipboard_enabled = Arc::new(AtomicBool::new(allow_clipboard));
        let clipboard: Arc<dyn wezterm_term::Clipboard> =
            Arc::new(crate::clipboard::RemoteClipboard(clipboard_enabled.clone()));
        terminal.set_clipboard(&clipboard);
        let frames = FrameStore::new(Snapshot::capture(
            &terminal,
            None,
            1,
            0,
            "Starting…".into(),
            0,
        ));
        let worker_frames = frames.clone();
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
                    size,
                ));
            })
            .expect("starting terminal worker");
        Self {
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
    mut size: TerminalSize,
) {
    let (output_tx, mut output) = mpsc::channel(32);
    let is_demo = matches!(source, Source::Demo);
    let is_ssh = matches!(source, Source::Ssh(_));
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
    let transport = match source {
        Source::Ssh(options) => {
            status = format!(
                "Connecting to {}@{}:{}…",
                options.user, options.host, options.port
            );
            Some(tokio::spawn(async move {
                let result = ssh::run(options, size, transport_input, output_tx.clone()).await;
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
                    terminal.advance_bytes(bytes);
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
    let mut generation = 1;
    let mut search = crate::search::Search::default();
    let mut search_dirty = false;
    let mut search_refresh = tokio::time::Instant::now();
    loop {
        generation += 1;
        let previous = frames.load();
        let mut frame = Snapshot::capture(
            &terminal,
            Some(&previous),
            generation,
            scroll_offset,
            status.clone(),
            total_bytes,
        );
        frame.search = search.snapshot(frame.first_stable_row, frame.rows.len(), frame.alternate);
        frames.publish(frame);
        tokio::select! {
            _ = tokio::time::sleep_until(search_refresh), if search.open && search_dirty => {
                search.refresh(&terminal, scroll_offset);
                search_dirty = false;
            }
            command = commands.recv() => {
                let changes_screen = matches!(&command,Some(Command::Resize(_) | Command::Key {..} | Command::Paste(_) | Command::Text(_) | Command::ReloadDemo));
                let result = match command {
                    Some(Command::Shutdown) | None => break,
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
                        let _ = reply.send((Snapshot::capture(&terminal, None, generation, 0, status.clone(), total_bytes), size));
                        Ok(())
                    }
                    Some(Command::Key { key, modifiers, down }) => {
                        scroll_offset = 0;
                        if is_demo && down { demo_key(&mut terminal, key, modifiers); Ok(()) }
                        else { terminal::send_key(&mut terminal, &writer, key, modifiers, down) }
                    }
                    Some(Command::Paste(text)) => {
                        scroll_offset = 0;
                        if is_demo { terminal.advance_bytes(text.replace('\n', "\r\n")); Ok(()) } else { terminal.send_paste(&text) }
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
                            size = new_size;
                            terminal.resize(size);
                            if !terminal.is_alt_screen_active() {scroll_offset=scroll_offset.min(crate::search::max_offset(&terminal));}
                            if is_demo && !demo_loaded {
                                let bytes = demo::bytes();
                                total_bytes += bytes.len() as u64;
                                terminal.advance_bytes(bytes);
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
                    Some(Command::ReloadDemo) => { if is_demo { terminal.advance_bytes(demo::bytes()); } Ok(()) }
                };
                if let Err(error) = result { status = format!("Input: {error:#}"); }
                if changes_screen && search.open && !search_dirty {search_dirty=true;search_refresh=tokio::time::Instant::now()+std::time::Duration::from_millis(150);}
            }
            Some(event) = output.recv() => {
                let anchor = (scroll_offset>0 && !terminal.is_alt_screen_active()).then(||terminal.screen().visible_row_to_stable_row(0)-scroll_offset as isize);
                match event {
                    ssh::Event::Data(bytes) => { total_bytes += bytes.len() as u64; terminal.advance_bytes(bytes); }
                    ssh::Event::Status(message) => { log::info!("{message}"); status = message; }
                }
                // Coalesce a bounded amount of already-arrived output per snapshot.
                for _ in 0..7 {
                    match output.try_recv() {
                        Ok(ssh::Event::Data(bytes)) => { total_bytes += bytes.len() as u64; terminal.advance_bytes(bytes); }
                        Ok(ssh::Event::Status(message)) => status = message,
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
