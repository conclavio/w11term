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
    bound: Cell<bool>,
    claimed: Cell<bool>,
    canceled: Cell<bool>,
    started: Instant,
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
                        offer.bound.set(true);
                        if offer.canceled.get()
                            || !offer.source.send(Message::TransferReady(offer.id()))
                        {
                            let _ = ctx.window().request_close();
                        }
                    } else if !self.window_restored {
                        self.restore_window();
                    }
                }
                Err(error) => self.window_error = Some(format!("Registering window: {error}")),
            }
        }
        if let Some(offer) = self.outgoing.clone() {
            if offer.claimed.get() {
                self.finish_transfer(offer.id(), ctx);
            } else if offer.canceled.get()
                || (!offer.bound.get() && offer.started.elapsed() > Duration::from_secs(10))
            {
                self.cancel_transfer(offer.id(), ctx);
            } else if offer.bound.get() && offer.payload.borrow().is_none() {
                self.offer_group(offer.id(), ctx);
            }
        }
        if self.native_bound && self.outgoing.is_none() {
            self.window_timer = None;
        } else {
            self.arm_window_tick(ctx);
        }
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
        let Some(panes) = layout
            .leaves()
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
            bound: Default::default(),
            claimed: Default::default(),
            canceled: Default::default(),
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
        if offer.claimed.get()
            || offer.canceled.get()
            || offer.payload.borrow().is_some()
            || !offer.bound.get()
        {
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
        if leaves.iter().any(|leaf| {
            matches!(self.tabs.get(*leaf).map(|t| &t.page), Some(Page::Terminal(t))
            if t.files.awaiting_native_result() || t.device_busy || t.prompt_id.is_some())
        }) {
            return; // A dialog or its background result still belongs to this HWND.
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
        self.cancel_frame_tasks();
        offer.payload.replace(self.tabs.take_group(offer.group));
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
            .filter(|o| o.id() == id && !o.canceled.get())
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
                live.clicked_link = None;
                live.remote_button = MouseButton::None;
                live.last_click = None;
                live.scroll_direction = 0;
                live.scroll_tick = false;
                live.gesture += 1;
            }
        }
        self.tabs
            .restore_group(group, self.tabs.headers().len(), true);
        self.activate(ctx);
        self.sync_selection(ctx);
        offer.claimed.set(true);
        offer.pin.take();
        offer.source.send(Message::TransferDone(id));
        self.incoming = None;
    }

    pub(super) fn finish_transfer(&mut self, id: u64, ctx: &ComponentContext<Self>) {
        if self
            .outgoing
            .as_ref()
            .is_some_and(|o| o.id() == id && o.claimed.get())
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
        if offer.claimed.get() {
            self.finish_transfer(id, ctx);
            return;
        }
        offer.canceled.set(true);
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
            offer.canceled.set(true);
            offer.source.send(Message::TransferCanceled(offer.id()));
        }
        if let Some(offer) = &self.outgoing
            && !offer.claimed.get()
            && offer.payload.borrow().is_none()
        {
            offer.canceled.set(true);
            offer.send_target(Message::TransferCanceled(offer.id()));
        }
        let _ = self.registry.unregister();
    }
}
