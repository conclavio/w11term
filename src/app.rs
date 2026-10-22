use crate::{
    input::{KeyboardBridge, Shortcut},
    renderer::{Metrics, PADDING, Renderer, STATUS_HEIGHT},
};
use anyhow::Context as _;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use w11term::{
    appearance::{Appearance, installed_fonts},
    control::{Pane, Registry, Server},
    interaction::{
        Link, Point, Selection, TabSwitcherMode, link_at, paste_text, safe_url, word_range,
    },
    options::{Authentication, Options, Source, SshOptions},
    profiles::ProfileStore,
    session::{Command, Session},
    tabs::{TabId, Tabs},
};
use wezterm_term::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind, TerminalSize};
use windows_canvas::{Canvas, Invalidator};
use windows_reactor::*;

const NEW_PROFILE: &str = "New Profile…";
const LAB: &str = "Terminal lab";

struct LiveTab {
    device_busy: bool,
    device_message: Option<String>,
    session: Session,
    last_size: Rc<Cell<Option<TerminalSize>>>,
    error: Option<String>,
    selection: Rc<RefCell<Option<Selection>>>,
    hover: Rc<RefCell<Option<Link>>>,
    remote_button: MouseButton,
    clicked_link: Option<String>,
    last_click: Option<(Instant, Point, u8)>,
    scroll_direction: isize,
    scroll_tick: bool,
    gesture: u64,
    find: Find,
    prompt_id: Option<u64>,
    answers: Vec<zeroize::Zeroizing<String>>,
    connection_generation: u64,
}
struct Find {
    open: bool,
    query: String,
    sensitive: bool,
    epoch: u64,
    input: ElementRef<TextBox>,
    focused: bool,
}
impl Default for Find {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            sensitive: false,
            epoch: 0,
            input: ElementRef::new(),
            focused: false,
        }
    }
}
struct TabSwitcher {
    ids: Vec<TabId>,
    index: usize,
}
#[derive(Clone, Copy)]
pub enum InteractionField {
    CopyOnSelect,
    TrimBlock,
    TrimPaste,
    Snap,
    ScrollZoom,
    DetectUrls,
    Osc52,
}
enum Page {
    Terminal(LiveTab),
    Settings,
    NewProfile(ProfileDraft),
}
struct AppTab {
    label: String,
    page: Page,
}

#[derive(Clone)]
struct ProfileDraft {
    name: String,
    host: String,
    user: String,
    port: String,
    identity: String,
    fingerprint: String,
    authentication: usize,
    error: Option<String>,
    player: w11term::options::PlayerOptions,
    device: Option<w11term::keys::DeviceKey>,
    generating: bool,
}

impl Default for ProfileDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            user: String::new(),
            port: "22".into(),
            identity: String::new(),
            fingerprint: String::new(),
            authentication: 0,
            error: None,
            player: Default::default(),
            device: None,
            generating: false,
        }
    }
}

#[derive(Clone, Copy)]
pub enum Field {
    Name,
    Host,
    User,
    Port,
    Identity,
    Fingerprint,
}

pub struct TerminalApp {
    windows: Rc<RefCell<crate::window_state::Windows>>,
    window_profile: Option<String>,
    window_restored: bool,
    // Unhook before dropping tabs and shutting down their independent sessions.
    keyboard: KeyboardBridge,
    _control: Server,
    registry: Registry,
    control_rx: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<w11term::control::UiRequest>>>,
    control_task: Option<ComponentTask>,
    tabs: Tabs<AppTab>,
    renderer: Rc<RefCell<Renderer>>,
    metrics: Rc<Cell<Metrics>>,
    invalidator: Invalidator,
    header: RefCell<Option<View>>,
    frame_task: Option<ComponentTask>,
    watcher_retry: Option<crate::ui_timer::RetryTimer>,
    watch_epoch: u64,
    default_source: Source,
    default_label: String,
    profiles: BTreeMap<String, SshOptions>,
    store: ProfileStore,
    profile_error: Option<String>,
    width: f64,
    selection_pending: bool,
    selection_epoch: u64,
    client_size: (f64, f64),
    window_height: f64,
    initial_grid: Rc<crate::window_state::RequestedGrid>,
    appearance: Arc<RwLock<Appearance>>,
    fonts: Vec<String>,
    appearance_error: Option<String>,
    terminal_area: Rc<Cell<Option<(f32, f32, f32)>>>,
    mru: Vec<TabId>,
    switcher: Option<TabSwitcher>,
}

