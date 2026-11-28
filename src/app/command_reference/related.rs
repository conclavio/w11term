use super::*;
use std::collections::BTreeSet;

const MAX_VISITED: usize = 16;

pub enum Action {
    Open { from: Key, label: String },
    Back,
}

pub(super) struct Location {
    query: String,
    mode: SearchMode,
    page: Key,
}

impl Panel {
    pub(super) fn related_view(
        &self,
        from: &Key,
        page: &catalog::Page,
        preferred: Platform,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let Some(library) = &self.library else {
            return View::empty();
        };
        let mut items = vec![KeyedView::new("heading", TextBlock::new().text("See also"))];
        let mut seen = BTreeSet::new();
        for label in &page.related {
            let name = catalog::query(label);
            if !seen.insert(name.clone()) {
                continue;
            }
            let available = library
                .catalog
                .exact(label, preferred)
                .is_some_and(|target| target != *from);
            let item = if available {
                let from = from.clone();
                let reference = label.clone();
                HyperlinkButton::new()
                    .automation_name(format!("Open manual for {label}"))
                    .on_click(ctx.callback(move |_| {
                        Message::CommandReference(super::Action::Related(Action::Open {
                            from: from.clone(),
                            label: reference.clone(),
                        }))
                    }))
                    .content(label.as_str())
            } else {
                TextBlock::new()
                    .text(label)
                    .text_wrapping(TextWrapping::Wrap)
                    .into()
            };
            items.push(KeyedView::new(format!("manual-{name}"), item));
        }
        StackPanel::new().spacing(2.0).keyed_children(items)
    }

    pub(super) fn related_action(&mut self, action: Action, auto: Platform) {
        if !self.open || self.subpanel_open() {
            return;
        }
        let preferred = self.platform.unwrap_or(auto);
        match action {
            Action::Open { from, label } => {
                let results = self.results(preferred);
                if results.get(self.selected).or_else(|| results.first()) != Some(&from) {
                    return;
                }
                drop(results);
                let Some(library) = &self.library else {
                    return;
                };
                let catalog = &library.catalog;
                let Some(target) = catalog
                    .page(&from)
                    .filter(|page| page.related.contains(&label))
                    .and_then(|_| catalog.exact(&label, preferred))
                    .filter(|target| target != &from)
                else {
                    return;
                };
                if self.related_history.len() == MAX_VISITED {
                    self.related_history.pop_front();
                }
                self.related_history.push_back(Location {
                    query: self.query.clone(),
                    mode: self.mode.clone(),
                    page: from,
                });
                self.set_query(target.name.clone());
                self.mode = SearchMode::Page(target);
                self.selected = 0;
            }
            Action::Back => {
                let Some(previous) = self.related_history.pop_back() else {
                    return;
                };
                self.set_query(previous.query);
                self.mode = previous.mode;
                let index = self
                    .results(preferred)
                    .iter()
                    .position(|key| key == &previous.page);
                self.selected = index.unwrap_or(0);
                if index.is_none()
                    && self
                        .library
                        .as_ref()
                        .is_some_and(|library| library.catalog.page(&previous.page).is_some())
                {
                    self.mode = SearchMode::Page(previous.page);
                }
            }
        }
        self.command_lookup = None;
        self.draft = None;
        self.focus = self.focus.wrapping_add(1);
    }
}
