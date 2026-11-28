use super::*;
use command_palette::{CommandId, Row};
use w11term::saved_reference_pages::{Entry, Store};

pub struct Lookup {
    entries: Vec<Entry>,
    library: Option<Loaded>,
}

impl Lookup {
    pub(in crate::app) fn load() -> anyhow::Result<Self> {
        let entries = Store::user()?.list()?;
        let library = (!entries.is_empty()).then(catalog::load).transpose()?;
        Ok(Self { entries, library })
    }

    pub(in crate::app) fn rows(&self, query: &str) -> Vec<Row> {
        let rows = self.entries.iter().map(|entry| {
            let key = key(entry);
            let platform = key
                .as_ref()
                .map_or(entry.platform.as_str(), |key| key.platform.1);
            let page = key
                .as_ref()
                .and_then(|key| self.library.as_ref()?.catalog.page(key));
            let (label, chord) = match page {
                Some(page) if page.title.eq_ignore_ascii_case(&entry.name) => {
                    (page.title.clone(), platform.to_owned())
                }
                Some(page) => (page.title.clone(), format!("{platform} · {}", entry.name)),
                None => (entry.name.clone(), format!("{platform} · Unavailable")),
            };
            Row {
                id: CommandId::OpenSavedManual(entry.clone()),
                label,
                chord,
                enabled: page.is_some(),
            }
        });
        command_palette::filter(rows.collect(), query)
    }
}

impl TerminalApp {
    pub(in crate::app) fn open_saved_manual_from_palette(&mut self, lookup: Lookup, entry: Entry) {
        if self.command_reference.subpanel_open() {
            return;
        }
        let Some(key) = key(&entry) else { return };
        let Some(library) = lookup
            .library
            .filter(|library| library.catalog.page(&key).is_some())
        else {
            return;
        };
        let entries = match Store::user().and_then(|store| store.list()) {
            Ok(entries) if entries.contains(&entry) => entries,
            Ok(_) => {
                self.window_error =
                    Some("This saved manual was removed. Open the search again.".into());
                return;
            }
            Err(error) => {
                self.window_error = Some(format!("{error:#}"));
                return;
            }
        };
        let auto = self.reference_platform();
        self.set_diagnostics_drawer(None);
        let panel = &mut self.command_reference;
        panel.open = true;
        panel.library = Some(library);
        panel.error = None;
        panel.cached.replace(None);
        panel.saved_pages.close();
        if panel.saved_pages.pending.is_none() {
            panel.saved_pages.entries = Some(entries);
            panel.saved_pages.error = None;
        }
        panel.show_page(key, auto);
    }
}

pub enum Action {
    Expanded(bool),
    Open(Entry),
    Set(Entry, bool),
    Finished(Arc<()>, Result<Vec<Entry>, String>),
}

#[derive(Default)]
pub(super) struct State {
    expanded: bool,
    entries: Option<Vec<Entry>>,
    pending: Option<Arc<()>>,
    error: Option<String>,
}

fn message(action: Action) -> Message {
    Message::CommandReference(super::Action::SavedPage(action))
}

fn key(entry: &Entry) -> Option<Key> {
    let platform = PLATFORMS
        .iter()
        .find(|platform| platform.0 == entry.platform)?;
    Some(Key {
        platform: *platform,
        name: entry.name.clone(),
    })
}

fn entry(key: &Key) -> Entry {
    Entry {
        platform: key.platform.0.into(),
        name: key.name.clone(),
    }
}

impl Panel {
    pub(super) fn saved_page_action(
        &mut self,
        action: Action,
        auto: Platform,
        ctx: &ComponentContext<TerminalApp>,
    ) {
        match action {
            Action::Expanded(expanded)
                if self.open && !self.subpanel_open() && self.saved_pages.expanded != expanded =>
            {
                self.saved_pages.expanded = expanded;
                if expanded {
                    self.saved_pages.refresh(ctx);
                }
            }
            Action::Open(page)
                if self.open && !self.subpanel_open() && self.saved_pages.contains(&page) =>
            {
                if let Some(key) = key(&page)
                    && self
                        .library
                        .as_ref()
                        .is_some_and(|library| library.catalog.page(&key).is_some())
                {
                    self.saved_pages.close();
                    self.show_page(key, auto);
                }
            }
            Action::Set(page, saved) if self.open && !self.subpanel_open() => {
                if saved {
                    let results = self.results(self.platform.unwrap_or(auto));
                    let selected = results.get(self.selected).or_else(|| results.first());
                    if selected.is_none_or(|key| entry(key) != page) {
                        return;
                    }
                } else if !self.saved_pages.contains(&page) {
                    return;
                }
                self.saved_pages.start(Some((page, saved)), ctx);
            }
            Action::Finished(ticket, result)
                if self
                    .saved_pages
                    .pending
                    .as_ref()
                    .is_some_and(|pending| Arc::ptr_eq(pending, &ticket)) =>
            {
                self.saved_pages.pending = None;
                match result {
                    Ok(entries) => self.saved_pages.entries = Some(entries),
                    Err(error) => {
                        self.saved_pages.error = Some(error);
                        self.saved_pages.expanded = true;
                    }
                }
            }
            _ => {}
        }
    }
}

