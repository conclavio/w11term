use super::*;
use std::path::PathBuf;
use w11term::{
    options::JumpHost,
    ssh_config::{ImportReport, ImportedAlias, import_file},
};

mod group;

pub(super) struct ImportPage {
    path: String,
    revision: u64,
    reading: bool,
    picking: Option<u64>,
    report: Option<ImportReport>,
    selected: Option<usize>,
    status: String,
    group: Option<Box<group::Editor>>,
}
impl Default for ImportPage {
    fn default() -> Self {
        Self {
            path: std::env::home_dir()
                .map(|home| home.join(".ssh").join("config").display().to_string())
                .unwrap_or_default(),
            revision: 0,
            reading: false,
            picking: None,
            report: None,
            selected: None,
            status: String::new(),
            group: None,
        }
    }
}
impl ImportPage {
    fn begin_picker(&mut self) -> Option<u64> {
        if self.picking.is_some() || self.reading {
            return None;
        }
        // A fresh ticket also separates two Browse gestures at the same typed path.
        self.revision += 1;
        self.picking = Some(self.revision);
        self.picking
    }
    fn finish_picker(
        &mut self,
        ticket: u64,
        active: bool,
        result: Result<Option<PathBuf>, String>,
    ) -> Option<PathBuf> {
        if self.picking != Some(ticket) {
            return None;
        }
        self.picking = None;
        if self.revision != ticket || !active {
            return None;
        }
        match result {
            Ok(Some(path)) => {
                if let Some(text) = path.to_str() {
                    self.path = text.to_owned();
                    return Some(path);
                }
                self.status = "The selected path cannot be represented as Unicode.".into();
            }
            Err(error) => self.status = format!("Could not choose a config file: {error}"),
            Ok(None) => {}
        }
        None
    }
    fn reset_preview(&mut self) {
        self.revision += 1;
        self.report = None;
        self.selected = None;
        self.group = None;
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
    fn preview(&mut self, id: TabId, path: PathBuf, context: &ComponentContext<TerminalApp>) {
        self.reset_preview();
        self.reading = true;
        let revision = self.revision;
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
}

pub enum Action {
    Show,
    Path(TabId, String),
    Browse(TabId),
    Picked(TabId, u64, Result<Option<PathBuf>, String>),
    Preview(TabId),
    Read(TabId, u64, Result<ImportReport, String>),
    Select(TabId, u64, Option<usize>),
    Save(TabId, u64, usize),
    OpenGroup(TabId, u64),
    Group(TabId, u64, group::Change),
}

pub(super) fn endpoint(
    host: &str,
    port: u16,
    user: &str,
    auth: &Authentication,
    host_key_alias: Option<&str>,
) -> String {
    let auth = match auth {
        Authentication::None => "none".to_owned(),
        Authentication::Agent => "SSH agent".to_owned(),
        Authentication::Identity {
            identity,
            certificate,
        } => match certificate {
            Some(path) => format!(
                "key file {} with user certificate {}",
                identity.display(),
                path.display()
            ),
            None => format!("key file {}", identity.display()),
        },
        Authentication::Password => "password".to_owned(),
        Authentication::KeyboardInteractive => "keyboard-interactive".to_owned(),
    };
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    match host_key_alias {
        Some(alias) => format!("{user}@{host}:{port} ({auth}, host key alias {alias})"),
        None => format!("{user}@{host}:{port} ({auth})"),
    }
}

fn jump_summary(hops: &[JumpHost]) -> String {
    let mut text = String::from("ProxyJump route");
    for hop in hops {
        text.push('\n');
        text.push_str(&endpoint(
            &hop.host,
            hop.port,
            &hop.user,
            &hop.authentication,
            hop.host_key_alias.as_deref(),
        ));
        if hop.keepalive != SshKeepalive::default() {
            text.push_str("\n  ");
            text.push_str(&keepalive_summary(hop.keepalive));
        }
        if let Some(seconds) = hop.connect_timeout_seconds {
            text.push_str(&format!("\n  SSH connection timeout · {seconds} s"));
        }
    }
    text
}

fn keepalive_summary(keepalive: SshKeepalive) -> String {
    if keepalive.interval_seconds == 0 {
        "SSH keepalives disabled".into()
    } else {
        format!(
            "SSH keepalive every {} s · {} unanswered allowed",
            keepalive.interval_seconds, keepalive.max_unanswered
        )
    }
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
            Action::Browse(id) => {
                if self.tabs.active_id() != Some(id) {
                    return;
                }
                let owner = self.keyboard.hwnd();
                let Some(page) = self.import_page_mut(id) else {
                    return;
                };
                let Some(owner) = owner else {
                    page.status = "The window is unavailable.".into();
                    return;
                };
                let Some(ticket) = page.begin_picker() else {
                    return;
                };
                let current = PathBuf::from(page.path.trim());
                let owner = owner.0 as usize;
                context.spawn_background_with_rejection(
                    move |cancel| {
                        let result = if cancel.is_cancelled() {
                            Ok(None)
                        } else {
                            file_picker::pick(owner, file_picker::Purpose::SshConfig(current))
                                .map(|chosen| chosen.map(|file| file.path))
                                .map_err(|e| format!("{e:#}"))
                        };
                        Message::ImportConfig(Action::Picked(id, ticket, result))
                    },
                    Message::ImportConfig(Action::Picked(
                        id,
                        ticket,
                        Err("File picker unavailable".into()),
                    )),
                );
            }
            Action::Picked(id, ticket, result) => {
                let active = self.tabs.active_id() == Some(id);
                if let Some(page) = self.import_page_mut(id)
                    && let Some(path) = page.finish_picker(ticket, active, result)
                {
                    page.preview(id, path, context);
                }
            }
            Action::Preview(id) => {
                let Some(page) = self.import_page_mut(id) else {
                    return;
                };
                if page.reading || page.picking.is_some() || page.path.trim().is_empty() {
                    return;
                }
                page.preview(id, PathBuf::from(page.path.trim()), context);
            }
            Action::Read(id, revision, result) => {
                if let Some(page) = self.import_page_mut(id) {
                    page.finish(revision, result);
                }
            }
            Action::OpenGroup(id, revision) => {
                if self.tabs.active_id() == Some(id)
                    && let Some(page) = self.import_page_mut(id)
                    && page.revision == revision
                    && page.picking.is_none()
                    && page.group.is_none()
                    && page
                        .report
                        .as_ref()
                        .is_some_and(|report| !report.aliases.is_empty())
                {
                    page.revision += 1;
                    page.group = Some(Box::default());
                }
            }
            Action::Group(id, revision, change) => self.edit_config_group(id, revision, change),
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
        if let Some(group) = &page.group
            && let Some(report) = &page.report
        {
            return group.view(id, revision, report, &self.catalog, context);
        }
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
                Grid::new()
                    .column_spacing(8.0)
                    .columns([GridLength::Star(1.0), GridLength::Auto])
                    .children((
                        TextBox::new()
                            .automation_name("OpenSSH config path")
                            .text(&page.path)
                            .on_text_changed(context.callback(move |path| {
                                Message::ImportConfig(Action::Path(id, path))
                            })),
                        Button::new()
                            .grid_column(1)
                            .is_enabled(!page.reading && page.picking.is_none())
                            .on_click(
                                context
                                    .callback(move |_| Message::ImportConfig(Action::Browse(id))),
                            )
                            .content("Browse…"),
                    )),
            ),
            KeyedView::new(
                "preview",
                Button::new()
                    .is_enabled(
                        !page.reading && page.picking.is_none() && !page.path.trim().is_empty(),
                    )
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
                fields.push(KeyedView::new(
                    "create-group",
                    Button::new()
                        .is_enabled(page.picking.is_none())
                        .on_click(context.callback(move |_| {
                            Message::ImportConfig(Action::OpenGroup(id, revision))
                        }))
                        .content("Create SSH group from aliases…"),
                ));
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
                            &options.authentication,
                            options.host_key_alias.as_deref()
                        )
                    ),
                ));
                if options.keepalive != SshKeepalive::default() {
                    fields.push(text("keepalive", keepalive_summary(options.keepalive)));
                }
                if let Some(seconds) = options.connect_timeout_seconds {
                    fields.push(text(
                        "connect-timeout",
                        format!("SSH connection timeout · {seconds} s"),
                    ));
                }
                if !options.proxy_jump.is_empty() {
                    fields.push(text("hops", jump_summary(&options.proxy_jump)));
                }
                if !options.local_forwards.is_empty() {
                    fields.push(text("forwards", "Local forwarding presets".into()));
                    for (index, &preset) in options.local_forwards.iter().enumerate() {
                        fields.push(text(
                            &format!("forward-{index}"),
                            local_forwards::preset_label(preset),
                        ));
                    }
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
        use std::path::{Path, PathBuf};
        let mut next = step + 1;
        match step {
            0 => {
                self.utility(true, context);
                self.update(Message::ImportConfig(Action::Show), context);
                let id = self.tabs.active_id().unwrap();
                let fixture = PathBuf::from(std::env::var_os("W11TERM_IMPORT_SMOKE").unwrap());
                let page = self.import_page_mut(id).unwrap();
                assert!(std::path::Path::new(&page.path).ends_with(".ssh/config"));
                let previous = fixture
                    .with_file_name("previous.conf")
                    .display()
                    .to_string();
                self.update(Message::ImportConfig(Action::Path(id, previous)), context);
                self.update(Message::ImportConfig(Action::Preview(id)), context);
            }
            1 => {
                let id = self.tabs.active_id().unwrap();
                let page = self.import_page_mut(id).unwrap();
                // The desktop fixture cancels Browse once, then selects the extensionless
                // config. Continue with the ordinary save/edit checks after its preview.
                if page.reading
                    || Path::new(&page.path)
                        .file_name()
                        .is_none_or(|n| n != "config")
                {
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
                    assert!(alias.options.is_none());
                    assert!(
                        alias.warnings.iter().any(|warning| {
                            warning.contains("30-blocked.conf:3")
                                && warning.to_lowercase().contains("proxycommand")
                        }),
                        "Missing fragment and line for refused route: {:?}",
                        alias.warnings
                    );
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
                assert_eq!(options.host, "target.test");
                assert_eq!(options.user, "pilot");
                assert_eq!(options.port, 2222);
                assert_eq!(options.proxy_jump[0].host, "gateway.test");
                assert_eq!(options.keepalive.interval_seconds, 15);
                assert_eq!(options.keepalive.max_unanswered.get(), 3);
                assert_eq!(options.proxy_jump[0].keepalive.interval_seconds, 0);
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
                assert_eq!(draft.keepalive_interval, "15");
                assert_eq!(draft.keepalive_max, "3");
                self.update(Message::TerminalPreset(id, Some(1)), context);
                self.save_profile_action(id, false, context);
                let saved = self.store.load().unwrap();
                assert_eq!(saved.len(), 1);
                assert_eq!(saved["via"].proxy_jump, hops);
                assert_eq!(saved["via"].keepalive.interval_seconds, 15);
                assert_eq!(saved["via"].keepalive.max_unanswered.get(), 3);
                assert_eq!(saved["via"].terminal_type.as_str(), TerminalType::SIXEL);
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

    fn previewed_page() -> ImportPage {
        ImportPage {
            path: "previous config".into(),
            report: Some(ImportReport {
                aliases: vec![ImportedAlias {
                    name: "previous".into(),
                    options: None,
                    warnings: vec![],
                }],
                warnings: vec!["preview warning".into()],
            }),
            selected: Some(0),
            status: "previous status".into(),
            ..ImportPage::default()
        }
    }

    #[test]
    fn cancel_keeps_the_path_selection_and_preview_and_cannot_complete_a_later_picker() {
        let mut page = previewed_page();
        let first = page.begin_picker().unwrap();
        assert!(page.begin_picker().is_none());
        assert!(page.finish_picker(first, true, Ok(None)).is_none());
        assert_eq!(page.path, "previous config");
        assert_eq!(page.alias().unwrap().name, "previous");
        assert_eq!(page.report.as_ref().unwrap().warnings, ["preview warning"]);
        assert_eq!(page.status, "previous status");

        let second = page.begin_picker().unwrap();
        assert!(
            page.finish_picker(first, true, Ok(Some("obsolete".into())))
                .is_none()
        );
        assert_eq!(page.picking, Some(second));
        let picked = PathBuf::from(r"C:\fixture\config");
        assert_eq!(
            page.finish_picker(second, true, Ok(Some(picked.clone()))),
            Some(picked.clone())
        );
        assert_eq!(page.path, picked.to_str().unwrap());
        assert!(page.picking.is_none());
    }

    #[test]
    fn a_picker_finishing_after_a_path_edit_or_tab_switch_cannot_replace_the_path() {
        let mut page = previewed_page();
        let first = page.begin_picker().unwrap();
        page.reset_preview();
        page.path = "typed while choosing".into();
        assert!(
            page.finish_picker(first, true, Ok(Some("stale file".into())))
                .is_none()
        );
        assert_eq!(page.path, "typed while choosing");
        assert!(page.report.is_none());

        let second = page.begin_picker().unwrap();
        assert!(
            page.finish_picker(second, false, Ok(Some("another file".into())))
                .is_none()
        );
        assert_eq!(page.path, "typed while choosing");
        assert!(page.begin_picker().is_some());
    }

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
