use super::folder_sharing_state::{Authority, FolderRow};
use super::*;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use w11term::ssh::{FolderMountStatus, MountedFolder, valid_mount_name};

pub(super) const LABEL: &str = "Shared folders…";
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

pub(super) fn token() -> u64 {
    NEXT_TOKEN.fetch_add(1, Ordering::Relaxed)
}

pub(super) struct Panel {
    token: u64,
    authority: Authority,
    rows: Vec<FolderRow>,
    feedback: String,
    pending: Option<u64>,
    resolving: Option<u64>,
    draft: Option<(PathBuf, String)>,
}

pub enum Action {
    Open(u64),
    Panel(u64, Edit),
}
pub enum Edit {
    Refresh,
    Back,
    Add,
    Picked(u64, Result<Option<PathBuf>, String>),
    Name(String),
    Save,
    Cancel,
    Row(RowAction, String, PathBuf),
    Mounted(MountedAction, String, String),
    Resolved(u64, MountedAction, String, Result<MountedFolder, String>),
}
#[derive(Clone, Copy)]
pub enum RowAction {
    Mount,
    Unmount,
    Forget,
}
#[derive(Clone, Copy)]
pub enum MountedAction {
    Browse,
    CopyPath,
}

fn message(token: u64, edit: Edit) -> Message {
    Message::FolderShare(Action::Panel(token, edit))
}

fn suggested_name(path: &std::path::Path) -> String {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let mut result = String::new();
    for c in name.chars() {
        if result.len() == 64 {
            break;
        }
        if c.is_ascii_alphanumeric() || (!result.is_empty() && "._-".contains(c)) {
            result.push(c);
        } else if !result.is_empty() && !result.ends_with('-') {
            result.push('-');
        }
    }
    if result.is_empty() {
        "files".to_string()
    } else {
        result
    }
}

fn status_text(status: &FolderMountStatus) -> String {
    match status {
        FolderMountStatus::Starting => "Mounting…".into(),
        FolderMountStatus::Mounted => "Mounted".into(),
        FolderMountStatus::Unmounting => "Unmounting…".into(),
        FolderMountStatus::Stopped => "Unmounted".into(),
        FolderMountStatus::Failed(reason) => format!("Mount failed · {reason}"),
    }
}

fn still_mounted(rows: &[FolderRow], name: &str, expected: &MountedFolder) -> bool {
    rows.iter().any(|row| {
        row.name == name
            && row.status == FolderMountStatus::Mounted
            && row.mounted.as_ref() == Some(expected)
    })
}

impl TerminalApp {
    pub(super) fn folder_editor_target(&self, draft: &ProfileDraft) -> Option<u64> {
        let base = draft.origin.editing()?;
        let mut sources = self.tabs.entries().iter().filter_map(|entry| {
            let pane = entry.value.pane_id()?;
            if entry.value.ssh_profile_name()? != base.name {
                return None;
            }
            let authority = self.folder_authority(pane)?;
            (authority.profile == draft.name
                && authority.host.eq_ignore_ascii_case(&draft.host)
                && authority.user == draft.user
                && Some(authority.port) == draft.port.parse().ok())
            .then_some(pane)
        });
        let only = sources.next()?;
        sources.next().is_none().then_some(only)
    }

    pub(super) fn activate_folder_panel(&mut self, ctx: &ComponentContext<Self>) {
        let active = self
            .tabs
            .active_id()
            .and_then(|id| match &self.tabs.get(id)?.page {
                Page::SharedFolders(page) => Some((id, page.token)),
                _ => None,
            });
        if let Some((old, _)) = &self.folder_panel_timer
            && Some(*old) == active.map(|(id, _)| id)
        {
            return;
        }
        if let Some((previous, _timer)) = self.folder_panel_timer.take()
            && let Some(AppTab {
                page: Page::SharedFolders(panel),
                ..
            }) = self.tabs.get_mut(previous)
        {
            panel.pending = None;
            panel.resolving = None;
        }
        if let Some((id, page)) = active {
            let sender = ctx.sender();
            let mut last = Instant::now();
            self.folder_panel_timer = crate::ui_timer::RetryTimer::new(move || {
                if last.elapsed() >= Duration::from_millis(250) {
                    last = Instant::now();
                    sender.send(message(page, Edit::Refresh));
                }
            })
            .ok()
            .map(|timer| (id, timer));
            self.refresh_folder_panel(id);
        }
    }