impl State {
    pub(super) fn close(&mut self) {
        self.expanded = false;
    }

    pub(super) fn refresh(&mut self, ctx: &ComponentContext<TerminalApp>) {
        self.start(None, ctx);
    }

    fn contains(&self, page: &Entry) -> bool {
        self.entries
            .as_ref()
            .is_some_and(|entries| entries.contains(page))
    }

    fn start(&mut self, change: Option<(Entry, bool)>, ctx: &ComponentContext<TerminalApp>) {
        if self.pending.is_some() {
            return;
        }
        let ticket = Arc::new(());
        self.pending = Some(ticket.clone());
        self.error = None;
        let rejected = message(Action::Finished(
            ticket.clone(),
            Err("Saved pages worker unavailable".into()),
        ));
        ctx.spawn_background_with_rejection(
            move |_| {
                let result = Store::user().and_then(|store| match change {
                    Some((page, saved)) => store.set_saved(page, saved),
                    None => store.list(),
                });
                message(Action::Finished(
                    ticket,
                    result.map_err(|error| format!("{error:#}")),
                ))
            },
            rejected,
        );
    }

    pub(super) fn button(&self, key: &Key, ctx: &ViewContext<TerminalApp>) -> KeyedView {
        let page = entry(key);
        let saved = self.contains(&page);
        KeyedView::new(
            "save-reference-page",
            Button::new()
                .is_enabled(self.pending.is_none() && self.entries.is_some())
                .on_click(ctx.callback(move |_| message(Action::Set(page.clone(), !saved))))
                .content(if saved {
                    "Remove from saved pages"
                } else {
                    "Save page"
                }),
        )
    }

    pub(super) fn section(
        &self,
        library: Option<&Loaded>,
        ctx: &ViewContext<TerminalApp>,
    ) -> KeyedView {
        let mut rows = Vec::new();
        if self.expanded {
            if let Some(error) = &self.error {
                rows.push(KeyedView::new(
                    "error",
                    TextBlock::new()
                        .text(error)
                        .text_wrapping(TextWrapping::Wrap),
                ));
            }
            if self.pending.is_some() {
                rows.push(KeyedView::new(
                    "loading",
                    TextBlock::new().text("Updating saved pages…"),
                ));
            } else if self.entries.as_ref().is_some_and(Vec::is_empty) {
                rows.push(KeyedView::new(
                    "empty",
                    TextBlock::new().text("No saved pages"),
                ));
            }
            if let (Some(entries), Some(library)) = (&self.entries, library) {
                rows.extend(
                    entries
                        .iter()
                        .map(|entry| row(entry, library, self.pending.is_none(), ctx)),
                );
            }
        }
        KeyedView::new(
            "saved-reference-pages",
            Expander::new()
                .automation_id("saved-reference-pages")
                .automation_name("Saved pages")
                .is_expanded(self.expanded)
                .on_is_expanded_changed(
                    ctx.callback(|expanded| message(Action::Expanded(expanded))),
                )
                .slots([
                    SlotView::new(ExpanderSlot::Header, TextBlock::new().text("Saved pages")),
                    SlotView::new(
                        ExpanderSlot::Content,
                        StackPanel::new().spacing(6.0).keyed_children(rows),
                    ),
                ]),
        )
    }
}

fn row(entry: &Entry, library: &Loaded, idle: bool, ctx: &ViewContext<TerminalApp>) -> KeyedView {
    let key = key(entry);
    let platform = key
        .as_ref()
        .map_or(entry.platform.as_str(), |key| key.platform.1);
    let page = key.as_ref().and_then(|key| library.catalog.page(key));
    let entry_key = format!("page-{}/{}", entry.platform, entry.name);
    match page {
        Some(page) => {
            let title = format!("{} · {platform}", page.title);
            let entry = entry.clone();
            KeyedView::new(
                entry_key,
                Button::new()
                    .automation_name(format!("Open saved page {title}"))
                    .is_enabled(idle)
                    .on_click(ctx.callback(move |_| message(Action::Open(entry.clone()))))
                    .content(
                        TextBlock::new()
                            .text(title)
                            .text_wrapping(TextWrapping::Wrap),
                    ),
            )
        }
        None => {
            let title = format!("{} · {platform}", entry.name);
            let entry = entry.clone();
            KeyedView::new(
                entry_key,
                Button::new()
                    .automation_name(format!("Remove unavailable saved page {title}"))
                    .is_enabled(idle)
                    .on_click(ctx.callback(move |_| message(Action::Set(entry.clone(), false))))
                    .content(
                        TextBlock::new()
                            .text(format!("Remove unavailable page · {title}"))
                            .text_wrapping(TextWrapping::Wrap),
                    ),
            )
        }
    }
}
