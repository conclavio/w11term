use super::*;
use std::cell::Ref;
use w11term::durable::{Backend, DiscoveredSession, DurableReference};
use w11term::ssh::{DurableAction, DurablePhase};

const PER_PAGE: usize = 32;

#[derive(Default)]
pub(super) struct Browser {
    query: String,
    pub(super) query_revision: u64,
    page: usize,
    input: ElementRef<TextBox>,
    cached: RefCell<Option<Matches>>,
}

struct Matches {
    listing: Arc<[DiscoveredSession]>,
    indices: Vec<usize>,
}

impl Browser {
    fn version(&self, listing: u64) -> ListVersion {
        ListVersion {
            listing,
            query: self.query_revision,
        }
    }

    fn set_query(&mut self, query: String) {
        if self.query != query {
            self.query = query;
            self.query_revision = self.query_revision.wrapping_add(1);
            self.page = 0;
            self.cached.get_mut().take();
        }
    }

    fn matches(&self, listing: &Arc<[DiscoveredSession]>) -> Ref<'_, Matches> {
        if !self
            .cached
            .borrow()
            .as_ref()
            .is_some_and(|m| Arc::ptr_eq(&m.listing, listing))
        {
            let needle = self.query.to_lowercase();
            let indices = listing
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    let found = needle.is_empty()
                        || [
                            entry.display_name.as_str(),
                            &entry.reference.name,
                            backend_name(entry.reference.backend),
                            &entry.command,
                            &entry.directory,
                        ]
                        .iter()
                        .any(|field| field.to_lowercase().contains(&needle));
                    found.then_some(index)
                })
                .collect();
            self.cached.replace(Some(Matches {
                listing: listing.clone(),
                indices,
            }));
        }
        Ref::map(self.cached.borrow(), |cached| cached.as_ref().unwrap())
    }

    fn unique(&self, listing: &Arc<[DiscoveredSession]>) -> Option<DurableReference> {
        if self.query.trim().is_empty() {
            return None;
        }
        let matches = self.matches(listing);
        let [index] = matches.indices.as_slice() else {
            return None;
        };
        Some(listing[*index].reference.clone())
    }
}

