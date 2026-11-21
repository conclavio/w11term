use super::*;
use w11term::{
    options::JumpHost,
    ssh_config::{ImportReport, ImportedAlias, import_file},
};

pub(super) struct ImportPage {
    path: String,
    revision: u64,
    reading: bool,
    report: Option<ImportReport>,
    selected: Option<usize>,
    status: String,
}
impl Default for ImportPage {
    fn default() -> Self {
        Self {
            path: std::env::home_dir()
                .map(|home| home.join(".ssh").join("config").display().to_string())
                .unwrap_or_default(),
            revision: 0,
            reading: false,
            report: None,
            selected: None,
            status: String::new(),
        }
    }
}
impl ImportPage {
    fn reset_preview(&mut self) {
        self.revision += 1;
        self.report = None;
        self.selected = None;
        self.reading = false;
        self.status.clear();
    }
    fn finish(&mut self, revision: u64, result: Result<ImportReport, String>) {
        if revision != self.revision || !self.reading {
            return;
        }
        self.reading = false;
        match result {
            Ok(report) => {
                self.selected = (!report.aliases.is_empty()).then_some(0);
                self.report = Some(report);
            }
            Err(error) => self.status = error,
        }
    }
    fn alias(&self) -> Option<&ImportedAlias> {
        self.report.as_ref()?.aliases.get(self.selected?)
    }
}

pub enum Action {
    Show,
    Path(TabId, String),
    Preview(TabId),
    Read(TabId, u64, Result<ImportReport, String>),
    Select(TabId, u64, Option<usize>),
    Save(TabId, u64, usize),
}

fn endpoint(host: &str, port: u16, user: &str, auth: &Authentication) -> String {
    let auth = match auth {
        Authentication::None => "none".to_owned(),
        Authentication::Agent => "SSH agent".to_owned(),
        Authentication::Identity { identity } => format!("key file {}", identity.display()),
        Authentication::Password => "password".to_owned(),
        Authentication::KeyboardInteractive => "keyboard-interactive".to_owned(),
    };
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    format!("{user}@{host}:{port} ({auth})")
}

pub(super) fn jump_summary(hops: &[JumpHost]) -> String {
    let mut text = String::from("ProxyJump route");
    for hop in hops {
        text.push('\n');
        text.push_str(&endpoint(
            &hop.host,
            hop.port,
            &hop.user,
            &hop.authentication,
        ));
    }
    text
}

impl TerminalApp {
    fn import_page_mut(&mut self, id: TabId) -> Option<&mut ImportPage> {
        match &mut self.tabs.get_mut(id)?.page {
            Page::ImportConfig(page) => Some(page),
            _ => None,
        }
    }
    pub(super) fn import_action(&mut self, action: Action, context: &ComponentContext<Self>) {
        match action {
            Action::Show => {
                if let Some(id) = self
                    .tabs
                    .entries()
                    .iter()
                    .find(|tab| matches!(tab.value.page, Page::ImportConfig(_)))
                    .map(|tab| tab.id)
                {
                    self.tabs.select(id);
                } else {
                    self.tabs.insert(AppTab {
                        label: "Import OpenSSH config".into(),
                        page: Page::ImportConfig(ImportPage::default()),
                    });
                    self.sync_selection(context);
                }
                self.activate(context);
            }
            Action::Path(id, path) => {
                if let Some(page) = self.import_page_mut(id)
                    && page.path != path
                {
                    page.reset_preview();
                    page.path = path;
                }
            }
            Action::Preview(id) => {
                let Some(page) = self.import_page_mut(id) else {
                    return;
                };
                if page.reading || page.path.trim().is_empty() {
                    return;
                }
                page.reset_preview();
                page.reading = true;
                let revision = page.revision;
                let path = std::path::PathBuf::from(page.path.trim());
                context.spawn_background_with_rejection(
                    move |_| {
                        Message::ImportConfig(Action::Read(
                            id,
                            revision,
                            import_file(&path).map_err(|e| format!("{e:#}")),
                        ))
                    },
                    Message::ImportConfig(Action::Read(
                        id,
                        revision,
                        Err("Could not start the config preview. Try again.".into()),
                    )),
                );
            }
            Action::Read(id, revision, result) => {
                if let Some(page) = self.import_page_mut(id) {
                    page.finish(revision, result);
                }
            }
            Action::Select(id, revision, selected) => {
                if let Some(page) = self.import_page_mut(id)
                    && page.revision == revision
                    && let Some(i) = selected
                    && page
                        .report
                        .as_ref()
                        .is_some_and(|report| i < report.aliases.len())
                    && page.selected != Some(i)
                {
                    page.selected = Some(i);
                    page.status.clear();
                }
            }
            Action::Save(id, revision, selected) => {
                let Some(page) = self.import_page_mut(id) else {
                    return;
                };
                if page.revision != revision || page.selected != Some(selected) {
                    return;
                }
                let Some(alias) = page.alias() else { return };
                let Some(options) = alias.options.clone() else {
                    return;
                };
                let name = alias.name.clone();
                let feedback = match self
                    .store
                    .add(&name, &options)
                    .and_then(|_| self.store.catalog())
                {
                    Ok(catalog) => {
                        self.catalog = catalog;
                        format!("Saved profile {name}")
                    }
                    Err(error) => format!("{error:#}"),
                };
                self.import_page_mut(id).unwrap().status = feedback;
            }
        }
    }