pub enum Message {
    DeviceAction(bool),
    DeviceDone(TabId, bool, Result<String, String>),
    DraftBell(TabId, String),
    RestoreWindow,
    TogglePlayer(String),
    ControlIdle,
    WatchRejected,
    RetryWatchers,
    Control(w11term::control::UiRequest),
    ControlResizeDone {
        pane: u64,
        cols: usize,
        rows: usize,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    PromptEdit(TabId, u64, usize, String),
    PromptReply(TabId, u64, ContentDialogResult),
    GenerateKey(TabId),
    KeyGenerated(TabId, Result<w11term::keys::DeviceKey, String>),
    CopyPublicKey(TabId),
    DraftPlayer(TabId, String, bool),
    FrameReady(TabId, u64),
    CloseWindow,
    Select(Option<usize>, u64, bool),
    CloseTab(String),
    Reorder(Vec<String>, Option<TabId>),
    SyncSelection(u64),
    Shortcut(Shortcut),
    ProfileMenu(String),
    SettingsMenu,
    Font(Option<usize>),
    Ligatures(bool),
    Interaction(InteractionField, bool),
    TabMode(Option<usize>),
    PickTab(TabId),
    CaptureLost(TabId),
    FinalizeCapture(TabId, u64),
    PointerExit(TabId),
    DragScroll(TabId),
    PasteRaw,
    Paste,
    RawPaste(Result<(), String>),
    Pointer(TabId, PointerEventInfo, MouseEventKind),
    RenderError(TabId, String),
    RetryRender(TabId),
    Edit(TabId, Field, String),
    Auth(TabId, Option<usize>),
    SaveProfile(TabId),
    WindowSize(WindowSize),
    Geometry(f64, f64, u64, bool),
    GridFallback(u64),
    FindEdit(TabId, String),
    FindCase(TabId, bool),
    FindApply(TabId, u64),
    FindFocus(TabId, u64, u8),
    FindFocusResult(TabId, u64, bool),
    #[cfg(feature = "ui-smoke")]
    Smoke(u8),
}

fn live_tab(label: String, source: Source, osc52: bool) -> AppTab {
    live_tab_sized(label, source, TerminalSize::default(), osc52)
}
fn live_tab_sized(label: String, source: Source, size: TerminalSize, osc52: bool) -> AppTab {
    AppTab {
        label,
        page: Page::Terminal(LiveTab {
            device_busy: false,
            device_message: None,
            session: Session::start_configured(source, size, osc52),
            last_size: Rc::new(Cell::new(None)),
            error: None,
            selection: Rc::new(RefCell::new(None)),
            hover: Rc::new(RefCell::new(None)),
            remote_button: MouseButton::None,
            clicked_link: None,
            last_click: None,
            scroll_direction: 0,
            scroll_tick: false,
            gesture: 0,
            find: Find::default(),
            prompt_id: None,
            answers: Vec::new(),
            connection_generation: 0,
        }),
    }
}

fn source_label(source: &Source) -> String {
    match source {
        Source::Demo => LAB.into(),
        Source::Replay(path) => format!("Replay · {}", path.display()),
        Source::Ssh(ssh) => format!("{}@{}:{}", ssh.user, ssh.host, ssh.port),
    }
}

fn profile_label(name: &str, ssh: &SshOptions) -> String {
    format!("{name}    {}@{}:{}", ssh.user, ssh.host, ssh.port)
}

impl TerminalApp {
    fn watch_control(&mut self, context: &ComponentContext<Self>) {
        let receiver = self.control_rx.clone();
        self.control_task = Some(context.spawn_background_with_rejection(
            move |cancel| {
                loop {
                    if cancel.is_cancelled() {
                        return Message::ControlIdle;
                    }
                    match receiver
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_millis(200))
                    {
                        Ok(request) => return Message::Control(request),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => return Message::ControlIdle,
                    }
                }
            },
            Message::WatchRejected,
        ));
    }
    fn connection_ui(&mut self, id: TabId) {
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get_mut(id)
        else {
            return;
        };
        let frame = tab.session.frames.load();
        let prompt_id = frame.prompt.as_ref().map(|p| p.id);
        let changed = prompt_id != tab.prompt_id;
        self.keyboard
            .set_game_mode(tab.session.player.read().unwrap().game_mode);
        if changed {
            tab.answers = match frame.prompt.as_ref().map(|p| &p.question) {
                Some(w11term::prompts::Question::Authentication { challenges, .. }) => (0
                    ..challenges.len())
                    .map(|_| zeroize::Zeroizing::new(String::new()))
                    .collect(),
                _ => Vec::new(),
            };
            tab.prompt_id = prompt_id;
        }
        if tab.connection_generation != frame.connection_generation {
            tab.connection_generation = frame.connection_generation;
            self.keyboard.reset_keys();
        }
        self.keyboard.set_modal(prompt_id.is_some());
        if changed {
            self.keyboard
                .set_target(if prompt_id.is_none() && !tab.device_busy {
                    Some((tab.session.input.clone(), tab.session.frames.clone()))
                } else {
                    None
                });
            self.keyboard
                .set_search(prompt_id.is_none() && tab.find.open);
        } else if prompt_id.is_some() || tab.device_busy {
            self.keyboard.set_target(None);
        }
    }
    fn prompt_view(
        &self,
        id: TabId,
        tab: &LiveTab,
        prompt: &w11term::prompts::Prompt,
        context: &ViewContext<Self>,
    ) -> View {
        use w11term::prompts::Question;
        let prompt_id = prompt.id;
        let (title,primary,children):(String,&str,Vec<View>)=match &prompt.question {
            Question::HostKey{host,port,algorithm,fingerprint}=>("Trust this SSH host?".into(),"Accept and save",vec![
                TextBlock::new().text(format!("First connection to {host}:{port}")).text_wrapping(TextWrapping::Wrap).into(),
                TextBlock::new().text(format!("{algorithm}\n{fingerprint}")).text_wrapping(TextWrapping::Wrap).into(),
                TextBlock::new().text("Verify this fingerprint with the server owner. Accepting saves it in your OpenSSH known_hosts file.").text_wrapping(TextWrapping::Wrap).into(),
            ]),
            Question::Authentication{name,instructions,challenges}=> {
                let mut children:Vec<View>=vec![TextBlock::new().text(instructions).text_wrapping(TextWrapping::Wrap).into()];
                for (index,challenge) in challenges.iter().enumerate(){
                    children.push(TextBlock::new().text(&challenge.label).text_wrapping(TextWrapping::Wrap).into());
                    let value=tab.answers.get(index).map_or("",|v|v.as_str());
                    children.push(if challenge.echo {
                        TextBox::new().text(value).automation_name(&challenge.label).on_text_changed(context.callback(move |value|Message::PromptEdit(id,prompt_id,index,value))).into()
                    }else{
                        PasswordBox::new().password(value).automation_name(&challenge.label).password_reveal_mode(PasswordRevealMode::Hidden).on_password_changed(context.callback(move |value|Message::PromptEdit(id,prompt_id,index,value))).into()
                    });
                }
                (if name.is_empty(){"SSH authentication".into()}else{name.clone()},"Sign in",children)
            }
        };
        ContentDialog::new()
            .is_open(true)
            .title(title)
            .primary_button_text(primary)
            .close_button_text("Cancel")
            .on_closed(context.callback(move |result| Message::PromptReply(id, prompt_id, result)))
            .content(
                StackPanel::new()
                    .max_width(500.0)
                    .spacing(12.0)
                    .keyed_children(
                        children
                            .into_iter()
                            .enumerate()
                            .map(|(i, v)| KeyedView::new(i.to_string(), v)),
                    ),
            )
    }
    fn find(&mut self, close: bool, direction: Option<isize>, context: &ComponentContext<Self>) {
        let Some(id) = self.tabs.active_id() else {
            return;
        };
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get_mut(id)
        else {
            return;
        };
        let configure = !tab.find.open
            || tab
                .session
                .frames
                .load()
                .search
                .as_ref()
                .is_none_or(|s| s.query != tab.find.query || s.sensitive != tab.find.sensitive);
        tab.find.open = !close;
        if close {
            tab.find.focused = false;
        }
        tab.find.epoch += 1;
        self.keyboard.set_search(!close);
        if close {
            let _ = tab.session.input.send(Command::SearchClose);
        } else {
            if configure {
                let _ = tab.session.input.send(Command::Search {
                    query: tab.find.query.clone(),
                    sensitive: tab.find.sensitive,
                });
            }
            if let Some(direction) = direction {
                let _ = tab.session.input.send(Command::SearchNavigate(direction));
            }
            let epoch = tab.find.epoch;
            context.spawn_background(move |_| {
                std::thread::sleep(Duration::from_millis(20));
                Message::FindFocus(id, epoch, 0)
            });
        }
        self.invalidator.invalidate();
    }
    fn apply_find(&self, id: TabId, epoch: u64) {
        if let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get(id)
            && tab.find.open
            && tab.find.epoch == epoch
        {
            let _ = tab.session.input.send(Command::Search {
                query: tab.find.query.clone(),
                sensitive: tab.find.sensitive,
            });
        }
    }
    fn find_view(&self, id: TabId, tab: &LiveTab, context: &ViewContext<Self>) -> View {
        let frame = tab.session.frames.load();
        let count = if tab.find.query.is_empty() {
            "Find in scrollback".to_string()
        } else if let Some(search) = frame
            .search
            .as_ref()
            .filter(|s| s.query == tab.find.query && s.sensitive == tab.find.sensitive)
        {
            if search.total == 0 {
                "No matches".into()
            } else {
                format!(
                    "{} / {}{}",
                    search.selected.map_or(0, |i| i + 1),
                    search.total,
                    if search.truncated { "+" } else { "" }
                )
            }
        } else {
            "Searching…".into()
        };
        Border::new()
            .grid_row(1)
            .horizontal_alignment(HorizontalAlignment::Right)
            .vertical_alignment(VerticalAlignment::Top)
            .margin(8.0)
            .padding(8.0)
            .corner_radius(6.0)
            .background(Color::rgb(40, 42, 48))
            .width((self.width - 16.0).clamp(300.0, 600.0))
            .content(
                Grid::new()
                    .columns([
                        GridLength::STAR,
                        GridLength::Auto,
                        GridLength::Auto,
                        GridLength::Auto,
                        GridLength::Auto,
                        GridLength::Auto,
                    ])
                    .children((
                        TextBox::new()
                            .element_ref(&tab.find.input)
                            .text(&tab.find.query)
                            .placeholder_text("Find · Enter next · Esc close")
                            .automation_name("Find in scrollback")
                            .on_text_changed(
                                context.callback(move |text| Message::FindEdit(id, text)),
                            ),
                        Border::new()
                            .grid_column(1)
                            .padding(6.0)
                            .vertical_alignment(VerticalAlignment::Center)
                            .content(TextBlock::new().text(count)),
                        CheckBox::new()
                            .grid_column(2)
                            .automation_name("Match case")
                            .is_checked(tab.find.sensitive)
                            .on_is_checked_changed(
                                context.callback(move |v| Message::FindCase(id, v)),
                            )
                            .content(TextBlock::new().text("Aa")),
                        Button::new()
                            .grid_column(3)
                            .style(ButtonStyle::Subtle)
                            .automation_name("Previous match (Shift+Enter)")
                            .on_click(
                                context.callback(|_| Message::Shortcut(Shortcut::FindPrevious)),
                            )
                            .content(TextBlock::new().text("↑")),
                        Button::new()
                            .grid_column(4)
                            .style(ButtonStyle::Subtle)
                            .automation_name("Next match (Enter)")
                            .on_click(context.callback(|_| Message::Shortcut(Shortcut::FindNext)))
                            .content(TextBlock::new().text("↓")),
                        Button::new()
                            .grid_column(5)
                            .style(ButtonStyle::Subtle)
                            .automation_name("Close find (Escape)")
                            .on_click(context.callback(|_| Message::Shortcut(Shortcut::FindClose)))
                            .content(TextBlock::new().text("×")),
                    )),
            )
    }
    fn active_selection(&self) -> bool {
        matches!(self.tabs.active(),Some(AppTab{page:Page::Terminal(tab),..}) if tab.selection.borrow().as_ref().is_some_and(|s|{let frame=tab.session.frames.load();s.active && s.alternate==frame.alternate && s.cols==frame.cols}))
    }
    fn clear_selection(&mut self) {
        if let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.active_id().and_then(|id| self.tabs.get_mut(id))
        {
            tab.selection.borrow_mut().take();
            tab.scroll_direction = 0;
        }
        self.keyboard.set_selection(false);
        self.invalidator.invalidate();
    }
    fn copy(&self) {
        if let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.active()
            && let Some(selection) = tab.selection.borrow().as_ref()
        {
            let text = selection.text(
                &tab.session.frames.load(),
                self.appearance
                    .read()
                    .unwrap()
                    .interaction
                    .trim_block_selection,
            );
            if !text.is_empty()
                && let Err(e) = w11term::clipboard::write(&text)
            {
                log::warn!("Copy: {e:#}");
            }
        }
    }
    fn paste(&mut self, raw: bool, context: &ComponentContext<Self>) {
        if let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.active()
        {
            match w11term::clipboard::read() {
                Ok(text) => {
                    if raw {
                        if !text.is_empty() {
                            let (tx, rx) = tokio::sync::oneshot::channel();
                            if tab
                                .session
                                .input
                                .send(Command::Raw(text.into_bytes(), tx))
                                .is_ok()
                            {
                                context.spawn_background(move |_| {
                                    Message::RawPaste(rx.blocking_recv().unwrap_or_else(|_| {
                                        Err("SSH session closed before acknowledging paste".into())
                                    }))
                                });
                            }
                        }
                    } else {
                        let preferences = self.appearance.read().unwrap();
                        let text = paste_text(&text, preferences.interaction.trim_paste);
                        if !text.is_empty() {
                            let _ = tab.session.input.send(Command::Paste(text.into()));
                        }
                    }
                }
                Err(e) => log::warn!("Paste: {e:#}"),
            }
        }
        self.clear_selection();
    }
    fn switch_tabs(&mut self, delta: isize, context: &ComponentContext<Self>) {
        let mode = self
            .appearance
            .read()
            .unwrap()
            .interaction
            .tab_switcher_mode;
        if mode == TabSwitcherMode::Disabled {
            self.tabs.cycle(delta);
            self.activate(context);
            return;
        }
        if self.switcher.is_none() {
            let ids = if mode == TabSwitcherMode::Mru {
                self.mru
                    .iter()
                    .copied()
                    .filter(|id| self.tabs.get(*id).is_some())
                    .collect()
            } else {
                self.tabs.entries().iter().map(|tab| tab.id).collect()
            };
            let ids: Vec<TabId> = ids;
            let index = ids
                .iter()
                .position(|id| Some(*id) == self.tabs.active_id())
                .unwrap_or(0);
            self.switcher = Some(TabSwitcher { ids, index });
        }
        if let Some(switcher) = &mut self.switcher
            && !switcher.ids.is_empty()
        {
            switcher.index =
                (switcher.index as isize + delta).rem_euclid(switcher.ids.len() as isize) as usize;
        }
        self.keyboard.set_switcher(true);
    }
    fn finish_switcher(&mut self, commit: bool, context: &ComponentContext<Self>) {
        if let Some(switcher) = self.switcher.take()
            && commit
            && let Some(id) = switcher.ids.get(switcher.index)
        {
            self.tabs.select(*id);
            self.activate(context);
        }
        self.keyboard.set_switcher(false);
    }
    fn pointer(
        &mut self,
        id: TabId,
        pointer: PointerEventInfo,
        kind: MouseEventKind,
        context: &ComponentContext<Self>,
    ) {
        if self.tabs.active_id() != Some(id) {
            return;
        }
        let preferences = self.appearance.read().unwrap().interaction.clone();
        let metrics = self.metrics.get();
        let modifiers = crate::input::modifiers();
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get_mut(id)
        else {
            return;
        };
        let frame = tab.session.frames.load();
        let x = (((pointer.x as f32 - PADDING).max(0.0) / metrics.cell_width).floor() as usize)
            .min(metrics.cols.saturating_sub(1));
        let y = (((pointer.y as f32 - PADDING).max(0.0) / metrics.cell_height).floor() as usize)
            .min(metrics.rows.saturating_sub(1));
        let point = Point {
            row: frame.first_stable_row + y as isize,
            col: x,
        };
        let link = link_at(&frame, point, preferences.detect_urls);
        if *tab.hover.borrow() != link {
            *tab.hover.borrow_mut() = link.clone();
            self.invalidator.invalidate();
        }
        let button = if pointer.is_left_button_pressed {
            MouseButton::Left
        } else if pointer.is_right_button_pressed {
            MouseButton::Right
        } else if pointer.is_middle_button_pressed {
            MouseButton::Middle
        } else {
            MouseButton::None
        };
        let local_drag = tab.selection.borrow().as_ref().is_some_and(|s| s.dragging)
            || tab.clicked_link.is_some();
        if frame.mouse_grabbed
            && !local_drag
            && !modifiers.contains(KeyModifiers::SHIFT)
            && !(modifiers.contains(KeyModifiers::CTRL) && link.is_some())
        {
            let button = if kind == MouseEventKind::Release {
                std::mem::replace(&mut tab.remote_button, MouseButton::None)
            } else {
                if kind == MouseEventKind::Press {
                    tab.remote_button = button;
                }
                button
            };
            let _ = tab.session.input.send(Command::Mouse(MouseEvent {
                kind,
                x,
                y: y as i64,
                x_pixel_offset: crate::input::cell_pixel_offset(
                    pointer.x as f32 - PADDING,
                    metrics.cell_width,
                    metrics.scale,
                ),
                y_pixel_offset: crate::input::cell_pixel_offset(
                    pointer.y as f32 - PADDING,
                    metrics.cell_height,
                    metrics.scale,
                ),
                button,
                modifiers,
            }));
            return;
        }
        let mut should_copy = false;
        let mut should_paste = false;
        let mut open = None;
        match kind {
            MouseEventKind::Press if button == MouseButton::Middle => {}
            MouseEventKind::Press if button == MouseButton::Right => {
                tab.gesture += 1;
                if !preferences.copy_on_select
                    && tab.selection.borrow().as_ref().is_some_and(|s| s.active)
                {
                    should_copy = true;
                } else {
                    should_paste = true;
                }
            }
            MouseEventKind::Press if button == MouseButton::Left => {
                tab.gesture += 1;
                if modifiers.contains(KeyModifiers::CTRL)
                    && let Some(link) = link
                {
                    tab.clicked_link = Some(link.uri);
                } else {
                    let clicks = tab
                        .last_click
                        .filter(|(time, p, _)| {
                            time.elapsed() < Duration::from_millis(500) && *p == point
                        })
                        .map_or(1, |(_, _, count)| count % 3 + 1);
                    tab.last_click = Some((Instant::now(), point, clicks));
                    let mut selection = if modifiers.contains(KeyModifiers::SHIFT) {
                        tab.selection.borrow().clone().unwrap_or_else(|| {
                            Selection::new(
                                point,
                                modifiers.contains(KeyModifiers::ALT),
                                frame.alternate,
                            )
                        })
                    } else {
                        Selection::new(
                            point,
                            modifiers.contains(KeyModifiers::ALT),
                            frame.alternate,
                        )
                    };
                    selection.dragging = true;
                    selection.update(point);
                    if clicks == 2 {
                        let range = word_range(&frame, point, &preferences.word_delimiters);
                        selection.anchor.col = range.start;
                        selection.end.col = range.end.saturating_sub(1);
                        selection.active = true;
                    } else if clicks == 3 {
                        selection.anchor.col = 0;
                        selection.end.col = frame.cols.saturating_sub(1);
                        selection.active = true;
                    }
                    selection.observe(&frame);
                    *tab.selection.borrow_mut() = Some(selection);
                    self.invalidator.invalidate();
                }
            }
            MouseEventKind::Move if pointer.is_left_button_pressed => {
                if let Some(selection) = tab.selection.borrow_mut().as_mut().filter(|s| s.dragging)
                {
                    selection.update(point);
                    selection.observe(&frame);
                }
                tab.scroll_direction = if pointer.y < PADDING as f64 {
                    2
                } else if pointer.y > (PADDING + metrics.rows as f32 * metrics.cell_height) as f64 {
                    -2
                } else {
                    0
                };
                if tab.scroll_direction != 0 && !tab.scroll_tick {
                    tab.scroll_tick = true;
                    context.spawn_background(move |_| {
                        std::thread::sleep(Duration::from_millis(40));
                        Message::DragScroll(id)
                    });
                }
                self.invalidator.invalidate();
            }
            MouseEventKind::Release => {
                tab.scroll_direction = 0;
                if let Some(uri) = tab.clicked_link.take()
                    && link.as_ref().is_some_and(|l| l.uri == uri)
                {
                    open = Some(uri);
                }
                if let Some(selection) = tab.selection.borrow_mut().as_mut().filter(|s| s.dragging)
                {
                    selection.dragging = false;
                    selection.observe(&frame);
                    should_copy = preferences.copy_on_select && selection.active;
                }
                self.invalidator.invalidate();
            }
            _ => {}
        }
        let selected = tab.selection.borrow().as_ref().is_some_and(|s| s.active);
        self.keyboard.set_selection(selected);
        if should_copy {
            self.copy();
            if kind == MouseEventKind::Press
                && button == MouseButton::Right
                && !preferences.copy_on_select
            {
                self.clear_selection();
            }
        }
        if should_paste {
            self.paste(false, context);
        }
        if let Some(uri) = open
            && safe_url(&uri)
        {
            use windows::{
                Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
                core::PCWSTR,
            };
            let uri: Vec<_> = uri.encode_utf16().chain(Some(0)).collect();
            let result = unsafe {
                ShellExecuteW(
                    None,
                    windows::core::w!("open"),
                    PCWSTR(uri.as_ptr()),
                    None,
                    None,
                    SW_SHOWNORMAL,
                )
            };
            if result.0 as isize <= 32 {
                log::warn!("Could not open URL");
            }
        }
    }
    fn save_preferences(&mut self, appearance: Appearance) {
        self.keyboard
            .set_preferences(appearance.interaction.clone());
        for entry in self.tabs.entries() {
            if let Page::Terminal(tab) = &entry.value.page {
                tab.session.clipboard_enabled.store(
                    appearance.interaction.osc52,
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
        }
        *self.appearance.write().unwrap() = appearance.clone();
        self.appearance_error = Appearance::path()
            .and_then(|path| appearance.save(&path))
            .err()
            .map(|e| format!("Settings could not be saved: {e:#}"));
    }
    fn apply_appearance(&mut self, appearance: Appearance) {
        let mut renderer = match Renderer::with_size(
            &appearance.font_family,
            appearance.ligatures,
            appearance.font_size,
        ) {
            Ok(renderer) => renderer,
            Err(e) => {
                self.appearance_error = Some(format!("Unable to use font: {e}"));
                return;
            }
        };
        if let Some((width, height, scale)) = self.terminal_area.get() {
            let metrics = renderer.measure(width, height, scale);
            self.metrics.set(metrics);
            let size = TerminalSize {
                cols: metrics.cols,
                rows: metrics.rows,
                pixel_width: (metrics.cols as f32 * metrics.cell_width * scale).round() as usize,
                pixel_height: (metrics.rows as f32 * metrics.cell_height * scale).round() as usize,
                dpi: (96.0 * scale).round() as u32,
            };
            for entry in self.tabs.entries() {
                if let Page::Terminal(tab) = &entry.value.page {
                    tab.selection.borrow_mut().take();
                    tab.hover.borrow_mut().take();
                    tab.last_size.set(Some(size));
                    let _ = tab.session.input.send(Command::Resize(size));
                }
            }
        }
        *self.renderer.borrow_mut() = renderer;
        self.keyboard.set_selection(false);
        self.save_preferences(appearance);
        self.invalidator.invalidate();
    }

    fn settings_view(&self, context: &ViewContext<Self>) -> View {
        let appearance = self.appearance.read().unwrap().clone();
        let mut children:Vec<View>=vec![
            TextBlock::new().text("Settings").font_size(26.0).into(),
            TextBlock::new().text("Terminal font").font_size(18.0).into(),
            ComboBox::new().width(360.0).automation_name("Terminal font")
                .items_source(self.fonts.iter().cloned())
                .selected_index(self.fonts.iter().position(|font|font==&appearance.font_family))
                .on_selection_changed(context.callback(Message::Font)).into(),
            TextBlock::new().text("Installed Cascadia Code, Cascadia Mono, Consolas, JetBrains Mono, Monaspace and Fira Code fonts.").text_wrapping(TextWrapping::Wrap).into(),
            TextBlock::new().text("Programming ligatures").into(),
            ToggleSwitch::new().automation_name("Programming ligatures").is_on(appearance.ligatures).on_toggled(context.callback(Message::Ligatures)).into(),
            TextBlock::new().text("Used by fonts with ligature support, such as Cascadia Code, JetBrains Mono and Fira Code.").text_wrapping(TextWrapping::Wrap).into(),
            TextBlock::new().text("Applies to all tabs. Changes are saved automatically.").into(),
        ];
        children.push(
            TextBlock::new()
                .text(format!(
                    "Font size: {}  ·  Ctrl++ / Ctrl+- / Ctrl+0",
                    appearance.font_size
                ))
                .into(),
        );
        children.push(TextBlock::new().text("Interaction").font_size(20.0).into());
        for (field, label, value) in [
            (
                InteractionField::CopyOnSelect,
                "Automatically copy selection",
                appearance.interaction.copy_on_select,
            ),
            (
                InteractionField::TrimBlock,
                "Remove trailing whitespace in block selections",
                appearance.interaction.trim_block_selection,
            ),
            (
                InteractionField::TrimPaste,
                "Trim trailing whitespace on paste",
                appearance.interaction.trim_paste,
            ),
            (
                InteractionField::Snap,
                "Snap window resizing to the character grid",
                appearance.interaction.snap_to_grid_on_resize,
            ),
            (
                InteractionField::ScrollZoom,
                "Ctrl+scroll to zoom",
                appearance.interaction.scroll_to_zoom,
            ),
            (
                InteractionField::DetectUrls,
                "Detect URLs (Ctrl+click to open)",
                appearance.interaction.detect_urls,
            ),
            (
                InteractionField::Osc52,
                "Allow remote clipboard copy (OSC 52)",
                appearance.interaction.osc52,
            ),
        ] {
            children.push(
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new().text(label),
                    ToggleSwitch::new()
                        .automation_name(label)
                        .is_on(value)
                        .on_toggled(
                            context.callback(move |value| Message::Interaction(field, value)),
                        ),
                )),
            );
        }
        children.push(TextBlock::new().text("Clipboard copies contain plain text only. Alt+drag selects a block. Hold Shift to select or paste over a mouse-reporting TUI.").text_wrapping(TextWrapping::Wrap).into());
        children.push(TextBlock::new().text("Tab switcher").into());
        children.push(
            ComboBox::new()
                .width(360.0)
                .items_source(["In tab order", "Most recently used", "Disabled"])
                .selected_index(Some(match appearance.interaction.tab_switcher_mode {
                    TabSwitcherMode::InOrder => 0,
                    TabSwitcherMode::Mru => 1,
                    TabSwitcherMode::Disabled => 2,
                }))
                .on_selection_changed(context.callback(Message::TabMode))
                .into(),
        );
        if let Some(error) = &self.appearance_error {
            children.push(
                TextBlock::new()
                    .text(error.clone())
                    .foreground(Color::rgb(255, 150, 130))
                    .text_wrapping(TextWrapping::Wrap)
                    .into(),
            );
        }
        ScrollViewer::new().content(
            StackPanel::new()
                .spacing(16.0)
                .margin(32.0)
                .max_width(720.0)
                .horizontal_alignment(HorizontalAlignment::Left)
                .keyed_children(
                    children
                        .into_iter()
                        .enumerate()
                        .map(|(index, view)| KeyedView::new(index.to_string(), view)),
                ),
        )
    }
    fn publish_panes(&self) {
        self.registry
            .replace(self.tabs.entries().iter().filter_map(|entry| {
                let Page::Terminal(tab) = &entry.value.page else {
                    return None;
                };
                Some(Pane {
                    id: entry.id,
                    label: entry.value.label.clone(),
                    active: self.tabs.active_id() == Some(entry.id),
                    input: tab.session.input.clone(),
                })
            }));
    }
    fn sync_selection(&mut self, context: &ComponentContext<Self>) {
        // Native collection resets select the first item after Reactor's property pass.
        // Reapply our selected index after mounting, and ignore intermediate reset events.
        self.selection_pending = true;
        self.selection_epoch += 1;
        let epoch = self.selection_epoch;
        context.spawn_background(move |_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            Message::SyncSelection(epoch)
        });
    }
    fn watch_frames(&mut self, context: &ComponentContext<Self>) {
        let Some(id) = self.tabs.active_id() else {
            return;
        };
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get(id)
        else {
            return;
        };
        let frames = tab.session.frames.clone();
        let generation = frames.load().generation;
        let epoch = self.watch_epoch;
        self.frame_task = Some(context.spawn_background_with_rejection(
            move |cancel| {
                frames.wait_for_change(generation, || cancel.is_cancelled());
                Message::FrameReady(id, epoch)
            },
            Message::WatchRejected,
        ));
    }

    fn activate(&mut self, context: &ComponentContext<Self>) {
        let profile = self
            .tabs
            .active()
            .filter(|t| self.profiles.contains_key(&t.label))
            .map(|t| t.label.clone());
        if profile != self.window_profile {
            if let Some(previous) = self.window_profile.take() {
                let _ = self.windows.borrow_mut().remember(&previous);
            }
            self.window_profile = profile;
            self.restore_window();
        }
        let windows = self.windows.clone();
        let profile = self.window_profile.clone();
        self.keyboard.on_close(move || {
            if let Some(name) = &profile {
                let _ = windows.borrow_mut().remember(name);
            }
        });
        if let Some(id) = self.tabs.active_id() {
            self.mru.retain(|i| *i != id);
            self.mru.insert(0, id);
        }
        self.watch_epoch += 1;
        if let Some(task) = self.frame_task.take() {
            task.cancel();
        }
        let target = match self.tabs.active() {
            Some(AppTab {
                page: Page::Terminal(tab),
                ..
            }) => Some((tab.session.input.clone(), tab.session.frames.clone())),
            _ => None,
        };
        self.keyboard.set_target(target);
        if let Some(id) = self.tabs.active_id() {
            self.connection_ui(id);
        }
        self.keyboard.set_search(
            matches!(self.tabs.active(),Some(AppTab{page:Page::Terminal(tab),..}) if tab.find.open && tab.prompt_id.is_none()),
        );
        if let Some(id) = self.tabs.active_id()
            && let Some(AppTab {
                page: Page::Terminal(tab),
                ..
            }) = self.tabs.get_mut(id)
            && tab.find.open
        {
            tab.find.focused = false;
            let epoch = tab.find.epoch;
            context.spawn_background(move |_| {
                std::thread::sleep(Duration::from_millis(20));
                Message::FindFocus(id, epoch, 0)
            });
        }
        self.keyboard
            .set_preferences(self.appearance.read().unwrap().interaction.clone());
        self.keyboard.set_selection(self.active_selection());
        self.invalidator.invalidate();
        self.watch_frames(context);
        self.publish_panes();
    }

    fn restore_window(&self) {
        if self.initial_grid.get().is_some() {
            return;
        }
        if let Some(name) = &self.window_profile {
            let windows = self.windows.borrow();
            if windows.fullscreen() {
                return;
            }
            if windows.restore(name).is_ok()
                && let Some(placement) = windows.profiles.get(name)
                && let Some((cols, rows)) = placement.cols.zip(placement.rows)
                && (1..=1000).contains(&cols)
                && (1..=500).contains(&rows)
            {
                self.initial_grid.set(Some((cols, rows)));
            }
        }
    }

    fn close_tab(&mut self, id: TabId, context: &ComponentContext<Self>) {
        let active = self.tabs.active_id() == Some(id);
        if active {
            self.keyboard.set_target(None);
            if let Some(task) = self.frame_task.take() {
                task.cancel();
            }
        }
        self.tabs.remove(id);
        self.publish_panes();
        if self.tabs.entries().is_empty() {
            let _ = context.window().request_close();
        } else {
            self.sync_selection(context);
            if active {
                self.activate(context);
            }
        }
    }

    fn open(&mut self, label: String, source: Source, context: &ComponentContext<Self>) {
        self.tabs.insert(live_tab(
            label,
            source,
            self.appearance.read().unwrap().interaction.osc52,
        ));
        self.sync_selection(context);
        self.activate(context);
    }

    fn utility(&mut self, settings: bool, context: &ComponentContext<Self>) {
        let existing = self
            .tabs
            .entries()
            .iter()
            .find(|t| {
                if settings {
                    matches!(t.value.page, Page::Settings)
                } else {
                    matches!(t.value.page, Page::NewProfile(_))
                }
            })
            .map(|t| t.id);
        if let Some(id) = existing {
            self.tabs.select(id);
        } else {
            self.tabs.insert(AppTab {
                label: if settings {
                    "Settings".into()
                } else {
                    "New Profile".into()
                },
                page: if settings {
                    Page::Settings
                } else {
                    Page::NewProfile(ProfileDraft::default())
                },
            });
            self.sync_selection(context);
        }
        self.activate(context);
    }

    fn save_profile(&mut self, id: TabId, context: &ComponentContext<Self>) {
        let Some(AppTab {
            page: Page::NewProfile(draft),
            ..
        }) = self.tabs.get(id)
        else {
            return;
        };
        let draft = draft.clone();
        let result = (|| -> anyhow::Result<SshOptions> {
            let name = draft.name.trim();
            anyhow::ensure!(!name.is_empty(), "Enter a profile name");
            let host = draft.host.trim().trim_matches(['[', ']']);
            let user = draft.user.trim();
            anyhow::ensure!(
                !host.is_empty() && !user.is_empty(),
                "Enter a host and user"
            );
            anyhow::ensure!(
                !host.chars().any(char::is_whitespace) && !user.chars().any(char::is_whitespace),
                "Host and user cannot contain spaces"
            );
            let authentication = match draft.authentication {
                1 => {
                    anyhow::ensure!(
                        !draft.identity.trim().is_empty(),
                        "Enter the private key file path"
                    );
                    Authentication::Identity {
                        identity: draft.identity.trim().into(),
                    }
                }
                2 => Authentication::None,
                3 => Authentication::Password,
                4 => Authentication::KeyboardInteractive,
                _ => Authentication::Agent,
            };
            let options = SshOptions {
                host: host.into(),
                user: user.into(),
                port: draft
                    .port
                    .trim()
                    .parse()
                    .map_err(|_| anyhow::anyhow!("Port must be between 1 and 65535"))?,
                authentication,
                host_key: (!draft.fingerprint.trim().is_empty())
                    .then(|| draft.fingerprint.trim().into()),
                player: draft.player.clone(),
            };
            self.store.add(name, &options)?;
            self.profiles = self.store.load()?;
            self.profile_error = None;
            Ok(self.profiles[name].clone())
        })();
        match result {
            Ok(options) => {
                if let Some(tab) = self.tabs.get_mut(id) {
                    *tab = live_tab(
                        draft.name.trim().into(),
                        Source::Ssh(options),
                        self.appearance.read().unwrap().interaction.osc52,
                    );
                }
                self.activate(context);
            }
            Err(error) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    draft.error = Some(format!("{error:#}"));
                }
            }
        }
    }

    fn header_view(&self, context: &ViewContext<Self>) -> View {
        let tab_strip = TabView::new()
            .height(44.0)
            .width(
                (self.tabs.entries().len() as f64 * 110.0 + 16.0)
                    .min((self.width - 300.0).max(160.0)),
            )
            .is_add_tab_button_visible(false)
            .can_reorder_tabs(true)
            .selected_index(if self.selection_pending {
                None
            } else {
                self.tabs.active_index()
            })
            .on_selection_changed({
                let epoch = self.selection_epoch;
                let pending = self.selection_pending;
                context.callback(move |index| Message::Select(index, epoch, pending))
            })
            .on_close_requested(context.callback(Message::CloseTab))
            .on_reordered({
                let selected = self.tabs.active_id();
                context.callback(move |order| Message::Reorder(order, selected))
            })
            .collection_slot(
                TabViewSlot::TabItems,
                self.tabs.entries().iter().map(|tab| {
                    KeyedView::new(
                        tab.id.to_string(),
                        TabViewItem::new()
                            .header(&tab.value.label)
                            .tag(tab.id.to_string())
                            .is_closable(true),
                    )
                }),
            );
        let mut menu: Vec<_> = self
            .profiles
            .iter()
            .map(|(name, ssh)| MenuItem::item(format!("profile:{name}"), profile_label(name, ssh)))
            .collect();
        if self.profile_error.is_some() {
            menu.push(MenuItem::disabled("error", "Could not load profiles"));
        }
        menu.extend([
            MenuItem::separator("profiles-end"),
            MenuItem::item("new-profile", NEW_PROFILE),
            MenuItem::item("lab", LAB),
        ]);
        let plus = Button::new()
            .style(ButtonStyle::Subtle)
            .width(40.0)
            .height(36.0)
            .automation_name("New tab")
            .content(TextBlock::new().text("+").font_size(22.0))
            // Reactor 0.100 cannot currently nest a Menu owner under a Tooltip wrapper.
            .menu(Menu::new(menu, context.callback(Message::ProfileMenu)));
        let caret = Button::new()
            .style(ButtonStyle::Subtle)
            .width(44.0)
            .height(36.0)
            .automation_name("Tab menu")
            .content(FontIcon::new().glyph("\u{e70d}"))
            .menu(Menu::new(
                [
                    MenuItem::item("paste", "Paste"),
                    MenuItem::item("paste-raw", "Paste raw (no bracketed paste)"),
                    MenuItem::separator("paste-end"),
                    MenuItem::item("settings", "Settings"),
                    MenuItem::item("fullscreen", "Toggle fullscreen (F11)"),
                    MenuItem::item("mute", "Toggle profile mute"),
                    MenuItem::item("game-mode", "Toggle game mode (suppress key repeat)"),
                    MenuItem::item("alternate-scroll", "Toggle alternate-screen wheel keys"),
                    MenuItem::item("remote-sounds", "Toggle remote named sounds"),
                    MenuItem::item("sync-assets", "Toggle remote asset synchronization"),
                    MenuItem::item("enroll", "Enroll this device (at game prompt)"),
                    MenuItem::item("forget-device", "Forget this device (revoke game key)"),
                ],
                context.callback(|item: String| match item.as_str() {
                    "Paste" => Message::Paste,
                    "Paste raw (no bracketed paste)" => Message::PasteRaw,
                    "Toggle fullscreen (F11)" => Message::Shortcut(Shortcut::Fullscreen),
                    "Toggle profile mute" => Message::TogglePlayer("mute".into()),
                    "Toggle game mode (suppress key repeat)" => {
                        Message::TogglePlayer("game_mode".into())
                    }
                    "Toggle alternate-screen wheel keys" => {
                        Message::TogglePlayer("alternate_scroll".into())
                    }
                    "Toggle remote named sounds" => Message::TogglePlayer("remote_sounds".into()),
                    "Toggle remote asset synchronization" => {
                        Message::TogglePlayer("sync_assets".into())
                    }
                    "Enroll this device (at game prompt)" => Message::DeviceAction(false),
                    "Forget this device (revoke game key)" => Message::DeviceAction(true),
                    _ => Message::SettingsMenu,
                }),
            ));
        TitleBar::new()
            .preferred_height(WindowTitleBarHeight::Tall)
            .slot(
                TitleBarSlot::Content,
                Grid::new()
                    // TitleBar centers its Content by default. Occupy its available width so
                    // tabs start at the left rather than sitting in the middle of the caption.
                    .width((self.width - 180.0).max(400.0))
                    .columns([
                        GridLength::Auto,
                        GridLength::Auto,
                        GridLength::Auto,
                        GridLength::STAR,
                    ])
                    .children((
                        tab_strip,
                        Border::new().grid_column(1).content(plus),
                        Border::new().grid_column(2).content(caret),
                    )),
            )
    }

    fn terminal_view(&self, id: TabId, tab: &LiveTab, context: &ViewContext<Self>) -> View {
        if let Some(error) = &tab.error {
            return StackPanel::new().spacing(12.0).children((
                TextBlock::new().text(format!("Unable to render terminal: {error}")),
                Button::new()
                    .on_click(context.callback(move |_| Message::RetryRender(id)))
                    .content(TextBlock::new().text("Retry rendering")),
            ));
        }
        let renderer = self.renderer.clone();
        let frames = tab.session.frames.clone();
        let metrics = self.metrics.clone();
        let sender = tab.session.input.clone();
        let last_size = tab.last_size.clone();
        let error_sender = context.sender();
        let geometry_sender = context.sender();
        let initial_grid = self.initial_grid.clone();
        let window_state = self.windows.clone();
        let window_size = (self.width, self.window_height);
        let terminal_area = self.terminal_area.clone();
        let selection = tab.selection.clone();
        let hover = tab.hover.clone();
        let canvas: View = Canvas::invalidated(&self.invalidator, move |ctx| {
            let frame = frames.load();
            let measured = renderer.borrow_mut().draw(ctx, &frame)?;
            renderer.borrow().draw_interaction(
                ctx,
                &frame,
                selection.borrow().as_ref(),
                hover.borrow().as_ref(),
            )?;
            metrics.set(measured);
            if initial_grid
                .get()
                .is_none_or(|grid| grid == (measured.cols, measured.rows))
            {
                window_state.borrow_mut().grid = Some((measured.cols, measured.rows));
            }
            let scale = measured.scale;
            terminal_area.set(Some((ctx.width, ctx.height, scale)));
            crate::input::set_grid(
                measured.cell_width,
                measured.cell_height,
                ctx.width,
                ctx.height,
                scale,
            );
            if let Some(((cols, rows), first)) =
                initial_grid.correction((measured.cols, measured.rows))
            {
                geometry_sender.send(Message::Geometry(
                    window_size.0
                        + (cols as f32 * measured.cell_width + PADDING * 2.0 - ctx.width) as f64,
                    window_size.1
                        + (rows as f32 * measured.cell_height + PADDING * 2.0 + STATUS_HEIGHT
                            - ctx.height) as f64,
                    initial_grid.epoch(),
                    first,
                ));
                return Ok(());
            }
            let size = TerminalSize {
                rows: measured.rows,
                cols: measured.cols,
                pixel_width: (measured.cols as f32 * measured.cell_width * scale).round() as usize,
                pixel_height: (measured.rows as f32 * measured.cell_height * scale).round()
                    as usize,
                dpi: (96.0 * scale).round() as u32,
            };
            if last_size.replace(Some(size)) != Some(size) {
                let _ = sender.send(Command::Resize(size));
            }
            Ok(())
        })
        .on_error(move |error| {
            error_sender.send(Message::RenderError(id, format!("{error:?}")));
        })
        .into();
        Border::new()
            .background(Color::rgb(14, 16, 20))
            .capture_pointer_on_press(true)
            .on_pointer_capture_lost(context.callback(move |_| Message::CaptureLost(id)))
            .on_pointer_exited(context.callback(move |_| Message::PointerExit(id)))
            .on_pointer_pressed(
                context.callback(move |p| Message::Pointer(id, p, MouseEventKind::Press)),
            )
            .on_pointer_released(
                context.callback(move |p| Message::Pointer(id, p, MouseEventKind::Release)),
            )
            .on_pointer_moved(
                context.callback(move |p| Message::Pointer(id, p, MouseEventKind::Move)),
            )
            .content(canvas)
    }

    fn profile_form(&self, id: TabId, draft: &ProfileDraft, context: &ViewContext<Self>) -> View {
        let mut fields: Vec<View> =
            vec![TextBlock::new().text("New Profile").font_size(26.0).into()];
        for (field, label, text, placeholder) in [
            (Field::Name, "Profile name", &draft.name, "My server"),
            (Field::Host, "Host", &draft.host, "example.com"),
            (Field::User, "User", &draft.user, "root"),
            (Field::Port, "Port", &draft.port, "22"),
        ] {
            fields.push(
                StackPanel::new().spacing(5.0).children((
                    TextBlock::new().text(label),
                    TextBox::new()
                        .text(text)
                        .placeholder_text(placeholder)
                        .automation_name(label)
                        .on_text_changed(context.callback(move |v| Message::Edit(id, field, v))),
                )),
            );
        }
        fields.push(
            StackPanel::new().spacing(5.0).children((
                TextBlock::new().text("Authentication"),
                ComboBox::new()
                    .items_source([
                        "Windows SSH agent",
                        "Private key file",
                        "None (keyless server)",
                        "Password",
                        "Keyboard-interactive (password / MFA)",
                    ])
                    .selected_index(draft.authentication)
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .on_selection_changed(context.callback(move |v| Message::Auth(id, v))),
            )),
        );
        if draft.authentication == 1 {
            fields.push(
                StackPanel::new().spacing(5.0).children((
                    TextBlock::new().text("Private key file"),
                    TextBox::new()
                        .text(&draft.identity)
                        .placeholder_text("C:\\path\\to\\key or a path beside profiles.json")
                        .automation_name("Private key file")
                        .on_text_changed(
                            context.callback(move |v| Message::Edit(id, Field::Identity, v)),
                        ),
                )),
            );
        }
        fields.push(
            StackPanel::new().spacing(5.0).children((
                TextBlock::new().text("Host fingerprint (optional)"),
                TextBox::new()
                    .text(&draft.fingerprint)
                    .placeholder_text("SHA256:… — otherwise use OpenSSH known_hosts")
                    .automation_name("Host fingerprint")
                    .on_text_changed(
                        context.callback(move |v| Message::Edit(id, Field::Fingerprint, v)),
                    ),
            )),
        );
        if let Some(error) = &draft.error {
            fields.push(
                TextBlock::new()
                    .text(error)
                    .text_wrapping(TextWrapping::Wrap)
                    .foreground(Color::rgb(255, 160, 150))
                    .into(),
            );
        }
        fields.push(
            Button::new()
                .is_enabled(!draft.generating)
                .on_click(context.callback(move |_| Message::GenerateKey(id)))
                .content(if draft.generating {
                    "Generating device key…"
                } else {
                    "Generate device key (Ed25519)"
                }),
        );
        if let Some(device) = &draft.device {
            fields.push(
                TextBlock::new()
                    .text(format!(
                        "{} · {}\n{}",
                        device.device,
                        device.fingerprint,
                        device.path.display()
                    ))
                    .text_wrapping(TextWrapping::Wrap)
                    .into(),
            );
            fields.push(
                Button::new()
                    .on_click(context.callback(move |_| Message::CopyPublicKey(id)))
                    .content("Copy public key"),
            );
            fields.push(TextBlock::new().text("Private key: Windows-user DPAPI encryption and a restricted ACL. For another host, append the copied public line to ~/.ssh/authorized_keys. OpenSSH cannot read the DPAPI file directly.").text_wrapping(TextWrapping::Wrap).into());
        }
        for (name, label, value) in [
            (
                "rustmush",
                "Rustmush server (device-key enrollment)",
                draft.player.rustmush,
            ),
            (
                "game_mode",
                "Game mode (suppress held-key repeats)",
                draft.player.game_mode,
            ),
            (
                "reconnect",
                "Reconnect after a network interruption",
                draft.player.reconnect,
            ),
            ("mute", "Mute this profile", draft.player.mute),
            (
                "remote_sounds",
                "Allow remote named sounds (OSC 440)",
                draft.player.remote_sounds_enabled(),
            ),
            (
                "sync_assets",
                "Allow remote asset synchronization over SFTP",
                draft.player.sync_assets_enabled(),
            ),
            (
                "alternate_scroll",
                "Send wheel as arrow keys on the alternate screen",
                draft.player.alternate_scroll_enabled(),
            ),
        ] {
            fields.push(
                CheckBox::new()
                    .is_checked(value)
                    .on_is_checked_changed(
                        context.callback(move |value| Message::DraftPlayer(id, name.into(), value)),
                    )
                    .content(label),
            );
        }
        fields.push(
            TextBlock::new()
                .text("Bell sound name (system, off, or a named WAV)")
                .into(),
        );
        fields.push(
            TextBox::new()
                .text(&draft.player.bell)
                .on_text_changed(context.callback(move |v| Message::DraftBell(id, v)))
                .into(),
        );
        fields.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(10.0)
                .children((
                    Button::new()
                        .style(ButtonStyle::Accent)
                        .on_click(context.callback(move |_| Message::SaveProfile(id)))
                        .content("Save and connect"),
                    Button::new()
                        .on_click(context.callback(move |_| Message::CloseTab(id.to_string())))
                        .content("Cancel"),
                )),
        );
        ScrollViewer::new().content(
            StackPanel::new()
                .spacing(14.0)
                .width(520.0)
                .horizontal_alignment(HorizontalAlignment::Left)
                .margin(32.0)
                .keyed_children(
                    fields
                        .into_iter()
                        .enumerate()
                        .map(|(index, field)| KeyedView::new(index.to_string(), field)),
                ),
        )
    }
}