    fn refresh_folder_panel(&mut self, id: TabId) {
        let Some(AppTab {
            page: Page::SharedFolders(panel),
            ..
        }) = self.tabs.get(id)
        else {
            return;
        };
        let current =
            self.folder_authority(panel.authority.pane).as_ref() == Some(&panel.authority);
        let rows = current.then(|| self.folder_rows(&panel.authority));
        let Page::SharedFolders(panel) = &mut self.tabs.get_mut(id).unwrap().page else {
            return;
        };
        match rows {
            Some(Ok(rows)) => panel.rows = rows,
            Some(Err(error)) => {
                panel.rows.clear();
                panel.feedback = format!("Could not load shared folders · {error:#}");
            }
            None => {
                panel.rows.clear();
                panel.pending = None;
                panel.resolving = None;
                panel.draft = None;
            }
        }
    }

    pub(super) fn folder_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        if let Action::Open(pane) = action {
            let source = self.tabs.active().and_then(|active| match &active.page {
                Page::NewProfile(draft) => self.folder_editor_target(draft),
                _ => active.pane_id(),
            });
            if source != Some(pane) {
                return;
            }
            let Some(authority) = self.folder_authority(pane) else {
                return;
            };
            let existing = self.tabs.entries().iter().find(|entry| {
                matches!(&entry.value.page, Page::SharedFolders(page) if page.authority.pane == pane)
            }).map(|e| e.id);
            let label = format!("Shared folders · {}", authority.profile);
            let page = Page::SharedFolders(Panel {
                token: token(),
                authority,
                rows: vec![],
                feedback: String::new(),
                pending: None,
                resolving: None,
                draft: None,
            });
            if let Some(id) = existing {
                *self.tabs.get_mut(id).unwrap() = AppTab { label, page };
                self.tabs.select(id);
            } else {
                self.tabs.insert(AppTab { label, page });
            }
            self.sync_selection(ctx);
            self.activate(ctx);
            return;
        }
        let Action::Panel(origin, edit) = action else {
            return;
        };
        let Some(id) = self.tabs.active_id() else {
            return;
        };
        let Some(AppTab {
            page: Page::SharedFolders(panel),
            ..
        }) = self.tabs.get(id)
        else {
            return;
        };
        if panel.token != origin {
            return;
        }
        let authority = panel.authority.clone();
        if matches!(edit, Edit::Back) {
            if let Some(source) = self.tab_for_pane(authority.pane) {
                self.tabs.select(source);
                self.sync_selection(ctx);
                self.activate(ctx);
            }
            return;
        }
        if matches!(edit, Edit::Refresh)
            || self.folder_authority(authority.pane).as_ref() != Some(&authority)
        {
            self.refresh_folder_panel(id);
            return;
        }
        let Page::SharedFolders(panel) = &mut self.tabs.get_mut(id).unwrap().page else {
            return;
        };
        let result = match edit {
            Edit::Add if panel.pending.is_none() && panel.draft.is_none() => {
                let Some(hwnd) = self.keyboard.hwnd() else {
                    panel.feedback = "The window is unavailable".into();
                    return;
                };
                let owner = hwnd.0 as usize;
                let request = token();
                panel.resolving = None;
                panel.pending = Some(request);
                panel.feedback.clear();
                ctx.spawn_background_with_rejection(
                    move |cancel| {
                        let result = if cancel.is_cancelled() {
                            Ok(None)
                        } else {
                            file_picker::pick(owner, file_picker::Purpose::Folder)
                                .map(|picked| picked.map(|selection| selection.path))
                                .map_err(|err| format!("{err:#}"))
                        };
                        message(origin, Edit::Picked(request, result))
                    },
                    message(
                        origin,
                        Edit::Picked(request, Err("Folder picker unavailable".into())),
                    ),
                );
                return;
            }
            Edit::Picked(request, result) if panel.pending == Some(request) => {
                panel.pending = None;
                match result {
                    Err(error) => panel.feedback = error,
                    Ok(Some(path)) => {
                        let name = suggested_name(&path);
                        panel.draft = Some((path, name));
                    }
                    Ok(None) => {}
                }
                return;
            }
            Edit::Name(value) => {
                if let Some((_, name)) = &mut panel.draft {
                    *name = value;
                }
                return;
            }
            Edit::Cancel => {
                panel.draft = None;
                panel.feedback.clear();
                return;
            }
            Edit::Save => {
                let Some((path, name)) = panel.draft.clone() else {
                    return;
                };
                if !valid_mount_name(&name) {
                    return;
                }
                self.folder_add(&authority, &name, path)
                    .map(|()| format!("Requested read-only mount at ~/windows/{name}"))
            }
            Edit::Row(action, name, path) => {
                self.folder_row_action(&authority, action, &name, path)
            }
            Edit::Mounted(action, name, generation) => {
                if panel.resolving.is_some() || panel.pending.is_some() || panel.draft.is_some() {
                    return;
                }
                match self.resolve_mounted_folder(&authority, &name, &generation) {
                    Ok(resolve) => {
                        let request = token();
                        let rejected = message(
                            origin,
                            Edit::Resolved(
                                request,
                                action,
                                name.clone(),
                                Err("Could not check the mounted folder. Try again.".into()),
                            ),
                        );
                        let Page::SharedFolders(panel) = &mut self.tabs.get_mut(id).unwrap().page
                        else {
                            return;
                        };
                        panel.resolving = Some(request);
                        panel.feedback.clear();
                        ctx.spawn_background_with_rejection(
                            move |_| {
                                message(
                                    origin,
                                    Edit::Resolved(
                                        request,
                                        action,
                                        name,
                                        resolve().map_err(|error| format!("{error:#}")),
                                    ),
                                )
                            },
                            rejected,
                        );
                        return;
                    }
                    Err(error) => Err(error),
                }
            }
            Edit::Resolved(request, action, name, result) if panel.resolving == Some(request) => {
                panel.resolving = None;
                result.map_err(anyhow::Error::msg).and_then(|mounted| {
                    self.mounted_folder_action(&authority, action, &name, mounted, ctx)
                })
            }
            _ => return,
        };
        let Page::SharedFolders(panel) = &mut self.tabs.get_mut(id).unwrap().page else {
            return;
        };
        match result {
            Ok(text) => {
                panel.feedback = text;
                panel.draft = None;
            }
            Err(error) => panel.feedback = format!("{error:#}"),
        }
        self.refresh_folder_panel(id);
    }

    fn folder_row_action(
        &self,
        authority: &Authority,
        action: RowAction,
        name: &str,
        path: PathBuf,
    ) -> anyhow::Result<String> {
        anyhow::ensure!(
            self.folder_rows(authority)?
                .iter()
                .any(|row| row.name == name && row.path == path),
            "This shared folder has changed. Review the current list."
        );
        match action {
            RowAction::Mount => self
                .folder_mount(authority, name)
                .map(|()| format!("Requested mount of {name}")),
            RowAction::Unmount => self
                .folder_unmount(authority, name)
                .map(|()| format!("Requested unmount of {name}")),
            RowAction::Forget => self
                .folder_forget(authority, name)
                .map(|()| format!("Requested removal of {name}")),
        }
    }

    fn mounted_folder_action(
        &mut self,
        authority: &Authority,
        action: MountedAction,
        name: &str,
        mounted: MountedFolder,
        ctx: &ComponentContext<Self>,
    ) -> anyhow::Result<String> {
        // The disk check was asynchronous. The source pane or this exact
        // lease could have ended or been replaced while the result was queued.
        anyhow::ensure!(
            still_mounted(&self.folder_rows(authority)?, name, &mounted),
            "The folder mount changed. Reopen its action from the current list."
        );
        match action {
            MountedAction::Browse => {
                self.browse_mounted_share(authority, &mounted.path, ctx)?;
                Ok(format!("Opened {name} in the SFTP drawer."))
            }
            MountedAction::CopyPath => {
                w11term::clipboard::write(&mounted.path)
                    .context("Could not copy the Linux path")?;
                Ok(format!("Copied the Linux path for {name}."))
            }
        }
    }

    pub(super) fn folder_panel_view(&self, panel: &Panel, ctx: &ViewContext<Self>) -> View {
        let origin = panel.token;
        let a = &panel.authority;
        let heading: [View; 4] = [
            TextBlock::new()
                .font_size(26.0)
                .text("Shared folders")
                .into(),
            TextBlock::new()
                .text(format!(
                    "{} · {}@{}:{} · Read-only",
                    a.profile, a.user, a.host, a.port
                ))
                .into(),
            TextBlock::new()
                .text(&a.fingerprint)
                .text_wrapping(TextWrapping::Wrap)
                .into(),
            Button::new()
                .is_enabled(self.tab_for_pane(a.pane).is_some())
                .on_click(ctx.callback(move |_| message(origin, Edit::Back)))
                .content("Back to terminal"),
        ];
        let mut items: Vec<_> = heading
            .into_iter()
            .zip(["heading", "endpoint", "key", "back"])
            .map(|(element, key)| KeyedView::new(key, element))
            .collect();
        if self.folder_authority(a.pane).as_ref() == Some(a) {
            if panel.rows.is_empty() && panel.draft.is_none() {
                items.push(KeyedView::new("empty", TextBlock::new().text("Choose a Windows folder to mount under ~/windows/ on this SSH account.").text_wrapping(TextWrapping::Wrap)));
            }
            for row in &panel.rows {
                let mounted_button = |action, label, accessible: String| {
                    let name = row.name.clone();
                    let generation = row.mounted.as_ref().map(|mount| mount.generation.clone());
                    Button::new()
                        .automation_name(accessible)
                        .is_enabled(
                            generation.is_some()
                                && row.status == FolderMountStatus::Mounted
                                && panel.resolving.is_none()
                                && panel.pending.is_none()
                                && panel.draft.is_none(),
                        )
                        .on_click(ctx.callback(move |_| match &generation {
                            Some(generation) => message(
                                origin,
                                Edit::Mounted(action, name.clone(), generation.clone()),
                            ),
                            None => Message::ControlIdle,
                        }))
                        .content(label)
                };
                let row_button = |action, text, enabled| {
                    let name = row.name.clone();
                    let path = row.path.clone();
                    Button::new()
                        .automation_name(format!("{text} {name}"))
                        .is_enabled(enabled)
                        .on_click(ctx.callback(move |_| {
                            message(origin, Edit::Row(action, name.clone(), path.clone()))
                        }))
                        .content(text)
                };
                let stopped = matches!(
                    row.status,
                    FolderMountStatus::Stopped | FolderMountStatus::Failed(_)
                );
                items.push(KeyedView::new(
                    format!("share/{}", row.name),
                    Border::new()
                        .padding(10.0)
                        .corner_radius(6.0)
                        .background(Color::rgb(37, 39, 44))
                        .content(
                            StackPanel::new().spacing(4.0).children((
                                TextBlock::new()
                                    .text(row.mounted.as_ref().map_or_else(
                                        || format!("~/windows/{}", row.name),
                                        |mount| mount.path.clone(),
                                    ))
                                    .font_size(17.0),
                                TextBlock::new()
                                    .text(row.path.display().to_string())
                                    .text_wrapping(TextWrapping::Wrap),
                                TextBlock::new()
                                    .text(status_text(&row.status))
                                    .text_wrapping(TextWrapping::Wrap),
                                StackPanel::new()
                                    .orientation(Orientation::Horizontal)
                                    .spacing(8.0)
                                    .children((
                                        if stopped {
                                            row_button(RowAction::Mount, "Mount", true)
                                        } else {
                                            row_button(
                                                RowAction::Unmount,
                                                "Unmount",
                                                !matches!(
                                                    row.status,
                                                    FolderMountStatus::Unmounting
                                                ),
                                            )
                                        },
                                        row_button(RowAction::Forget, "Forget", true),
                                    )),
                                StackPanel::new()
                                    .orientation(Orientation::Horizontal)
                                    .spacing(8.0)
                                    .children((
                                        mounted_button(
                                            MountedAction::Browse,
                                            "Open in SFTP drawer",
                                            format!("Open {} in SFTP drawer", row.name),
                                        ),
                                        mounted_button(
                                            MountedAction::CopyPath,
                                            "Copy Linux path",
                                            format!("Copy Linux path for {}", row.name),
                                        ),
                                    )),
                            )),
                        ),
                ));
            }
            if let Some((path, name)) = &panel.draft {
                items.push(KeyedView::new("draft", StackPanel::new().spacing(6.0).children((
                    TextBlock::new().text(path.display().to_string()).text_wrapping(TextWrapping::Wrap),
                    TextBlock::new().text("Mount name"),
                    TextBox::new().automation_name("Mount name").text(name)
                        .on_text_changed(ctx.callback(move |text| message(origin, Edit::Name(text)))),
                    TextBlock::new().text(if valid_mount_name(name) {
                        format!("Remember this folder and mount it read-only at ~/windows/{name}")
                    } else { "Use 1–64 letters, digits, dots, underscores or hyphens. Start with a letter or digit.".to_owned() }).text_wrapping(TextWrapping::Wrap),
                    StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children((
                        Button::new().style(ButtonStyle::Accent).is_enabled(valid_mount_name(name))
                            .on_click(ctx.callback(move |_| message(origin, Edit::Save))).content("Save folder"),
                        Button::new().on_click(ctx.callback(move |_| message(origin, Edit::Cancel))).content("Cancel"),
                    ))
                ))));
            } else {
                items.push(KeyedView::new(
                    "add",
                    Button::new()
                        .is_enabled(panel.pending.is_none())
                        .on_click(ctx.callback(move |_| message(origin, Edit::Add)))
                        .content("Add folder…"),
                ));
            }
            items.push(KeyedView::new(
                "feedback",
                TextBlock::new()
                    .text(&panel.feedback)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        } else {
            items.push(KeyedView::new("expired", TextBlock::new().text("This folder view has expired. Reopen Shared folders from a connected SSH pane.").text_wrapping(TextWrapping::Wrap)));
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
    fn a_queued_mount_action_requires_the_original_live_mount() {
        let original = MountedFolder {
            generation: "first".into(),
            path: "/srv/pilot 雪/windows/docs".into(),
        };
        let mut rows = vec![FolderRow {
            name: "docs".into(),
            path: r"C:\selected".into(),
            status: FolderMountStatus::Mounted,
            mounted: Some(original.clone()),
        }];
        assert!(still_mounted(&rows, "docs", &original));
        assert!(!still_mounted(&rows, "other", &original));
        rows[0].status = FolderMountStatus::Unmounting;
        assert!(!still_mounted(&rows, "docs", &original));
        rows[0].status = FolderMountStatus::Mounted;
        rows[0].mounted.as_mut().unwrap().generation = "remounted".into();
        assert!(!still_mounted(&rows, "docs", &original));
        rows[0].mounted = None;
        assert!(!still_mounted(&rows, "docs", &original));
    }

    #[test]
    fn folder_picker_suggests_an_editable_valid_name_for_unicode_and_long_paths() {
        for (local, expected) in [
            ("Selected folder", "Selected-folder"),
            ("資料", "files"),
            ("._-Notes", "Notes"),
            ("Level_2.1", "Level_2.1"),
        ] {
            let suggested = suggested_name(std::path::Path::new(local));
            assert_eq!(suggested, expected);
            assert!(valid_mount_name(&suggested));
        }
        let long = suggested_name(std::path::Path::new(&"a".repeat(200)));
        assert_eq!(long.len(), 64);
        assert!(valid_mount_name(&long));
    }
}