    pub(super) fn import_view(
        &self,
        id: TabId,
        page: &ImportPage,
        context: &ViewContext<Self>,
    ) -> View {
        let revision = page.revision;
        let mut fields = vec![
            KeyedView::new(
                "title",
                TextBlock::new()
                    .font_size(26.0)
                    .text("Import OpenSSH config"),
            ),
            KeyedView::new("path-label", TextBlock::new().text("Config file")),
            KeyedView::new(
                "path",
                TextBox::new()
                    .automation_name("OpenSSH config path")
                    .text(&page.path)
                    .on_text_changed(
                        context.callback(move |path| Message::ImportConfig(Action::Path(id, path))),
                    ),
            ),
            KeyedView::new(
                "preview",
                Button::new()
                    .is_enabled(!page.reading && !page.path.trim().is_empty())
                    .on_click(context.callback(move |_| Message::ImportConfig(Action::Preview(id))))
                    .content(if page.reading {
                        "Reading config…"
                    } else {
                        "Preview hosts"
                    }),
            ),
        ];
        let text = |key: &str, value: String| {
            KeyedView::new(
                key,
                TextBlock::new()
                    .text(value)
                    .text_wrapping(TextWrapping::Wrap),
            )
        };
        if let Some(report) = &page.report {
            if !report.warnings.is_empty() {
                fields.push(text("config-warnings", report.warnings.join("\n")));
            }
            if !report.aliases.is_empty() {
                fields.push(text("host-label", "Host alias".into()));
                fields.push(KeyedView::new(
                    "hosts",
                    ComboBox::new()
                        .automation_name("Host alias")
                        .items_source(report.aliases.iter().map(|alias| alias.name.clone()))
                        .selected_index(page.selected)
                        .on_selection_changed(context.callback(move |selected| {
                            Message::ImportConfig(Action::Select(id, revision, selected))
                        })),
                ));
            }
        }
        if let Some(alias) = page.alias() {
            if let Some(options) = &alias.options {
                fields.push(text(
                    "destination",
                    format!(
                        "Destination\n{}",
                        endpoint(
                            &options.host,
                            options.port,
                            &options.user,
                            &options.authentication
                        )
                    ),
                ));
                if !options.proxy_jump.is_empty() {
                    fields.push(text("hops", jump_summary(&options.proxy_jump)));
                }
            } else {
                fields.push(text("unsupported", format!("Cannot import {}", alias.name)));
            }
            if !alias.warnings.is_empty() {
                fields.push(text("host-warnings", alias.warnings.join("\n")));
            }
            let selected = page.selected.unwrap();
            let exists = self.catalog.profiles.contains_key(&alias.name)
                || self.catalog.local.contains_key(&alias.name);
            fields.push(KeyedView::new(
                "save",
                Button::new()
                    .style(ButtonStyle::Accent)
                    .is_enabled(alias.options.is_some() && !exists)
                    .on_click(context.callback(move |_| {
                        Message::ImportConfig(Action::Save(id, revision, selected))
                    }))
                    .content(if exists {
                        "Profile already exists"
                    } else {
                        "Save profile"
                    }),
            ));
        }
        if !page.status.is_empty() {
            fields.push(text("status", page.status.clone()));
        }
        ScrollViewer::new().content(
            StackPanel::new()
                .spacing(12.0)
                .margin(32.0)
                .width(550.0)
                .horizontal_alignment(HorizontalAlignment::Left)
                .keyed_children(fields),
        )
    }
}

