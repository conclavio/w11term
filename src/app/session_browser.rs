use super::*;
use w11term::durable::DurableReference;
use w11term::ssh::{DurableAction, DurablePhase};

pub enum Action {
    Discover(String),
    Page(TabId, usize),
    Pick(TabId, DurableAction),
}

pub(super) fn discovery_tab(
    name: String,
    mut options: SshOptions,
    osc52: bool,
    saved: Option<DurableReference>,
) -> AppTab {
    options.durable = true;
    let size = TerminalSize::default();
    let source = Source::Ssh(options.clone());
    let session = match saved {
        Some(reference) => {
            Session::start_durable(source.clone(), size, osc52, None, Some(reference))
        }
        None => Session::discover_durable(options, size, osc52),
    };
    let mut tab = live_tab_for_session(name.clone(), source, session).with_profile(Some(name));
    if let Page::Terminal(live) = &mut tab.page {
        live.discovery_page = Some(0);
    }
    tab
}

pub(super) fn discovery_view(
    id: TabId,
    tab: &LiveTab,
    context: &ViewContext<TerminalApp>,
) -> Option<View> {
    let page = tab.discovery_page?;
    let state = tab.session.durable_state()?;
    if !state.phase.is_browser() {
        return None;
    }
    let mut rows = vec![
        KeyedView::new(
            "heading",
            TextBlock::new()
                .font_size(24.0)
                .text(format!("Remote sessions on {}", source_label(&tab.source))),
        ),
        KeyedView::new(
            "status",
            TextBlock::new()
                .text(&state.detail)
                .text_wrapping(TextWrapping::Wrap),
        ),
        KeyedView::new(
            "refresh",
            Button::new()
                .is_enabled(state.phase == DurablePhase::Choosing)
                .on_click(context.callback(move |_| {
                    Message::Discovery(Action::Pick(id, DurableAction::Refresh))
                }))
                .content("Refresh"),
        ),
    ];
    // Keep the WinUI tree bounded even for a busy multi-session account.
    const PER_PAGE: usize = 32;
    let last = state.candidates.len().saturating_sub(1) / PER_PAGE;
    let page = page.min(last);
    if last > 0 {
        for (key, label, target, enabled) in [
            (
                "previous",
                "Previous sessions",
                page.saturating_sub(1),
                page > 0,
            ),
            ("next", "Next sessions", page + 1, page < last),
        ] {
            rows.push(KeyedView::new(
                key,
                Button::new()
                    .is_enabled(enabled)
                    .on_click(
                        context.callback(move |_| Message::Discovery(Action::Page(id, target))),
                    )
                    .content(label),
            ));
        }
    }
    for entry in state.candidates.iter().skip(page * PER_PAGE).take(PER_PAGE) {
        let reference = entry.reference.clone();
        let name = &reference.name;
        let count = entry.clients;
        let clients = if count == 1 {
            "1 client".into()
        } else {
            format!("{count} clients")
        };
        let label = [name.as_str(), &entry.command, &entry.directory, &clients]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        rows.push(KeyedView::new(
            name.clone(),
            Button::new()
                .automation_name(format!("Attach to {label}"))
                .on_click(context.callback(move |_| {
                    Message::Discovery(Action::Pick(id, DurableAction::Choose(reference.clone())))
                }))
                .content(
                    TextBlock::new()
                        .text(label)
                        .text_wrapping(TextWrapping::Wrap),
                ),
        ));
    }
    Some(
        ScrollViewer::new().content(
            StackPanel::new()
                .margin(24.0)
                .spacing(12.0)
                .keyed_children(rows),
        ),
    )
}

impl TerminalApp {
    #[cfg(feature = "ui-smoke")]
    pub(super) fn discovery_smoke_step(&mut self, step: u8, context: &ComponentContext<Self>) {
        let id = self.tabs.active_id().unwrap();
        let Page::Terminal(tab) = &self.tabs.get(id).unwrap().page else {
            panic!("expected a terminal")
        };
        let state = tab.session.durable_state();
        let next = match (step, state.as_ref().map(|s| s.phase)) {
            (0, None) => {
                self.update(tab_menu_message("Find sessions on fixture".into()), context);
                assert_ne!(self.tabs.active_id(), Some(id));
                1
            }
            (1, Some(DurablePhase::Choosing)) => {
                self.split_pane(Axis::Columns, Some("fixture".into()), context);
                assert_ne!(self.tabs.active_id(), Some(id));
                2
            }
            (2, None) if self.focused_pane_rect().is_some() => {
                self.update(Message::PaneNeighbor(Direction::Left), context);
                3
            }
            (3, Some(DurablePhase::Choosing)) => {
                let state = state.unwrap();
                assert!(state.reference.is_none());
                assert_eq!(state.candidates.len(), 1, "{}", state.detail);
                let action = DurableAction::Choose(state.candidates[0].reference.clone());
                self.update(Message::Discovery(Action::Pick(id, action)), context);
                4
            }
            (4, Some(DurablePhase::Attached))
                if tab
                    .session
                    .frames
                    .load()
                    .rows
                    .iter()
                    .any(|row| row.as_str().contains("restore-screen")) =>
            {
                assert_eq!(tab.profile_name.as_deref(), Some("fixture"));
                eprintln!("Host discovery smoke passed");
                self.update(Message::CloseWindow, context);
                return;
            }
            (_, Some(DurablePhase::Failed | DurablePhase::Unavailable | DurablePhase::Missing)) => {
                panic!("Discovery smoke failed: {}", state.unwrap().detail)
            }
            _ => step,
        };
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(80));
            Message::Smoke(next)
        });
    }
    pub(super) fn discovery_menu(&self) -> MenuItem {
        let hosts: Vec<_> = self
            .profiles
            .iter()
            .filter(|(_, options)| !options.player.rustmush)
            .map(|(name, _)| {
                MenuItem::item(
                    format!("discover-{name}"),
                    format!("Find sessions on {name}"),
                )
            })
            .collect();
        if hosts.is_empty() {
            MenuItem::disabled("discover", "Find remote sessions (no SSH shell profiles)")
        } else {
            MenuItem::submenu("discover", "Find remote sessions", hosts)
        }
    }
    pub(super) fn discovery_action(&mut self, action: Action, context: &ComponentContext<Self>) {
        match action {
            Action::Page(id, page) => {
                if let Some(AppTab {
                    page: Page::Terminal(live),
                    ..
                }) = self.tabs.get_mut(id)
                    && let Some(current) = live.discovery_page.as_mut()
                {
                    *current = page;
                }
            }
            Action::Discover(name) => {
                if let Some(options) = self
                    .profiles
                    .get(&name)
                    .filter(|options| !options.player.rustmush)
                {
                    let osc52 = self.appearance.read().unwrap().interaction.osc52;
                    self.tabs
                        .insert(discovery_tab(name, options.clone(), osc52, None));
                    self.sync_selection(context);
                    self.activate(context);
                }
            }
            Action::Pick(id, choice) => {
                if let Some(AppTab {
                    page: Page::Terminal(live),
                    ..
                }) = self.tabs.get(id)
                    && live.session.durable_action(choice).is_ok()
                {
                    self.tabs.select(id);
                    self.sync_selection(context);
                    self.activate(context);
                }
            }
        }
    }
}