impl Component for TerminalApp {
    type Input = Options;
    type Message = Message;

    fn create(options: &Options, context: &ComponentContext<Self>) -> Self {
        let sender = context.sender();
        let keyboard = KeyboardBridge::install(move |shortcut| {
            sender.send(Message::Shortcut(shortcut));
        })
        .expect("installing terminal input adapter");
        let store = ProfileStore::new(options.profiles_path.clone())
            .expect("resolving profile configuration path");
        let (profiles, profile_error) = match store.load() {
            Ok(p) => (p, None),
            Err(e) => (BTreeMap::new(), Some(format!("{e:#}"))),
        };
        let label = options
            .initial_profile
            .clone()
            .unwrap_or_else(|| source_label(&options.source));
        let mut tabs = Tabs::default();
        let (mut appearance, mut appearance_error) =
            match Appearance::path().and_then(|path| Appearance::load(&path)) {
                Ok(a) => (a, None),
                Err(e) => (
                    Appearance::default(),
                    Some(format!("Unable to load settings: {e:#}")),
                ),
            };
        let fonts = match installed_fonts() {
            Ok(f) => f,
            Err(e) => {
                appearance_error = Some(format!("Unable to enumerate fonts: {e:#}"));
                Vec::new()
            }
        };
        if !fonts.contains(&appearance.font_family) {
            appearance.font_family = fonts
                .iter()
                .find(|font| font.as_str() == "Cascadia Mono")
                .or_else(|| fonts.iter().find(|font| font.as_str() == "Consolas"))
                .or_else(|| fonts.first())
                .cloned()
                .unwrap_or_else(|| "Cascadia Mono".into());
        }
        if !appearance.font_size.is_finite() {
            appearance.font_size = 16.0;
        }
        appearance.font_size = appearance.font_size.clamp(6.0, 72.0);
        let renderer = Renderer::with_size(
            &appearance.font_family,
            appearance.ligatures,
            appearance.font_size,
        )
        .expect("creating DirectWrite formats");
        let appearance = Arc::new(RwLock::new(appearance));
        let grid = if options.cols.is_some() || options.rows.is_some() {
            Some((options.cols.unwrap_or(120), options.rows.unwrap_or(32)))
        } else {
            None
        };
        let size = grid
            .map(|(cols, rows)| TerminalSize {
                cols,
                rows,
                pixel_width: cols * renderer.metrics.cell_width.ceil() as usize,
                pixel_height: rows * renderer.metrics.cell_height.ceil() as usize,
                ..TerminalSize::default()
            })
            .unwrap_or_default();
        let client_size = grid
            .map(|(cols, rows)| {
                (
                    (cols as f32 * renderer.metrics.cell_width.ceil() + PADDING * 2.0) as f64,
                    (rows as f32 * renderer.metrics.cell_height.ceil()
                        + PADDING * 2.0
                        + STATUS_HEIGHT
                        + 48.0) as f64,
                )
            })
            .unwrap_or((1120.0, 760.0));
        tabs.insert(live_tab_sized(
            label.clone(),
            options.source.clone(),
            size,
            appearance.read().unwrap().interaction.osc52,
        ));
        for name in &options.extra_profiles {
            if let Some(profile) = profiles.get(name) {
                tabs.insert(live_tab_sized(
                    name.clone(),
                    Source::Ssh(profile.clone()),
                    size,
                    appearance.read().unwrap().interaction.osc52,
                ));
            }
        }
        let registry = Registry::default();
        let (control_sender, control_rx) =
            std::sync::mpsc::sync_channel::<w11term::control::UiRequest>(8);
        registry.set_ui(move |request| {
            if let Err(
                std::sync::mpsc::TrySendError::Full(request)
                | std::sync::mpsc::TrySendError::Disconnected(request),
            ) = control_sender.try_send(request)
            {
                request.fail("GUI control queue is unavailable or full");
            }
        });
        let capture_appearance = appearance.clone();
        let control = Server::start(registry.clone(), move |frame, size, path| {
            let appearance = capture_appearance.read().unwrap().clone();
            crate::renderer::screenshot_with_font(frame, size, path, &appearance)
        })
        .expect("starting local control pipe");
        let mut app = Self {
            windows: Rc::new(RefCell::new(crate::window_state::Windows::load())),
            window_profile: None,
            window_restored: options.cols.is_some() || options.rows.is_some(),
            keyboard,
            _control: control,
            registry,
            control_rx: Arc::new(std::sync::Mutex::new(control_rx)),
            control_task: None,
            tabs,
            renderer: Rc::new(RefCell::new(renderer)),
            metrics: Rc::new(Cell::new(Metrics::default())),
            invalidator: Invalidator::new(),
            header: RefCell::new(None),
            frame_task: None,
            watcher_retry: None,
            watch_epoch: 0,
            default_source: options.source.clone(),
            default_label: label,
            profiles,
            store,
            profile_error,
            width: client_size.0,
            window_height: client_size.1,
            client_size,
            initial_grid: Rc::new(crate::window_state::RequestedGrid::new(grid)),
            appearance,
            fonts,
            appearance_error,
            terminal_area: Rc::new(Cell::new(None)),
            mru: Vec::new(),
            switcher: None,
            selection_pending: true,
            selection_epoch: 0,
        };
        app.activate(context);
        app.watch_control(context);
        app.sync_selection(context);
        #[cfg(feature = "ui-smoke")]
        if std::env::var_os("W11TERM_UI_SMOKE").is_some() {
            assert!(
                matches!(options.source, Source::Demo) && options.extra_profiles.is_empty(),
                "UI smoke uses only the local lab"
            );
            context.spawn_background(|_| {
                std::thread::sleep(std::time::Duration::from_secs(1));
                Message::Smoke(0)
            });
        }
        if let Some(seconds) = options.smoke_seconds {
            context.spawn_background(move |_| {
                std::thread::sleep(std::time::Duration::from_secs(seconds.min(30)));
                Message::CloseWindow
            });
        }
        app
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        if !matches!(&message, Message::FrameReady(..)) {
            self.header.borrow_mut().take();
        }
        if matches!(&message, Message::Control(_)) {
            self.watch_control(context);
        }
        match message {
            Message::DraftBell(id, value) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get_mut(id)
                    && value.len() <= 128
                {
                    draft.player.bell = value;
                }
            }
            Message::DeviceAction(remove) => {
                if let Some(id) = self.tabs.active_id()
                    && let Some(AppTab {
                        label,
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get_mut(id)
                    && !tab.device_busy
                {
                    let result = (|| -> anyhow::Result<_> {
                        let profile = self
                            .profiles
                            .get(label)
                            .context("Use a saved Rustmush profile")?;
                        anyhow::ensure!(
                            profile.player.rustmush,
                            "Enable Rustmush in this profile first"
                        );
                        let path = profile
                            .player
                            .device_key
                            .clone()
                            .context("Generate a device key in the connect wizard first")?;
                        anyhow::ensure!(
                            tab.session.frames.load().connection_generation > 0
                                && tab.session.prompts.current().is_none(),
                            "Sign in and reach the game prompt first"
                        );
                        Ok((path, profile.user.clone()))
                    })();
                    match result {
                        Ok((path, user)) => {
                            tab.device_busy = true;
                            tab.device_message = Some(
                                if remove {
                                    "Revoking this device and verifying the server listing…"
                                } else {
                                    "Enrolling this device and verifying its fingerprint…"
                                }
                                .into(),
                            );
                            let input = tab.session.input.clone();
                            self.keyboard.set_target(None);
                            context.spawn_background(move |_| {
                                let result = (|| -> anyhow::Result<String> {
                                    let key = w11term::keys::describe(&path)?;
                                    let fingerprint = key.fingerprint.clone();
                                    tokio::runtime::Builder::new_current_thread()
                                        .enable_all()
                                        .build()?
                                        .block_on(w11term::enroll::run(input, key, user, remove))?;
                                    Ok(fingerprint)
                                })();
                                Message::DeviceDone(
                                    id,
                                    remove,
                                    result.map_err(|e| format!("{e:#}")),
                                )
                            });
                        }
                        Err(error) => tab.device_message = Some(format!("{error:#}")),
                    }
                }
            }
            Message::DeviceDone(id, remove, result) => {
                let active = self.tabs.active_id() == Some(id);
                let mut auth_update = None;
                if let Some(AppTab {
                    label,
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    tab.device_busy = false;
                    tab.device_message = Some(match result {
                        Ok(fingerprint) => {
                            let result = (|| -> anyhow::Result<()> {
                                let mut profile = self
                                    .profiles
                                    .get(label)
                                    .context("profile was removed")?
                                    .clone();
                                let auth = if remove {
                                    Authentication::Password
                                } else {
                                    Authentication::Identity {
                                        identity: profile
                                            .player
                                            .device_key
                                            .clone()
                                            .context("device key path missing")?,
                                    }
                                };
                                profile.authentication = auth.clone();
                                self.store.update(label, &profile)?;
                                self.profiles.insert(label.clone(), profile);
                                auth_update = Some((label.clone(), auth));
                                Ok(())
                            })();
                            match result {
                                Ok(()) => format!(
                                    "{} · verified {fingerprint}",
                                    if remove {
                                        "Device revoked; future connections use a password"
                                    } else {
                                        "Device enrolled; future connections use its key"
                                    }
                                ),
                                Err(error) => format!(
                                    "Server operation succeeded ({fingerprint}), but saving the profile failed: {error:#}"
                                ),
                            }
                        }
                        Err(error) => error,
                    });
                    if active {
                        self.keyboard.set_target(Some((
                            tab.session.input.clone(),
                            tab.session.frames.clone(),
                        )));
                    }
                }
                if let Some((label, auth)) = auth_update {
                    for entry in self.tabs.entries() {
                        if entry.value.label == label
                            && let Page::Terminal(tab) = &entry.value.page
                        {
                            let _ = tab.session.input.send(Command::Authenticate(auth.clone()));
                        }
                    }
                }
            }
            Message::RestoreWindow => {
                self.restore_window();
            }
            Message::TogglePlayer(field) => {
                if let Some(AppTab {
                    label,
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.active()
                {
                    let mut player = tab.session.player.write().unwrap();
                    match field.as_str() {
                        "mute" => player.mute = !player.mute,
                        "game_mode" => player.game_mode = !player.game_mode,
                        "remote_sounds" => {
                            player.remote_sounds = Some(!player.remote_sounds_enabled())
                        }
                        "sync_assets" => player.sync_assets = Some(!player.sync_assets_enabled()),
                        "alternate_scroll" => {
                            player.alternate_scroll = Some(!player.alternate_scroll_enabled())
                        }
                        _ => {}
                    }
                    if let Some(profile) = self.profiles.get_mut(label) {
                        profile.player = player.clone();
                        if let Err(error) = self.store.update(label, profile) {
                            log::warn!("Saving player preferences: {error:#}");
                        }
                    }
                    let preferences = player.clone();
                    drop(player);
                    for entry in self.tabs.entries() {
                        if entry.value.label == *label
                            && let Page::Terminal(other) = &entry.value.page
                        {
                            *other.session.player.write().unwrap() = preferences.clone();
                        }
                    }
                }
                if let Some(id) = self.tabs.active_id() {
                    self.connection_ui(id);
                }
            }
            Message::ControlIdle => {}
            Message::WatchRejected => {
                if self.watcher_retry.is_none() {
                    let sender = context.sender();
                    match crate::ui_timer::RetryTimer::new(move || {
                        sender.send(Message::RetryWatchers);
                    }) {
                        Ok(timer) => self.watcher_retry = Some(timer),
                        Err(error) => log::error!("Cannot schedule watcher recovery: {error}"),
                    }
                }
            }
            Message::RetryWatchers => {
                if self
                    .control_task
                    .as_ref()
                    .is_none_or(|task| task.status() == ComponentTaskStatus::Rejected)
                {
                    self.watch_control(context);
                }
                if self
                    .frame_task
                    .as_ref()
                    .is_none_or(|task| task.status() == ComponentTaskStatus::Rejected)
                {
                    self.watch_frames(context);
                }
                if self
                    .control_task
                    .as_ref()
                    .is_some_and(|task| task.status() != ComponentTaskStatus::Rejected)
                    && self
                        .frame_task
                        .as_ref()
                        .is_none_or(|task| task.status() != ComponentTaskStatus::Rejected)
                {
                    self.watcher_retry = None;
                }
            }
            Message::Control(w11term::control::UiRequest::Open {
                profile,
                cols,
                rows,
                activate,
                reply,
            }) => {
                let result = (|| -> anyhow::Result<serde_json::Value> {
                    let ssh = self
                        .profiles
                        .get(&profile)
                        .with_context(|| format!("unknown profile {profile}"))?
                        .clone();
                    let metrics = self.metrics.get();
                    let explicit_grid = cols.is_some() || rows.is_some();
                    anyhow::ensure!(
                        !activate || !explicit_grid || !self.windows.borrow().fullscreen(),
                        "Exit fullscreen before opening an active tab with a different grid; background open is available"
                    );
                    let cols = cols.unwrap_or(metrics.cols.max(1));
                    let rows = rows.unwrap_or(metrics.rows.max(1));
                    let size = TerminalSize {
                        cols,
                        rows,
                        pixel_width: (cols as f32 * metrics.cell_width * metrics.scale).round()
                            as usize,
                        pixel_height: (rows as f32 * metrics.cell_height * metrics.scale).round()
                            as usize,
                        dpi: (96.0 * metrics.scale).round() as u32,
                    };
                    let previous = self.tabs.active_id();
                    let id = self.tabs.insert(live_tab_sized(
                        profile,
                        Source::Ssh(ssh),
                        size,
                        self.appearance.read().unwrap().interaction.osc52,
                    ));
                    if !activate && let Some(previous) = previous {
                        self.tabs.select(previous);
                    }
                    self.sync_selection(context);
                    if activate || previous.is_none() {
                        if explicit_grid {
                            self.initial_grid.set(Some((cols, rows)));
                            self.windows.borrow_mut().grid = Some((cols, rows));
                        }
                        self.activate(context);
                    } else {
                        self.publish_panes();
                    }
                    Ok(
                        serde_json::json!({"pane":id,"cols":cols,"rows":rows,"active":activate||previous.is_none()}),
                    )
                })();
                let _ = reply.send(result.map_err(|e| format!("{e:#}")));
            }
            Message::Control(w11term::control::UiRequest::Resize {
                pane,
                cols,
                rows,
                reply,
            }) => {
                if self.tabs.active_id() == Some(pane) && self.windows.borrow().fullscreen() {
                    let _ = reply.send(Err(
                        "Exit fullscreen before resizing the active window grid".into(),
                    ));
                    return;
                }
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get(pane)
                {
                    let metrics = self.metrics.get();
                    let size = TerminalSize {
                        cols,
                        rows,
                        pixel_width: (cols as f32 * metrics.cell_width * metrics.scale).round()
                            as usize,
                        pixel_height: (rows as f32 * metrics.cell_height * metrics.scale).round()
                            as usize,
                        dpi: (96.0 * metrics.scale).round() as u32,
                    };
                    let _ = tab.session.input.send(Command::Resize(size));
                    if self.tabs.active_id() == Some(pane) {
                        self.initial_grid.set(Some((cols, rows)));
                        self.windows.borrow_mut().grid = Some((cols, rows));
                        self.client_size.0 +=
                            (cols as f64 - metrics.cols as f64) * metrics.cell_width as f64;
                        self.client_size.1 +=
                            (rows as f64 - metrics.rows as f64) * metrics.cell_height as f64;
                        self.invalidator.invalidate();
                    }
                    context.spawn_background(move |_| {
                        std::thread::sleep(Duration::from_millis(300));
                        Message::ControlResizeDone {
                            pane,
                            cols,
                            rows,
                            reply,
                        }
                    });
                } else {
                    let _ = reply.send(Err(format!("unknown terminal pane {pane}")));
                }
            }
            Message::ControlResizeDone {
                pane,
                cols,
                rows,
                reply,
            } => {
                let result = if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get(pane)
                {
                    let frame = tab.session.frames.load();
                    if frame.cols == cols && frame.rows.len() == rows {
                        Ok(serde_json::json!({"pane":pane,"cols":cols,"rows":rows}))
                    } else {
                        self.initial_grid.set(None);
                        Err(format!(
                            "requested grid does not fit the current window/display; actual {}x{}",
                            frame.cols,
                            frame.rows.len()
                        ))
                    }
                } else {
                    Err("pane was closed".into())
                };
                let _ = reply.send(result);
            }
            Message::PromptEdit(id, prompt, index, value) => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                    && tab.prompt_id == Some(prompt)
                    && value.len() <= 4096
                    && let Some(answer) = tab.answers.get_mut(index)
                {
                    *answer = zeroize::Zeroizing::new(value);
                }
            }
            Message::PromptReply(id, prompt, result) => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                    && tab.prompt_id == Some(prompt)
                {
                    let question = tab.session.prompts.current();
                    let answer = if result == ContentDialogResult::Primary {
                        match question.map(|p| p.question) {
                            Some(w11term::prompts::Question::HostKey { .. }) => {
                                w11term::prompts::Answer::Trust(true)
                            }
                            _ => w11term::prompts::Answer::Authentication(std::mem::take(
                                &mut tab.answers,
                            )),
                        }
                    } else {
                        w11term::prompts::Answer::Cancel
                    };
                    let _ = tab.session.prompts.answer(prompt, answer);
                    tab.answers.clear();
                }
            }
            Message::GenerateKey(id) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get_mut(id)
                    && !draft.generating
                {
                    draft.generating = true;
                    draft.error = None;
                    let name = draft.name.clone();
                    context.spawn_background(move |_| {
                        Message::KeyGenerated(
                            id,
                            w11term::keys::generate(&name).map_err(|e| format!("{e:#}")),
                        )
                    });
                }
            }
            Message::KeyGenerated(id, result) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    draft.generating = false;
                    match result {
                        Ok(key) => {
                            draft.identity = key.path.to_string_lossy().into_owned();
                            draft.player.device_key = Some(key.path.clone());
                            draft.authentication = if draft.player.rustmush { 3 } else { 1 };
                            if let Err(e) = w11term::clipboard::write(&key.public_line) {
                                draft.error =
                                    Some(format!("Public key generated; clipboard: {e:#}"));
                            }
                            draft.device = Some(key);
                        }
                        Err(e) => draft.error = Some(e),
                    }
                }
            }
            Message::CopyPublicKey(id) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get(id)
                    && let Some(key) = &draft.device
                    && let Err(e) = w11term::clipboard::write(&key.public_line)
                {
                    log::warn!("Copying public key: {e:#}");
                }
            }
            Message::DraftPlayer(id, field, value) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    match field.as_str() {
                        "rustmush" => {
                            draft.player.rustmush = value;
                            if value {
                                draft.player.game_mode = true;
                                draft.authentication = 3;
                            }
                        }
                        "game_mode" => draft.player.game_mode = value,
                        "alternate_scroll" => {
                            // Native property reconciliation can emit the same
                            // value; that must not turn automatic policy into an override.
                            if draft.player.alternate_scroll_enabled() != value {
                                draft.player.alternate_scroll = Some(value);
                            }
                        }
                        "remote_sounds" => {
                            if draft.player.remote_sounds_enabled() != value {
                                draft.player.remote_sounds = Some(value);
                            }
                        }
                        "sync_assets" => {
                            if draft.player.sync_assets_enabled() != value {
                                draft.player.sync_assets = Some(value);
                            }
                        }
                        "reconnect" => draft.player.reconnect = value,
                        "mute" => draft.player.mute = value,
                        _ => {}
                    }
                }
            }
            #[cfg(feature = "ui-smoke")]
            Message::Smoke(step) => self.smoke_step(step, context),
            Message::FrameReady(id, epoch) => {
                if self.tabs.active_id() == Some(id) && self.watch_epoch == epoch {
                    if let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get_mut(id)
                    {
                        tab.error = None;
                    }
                    self.connection_ui(id);
                    if let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get(id)
                    {
                        let frame = tab.session.frames.load();
                        if tab
                            .selection
                            .borrow()
                            .as_ref()
                            .is_some_and(|s| s.alternate != frame.alternate || s.cols != frame.cols)
                        {
                            tab.selection.borrow_mut().take();
                            self.keyboard.set_selection(false);
                        }
                        if let Some(selection) =
                            tab.selection.borrow_mut().as_mut().filter(|s| s.dragging)
                        {
                            selection.observe(&frame);
                            if tab.scroll_direction != 0 {
                                let row = if tab.scroll_direction > 0 {
                                    frame.first_stable_row
                                } else {
                                    frame.first_stable_row
                                        + frame.rows.len().saturating_sub(1) as isize
                                };
                                selection.update(Point {
                                    row,
                                    col: selection.end.col,
                                });
                            }
                        }
                    }
                    self.invalidator.invalidate();
                    self.watch_frames(context);
                }
            }
            Message::CloseWindow => {
                if let Some(name) = &self.window_profile {
                    let _ = self.windows.borrow_mut().remember(name);
                }
                let _ = context.window().request_close();
            }
            Message::SyncSelection(epoch) => {
                if self.selection_epoch == epoch {
                    self.selection_pending = false;
                }
            }
            Message::Select(Some(index), epoch, pending) => {
                if pending || self.selection_pending || epoch != self.selection_epoch {
                    return;
                }
                if let Some(id) = self.tabs.entries().get(index).map(|t| t.id)
                    && self.tabs.active_id() != Some(id)
                {
                    self.tabs.select(id);
                    self.activate(context);
                }
            }
            Message::Select(None, _, _) => {}
            Message::CloseTab(id) => {
                if let Ok(id) = id.parse() {
                    self.close_tab(id, context);
                }
            }
            Message::Reorder(order, selected) => {
                if let Some(ids) = order
                    .iter()
                    .map(|id| id.parse().ok())
                    .collect::<Option<Vec<_>>>()
                    && self.tabs.reorder(&ids)
                {
                    let changed = self.tabs.active_id() != selected;
                    if let Some(id) = selected {
                        self.tabs.select(id);
                    }
                    self.sync_selection(context);
                    if changed {
                        self.activate(context);
                    }
                }
            }
            Message::Shortcut(shortcut) => match shortcut {
                Shortcut::Fullscreen => {
                    if let Err(error) = self.windows.borrow_mut().toggle() {
                        log::warn!("Fullscreen: {error:#}");
                    }
                    self.invalidator.invalidate();
                }
                Shortcut::BeginResize => {
                    self.initial_grid.set(None);
                }
                Shortcut::RememberWindow => {
                    if let Some(name) = &self.window_profile {
                        let _ = self.windows.borrow_mut().remember(name);
                    }
                }
                Shortcut::Find => self.find(false, None, context),
                Shortcut::FindNext => self.find(false, Some(1), context),
                Shortcut::FindPrevious => self.find(false, Some(-1), context),
                Shortcut::FindClose => self.find(true, None, context),
                Shortcut::NextTab | Shortcut::PreviousTab => {
                    self.switch_tabs(
                        if matches!(shortcut, Shortcut::NextTab) {
                            1
                        } else {
                            -1
                        },
                        context,
                    );
                }
                Shortcut::Copy => self.copy(),
                Shortcut::Paste => self.paste(false, context),
                Shortcut::PasteRaw => self.paste(true, context),
                Shortcut::ClearSelection => self.clear_selection(),
                Shortcut::Zoom(delta) => {
                    let mut appearance = self.appearance.read().unwrap().clone();
                    appearance.font_size = (appearance.font_size + delta as f32).clamp(6.0, 72.0);
                    self.apply_appearance(appearance);
                }
                Shortcut::ResetZoom => {
                    let mut appearance = self.appearance.read().unwrap().clone();
                    appearance.font_size = 16.0;
                    self.apply_appearance(appearance);
                }
                Shortcut::CommitTab => self.finish_switcher(true, context),
                Shortcut::CancelTab => self.finish_switcher(false, context),
                Shortcut::NewTab => self.open(
                    self.default_label.clone(),
                    self.default_source.clone(),
                    context,
                ),
                Shortcut::CloseTab => {
                    if let Some(id) = self.tabs.active_id() {
                        self.close_tab(id, context);
                    }
                }
                Shortcut::Select(index) => {
                    if let Some(id) = self.tabs.entries().get(index).map(|t| t.id) {
                        self.tabs.select(id);
                        self.activate(context);
                    }
                }
            },
            Message::ProfileMenu(label) => {
                if label == NEW_PROFILE {
                    self.utility(false, context);
                } else if label == LAB {
                    self.open(LAB.into(), Source::Demo, context);
                } else if let Some((name, profile)) = self
                    .profiles
                    .iter()
                    .find(|(name, ssh)| profile_label(name, ssh) == label)
                    .map(|(n, p)| (n.clone(), p.clone()))
                {
                    self.open(name, Source::Ssh(profile), context);
                }
            }
            Message::SettingsMenu => self.utility(true, context),
            Message::Paste => self.paste(false, context),
            Message::PasteRaw => self.paste(true, context),
            Message::RawPaste(result) => {
                if let Err(error) = result {
                    log::warn!("Raw paste: {error}");
                }
            }
            Message::Interaction(field, value) => {
                let mut appearance = self.appearance.read().unwrap().clone();
                let setting = match field {
                    InteractionField::CopyOnSelect => &mut appearance.interaction.copy_on_select,
                    InteractionField::TrimBlock => &mut appearance.interaction.trim_block_selection,
                    InteractionField::TrimPaste => &mut appearance.interaction.trim_paste,
                    InteractionField::Snap => &mut appearance.interaction.snap_to_grid_on_resize,
                    InteractionField::ScrollZoom => &mut appearance.interaction.scroll_to_zoom,
                    InteractionField::DetectUrls => &mut appearance.interaction.detect_urls,
                    InteractionField::Osc52 => &mut appearance.interaction.osc52,
                };
                if *setting != value {
                    *setting = value;
                    self.save_preferences(appearance);
                    self.invalidator.invalidate();
                }
            }
            Message::TabMode(Some(index)) if index < 3 => {
                let mut appearance = self.appearance.read().unwrap().clone();
                let mode = [
                    TabSwitcherMode::InOrder,
                    TabSwitcherMode::Mru,
                    TabSwitcherMode::Disabled,
                ][index];
                if mode != appearance.interaction.tab_switcher_mode {
                    appearance.interaction.tab_switcher_mode = mode;
                    self.save_preferences(appearance);
                    self.finish_switcher(false, context);
                }
            }
            Message::TabMode(_) => {}
            Message::PickTab(id) => {
                self.finish_switcher(false, context);
                self.tabs.select(id);
                self.activate(context);
            }
            Message::CaptureLost(id) => {
                // Reactor releases capture before queuing PointerReleased. Defer cancellation
                // so that normal releases still copy the selection or open a clicked URL.
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get(id)
                {
                    let gesture = tab.gesture;
                    context.spawn_background(move |_| {
                        std::thread::sleep(Duration::from_millis(10));
                        Message::FinalizeCapture(id, gesture)
                    });
                }
            }
            Message::FinalizeCapture(id, gesture) => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                    && tab.gesture == gesture
                {
                    if let Some(selection) = tab.selection.borrow_mut().as_mut() {
                        selection.dragging = false;
                    }
                    tab.scroll_direction = 0;
                    tab.clicked_link = None;
                }
            }
            Message::PointerExit(id) => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get(id)
                {
                    tab.hover.borrow_mut().take();
                    self.invalidator.invalidate();
                }
            }
            Message::DragScroll(id) => {
                let active = self.tabs.active_id() == Some(id);
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    tab.scroll_tick = false;
                    if active
                        && tab.scroll_direction != 0
                        && tab.selection.borrow().as_ref().is_some_and(|s| s.dragging)
                    {
                        let _ = tab
                            .session
                            .input
                            .send(Command::Scroll(tab.scroll_direction));
                        tab.scroll_tick = true;
                        context.spawn_background(move |_| {
                            std::thread::sleep(Duration::from_millis(40));
                            Message::DragScroll(id)
                        });
                    }
                }
            }
            Message::Font(Some(index)) => {
                if let Some(font) = self.fonts.get(index) {
                    let mut appearance = self.appearance.read().unwrap().clone();
                    if appearance.font_family != *font {
                        appearance.font_family = font.clone();
                        self.apply_appearance(appearance);
                    }
                }
            }
            Message::Font(None) => {}
            Message::Ligatures(enabled) => {
                let mut appearance = self.appearance.read().unwrap().clone();
                if appearance.ligatures != enabled {
                    appearance.ligatures = enabled;
                    self.apply_appearance(appearance);
                }
            }
            Message::WindowSize(size) => {
                self.width = size.width;
                self.window_height = size.height;
                if !self.window_restored {
                    self.window_restored = true;
                    context.spawn_background(|_| {
                        std::thread::sleep(Duration::from_millis(200));
                        Message::RestoreWindow
                    });
                }
            }
            Message::Geometry(width, height, epoch, first) => {
                if self.initial_grid.epoch() == epoch {
                    self.client_size = (width, height);
                    if first {
                        context.spawn_background_with_rejection(
                            move |_| {
                                std::thread::sleep(Duration::from_millis(300));
                                Message::GridFallback(epoch)
                            },
                            Message::GridFallback(epoch),
                        );
                    }
                }
            }
            Message::GridFallback(epoch) => {
                if self.initial_grid.epoch() == epoch && self.initial_grid.get().is_some() {
                    self.initial_grid.set(None);
                    self.invalidator.invalidate();
                }
            }
            Message::RenderError(id, error) => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    log::error!("Renderer: {error}");
                    tab.error = Some(error);
                }
            }
            Message::RetryRender(id) => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    tab.error = None;
                    self.invalidator.invalidate();
                }
            }
            Message::Edit(id, field, text) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    *match field {
                        Field::Name => &mut draft.name,
                        Field::Host => &mut draft.host,
                        Field::User => &mut draft.user,
                        Field::Port => &mut draft.port,
                        Field::Identity => &mut draft.identity,
                        Field::Fingerprint => &mut draft.fingerprint,
                    } = text;
                    draft.error = None;
                }
            }
            Message::Auth(id, Some(index)) => {
                if let Some(AppTab {
                    page: Page::NewProfile(draft),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    draft.authentication = index;
                }
            }
            Message::Auth(_, None) => {}
            Message::SaveProfile(id) => self.save_profile(id, context),
            Message::Pointer(id, pointer, kind) => self.pointer(id, pointer, kind, context),
            Message::FindEdit(id, mut text) => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                    && tab.find.query != text
                {
                    if text.len() > 4096 {
                        let mut end = 4096;
                        while !text.is_char_boundary(end) {
                            end -= 1;
                        }
                        text.truncate(end);
                    }
                    tab.find.query = text;
                    tab.find.epoch += 1;
                    let epoch = tab.find.epoch;
                    context.spawn_background(move |_| {
                        std::thread::sleep(Duration::from_millis(120));
                        Message::FindApply(id, epoch)
                    });
                }
            }
            Message::FindCase(id, sensitive) => {
                if let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.get_mut(id)
                {
                    tab.find.sensitive = sensitive;
                    tab.find.epoch += 1;
                    let epoch = tab.find.epoch;
                    self.apply_find(id, epoch);
                }
            }
            Message::FindApply(id, epoch) => self.apply_find(id, epoch),
            Message::FindFocus(id, epoch, attempt) => {
                let sender = context.sender();
                if self.tabs.active_id() == Some(id)
                    && let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get(id)
                    && tab.find.open
                    && tab.find.epoch == epoch
                    && !tab.find.input.request_focus_result(move |result| {
                        sender.send(Message::FindFocusResult(id, epoch, result.unwrap_or(false)));
                    })
                    && attempt < 3
                {
                    context.spawn_background(move |_| {
                        std::thread::sleep(Duration::from_millis(30));
                        Message::FindFocus(id, epoch, attempt + 1)
                    });
                }
            }
            Message::FindFocusResult(id, epoch, focused) => {
                if self.tabs.active_id() == Some(id)
                    && let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get_mut(id)
                    && tab.find.epoch == epoch
                    && tab.find.open
                {
                    tab.find.focused = focused;
                }
            }
        }
    }

    fn view(&self, _options: &Options, context: &mut ViewContext<Self>) -> View {
        let title = self
            .tabs
            .active()
            .map(|t| t.label.as_str())
            .unwrap_or("Terminal");
        context.window_title(format!("w11term · {title}"));
        context.window_visuals(
            WindowVisuals::new()
                .client_size(self.client_size.0, self.client_size.1)
                .theme(WindowTheme::Dark)
                .constraints(WindowConstraints {
                    min_width: Some(640.0_f64.min(self.client_size.0)),
                    min_height: Some(360.0_f64.min(self.client_size.1)),
                    ..Default::default()
                }),
        );
        context.on_window_size(context.callback(Message::WindowSize));
        let cached_header = self.header.borrow().clone();
        let header = cached_header.unwrap_or_else(|| {
            let header = self.header_view(context);
            *self.header.borrow_mut() = Some(header.clone());
            header
        });
        let content = if let Some(id) = self.tabs.active_id() {
            match &self.tabs.get(id).unwrap().page {
                Page::Terminal(tab) => self.terminal_view(id, tab, context),
                Page::Settings => self.settings_view(context),
                Page::NewProfile(draft) => self.profile_form(id, draft, context),
            }
        } else {
            Border::new().into()
        };
        let mut views = vec![
            KeyedView::new(
                "header",
                Border::new()
                    .background(Color::rgb(35, 35, 35))
                    .content(header),
            ),
            KeyedView::new("content", Border::new().grid_row(1).content(content)),
        ];
        if let Some(id) = self.tabs.active_id()
            && let Some(AppTab {
                page: Page::Terminal(tab),
                ..
            }) = self.tabs.get(id)
            && tab.find.open
        {
            views.push(KeyedView::new("find", self.find_view(id, tab, context)));
        }
        if let Some(switcher) = &self.switcher {
            let choices: Vec<_> = switcher
                .ids
                .iter()
                .enumerate()
                .filter_map(|(index, id)| {
                    self.tabs
                        .get(*id)
                        .map(|tab| (index, *id, tab.label.clone()))
                })
                .map(|(index, id, label)| {
                    KeyedView::new(
                        id.to_string(),
                        Button::new()
                            .style(if index == switcher.index {
                                ButtonStyle::Accent
                            } else {
                                ButtonStyle::Subtle
                            })
                            .horizontal_alignment(HorizontalAlignment::Stretch)
                            .on_click(context.callback(move |_| Message::PickTab(id)))
                            .content(TextBlock::new().text(label)),
                    )
                })
                .collect();
            views.push(KeyedView::new(
                "switcher",
                Border::new()
                    .grid_row(1)
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .vertical_alignment(VerticalAlignment::Top)
                    .margin(24.0)
                    .padding(12.0)
                    .corner_radius(8.0)
                    .background(Color::rgb(40, 42, 48))
                    .content(
                        StackPanel::new()
                            .min_width(320.0)
                            .spacing(6.0)
                            .keyed_children(choices),
                    ),
            ));
        }
        if let Some(id) = self.tabs.active_id()
            && let Some(AppTab {
                page: Page::Terminal(tab),
                ..
            }) = self.tabs.get(id)
            && let Some(prompt) = &tab.session.frames.load().prompt
        {
            views.push(KeyedView::new(
                format!("prompt-{}", prompt.id),
                self.prompt_view(id, tab, prompt, context),
            ));
        }
        if let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.active()
            && let Some(message) = &tab.device_message
        {
            views.push(KeyedView::new(
                "device-message",
                Border::new()
                    .grid_row(1)
                    .vertical_alignment(VerticalAlignment::Top)
                    .background(Color::rgb(40, 42, 48))
                    .padding(10.0)
                    .content(
                        TextBlock::new()
                            .text(message)
                            .text_wrapping(TextWrapping::Wrap),
                    ),
            ));
        }
        Grid::new()
            .rows([GridLength::Auto, GridLength::STAR])
            .background(Color::rgb(14, 16, 20))
            .keyed_children(views)
    }
}