#[cfg(feature = "ui-smoke")]
impl TerminalApp {
    pub(super) fn import_smoke_step(&mut self, step: u8, context: &ComponentContext<Self>) {
        let mut next = step + 1;
        match step {
            0 => {
                self.utility(true, context);
                self.update(Message::ImportConfig(Action::Show), context);
                let id = self.tabs.active_id().unwrap();
                let fixture = std::env::var("W11TERM_IMPORT_SMOKE").unwrap();
                let page = self.import_page_mut(id).unwrap();
                assert!(std::path::Path::new(&page.path).ends_with(".ssh/config"));
                self.update(Message::ImportConfig(Action::Path(id, fixture)), context);
                self.update(Message::ImportConfig(Action::Preview(id)), context);
            }
            1 => {
                let id = self.tabs.active_id().unwrap();
                let page = self.import_page_mut(id).unwrap();
                if page.reading {
                    next = step;
                } else {
                    let report = page.report.as_ref().expect("could not parse fixture");
                    let unusable = report
                        .aliases
                        .iter()
                        .position(|alias| alias.name == "blocked")
                        .unwrap();
                    let via = report
                        .aliases
                        .iter()
                        .position(|alias| alias.name == "via")
                        .unwrap();
                    let revision = page.revision;
                    self.update(
                        Message::ImportConfig(Action::Select(id, revision, Some(unusable))),
                        context,
                    );
                    let alias = self.import_page_mut(id).unwrap().alias().unwrap();
                    assert!(alias.options.is_none() && !alias.warnings.is_empty());
                    self.update(
                        Message::ImportConfig(Action::Save(id, revision, unusable)),
                        context,
                    );
                    assert!(!self.catalog.profiles.contains_key("blocked"));
                    self.update(
                        Message::ImportConfig(Action::Select(id, revision, Some(via))),
                        context,
                    );
                }
            }
            2 => {
                let id = self.tabs.active_id().unwrap();
                let page = self.import_page_mut(id).unwrap();
                let selected = page.selected.unwrap();
                let revision = page.revision;
                let alias = page.alias().unwrap();
                assert_eq!(alias.name, "via");
                let options = alias.options.as_ref().unwrap();
                assert_eq!(options.port, 2222);
                assert_eq!(options.proxy_jump[0].host, "gateway.test");
                assert!(
                    alias
                        .warnings
                        .iter()
                        .any(|w| w.to_lowercase().contains("serveraliveinterval"))
                );
                self.update(
                    Message::ImportConfig(Action::Save(id, revision, selected)),
                    context,
                );
                assert_eq!(
                    self.import_page_mut(id).unwrap().status,
                    "Saved profile via"
                );
                self.update(
                    Message::ImportConfig(Action::Save(id, revision, selected)),
                    context,
                );
                assert!(
                    self.import_page_mut(id)
                        .unwrap()
                        .status
                        .contains("already exists")
                );
                let terminals = self
                    .tabs
                    .entries()
                    .iter()
                    .filter(|entry| matches!(entry.value.page, Page::Terminal(_)))
                    .count();
                assert_eq!(terminals, 1);
                self.edit_profile("via".into(), context);
            }
            3 => {
                let id = self.tabs.active_id().unwrap();
                let Page::NewProfile(draft) = &self.tabs.get(id).unwrap().page else {
                    panic!("expected imported profile in the editor")
                };
                let hops = draft.proxy_jump.clone();
                assert_eq!(hops.len(), 1);
                self.save_profile_action(id, false, context);
                assert_eq!(self.store.load().unwrap()["via"].proxy_jump, hops);
                eprintln!("OpenSSH import UI smoke passed");
                self.update(Message::CloseWindow, context);
                return;
            }
            _ => panic!("unexpected import smoke step"),
        }
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(400));
            Message::Smoke(next)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_import_cannot_replace_a_newer_preview_or_revive_a_changed_path() {
        let mut page = ImportPage::default();
        page.reset_preview();
        let first = page.revision;
        page.reading = true;
        page.reset_preview();
        page.finish(first, Err("old error".into()));
        assert!(page.status.is_empty());
        page.reading = true;
        let report = ImportReport {
            aliases: vec![],
            warnings: vec!["second file".into()],
        };
        page.finish(first, Ok(report.clone()));
        assert!(page.reading && page.report.is_none());
        page.finish(page.revision, Ok(report));
        assert_eq!(page.report.as_ref().unwrap().warnings, ["second file"]);
        assert!(!page.reading);
        page.finish(page.revision, Err("late duplicate reply".into()));
        assert!(page.status.is_empty());
    }
}
