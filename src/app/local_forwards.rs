use super::*;
use std::num::NonZeroU16;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::watch;
use w11term::options::LocalForwardPreset;
use w11term::ssh::{LocalForward, LocalForwardStatus};

pub(super) mod overview;

pub(super) const LABEL: &str = "Forward a local port…";
const TITLE: &str = "Local port forwards";
const PRESET_BUTTON: &str = "Use saved forward…";

pub(super) fn preset_label(preset: LocalForwardPreset) -> String {
    format!(
        "Windows {} → remote {}",
        preset.local_port, preset.remote_port
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Origin {
    pane: u64,
    generation: u64,
}

impl Origin {
    fn from_tab(tab: &LiveTab) -> Option<Self> {
        let session = &tab.session;
        let generation = session.frames.load().connection_generation;
        session.ssh_connection_at(generation).map(|_| Self {
            pane: session.pane_id,
            generation,
        })
    }
}

struct Route {
    id: u64,
    generation: u64,
    handle: LocalForward,
    stopping: bool,
}

impl Route {
    fn state_label(&self, status: &LocalForwardStatus) -> String {
        if self.stopping {
            "Stopping…".into()
        } else if status.running {
            let count = status.active_clients;
            format!(
                "Listening · {count} active connection{}",
                if count == 1 { "" } else { "s" }
            )
        } else {
            "Stopped".into()
        }
    }

    fn copy_address(&self) -> Option<String> {
        if self.stopping || !self.handle.status().running {
            return None;
        }
        let address = windows_address(self.handle.local_port());
        Some(match w11term::clipboard::write(&address) {
            Ok(()) => format!("Copied Windows address {address}."),
            Err(error) => format!("Could not copy address · {error:#}"),
        })
    }

    fn stop(&mut self) {
        self.stopping = true;
        self.handle.stop();
    }
}

// Owned by the terminal pane. A manager can be opened and closed independently.
pub(super) struct Routes {
    entries: Vec<Route>,
    connection: watch::Receiver<()>,
}

impl Routes {
    pub(super) fn new(session: &Session) -> Self {
        Self {
            entries: Vec::new(),
            connection: session.ssh_connection_changes(),
        }
    }

    pub(super) fn connection_changed(&mut self) -> bool {
        if matches!(self.connection.has_changed(), Ok(true)) {
            self.connection.borrow_and_update();
            return true;
        }
        false
    }
}

#[derive(Default)]
struct Draft {
    remote: String,
    local: String,
}

pub(super) fn parse_port(value: &str) -> Option<NonZeroU16> {
    let value = value.trim();
    if !value.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

impl Draft {
    fn ports(&self) -> Result<(u16, u16), &'static str> {
        let remote =
            parse_port(&self.remote).ok_or("Enter a remote TCP port between 1 and 65535.")?;
        let local = if self.local.trim().is_empty() {
            0
        } else {
            parse_port(&self.local)
                .ok_or("Enter a Windows TCP port between 1 and 65535, or leave it blank.")?
                .get()
        };
        Ok((local, remote.get()))
    }
}

pub(super) struct Panel {
    origin: Origin,
    endpoint: String,
    presets: Vec<LocalForwardPreset>,
    preset_button: RefCell<Option<View>>,
    draft: Draft,
    feedback: String,
}

impl Panel {
    fn new(origin: Origin, connection: &SshOptions, launch: &Source) -> Self {
        // A different profile may have opened the shared SSH connection.
        let presets = match launch {
            Source::Ssh(options) => options.local_forwards.clone(),
            _ => Vec::new(),
        };
        Self {
            origin,
            endpoint: format!(
                "{}@{}:{}",
                connection.user, connection.host, connection.port
            ),
            presets,
            preset_button: RefCell::default(),
            draft: Draft::default(),
            feedback: String::new(),
        }
    }

    fn choose_preset(&mut self, preset: LocalForwardPreset) {
        if self.presets.contains(&preset) {
            self.draft.local = preset.local_port.to_string();
            self.draft.remote = preset.remote_port.to_string();
            self.feedback.clear();
        }
    }

    fn preset_picker(&self, id: TabId, ctx: &ViewContext<TerminalApp>) -> Option<View> {
        if self.presets.is_empty() {
            return None;
        }
        // Keep the native flyout open across listener status updates in this manager.
        let mut button = self.preset_button.borrow_mut();
        Some(
            button
                .get_or_insert_with(|| {
                    let origin = self.origin;
                    let choices: Vec<_> =
                        self.presets.iter().map(|&p| (preset_label(p), p)).collect();
                    let items = choices
                        .iter()
                        .enumerate()
                        .map(|(index, (label, _))| {
                            MenuItem::item(format!("preset-{index}"), label.clone())
                        })
                        .collect::<Vec<_>>();
                    Button::new()
                        .automation_name(PRESET_BUTTON)
                        .content(PRESET_BUTTON)
                        .menu(Menu::new(
                            items,
                            ctx.callback(move |label: String| {
                                choices
                                    .iter()
                                    .find(|(name, _)| *name == label)
                                    .map_or(Message::ControlIdle, |(_, p)| {
                                        edit(id, origin, Edit::Preset(*p))
                                    })
                            }),
                        ))
                })
                .clone(),
        )
    }
}

pub enum Action {
    Open(Origin),
    Panel(TabId, Origin, Edit),
    OpenOverview,
    Overview(TabId, overview::Edit),
}

pub enum Edit {
    Remote(String),
    Local(String),
    Preset(LocalForwardPreset),
    Start { local: u16, remote: u16 },
    Stop(u64),
    Copy(u64),
    Refresh,
    Back,
}

pub(super) fn open(origin: Option<Origin>) -> Message {
    origin.map_or(Message::ControlIdle, |origin| {
        Message::LocalForward(Action::Open(origin))
    })
}

fn edit(id: TabId, origin: Origin, edit: Edit) -> Message {
    Message::LocalForward(Action::Panel(id, origin, edit))
}

fn windows_address(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

fn draft_route(local: u16, remote: u16) -> String {
    let endpoint = if local == 0 {
        "127.0.0.1 (automatic port)".into()
    } else {
        windows_address(local)
    };
    format!("Windows {endpoint} → remote 127.0.0.1:{remote}")
}

// A receiver can close immediately after publishing its final status. Report that
// change once, so the view also sees the final state of a stopped listener.
fn changed<T>(watches: &mut Vec<watch::Receiver<T>>) -> bool {
    let mut changed = false;
    watches.retain_mut(|watch| match watch.has_changed() {
        Ok(false) => true,
        Ok(true) => {
            watch.borrow_and_update();
            changed = true;
            true
        }
        Err(_) => {
            changed = true;
            false
        }
    });
    changed
}

fn watch_updates(
    ctx: &ComponentContext<TerminalApp>,
    mut connections: Vec<watch::Receiver<()>>,
    mut listeners: Vec<watch::Receiver<LocalForwardStatus>>,
    refresh: impl Fn() -> Message + 'static,
) -> Option<crate::ui_timer::RetryTimer> {
    if connections.is_empty() && listeners.is_empty() {
        return None;
    }
    let sender = ctx.sender();
    const INTERVAL: u32 = 250;
    crate::ui_timer::RetryTimer::with_next_delay(INTERVAL, move || {
        let connection_changed = changed(&mut connections);
        if changed(&mut listeners) || connection_changed {
            sender.send(refresh());
        }
        INTERVAL
    })
    .ok()
}

impl TerminalApp {
    pub(super) fn forward_origin(&self) -> Option<Origin> {
        let Page::Terminal(tab) = &self.tabs.active()?.page else {
            return None;
        };
        Origin::from_tab(tab)
    }

    fn forward_source(&self, origin: Origin) -> Option<TabId> {
        let id = self.tab_for_pane(origin.pane)?;
        let Page::Terminal(tab) = &self.tabs.get(id)?.page else {
            return None;
        };
        (Origin::from_tab(tab) == Some(origin)).then_some(id)
    }

    pub(super) fn watch_forward_panel(&mut self, ctx: &ComponentContext<Self>) {
        self.forward_watch.take();
        let Some(id) = self.tabs.active_id() else {
            return;
        };
        if matches!(
            self.tabs.get(id).map(|tab| &tab.page),
            Some(Page::ForwardOverview(_))
        ) {
            self.watch_forward_overview(id, ctx);
            return;
        }
        let Some(AppTab {
            page: Page::LocalForwards(panel),
            ..
        }) = self.tabs.get(id)
        else {
            return;
        };
        let origin = panel.origin;
        let Some(source) = self.forward_source(origin) else {
            return;
        };
        let Page::Terminal(tab) = &self.tabs.get(source).unwrap().page else {
            return;
        };
        let connections = vec![tab.session.ssh_connection_changes()];
        let mut listeners = Vec::new();
        for route in &tab.forwards.entries {
            let watch = route.handle.subscribe();
            let running = watch.borrow().running;
            if running {
                listeners.push(watch);
            }
        }
        self.finish_stopped_forwards(id, source);
        self.forward_watch = watch_updates(ctx, connections, listeners, move || {
            edit(id, origin, Edit::Refresh)
        });
    }

    fn finish_stopped_forwards(&mut self, panel_id: TabId, source: TabId) {
        let Page::Terminal(tab) = &mut self.tabs.get_mut(source).unwrap().page else {
            return;
        };
        let mut stopped = None;
        tab.forwards.entries.retain(|route| {
            if route.stopping && !route.handle.status().running {
                stopped = Some(route.handle.local_port());
                return false;
            }
            true
        });
        if let Some(port) = stopped
            && let Page::LocalForwards(panel) = &mut self.tabs.get_mut(panel_id).unwrap().page
        {
            panel.feedback = format!("Stopped forwarding port {port}.");
        }
    }

    pub(super) fn forward_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        let (id, origin, action) = match action {
            Action::Open(origin) => {
                if self.forward_origin() != Some(origin) {
                    return;
                }
                let existing = self.tabs.entries().iter().find_map(|entry| {
                    matches!(&entry.value.page, Page::LocalForwards(p) if p.origin.pane == origin.pane)
                        .then_some(entry.id)
                });
                let active = self.tabs.active_id().unwrap();
                let Page::Terminal(tab) = &mut self.tabs.get_mut(active).unwrap().page else {
                    return;
                };
                let Some(link) = tab.session.ssh_connection_at(origin.generation) else {
                    return;
                };
                tab.forwards
                    .entries
                    .retain(|r| r.generation == origin.generation);
                let fresh = AppTab {
                    label: TITLE.into(),
                    page: Page::LocalForwards(Panel::new(origin, link.options(), &tab.source)),
                };
                if let Some(existing) = existing {
                    let page = self.tabs.get_mut(existing).unwrap();
                    if !matches!(&page.page, Page::LocalForwards(p) if p.origin == origin) {
                        *page = fresh;
                    }
                    self.tabs.select(existing);
                } else {
                    self.tabs.insert(fresh);
                }
                self.sync_selection(ctx);
                self.activate(ctx);
                return;
            }
            Action::Panel(id, origin, action) => (id, origin, action),
            Action::OpenOverview => return self.open_forward_overview(ctx),
            Action::Overview(id, action) => return self.edit_forward_overview(id, action, ctx),
        };
        if self.tabs.active_id() != Some(id) {
            return;
        }
        let Some(AppTab {
            page: Page::LocalForwards(panel),
            ..
        }) = self.tabs.get(id)
        else {
            return;
        };
        if panel.origin != origin {
            return;
        }
        if matches!(action, Edit::Back) {
            if let Some(source) = self.tab_for_pane(origin.pane) {
                self.tabs.select(source);
                self.sync_selection(ctx);
                self.activate(ctx);
            }
            return;
        }
        if matches!(action, Edit::Refresh) {
            if let Some(source) = self.forward_source(origin) {
                self.finish_stopped_forwards(id, source);
            } else {
                self.forward_watch.take();
            }
            return;
        }
        let Some(source) = self.forward_source(origin) else {
            return;
        };
        let Page::LocalForwards(panel) = &mut self.tabs.get_mut(id).unwrap().page else {
            return;
        };
        match action {
            Edit::Preset(preset) => {
                panel.choose_preset(preset);
                return;
            }
            Edit::Remote(text) => {
                panel.draft.remote = text;
                panel.feedback.clear();
                return;
            }
            Edit::Local(text) => {
                panel.draft.local = text;
                panel.feedback.clear();
                return;
            }
            Edit::Start { local, remote } if panel.draft.ports() != Ok((local, remote)) => {
                panel.feedback = "The ports changed. Check the route and start again.".into();
                return;
            }
            _ => {}
        }
        let Page::Terminal(tab) = &mut self.tabs.get_mut(source).unwrap().page else {
            return;
        };
        let feedback =
            match action {
                Edit::Start { local, remote } => {
                    match tab
                        .session
                        .start_local_forward(origin.generation, local, remote)
                    {
                        Ok(handle) => {
                            static NEXT_ROUTE: AtomicU64 = AtomicU64::new(1);
                            tab.forwards.entries.push(Route {
                                id: NEXT_ROUTE.fetch_add(1, Ordering::Relaxed),
                                generation: origin.generation,
                                handle,
                                stopping: false,
                            });
                            String::new()
                        }
                        Err(error) => format!("Could not start forwarding · {error:#}"),
                    }
                }
                Edit::Copy(route) | Edit::Stop(route) => {
                    let Some(route) = tab.forwards.entries.iter_mut().find(|r| {
                        r.id == route && r.generation == origin.generation && !r.stopping
                    }) else {
                        return;
                    };
                    if matches!(action, Edit::Copy(_)) {
                        let Some(feedback) = route.copy_address() else {
                            return;
                        };
                        feedback
                    } else {
                        route.stop();
                        format!("Stopping forwarding port {}…", route.handle.local_port())
                    }
                }
                _ => return,
            };
        let Page::LocalForwards(panel) = &mut self.tabs.get_mut(id).unwrap().page else {
            return;
        };
        panel.feedback = feedback;
        self.watch_forward_panel(ctx);
    }

    pub(super) fn forward_view(&self, id: TabId, panel: &Panel, ctx: &ViewContext<Self>) -> View {
        let origin = panel.origin;
        let source =
            self.forward_source(origin)
                .and_then(|source| match &self.tabs.get(source)?.page {
                    Page::Terminal(tab) => Some(tab),
                    _ => None,
                });
        let action = move |e| edit(id, origin, e);
        let mut items = vec![
            KeyedView::new("title", TextBlock::new().font_size(26.0).text(TITLE)),
            KeyedView::new("endpoint", TextBlock::new().text(&panel.endpoint)),
            KeyedView::new(
                "back",
                Button::new()
                    .is_enabled(self.tab_for_pane(origin.pane).is_some())
                    .on_click(ctx.callback(move |_| action(Edit::Back)))
                    .content("Back to terminal"),
            ),
        ];
        if let Some(tab) = source {
            let draft = &panel.draft;
            let validation = draft.ports();
            if let Some(picker) = panel.preset_picker(id, ctx) {
                items.push(KeyedView::new("presets", picker));
            }
            items.extend([
                KeyedView::new(
                    "remote-label",
                    TextBlock::new().text("Remote localhost port"),
                ),
                KeyedView::new(
                    "remote",
                    TextBox::new()
                        .automation_name("Remote localhost port")
                        .text(&draft.remote)
                        .on_text_changed(ctx.callback(move |s| action(Edit::Remote(s)))),
                ),
                KeyedView::new(
                    "local-label",
                    TextBlock::new().text("Windows local port (blank assigns an available port)"),
                ),
                KeyedView::new(
                    "local",
                    TextBox::new()
                        .automation_name("Windows local port")
                        .text(&draft.local)
                        .on_text_changed(ctx.callback(move |s| action(Edit::Local(s)))),
                ),
            ]);
            match validation {
                Ok((local, remote)) => items.push(KeyedView::new(
                    "preview",
                    TextBlock::new()
                        .text(draft_route(local, remote))
                        .text_wrapping(TextWrapping::Wrap),
                )),
                Err(error) if !draft.remote.is_empty() => {
                    items.push(KeyedView::new("validation", TextBlock::new().text(error)));
                }
                Err(_) => {}
            }
            items.push(KeyedView::new(
                "start",
                Button::new()
                    .style(ButtonStyle::Accent)
                    .is_enabled(validation.is_ok())
                    .on_click(ctx.callback(move |_| {
                        validation.map_or(Message::ControlIdle, |(local, remote)| {
                            action(Edit::Start { local, remote })
                        })
                    }))
                    .content("Start forwarding"),
            ));
            if !tab.forwards.entries.is_empty() {
                items.push(KeyedView::new(
                    "route-heading",
                    TextBlock::new().text("Windows address → remote localhost"),
                ));
            }
            for route in tab
                .forwards
                .entries
                .iter()
                .filter(|r| r.generation == origin.generation)
            {
                let key = route.id;
                let local = route.handle.local_port();
                let remote = route.handle.remote_port();
                let status = route.handle.status();
                let state = route.state_label(&status);
                let mut details = vec![
                    KeyedView::new(
                        "address",
                        TextBlock::new()
                            .text(format!("{} → 127.0.0.1:{remote}", windows_address(local))),
                    ),
                    KeyedView::new("status", TextBlock::new().text(state)),
                ];
                if let Some(reason) = status.last_error {
                    details.push(KeyedView::new(
                        "error",
                        TextBlock::new()
                            .text(reason)
                            .text_wrapping(TextWrapping::Wrap),
                    ));
                }
                details.push(KeyedView::new(
                    "actions",
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .automation_name(format!("Copy Windows address for port {local}"))
                                .is_enabled(status.running && !route.stopping)
                                .on_click(ctx.callback(move |_| action(Edit::Copy(key))))
                                .content("Copy address"),
                            Button::new()
                                .automation_name(format!("Stop forwarding port {local}"))
                                .is_enabled(!route.stopping)
                                .on_click(ctx.callback(move |_| action(Edit::Stop(key))))
                                .content(if status.running { "Stop" } else { "Remove" }),
                        )),
                ));
                items.push(KeyedView::new(
                    format!("route/{key}"),
                    StackPanel::new().spacing(4.0).keyed_children(details),
                ));
            }
            if !panel.feedback.is_empty() {
                items.push(KeyedView::new(
                    "feedback",
                    TextBlock::new()
                        .text(&panel.feedback)
                        .text_wrapping(TextWrapping::Wrap),
                ));
            }
        } else {
            items.push(KeyedView::new("expired", TextBlock::new()
                .text("This SSH connection is no longer available in this window. Open Forward a local port… from a connected SSH pane.")
                .text_wrapping(TextWrapping::Wrap)));
        }
        ScrollViewer::new().content(
            StackPanel::new()
                .margin(16.0)
                .spacing(8.0)
                .keyed_children(items),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choosing_a_saved_pair_only_fills_the_draft_and_an_unoffered_pair_cannot_overwrite_manual_edits()
     {
        let preset = |local: u16, remote: u16| LocalForwardPreset {
            local_port: local.try_into().unwrap(),
            remote_port: remote.try_into().unwrap(),
        };
        let first = preset(15432, 5432);
        let second = preset(15432, 8080);
        let mut connection: SshOptions =
            serde_json::from_value(serde_json::json!({"host":"host","user":"dev","auth":"none"}))
                .unwrap();
        let link_only = preset(6543, 443);
        connection.local_forwards.push(link_only);
        let launch = Source::Ssh(SshOptions {
            local_forwards: vec![first, second],
            ..connection.clone()
        });
        let mut panel = Panel::new(
            Origin {
                pane: 31,
                generation: 2,
            },
            &connection,
            &launch,
        );
        assert_eq!(panel.presets, [first, second]);
        assert!(panel.draft.ports().is_err());
        assert_eq!(preset_label(first), "Windows 15432 → remote 5432");
        panel.choose_preset(first);
        assert_eq!(panel.draft.ports(), Ok((15432, 5432)));
        panel.draft.local.clear();
        panel.draft.remote = "443".into();
        panel.choose_preset(link_only);
        assert_eq!(panel.draft.ports(), Ok((0, 443)));
        panel.choose_preset(second);
        assert_eq!(panel.draft.ports(), Ok((15432, 8080)));
    }

    #[test]
    fn blank_windows_port_requests_assignment_and_supplied_ports_are_decimal_tcp_ports() {
        let mut form = Draft {
            remote: " 65535 ".into(),
            local: String::new(),
        };
        assert_eq!(form.ports(), Ok((0, 65535)));
        form.local = " 00001 ".into();
        assert_eq!(form.ports(), Ok((1, 65535)));
        for bad in [
            "0",
            "65536",
            "-5",
            "+7",
            "0x50",
            "localhost:80",
            "８０",
            "8 0",
        ] {
            form.local = bad.into();
            assert!(form.ports().unwrap_err().contains("Windows"), "{bad}");
            form.local.clear();
            form.remote = bad.into();
            assert!(form.ports().unwrap_err().contains("remote"), "{bad}");
            form.remote = "42".into();
        }
        assert!(Draft::default().ports().is_err());
        assert_eq!(windows_address(50432), "127.0.0.1:50432");
    }

    #[test]
    fn watcher_updates_on_status_change_or_close_and_sleeps_between_changes() {
        let (first, rx1) = watch::channel(0);
        let (second, rx2) = watch::channel(4);
        let mut watches = vec![rx1, rx2];
        assert!(!changed(&mut watches));
        first.send_replace(1);
        first.send_replace(2);
        assert!(changed(&mut watches));
        assert_eq!(*watches[0].borrow(), 2);
        assert!(!changed(&mut watches));
        second.send_replace(0);
        drop(second);
        assert!(changed(&mut watches));
        assert_eq!(watches.len(), 1);
        assert!(!changed(&mut watches));
    }
}
