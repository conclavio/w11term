use super::*;
use std::{
    rc::Weak,
    sync::atomic::{AtomicU64, Ordering},
};
use w11term::control::PaneTransfer;
use w11term::options::Options;
use windows::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{
        GetWindowTextW, IsIconic, IsWindow, SW_RESTORE, SetForegroundWindow, ShowWindowAsync,
    },
};

const WINDOW_LABEL: &str = "Window ";

pub(super) fn menu_window(label: &str) -> Option<u64> {
    let (id, _) = label.strip_prefix(WINDOW_LABEL)?.split_once(" · ")?;
    id.parse().ok().filter(|id| *id != 0)
}

struct WindowEndpoint {
    hwnd: HWND,
    sender: LocalSender<Message>,
    outgoing: Weak<Transfer>,
}

thread_local! {
    static WINDOWS: RefCell<BTreeMap<u64, WindowEndpoint>> = const { RefCell::new(BTreeMap::new()) };
}

fn windows_changed() {
    let senders = WINDOWS.with_borrow(|windows| {
        windows
            .values()
            .map(|endpoint| endpoint.sender.clone())
            .collect::<Vec<_>>()
    });
    for sender in senders {
        sender.send(Message::WindowTargetsChanged);
    }
}

#[derive(Clone)]
pub struct WindowLaunch {
    pub(super) options: Options,
    pub(super) opening: Option<Rc<crate::workspace_window::Opening>>,
    pub(super) transfer: Option<Rc<Transfer>>,
}

impl WindowLaunch {
    pub fn initial((options, opening): crate::workspace_window::Launch) -> Self {
        Self {
            options,
            opening,
            transfer: None,
        }
    }
}

impl PartialEq for WindowLaunch {
    fn eq(&self, other: &Self) -> bool {
        self.options == other.options
            && self.opening == other.opening
            && self.transfer.as_ref().map(|t| t.id()) == other.transfer.as_ref().map(|t| t.id())
    }
}

// Both components live on the same UI thread. Only the numeric ticket passes
// through Reactor messages; the owned sessions move once through this slot.
pub(super) struct Transfer {
    ticket: u64,
    destination: u64,
    pub(super) new_window: Option<NewWindow>,
    pub(super) group: TabId,
    panes: Vec<u64>,
    source: LocalSender<Message>,
    pub(super) target: RefCell<Option<LocalSender<Message>>>,
    payload: RefCell<Option<OfferedTab<AppTab>>>,
    pin: RefCell<Option<PaneTransfer>>,
    phase: Cell<TransferPhase>,
    started: Instant,
}

pub(super) struct NewWindow {
    pub(super) registry: Registry,
    pub(super) server: Arc<Server>,
}

struct OfferedTab<T> {
    tab: Tabs<T>,
    index: usize,
    was_selected: bool,
    fallback_group: Option<TabId>,
}

impl<T> OfferedTab<T> {
    fn take(source: &mut Tabs<T>, group: TabId, index: usize) -> Option<Self> {
        let was_selected = source.active_group_id() == Some(group);
        let tab = source.take_group(group)?;
        Some(Self {
            tab,
            index,
            was_selected,
            fallback_group: source.active_group_id(),
        })
    }

