use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use w11term::control::PaneTransfer;
use w11term::options::Options;

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
    pub(super) registry: Registry,
    pub(super) server: Arc<Server>,
    group: TabId,
    panes: Vec<u64>,
    source: LocalSender<Message>,
    pub(super) target: RefCell<Option<LocalSender<Message>>>,
    payload: RefCell<Option<Tabs<AppTab>>>,
    pin: RefCell<Option<PaneTransfer>>,
    index: Cell<usize>,
    selected: Cell<bool>,
    phase: Cell<TransferPhase>,
    started: Instant,
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
        self.registry.window_id()
    }

    fn send_target(&self, message: Message) -> bool {
        self.target
            .borrow()
            .as_ref()
            .is_some_and(|sender| sender.send(message))
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
        if self.native_bound && self.outgoing.is_none() {
            self.window_timer = None;
        } else {
            self.arm_window_tick(ctx);
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

    pub(super) fn move_tab_window(&mut self, ctx: &ComponentContext<Self>) {
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
        static NEXT: AtomicU64 = AtomicU64::new(2);
        let registry = match self
            .registry
            .register_window(NEXT.fetch_add(1, Ordering::Relaxed), None)
        {
            Ok(registry) => registry,
            Err(error) => {
                self.window_error = Some(format!("Opening another window: {error}"));
                return;
            }
        };
        let transfer = Rc::new(Transfer {
            registry,
            server: self._control.clone(),
            group,
            panes,
            source: ctx.sender(),
            target: Default::default(),
            payload: Default::default(),
            pin: Default::default(),
            index: Default::default(),
            selected: Default::default(),
            phase: Default::default(),
            started: Instant::now(),
        });
        self.window_error = None;
        self.outgoing = Some(transfer.clone());
        let launch = WindowLaunch {
            options: self.window_launch.clone(),
            opening: None,
            transfer: Some(transfer.clone()),
        };
        if ctx.open_window(View::component::<Self>(launch)) {
            self.arm_window_tick(ctx);
        } else {
            self.cancel_transfer(transfer.id(), ctx);
        }
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
        offer.index.set(index);
        offer.selected.set(self.tabs.active_index() == Some(index));
        if offer.selected.get() {
            self.keyboard.set_target(None);
        }
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
        offer.payload.replace(self.tabs.take_group(offer.group));
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
        let Some(mut group) = offer.payload.take() else {
            return;
        };
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
        if self.incoming.as_ref().is_some_and(|o| o.id() == id) {
            self.incoming = None;
            if self.tabs.entries().is_empty() {
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
        if let Some(group) = offer.payload.take() {
            self.tabs
                .restore_group(group, offer.index.get(), offer.selected.get());
            self.activate(ctx);
            self.sync_selection(ctx);
        }
        offer.pin.take();
        if !offer.send_target(Message::TransferCanceled(id)) {
            let _ = offer.registry.unregister();
        }
        self.window_error =
            Some("The tab could not move. It is still available in this window.".into());
    }
}

impl Drop for TerminalApp {
    fn drop(&mut self) {
        if let Some(offer) = &self.incoming {
            offer.phase.set(TransferPhase::Canceled);
            offer.source.send(Message::TransferCanceled(offer.id()));
        }
        if let Some(offer) = &self.outgoing
            && offer.phase.get() != TransferPhase::Claimed
            && offer.payload.borrow().is_none()
        {
            offer.phase.set(TransferPhase::Canceled);
            if !offer.send_target(Message::TransferCanceled(offer.id())) {
                let _ = offer.registry.unregister();
            }
        }
        let _ = self.registry.unregister();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