fn backend_name(backend: Backend) -> &'static str {
    match backend {
        Backend::Zmx => "zmx",
        Backend::Tmux => "tmux",
        Backend::Herdr => "Herdr",
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListVersion {
    listing: u64,
    query: u64,
}

pub enum Action {
    Discover { name: String, herdr: bool },
    Query(TabId, String),
    Focus(TabId, u8),
    Page(TabId, ListVersion, usize),
    Pick(TabId, ListVersion, DurableAction),
}

pub(super) fn discovery_tab(
    name: String,
    mut options: SshOptions,
    osc52: bool,
    saved: Option<DurableReference>,
    herdr: bool,
) -> AppTab {
    options.durable = true;
    let size = TerminalSize::default();
    let source = Source::Ssh(options.clone());
    let session = match saved {
        Some(reference) => {
            Session::start_durable(source.clone(), size, osc52, None, Some(reference))
        }
        None if herdr => Session::discover_herdr(options, size, osc52),
        None => Session::discover_durable(options, size, osc52),
    };
    let mut tab = live_tab_for_session(name.clone(), source, session).with_profile(Some(name));
    if let Page::Terminal(live) = &mut tab.page {
        live.discovery = Some(Browser::default());
    }
    tab
}

pub(super) fn discovery_view(
    id: TabId,
    tab: &LiveTab,
    active: bool,
    keyboard: &KeyboardBridge,
    context: &ViewContext<TerminalApp>,
) -> Option<View> {
    let browser = tab.discovery.as_ref()?;
    let state = tab.session.durable_state()?;
    if !state.phase.is_browser() {
        return None;
    }
    let version = browser.version(state.listing_revision);
    let matches = browser.matches(&state.candidates);
    let count = matches.indices.len();
    let mut rows = vec![
        KeyedView::new(
            "heading",
            TextBlock::new().font_size(24.0).text(format!(
                "{} on {}",
                if state.herdr_browser {
                    "Herdr terminals"
                } else {
                    "Remote sessions"
                },
                source_label(&tab.source)
            )),
        ),
        KeyedView::new(
            "status",
            TextBlock::new()
                .text(&state.detail)
                .text_wrapping(TextWrapping::Wrap),
        ),
        KeyedView::new(
            "search",
            TextBox::new()
                .element_ref(&browser.input)
                .automation_name("Search remote sessions")
                .automation_id(keyboard.session_search_tag(id))
                .placeholder_text("Search remote sessions")
                .is_enabled(active)
                .text(&browser.query)
                .on_text_changed(
                    context.callback(move |query| Message::Discovery(Action::Query(id, query))),
                ),
        ),
        KeyedView::new(
            "refresh",
            Button::new()
                .is_enabled(active && state.phase == DurablePhase::Choosing)
                .on_click(context.callback(move |_| {
                    Message::Discovery(Action::Pick(id, version, DurableAction::Refresh))
                }))
                .content("Refresh"),
        ),
    ];
    if state.phase == DurablePhase::Choosing {
        let summary = match (browser.query.is_empty(), count) {
            (false, 0) => "No matching sessions".into(),
            (false, 1) => "1 match".into(),
            (false, n) => format!("{n} matches"),
            (true, 1) => "1 session".into(),
            (true, n) => format!("{n} sessions"),
        };
        rows.push(KeyedView::new(
            "count",
            TextBlock::new()
                .automation_id("session-search-count")
                .text(summary),
        ));
    }
    let last = count.saturating_sub(1) / PER_PAGE;
    let page = browser.page.min(last);
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
                    .is_enabled(active && enabled)
                    .on_click(
                        context.callback(move |_| {
                            Message::Discovery(Action::Page(id, version, target))
                        }),
                    )
                    .content(label),
            ));
        }
    }
    for &index in matches.indices.iter().skip(page * PER_PAGE).take(PER_PAGE) {
        let entry = &state.candidates[index];
        let reference = entry.reference.clone();
        let count = entry.clients;
        let clients = if count == 1 {
            "1 client".into()
        } else {
            format!("{count} clients")
        };
        let backend = backend_name(reference.backend);
        let name = if reference.backend == Backend::Tmux && entry.display_name != reference.name {
            format!("{} ({})", entry.display_name, reference.name)
        } else {
            entry.display_name.clone()
        };
        let windows = match entry.windows {
            Some(1) => "1 window".into(),
            Some(n) => format!("{n} windows"),
            None => String::new(),
        };
        let label = [
            name.as_str(),
            backend,
            &entry.command,
            &entry.directory,
            &windows,
            if reference.backend == Backend::Herdr {
                ""
            } else {
                &clients
            },
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
        rows.push(KeyedView::new(
            format!("{backend}-{}-{}", reference.name, reference.generation),
            Button::new()
                .is_enabled(active && state.phase == DurablePhase::Choosing)
                .automation_name(format!("Attach to {label}"))
                .on_click(context.callback(move |_| {
                    Message::Discovery(Action::Pick(
                        id,
                        version,
                        DurableAction::Choose(reference.clone()),
                    ))
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
                let command = if std::env::var("W11TERM_DISCOVERY_SMOKE").as_deref() == Ok("herdr")
                {
                    "Browse Herdr terminals on fixture"
                } else {
                    "Find sessions on fixture"
                };
                self.update(tab_menu_message(command.into()), context);
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
                let version = tab
                    .discovery
                    .as_ref()
                    .unwrap()
                    .version(state.listing_revision);
                self.update(
                    Message::Discovery(Action::Pick(id, version, action)),
                    context,
                );
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
            .catalog
            .profile_order
            .iter()
            .filter(|name| {
                self.catalog
                    .profiles
                    .get(*name)
                    .is_some_and(|options| !options.player.rustmush)
            })
            .flat_map(|name| {
                [
                    MenuItem::item(
                        format!("discover-{name}"),
                        format!("Find sessions on {name}"),
                    ),
                    MenuItem::item(
                        format!("herdr-{name}"),
                        format!("Browse Herdr terminals on {name}"),
                    ),
                ]
            })
            .collect();
        if hosts.is_empty() {
            MenuItem::disabled("discover", "Find remote sessions (no SSH shell profiles)")
        } else {
            MenuItem::submenu("discover", "Find remote sessions", hosts)
        }
    }
    pub(super) fn discovery_action(&mut self, action: Action, context: &ComponentContext<Self>) {
        let id = match &action {
            Action::Discover { name, herdr } => {
                if let Some(options) = self
                    .catalog
                    .profiles
                    .get(name)
                    .filter(|o| !o.player.rustmush)
                {
                    let osc52 = self.appearance.read().unwrap().interaction.osc52;
                    self.insert_tab(
                        discovery_tab(name.clone(), options.clone(), osc52, None, *herdr),
                        context,
                    );
                }
                return;
            }
            Action::Page(id, ..)
            | Action::Pick(id, ..)
            | Action::Query(id, _)
            | Action::Focus(id, _) => *id,
        };
        if !self.discovery_input_available(id) {
            return;
        }
        let Some(AppTab {
            page: Page::Terminal(live),
            ..
        }) = self.tabs.get_mut(id)
        else {
            return;
        };
        let Some(browser) = &mut live.discovery else {
            return;
        };
        let Some(state) = live.session.durable_state() else {
            return;
        };
        match action {
            Action::Query(_, text) => {
                browser.set_query(text);
                self.connection_ui(id);
            }
            Action::Focus(_, attempt) => {
                let sender = context.sender();
                let queued = browser.input.request_focus_result(move |result| {
                    if !result.unwrap_or(false) && attempt < 4 {
                        sender.send(Message::Discovery(Action::Focus(id, attempt + 1)));
                    }
                });
                if !queued && attempt < 4 {
                    context.spawn_background(move |_| {
                        std::thread::sleep(Duration::from_millis(20));
                        Message::Discovery(Action::Focus(id, attempt + 1))
                    });
                }
            }
            Action::Page(_, version, page) => {
                if version == browser.version(state.listing_revision) {
                    browser.page = page;
                }
            }
            Action::Pick(_, version, action) => {
                if version == browser.version(state.listing_revision) {
                    let _ = live
                        .session
                        .durable_action_for_listing(version.listing, action);
                }
            }
            Action::Discover { .. } => unreachable!(),
        }
    }

    fn discovery_input_available(&self, id: TabId) -> bool {
        self.tabs.active_id() == Some(id)
            && self.picker.is_none()
            && self.switcher.is_none()
            && self.title_editor.is_none()
            && !self.clipboard.is_open()
            && matches!(self.tabs.get(id).map(|t| &t.page), Some(Page::Terminal(live))
                if live.browsing_sessions() && live.session.frames.load().prompt.is_none() && live.session.prompts.current().is_none())
    }

    pub(super) fn submit_session_search(&self, id: TabId, listing: u64, query: u64) {
        if !self.discovery_input_available(id) {
            return;
        }
        let Some(AppTab {
            page: Page::Terminal(live),
            ..
        }) = self.tabs.get(id)
        else {
            return;
        };
        let Some(browser) = &live.discovery else {
            return;
        };
        if let Some(state) = live.session.durable_state()
            && state.phase == DurablePhase::Choosing
            && browser.version(state.listing_revision) == (ListVersion { listing, query })
            && let Some(reference) = browser.unique(&state.candidates)
        {
            let _ = live
                .session
                .durable_action_for_listing(listing, DurableAction::Choose(reference));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        backend: Backend,
        id: &str,
        name: &str,
        directory: &str,
        command: &str,
    ) -> DiscoveredSession {
        DiscoveredSession {
            reference: DurableReference {
                backend,
                host: "fixture.invalid".into(),
                host_key_alias: None,
                port: 22,
                user: "tester".into(),
                host_key: "test-key".into(),
                name: id.into(),
                generation: "session-generation".into(),
            },
            display_name: name.into(),
            windows: None,
            clients: 1,
            directory: directory.into(),
            command: command.into(),
        }
    }

    #[test]
    fn session_search_filters_all_pages_and_fields_without_treating_names_as_patterns() {
        let mut entries: Vec<_> = (0..PER_PAGE)
            .map(|n| candidate(Backend::Tmux, &format!("${n}"), "running", "", ""))
            .collect();
        entries.push(candidate(
            Backend::Zmx,
            "remote-777",
            "Night Shift",
            "/tmp/ÉTÉ[1]",
            "Cargo WATCH",
        ));
        entries.push(candidate(Backend::Herdr, "t17", "Build (t17)", "", ""));
        let listing: Arc<[_]> = entries.into();
        let mut browser = Browser::default();
        assert_eq!(browser.matches(&listing).indices.len(), PER_PAGE + 2);
        browser.page = 1;
        for query in [
            "nIgHt ShIfT",
            "remote-777",
            "zMx",
            "cargo watch",
            "/TMP/été[1]",
        ] {
            browser.set_query(query.into());
            assert_eq!(browser.page, 0);
            assert_eq!(
                browser.unique(&listing),
                Some(listing[PER_PAGE].reference.clone())
            );
        }
        browser.set_query("herdr".into());
        assert_eq!(browser.matches(&listing).indices, [PER_PAGE + 1]);
        browser.set_query(".*".into());
        assert!(browser.matches(&listing).indices.is_empty());
        assert!(browser.unique(&listing).is_none());
    }

    #[test]
    fn session_search_refresh_keeps_query_and_enter_requires_one_verified_nonempty_match() {
        let first = candidate(Backend::Tmux, "$12", "ops", "", "");
        let second = candidate(Backend::Tmux, "$23", "ops", "", "");
        let both: Arc<[_]> = vec![first.clone(), second.clone()].into();
        let refreshed: Arc<[_]> = vec![second.clone()].into();
        let mut browser = Browser::default();
        assert!(browser.unique(&refreshed).is_none());
        browser.set_query("ops".into());
        let offered = browser.version(1);
        assert!(browser.unique(&both).is_none());
        assert_eq!(browser.unique(&refreshed), Some(second.reference.clone()));
        assert_ne!(
            offered,
            browser.version(2),
            "refresh invalidates the offered row"
        );
        browser.set_query("$12".into());
        assert_ne!(
            offered,
            browser.version(1),
            "editing invalidates the old query"
        );
        assert_eq!(browser.unique(&both), Some(first.reference));
        assert!(browser.unique(&refreshed).is_none());
        browser.set_query(" ".into());
        assert!(browser.unique(&both).is_none());
    }
}