#[cfg(feature = "ui-smoke")]
impl TerminalApp {
    fn smoke_step(&mut self, step: u8, context: &ComponentContext<Self>) {
        if std::env::var_os("W11TERM_PLAYER_SMOKE").is_some() {
            self.player_smoke_step(step, context);
            return;
        }
        if std::env::var_os("W11TERM_SEARCH_SMOKE").is_some() {
            self.search_smoke_step(step, context);
            return;
        }
        let text = |tab: &AppTab| match &tab.page {
            Page::Terminal(tab) => tab
                .session
                .frames
                .load()
                .rows
                .iter()
                .map(|l| l.as_str().to_string())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        match step {
            0 => {
                if let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page {
                    // Seed lab geometry before typing: native first-paint timing varies when
                    // the smoke window starts behind another window or a release build.
                    tab.session
                        .input
                        .send(Command::Resize(TerminalSize {
                            pixel_width: 800,
                            pixel_height: 600,
                            ..Default::default()
                        }))
                        .unwrap();
                    tab.session
                        .input
                        .send(Command::Text("tab-one".into()))
                        .unwrap();
                }
            }
            1 => self.open("Second lab".into(), Source::Demo, context),
            2 => {
                if let Page::Terminal(tab) = &self.tabs.get(2).unwrap().page {
                    tab.session
                        .input
                        .send(Command::Resize(TerminalSize {
                            pixel_width: 800,
                            pixel_height: 600,
                            ..Default::default()
                        }))
                        .unwrap();
                    tab.session
                        .input
                        .send(Command::Text("tab-two".into()))
                        .unwrap();
                }
            }
            3 => {
                assert!(text(self.tabs.get(1).unwrap()).contains("tab-one"));
                assert!(text(self.tabs.get(2).unwrap()).contains("tab-two"));
                self.tabs.select(1);
                self.activate(context);
                if std::env::var_os("W11TERM_INTERACTION_SMOKE").is_some() {
                    assert!(std::env::var_os("W11TERM_SETTINGS_PATH").is_some());
                    self.update(
                        Message::Interaction(InteractionField::CopyOnSelect, false),
                        context,
                    );
                    let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get(1)
                    else {
                        panic!()
                    };
                    let frame = tab.session.frames.load();
                    let (row, col) = frame
                        .rows
                        .iter()
                        .enumerate()
                        .find_map(|(row, line)| line.as_str().find("tab-one").map(|col| (row, col)))
                        .expect("text to select");
                    let metrics = self.metrics.get();
                    let pointer = |col| PointerEventInfo {
                        x: (PADDING + (col as f32 + 0.1) * metrics.cell_width) as f64,
                        y: (PADDING + (row as f32 + 0.1) * metrics.cell_height) as f64,
                        is_left_button_pressed: true,
                        ..Default::default()
                    };
                    self.pointer(1, pointer(col), MouseEventKind::Press, context);
                    self.pointer(1, pointer(col + 6), MouseEventKind::Move, context);
                    self.update(Message::CaptureLost(1), context);
                    self.pointer(
                        1,
                        PointerEventInfo {
                            is_left_button_pressed: false,
                            ..pointer(col + 6)
                        },
                        MouseEventKind::Release,
                        context,
                    );
                    let Some(AppTab {
                        page: Page::Terminal(tab),
                        ..
                    }) = self.tabs.get(1)
                    else {
                        panic!()
                    };
                    assert_eq!(
                        tab.selection.borrow().as_ref().unwrap().text(&frame, true),
                        "tab-one"
                    );
                    self.switch_tabs(1, context);
                    assert_eq!(self.tabs.active_id(), Some(1));
                    assert!(self.switcher.is_some());
                }
            }
            4 => {
                if std::env::var_os("W11TERM_INTERACTION_SMOKE").is_some() {
                    self.finish_switcher(false, context);
                    assert_eq!(self.tabs.active_id(), Some(1));
                    self.switch_tabs(1, context);
                    self.finish_switcher(true, context);
                    assert_eq!(self.tabs.active_id(), Some(2));
                    self.tabs.select(1);
                    self.activate(context);
                    self.update(Message::Shortcut(Shortcut::Zoom(2)), context);
                    assert_eq!(self.appearance.read().unwrap().font_size, 18.0);
                    self.update(Message::Shortcut(Shortcut::ResetZoom), context);
                }
                assert!(self.tabs.reorder(&[2, 1]));
                self.sync_selection(context);
                assert_eq!(self.tabs.active_id(), Some(1));
            }
            5 => {
                self.close_tab(2, context);
                assert_eq!(self.tabs.active_id(), Some(1));
            }
            6 => {
                self.utility(true, context);
                if std::env::var_os("W11TERM_FONT_SMOKE").is_some() {
                    assert!(
                        std::env::var_os("W11TERM_SETTINGS_PATH").is_some(),
                        "font smoke needs isolated settings"
                    );
                    let index = self
                        .fonts
                        .iter()
                        .position(|font| font == "Cascadia Code")
                        .expect("font smoke requires Cascadia Code");
                    self.update(Message::Font(Some(index)), context);
                    self.update(Message::Ligatures(false), context);
                    assert_eq!(self.appearance.read().unwrap().font_family, "Cascadia Code");
                    assert!(!self.appearance.read().unwrap().ligatures);
                    let saved = Appearance::load(&Appearance::path().unwrap()).unwrap();
                    assert_eq!(saved.font_family, "Cascadia Code");
                    assert!(!saved.ligatures);
                    assert!(
                        self.appearance_error.is_none(),
                        "{:?}",
                        self.appearance_error
                    );
                }
            }
            7 => self.utility(false, context),
            8 => {
                assert!(!self.keyboard.has_target());
                self.save_profile(4, context);
                let Page::NewProfile(draft) = &self.tabs.get(4).unwrap().page else {
                    panic!("missing form")
                };
                assert!(draft.error.is_some());
            }
            9 => self.close_tab(4, context),
            10 => self.close_tab(3, context),
            _ => {
                assert_eq!(self.tabs.active_id(), Some(1));
                assert!(text(self.tabs.get(1).unwrap()).contains("tab-one"));
                assert!(!text(self.tabs.get(1).unwrap()).contains("tab-two"));
                eprintln!(
                    "Tabbed UI smoke passed: independent buffers, switch, reorder, close, settings, profile form"
                );
                self.close_tab(1, context);
                return;
            }
        }
        if step == 7 && std::env::var_os("W11TERM_UI_SMOKE_PAUSE").is_some() {
            return;
        }
        context.spawn_background(move |_| {
            std::thread::sleep(std::time::Duration::from_millis(500));
            Message::Smoke(step + 1)
        });
    }
    fn player_smoke_step(&mut self, step: u8, context: &ComponentContext<Self>) {
        use w11term::prompts::{Answer, Challenge, Question};
        eprintln!("Player UI smoke step {step}");
        match step {
            0 | 2 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                let prompts = tab.session.prompts.clone();
                context.spawn_background(move |_|{let question=if step==0{Question::HostKey{host:"loopback.test".into(),port:22,algorithm:"ssh-ed25519".into(),fingerprint:"SHA256:test-fingerprint".into()}}else{Question::Authentication{name:"MFA test".into(),instructions:"Isolated native prompt test".into(),challenges:vec![Challenge{label:"Account".into(),echo:true},Challenge{label:"Password".into(),echo:false}]}};
                    let answer=tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(prompts.ask(question)).unwrap();
                    if step==0{assert!(matches!(answer,Answer::Trust(true)));}else{assert!(matches!(answer,Answer::Authentication(ref v)if v.len()==2&&v[0].as_str()=="pilot"&&v[1].as_str()=="secret"));}Message::Smoke(step+2)
                });
                context.spawn_background(move |_| {
                    std::thread::sleep(Duration::from_millis(500));
                    Message::Smoke(step + 1)
                });
            }
            1 | 3 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                let prompt = tab.session.prompts.current().unwrap();
                assert_eq!(tab.prompt_id, Some(prompt.id));
                assert!(!self.keyboard.has_target());
                if step == 3 {
                    self.update(
                        Message::PromptEdit(1, prompt.id, 0, "pilot".into()),
                        context,
                    );
                    self.update(
                        Message::PromptEdit(1, prompt.id, 1, "secret".into()),
                        context,
                    );
                }
                self.update(
                    Message::PromptReply(1, prompt.id, ContentDialogResult::Primary),
                    context,
                );
            }
            4 => {
                self.windows.borrow_mut().remember("ui-smoke").unwrap();
                self.update(Message::Shortcut(Shortcut::Fullscreen), context);
                assert!(self.windows.borrow().fullscreen());
                context.spawn_background(|_| {
                    std::thread::sleep(Duration::from_millis(250));
                    Message::Smoke(5)
                });
            }
            5 => {
                let expected = {
                    let windows = self.windows.borrow();
                    let placement = &windows.profiles["ui-smoke"];
                    (placement.cols, placement.rows)
                };
                self.windows.borrow_mut().remember("ui-smoke").unwrap();
                {
                    let windows = self.windows.borrow();
                    let placement = &windows.profiles["ui-smoke"];
                    assert_eq!(
                        (placement.cols, placement.rows),
                        expected,
                        "fullscreen replaced the saved normal grid"
                    );
                }
                self.update(Message::Shortcut(Shortcut::Fullscreen), context);
                assert!(!self.windows.borrow().fullscreen());
                self.utility(false, context);
                context.spawn_background(|_| {
                    std::thread::sleep(Duration::from_millis(250));
                    Message::Smoke(6)
                });
            }
            6 => {
                assert!(!self.keyboard.has_target());
                let id = self.tabs.active_id().unwrap();
                self.update(Message::DraftPlayer(id, "rustmush".into(), true), context);
                let Page::NewProfile(draft) = &self.tabs.get(id).unwrap().page else {
                    panic!()
                };
                assert_eq!(draft.authentication, 3);
                assert!(draft.player.game_mode);
                assert!(!draft.player.alternate_scroll_enabled());
                self.update(
                    Message::DraftPlayer(id, "alternate_scroll".into(), false),
                    context,
                );
                let Page::NewProfile(draft) = &self.tabs.get(id).unwrap().page else {
                    panic!()
                };
                assert!(
                    draft.player.alternate_scroll.is_none(),
                    "native reconciliation overwrote automatic policy"
                );
                self.update(
                    Message::DraftPlayer(id, "alternate_scroll".into(), true),
                    context,
                );
                let Page::NewProfile(draft) = &self.tabs.get(id).unwrap().page else {
                    panic!()
                };
                assert_eq!(draft.player.alternate_scroll, Some(true));
                eprintln!(
                    "Player UI smoke passed: trust and private/native prompts, fullscreen roundtrip, wizard wheel policy."
                );
                let _ = context.window().request_close();
            }
            _ => {}
        }
    }
    fn search_smoke_step(&mut self, step: u8, context: &ComponentContext<Self>) {
        eprintln!("Search UI smoke step {step}");
        match step {
            0 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                tab.session
                    .input
                    .send(Command::Text(
                        (0..80).map(|i| format!("needle-{i:03}\r")).collect(),
                    ))
                    .unwrap();
            }
            1 => {
                let mut prefs = self.appearance.read().unwrap().interaction.clone();
                prefs.scroll_to_zoom = false;
                self.keyboard.set_preferences(prefs);
                assert!(
                    self.keyboard.smoke_wheel(120),
                    "native wheel callback must consume the event"
                );
            }
            2 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                assert!(
                    tab.session.frames.load().scroll_offset > 0,
                    "native wheel did not scroll history"
                );
                self.find(false, None, context);
            }
            3 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                assert!(
                    tab.find.open && tab.find.focused,
                    "find textbox did not receive native focus"
                );
                self.update(Message::FindEdit(1, "needle-000".into()), context);
            }
            4 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                let frame = tab.session.frames.load();
                let search = frame.search.as_ref().expect("search snapshot");
                assert_eq!(search.total, 1);
                assert!(!search.visible.is_empty());
                assert!(frame.scroll_offset > 0);
                self.update(Message::FindEdit(1, "needle-".into()), context);
            }
            5 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                let frame = tab.session.frames.load();
                assert_eq!(frame.search.as_ref().unwrap().total, 80);
                assert_eq!(frame.search.as_ref().unwrap().selected, Some(0));
                self.find(false, Some(1), context);
            }
            6 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                assert_eq!(
                    tab.session.frames.load().search.as_ref().unwrap().selected,
                    Some(1)
                );
                self.open("Second search lab".into(), Source::Demo, context);
            }
            7 => {
                let Page::Terminal(tab) = &self.tabs.get(2).unwrap().page else {
                    panic!()
                };
                assert!(!tab.find.open);
                assert!(tab.find.query.is_empty());
                self.tabs.select(1);
                self.activate(context);
            }
            8 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                assert!(tab.find.open && tab.find.focused);
                assert_eq!(tab.find.query, "needle-");
                self.find(true, None, context);
            }
            _ => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    panic!()
                };
                assert!(!tab.find.open);
                assert!(tab.session.frames.load().search.is_none());
                eprintln!(
                    "Search UI smoke passed: native wheel, textbox focus, scrollback matches/highlights, navigation, independent tabs, restore focus, Escape close"
                );
                let _ = context.window().request_close();
                return;
            }
        }
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(400));
            Message::Smoke(step + 1)
        });
    }
}