    fn return_to(self, source: &mut Tabs<T>) {
        // The automatic fallback may still be active with a different focused split.
        // A different surviving tab reflects a newer choice in the source window.
        let select = self.was_selected && source.active_group_id() == self.fallback_group;
        source.restore_group(self.tab, self.index, select);
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
enum TransferPhase {
    #[default]
    Opening,
    Ready,
    Offered,
    Claimed,
    Canceled,
}

impl TransferPhase {
    fn on_tick(self, elapsed: Duration) -> Self {
        if elapsed >= Duration::from_secs(10) && self != Self::Claimed {
            Self::Canceled
        } else {
            self
        }
    }
}

impl Transfer {
    fn id(&self) -> u64 {
        self.ticket
    }

    fn send_target(&self, message: Message) -> bool {
        self.target
            .borrow()
            .as_ref()
            .is_some_and(|sender| sender.send(message))
    }

    fn cancel_target(&self) {
        if !self.send_target(Message::TransferCanceled(self.id()))
            && let Some(window) = &self.new_window
        {
            let _ = window.registry.unregister();
        }
    }
}

impl TerminalApp {
    pub(super) fn arm_window_tick(&mut self, ctx: &ComponentContext<Self>) {
        if self.window_timer.is_none() {
            let sender = ctx.sender();
            self.window_timer = crate::ui_timer::RetryTimer::new(move || {
                sender.send(Message::WindowTick);
            })
            .ok();
            if self.window_timer.is_none() {
                ctx.spawn_background(|_| {
                    std::thread::sleep(Duration::from_millis(100));
                    Message::WindowTick
                });
            }
        }
    }

    pub(super) fn window_tick(&mut self, ctx: &ComponentContext<Self>) {
        if let Some(offer) = self
            .incoming
            .as_ref()
            .filter(|o| o.phase.get() == TransferPhase::Canceled)
        {
            self.cancel_transfer(offer.id(), ctx);
            return;
        }
        if !self.native_bound
            && let Some(hwnd) = self.keyboard.try_bind()
        {
            match self.registry.set_hwnd(hwnd.0 as usize) {
                Ok(()) => {
                    self.native_bound = true;
                    self.refresh_remote_clipboard_focus();
                    WINDOWS.with_borrow_mut(|windows| {
                        windows.insert(
                            self.registry.window_id(),
                            WindowEndpoint {
                                hwnd,
                                sender: ctx.sender(),
                                outgoing: Weak::new(),
                            },
                        );
                    });
                    windows_changed();
                    if let Some(opening) = self.opening.take() {
                        opening.published();
                    }
                    if let Some(offer) = &self.incoming {
                        if offer.phase.get() != TransferPhase::Opening
                            || !offer.source.send(Message::TransferReady(offer.id()))
                        {
                            let _ = ctx.window().request_close();
                        } else {
                            offer.phase.set(TransferPhase::Ready);
                        }
                    } else if !self.window_restored {
                        self.restore_window();
                    }
                }
                Err(error) => self.window_error = Some(format!("Registering window: {error}")),
            }
        }
        if let Some(offer) = self.outgoing.clone() {
            match offer.phase.get().on_tick(offer.started.elapsed()) {
                TransferPhase::Claimed => self.finish_transfer(offer.id(), ctx),
                TransferPhase::Canceled => self.cancel_transfer(offer.id(), ctx),
                TransferPhase::Ready => self.offer_group(offer.id(), ctx),
                TransferPhase::Opening | TransferPhase::Offered => {}
            }
        }
        if self.native_bound && self.outgoing.is_none() && self.incoming.is_none() {
            self.window_timer = None;
        } else {
            self.arm_window_tick(ctx);
        }
    }

    pub(super) fn existing_window_menu(&self) -> MenuItem {
        let windows = if self.incoming.is_some() || self.outgoing.is_some() {
            Vec::new()
        } else {
            WINDOWS.with_borrow(|windows| {
                windows
                    .iter()
                    .filter(|(id, _)| **id != self.registry.window_id())
                    .filter_map(|(id, endpoint)| {
                        if !unsafe { IsWindow(Some(endpoint.hwnd)) }.as_bool() {
                            return None;
                        }
                        let mut title = [0; 256];
                        let len = unsafe { GetWindowTextW(endpoint.hwnd, &mut title) } as usize;
                        let title = String::from_utf16_lossy(&title[..len]);
                        let title = title
                            .strip_prefix("w11term · ")
                            .filter(|s| !s.is_empty())
                            .unwrap_or("Terminal");
                        Some(MenuItem::item(
                            format!("move-tab-existing-{id}"),
                            format!("{WINDOW_LABEL}{id} · {title}"),
                        ))
                    })
                    .collect()
            })
        };
        let key = "move-tab-existing";
        let label = "Move tab to existing window";
        if windows.is_empty() {
            MenuItem::disabled(key, label)
        } else {
            MenuItem::submenu(key, label, windows)
        }
    }

    fn tab_move_blocker(&self, leaves: &[TabId]) -> Option<&'static str> {
        leaves.iter().find_map(|id| {
            let Page::Terminal(tab) = &self.tabs.get(*id)?.page else {
                return None;
            };
            if (self.tabs.active_id() == Some(*id) && tab.prompt_id.is_some())
                || tab.session.prompts.current().is_some()
            {
                Some("Answer or cancel the SSH prompt before moving this tab.")
            } else if tab.files.awaiting_native_result() {
                Some("Finish choosing or preparing files before moving this tab.")
            } else if tab.device_busy {
                Some("Wait for the SSH key operation to finish before moving this tab.")
            } else {
                None
            }
        })
    }

    pub(super) fn move_tab_window(
        &mut self,
        destination: Option<u64>,
        ctx: &ComponentContext<Self>,
    ) {
        if self.outgoing.is_some() || self.incoming.is_some() {
            return;
        }
        let Some(id) = self.tabs.active_id() else {
            return;
        };
        let group = self.tabs.group_id(id).unwrap();
        let Some(layout) = self.tabs.active_layout() else {
            return;
        };
        let leaves = layout.leaves();
        if let Some(reason) = self.tab_move_blocker(&leaves) {
            self.window_error = Some(reason.into());
            return;
        }
        let Some(panes) = leaves
            .into_iter()
            .map(|leaf| self.tabs.get(leaf)?.pane_id())
            .collect::<Option<Vec<_>>>()
        else {
            return;
        };
        let (destination, new_window, target) = if let Some(destination) = destination {
            let source = self.registry.window_id();
            let target = WINDOWS.with_borrow(|windows| {
                let endpoint = windows.get(&destination)?;
                (destination != source
                    && windows.contains_key(&source)
                    && unsafe { IsWindow(Some(endpoint.hwnd)) }.as_bool())
                .then(|| endpoint.sender.clone())
            });
            let Some(target) = target else {
                self.window_error = Some("That window is no longer available.".into());
                return;
            };
            (destination, None, Some(target))
        } else {
            static NEXT_WINDOW: AtomicU64 = AtomicU64::new(2);
            let destination = NEXT_WINDOW.fetch_add(1, Ordering::Relaxed);
            let registry = match self.registry.register_window(destination, None) {
                Ok(registry) => registry,
                Err(error) => {
                    self.window_error = Some(format!("Opening another window: {error}"));
                    return;
                }
            };
            let new_window = NewWindow {
                registry,
                server: self._control.clone(),
            };
            (destination, Some(new_window), None)
        };
        static NEXT_TRANSFER: AtomicU64 = AtomicU64::new(1);
        let transfer = Rc::new(Transfer {
            ticket: NEXT_TRANSFER.fetch_add(1, Ordering::Relaxed),
            destination,
            new_window,
            group,
            panes,
            source: ctx.sender(),
            target: RefCell::new(target),
            payload: Default::default(),
            pin: Default::default(),
            phase: Default::default(),
            started: Instant::now(),
        });
        self.window_error = None;
        self.outgoing = Some(transfer.clone());
        let started = if transfer.new_window.is_some() {
            let launch = WindowLaunch {
                options: self.window_launch.clone(),
                opening: None,
                transfer: Some(transfer.clone()),
            };
            ctx.open_window(View::component::<Self>(launch))
        } else {
            let source = self.registry.window_id();
            WINDOWS.with_borrow_mut(|windows| {
                if let Some(endpoint) = windows.get_mut(&source) {
                    endpoint.outgoing = Rc::downgrade(&transfer);
                }
            });
            transfer.send_target(Message::TransferRequest(source, transfer.id()))
        };
        if started {
            self.arm_window_tick(ctx);
        } else {
            self.cancel_transfer(transfer.id(), ctx);
        }
    }

    fn existing_destination_blocked(&self) -> bool {
        self.tabs.entries().is_empty()
            || self.outgoing.is_some()
            || self.title_editor.is_some()
            || self.clipboard.is_open()
            || self
                .tabs
                .active_id()
                .is_some_and(|id| self.tab_move_blocker(&[id]).is_some())
    }

    pub(super) fn accept_transfer(&mut self, source: u64, id: u64, ctx: &ComponentContext<Self>) {
        let offer = WINDOWS.with_borrow(|windows| windows.get(&source)?.outgoing.upgrade());
        let Some(offer) = offer.filter(|offer| {
            offer.id() == id
                && offer.destination == self.registry.window_id()
                && offer.phase.get() == TransferPhase::Opening
        }) else {
            return;
        };
        if self.incoming.is_some() || self.existing_destination_blocked() {
            offer.phase.set(TransferPhase::Canceled);
            offer.source.send(Message::TransferCanceled(id));
            return;
        }
        offer.phase.set(TransferPhase::Ready);
        offer.source.send(Message::TransferReady(id));
        self.incoming = Some(offer);
        self.arm_window_tick(ctx);
    }

    pub(super) fn offer_group(&mut self, id: u64, ctx: &ComponentContext<Self>) {
        let Some(offer) = self.outgoing.clone().filter(|o| o.id() == id) else {
            return;
        };
        if offer.phase.get() != TransferPhase::Ready {
            return;
        }
        let headers = self.tabs.headers();
        let Some(index) = headers
            .iter()
            .position(|t| self.tabs.group_id(t.id) == Some(offer.group))
        else {
            self.cancel_transfer(id, ctx);
            return;
        };
        let leaves = self.tabs.layout_for(headers[index].id).unwrap().leaves();
        let panes: Vec<_> = leaves
            .iter()
            .filter_map(|leaf| self.tabs.get(*leaf)?.pane_id())
            .collect();
        if panes != offer.panes {
            self.cancel_transfer(id, ctx);
            return;
        }
        if let Some(reason) = self.tab_move_blocker(&leaves) {
            self.cancel_transfer(id, ctx);
            self.window_error = Some(reason.into());
            return;
        }
        let Ok(pin) = self.registry.begin_transfer(&panes) else {
            self.cancel_transfer(id, ctx);
            return;
        };
        offer.pin.replace(Some(pin));
        for leaf in leaves {
            if let Some(AppTab {
                page: Page::Terminal(tab),
                ..
            }) = self.tabs.get_mut(leaf)
            {
                tab.end_pointer_gesture();
            }
        }
        self.cancel_frame_tasks();
        let Some(payload) = OfferedTab::take(&mut self.tabs, offer.group, index) else {
            self.cancel_transfer(id, ctx);
            return;
        };
        if payload.was_selected {
            self.keyboard.set_target(None);
        }
        offer.payload.replace(Some(payload));
        offer.phase.set(TransferPhase::Offered);
        self.mru_groups.retain(|g| *g != offer.group);
        self.sync_selection(ctx);
        self.activate(ctx);
        if !offer.send_target(Message::TransferClaim(id)) {
            self.cancel_transfer(id, ctx);
        }
    }

    pub(super) fn claim_group(&mut self, id: u64, ctx: &ComponentContext<Self>) {
        let Some(offer) = self
            .incoming
            .clone()
            .filter(|o| o.id() == id && o.phase.get() == TransferPhase::Offered)
        else {
            return;
        };
        let existing = offer.new_window.is_none();
        if existing && self.existing_destination_blocked() {
            offer.phase.set(TransferPhase::Canceled);
            offer.source.send(Message::TransferCanceled(id));
            self.cancel_transfer(id, ctx);
            return;
        }
        let Some(payload) = offer.payload.take() else {
            return;
        };
        let mut group = payload.tab;
        let leaves: Vec<_> = group.entries().iter().map(|t| t.id).collect();
        for leaf in leaves {
            if let Some(AppTab {
                page: Page::Terminal(live),
                ..
            }) = group.get_mut(leaf)
            {
                live.surface.window_hidden();
                live.surface = PaneSurface::default();
                live.last_size = Default::default();
                live.files.bind_window(ctx.sender());
                live.hover = Default::default();
                live.find.input = ElementRef::new();
                live.find.focused = false;
                live.find.epoch += 1;
                live.last_click = None;
            }
        }
        self.tabs
            .restore_group(group, self.tabs.headers().len(), true);
        self.activate(ctx);
        self.sync_selection(ctx);
        offer.phase.set(TransferPhase::Claimed);
        offer.pin.take();
        offer.source.send(Message::TransferDone(id));
        self.incoming = None;
        if existing && let Some(hwnd) = self.keyboard.hwnd() {
            unsafe {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindowAsync(hwnd, SW_RESTORE);
                }
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }

    pub(super) fn finish_transfer(&mut self, id: u64, ctx: &ComponentContext<Self>) {
        if self
            .outgoing
            .as_ref()
            .is_some_and(|o| o.id() == id && o.phase.get() == TransferPhase::Claimed)
        {
            self.outgoing = None;
            if self.tabs.entries().is_empty() {
                let _ = ctx.window().request_close();
            }
        }
    }

    pub(super) fn cancel_transfer(&mut self, id: u64, ctx: &ComponentContext<Self>) {
        if let Some(offer) = self.incoming.take_if(|o| o.id() == id) {
            if offer.new_window.is_some() && self.tabs.entries().is_empty() {
                let _ = ctx.window().request_close();
            }
            return;
        }
        let Some(offer) = self.outgoing.as_ref().filter(|o| o.id() == id).cloned() else {
            return;
        };
        if offer.phase.get() == TransferPhase::Claimed {
            self.finish_transfer(id, ctx);
            return;
        }
        offer.phase.set(TransferPhase::Canceled);
        self.outgoing = None;
        if let Some(payload) = offer.payload.take() {
            payload.return_to(&mut self.tabs);
            self.activate(ctx);
            self.sync_selection(ctx);
        }
        offer.pin.take();
        offer.cancel_target();
        self.window_error =
            Some("The tab could not move. It is still available in this window.".into());
    }
}

impl Drop for TerminalApp {
    fn drop(&mut self) {
        if WINDOWS
            .try_with(|windows| windows.borrow_mut().remove(&self.registry.window_id()))
            .is_ok_and(|window| window.is_some())
        {
            windows_changed();
        }
        if let Some(offer) = &self.incoming {
            offer.phase.set(TransferPhase::Canceled);
            offer.source.send(Message::TransferCanceled(offer.id()));
        }
        if let Some(offer) = &self.outgoing
            && offer.phase.get() != TransferPhase::Claimed
            && offer.payload.borrow().is_none()
        {
            offer.phase.set(TransferPhase::Canceled);
            offer.cancel_target();
        }
        let _ = self.registry.unregister();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_window_menu_routes_by_id_and_binds_the_selected_tab() {
        let label = "Window 27 · Work: pane · Window 12    F7";
        assert_eq!(menu_window(label), Some(27));
        assert_eq!(menu_window("Window 0 · Terminal"), None);
        assert_eq!(menu_window("Window 27"), None);
        assert!(matches!(
            bind_tab_action(tab_menu_message(label.into()), Some(42)),
            Message::MoveTabToExistingWindow(Some(42), 27)
        ));
    }

    #[test]
    fn rollback_keeps_a_newer_source_tab_and_recognizes_the_fallback_by_group() {
        let mut source = Tabs::default();
        let first = source.insert("first");
        let moving = source.insert("moving");
        let fallback = source.insert("fallback");
        let sibling = source.split(fallback, "second pane", Axis::Rows).unwrap();
        let group = source.group_id(moving).unwrap();
        source.select(moving);

        let offer = OfferedTab::take(&mut source, group, 1).unwrap();
        assert_eq!(source.active_id(), Some(sibling));
        source.select(first);
        offer.return_to(&mut source);
        assert_eq!(source.active_id(), Some(first));
        assert_eq!(source.headers()[1].id, moving);

        source.select(moving);
        let offer = OfferedTab::take(&mut source, group, 1).unwrap();
        source.select(fallback);
        offer.return_to(&mut source);
        assert_eq!(source.active_id(), Some(moving));
        assert_eq!(source.headers()[1].id, moving);

        source.select(first);
        let background = OfferedTab::take(&mut source, group, 1).unwrap();
        background.return_to(&mut source);
        assert_eq!(source.active_id(), Some(first));
    }

    #[test]
    fn transfer_deadline_covers_window_binding_offer_and_unclaimed_payload() {
        for phase in [
            TransferPhase::Opening,
            TransferPhase::Ready,
            TransferPhase::Offered,
        ] {
            assert_eq!(phase.on_tick(Duration::from_secs(9)), phase);
            assert_eq!(
                phase.on_tick(Duration::from_secs(10)),
                TransferPhase::Canceled
            );
        }
        assert_eq!(
            TransferPhase::Claimed.on_tick(Duration::from_secs(11)),
            TransferPhase::Claimed
        );
    }

    #[tokio::test]
    async fn ending_source_capture_keeps_selection_and_balances_reported_mouse_input() {
        use w11term::terminal::{TransportInput, create};
        let Page::Terminal(mut tab) = live_tab("pointer test".into(), Source::Demo, false).page
        else {
            unreachable!()
        };
        let point = Point { col: 1, row: 0 };
        let end = Point { col: 4, row: 0 };
        let mut selection = Selection::new(point, false, false);
        selection.update(end);
        tab.selection.replace(Some(selection));
        tab.scroll_direction = 2;
        tab.scroll_tick = true;
        tab.clicked_link = Some("example".into());

        let press = MouseEvent {
            kind: MouseEventKind::Press,
            button: MouseButton::Left,
            modifiers: KeyModifiers::NONE,
            x: 1,
            y: 3,
            x_pixel_offset: 0,
            y_pixel_offset: 1,
        };
        let motion = MouseEvent {
            kind: MouseEventKind::Move,
            x: 4,
            ..press
        };
        let release = MouseEvent {
            kind: MouseEventKind::Release,
            button: MouseButton::None,
            ..motion
        };
        let (wire, mut replies) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = create(TerminalSize::default(), wire);
        terminal.advance_bytes(b"\x1b[?1002h\x1b[?1006h");
        terminal
            .mouse_event(tab.remote_mouse.event(press).unwrap())
            .unwrap();
        terminal
            .mouse_event(tab.remote_mouse.event(motion).unwrap())
            .unwrap();

        // Intercept the production teardown command and feed it to the actual VT encoder.
        let (input, mut commands) = tokio::sync::mpsc::unbounded_channel();
        let worker = std::mem::replace(&mut tab.session.input, input);
        tab.end_pointer_gesture();
        tab.end_pointer_gesture();
        tab.session.input = worker;
        let Command::Mouse(up) = commands.try_recv().unwrap() else {
            panic!("expected a mouse release")
        };
        terminal.mouse_event(up).unwrap();
        assert!(commands.try_recv().is_err());
        assert_eq!(
            tab.remote_mouse.event(release),
            None,
            "late native release after handoff"
        );
        terminal
            .mouse_event(tab.remote_mouse.event(press).unwrap())
            .unwrap();
        terminal
            .mouse_event(tab.remote_mouse.event(release).unwrap())
            .unwrap();
        // Dropping the terminal closes its asynchronous writer after the last flush.
        drop(terminal);
        let bytes = tokio::time::timeout(Duration::from_secs(2), async {
            let mut bytes = Vec::new();
            while let Some(TransportInput::Data(part)) = replies.recv().await {
                bytes.extend(part);
            }
            bytes
        })
        .await
        .expect("VT reply writer did not drain");
        assert_eq!(
            bytes,
            b"\x1b[<0;2;4M\x1b[<32;5;4M\x1b[<0;5;4m\x1b[<0;2;4M\x1b[<0;5;4m"
        );
        let selected = tab.selection.borrow();
        let selected = selected.as_ref().unwrap();
        assert!(!selected.dragging && selected.active);
        assert_eq!((selected.anchor, selected.end), (point, end));
        assert_eq!(tab.scroll_direction, 0);
        assert!(!tab.scroll_tick && tab.clicked_link.is_none());
    }
}
