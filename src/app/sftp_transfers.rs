use super::{
    file_drawer::{TransferId, TransferView, transfer_progress_views},
    *,
};
use std::cmp::Reverse;
use w11term::{
    control::SftpTransferStatus,
    events::{
        RECENT_SFTP_TRANSFER_LIMIT, RecentSftpTransfer, SavedDownload, SftpTransferDetails,
        SftpTransferItem,
    },
    options::SshOptions,
    profiles::bookmark_directory,
    sftp::{TransferOperation, TransferOutcome, TransferPhase},
};

mod report;

pub(super) const LABEL: &str = "SFTP transfers";
const SAVE_HISTORY: &str = "Save recent transfers as JSON…";

pub enum Action {
    Open,
    Visit(TransferId),
    Cancel(TransferId),
    ShowDownload(TransferId),
    ShowDownloadedItem(TransferId, usize),
    ShowRemoteItem(TransferId, usize),
    ExpandDetails(TransferId, bool),
    SaveHistory,
}

struct Recent<'a> {
    label: &'a str,
    ssh: &'a SshOptions,
    pane: &'a LiveTab,
    result: RecentSftpTransfer,
}

impl TerminalApp {
    pub(super) fn handle_sftp_transfers(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        if !matches!(action, Action::Open)
            && !matches!(
                self.tabs.active().map(|tab| &tab.page),
                Some(Page::SftpTransfers(_))
            )
        {
            return;
        }
        match action {
            Action::Open => {
                let existing = self
                    .tabs
                    .entries()
                    .iter()
                    .find(|entry| matches!(entry.value.page, Page::SftpTransfers(_)))
                    .map(|entry| entry.id);
                if let Some(id) = existing {
                    self.tabs.select(id);
                    self.sync_selection(ctx);
                    self.activate(ctx);
                } else {
                    self.insert_tab(
                        AppTab {
                            label: LABEL.into(),
                            page: Page::SftpTransfers(None),
                        },
                        ctx,
                    );
                }
            }
            Action::Visit(transfer) | Action::Cancel(transfer) => {
                let pane = transfer.pane;
                let Some(id) = self.tab_for_pane(pane) else {
                    return;
                };
                let Some(Page::Terminal(tab)) = self.tabs.get(id).map(|tab| &tab.page) else {
                    return;
                };
                let generation = tab.session.frames.load().connection_generation;
                if !tab
                    .files
                    .active_transfer(generation)
                    .is_some_and(|current| current.id == transfer)
                {
                    return;
                }
                if matches!(action, Action::Cancel(_)) {
                    self.file_action(FileAction::CancelTransfer(transfer), ctx);
                } else if self.tabs.select(id) {
                    self.file_action(FileAction::ShowTransfer(pane), ctx);
                    self.sync_selection(ctx);
                    self.activate(ctx);
                }
            }
            Action::ShowDownload(id) => {
                if let Some((_, _, result)) = self.retained_sftp_transfer(id)
                    && let Some(saved) = result.saved_download
                {
                    explorer::show_download(saved, ctx);
                }
            }
            Action::ShowDownloadedItem(id, index) => {
                if let Some((_, _, result)) = self.retained_sftp_transfer(id)
                    && matches!(result.saved_download, Some(SavedDownload::BatchFolder(_)))
                    && let Some(item) = completed_item(&result, index)
                    && let Some(saved) = save_location(item)
                {
                    explorer::show_download(saved, ctx);
                }
            }
            Action::ShowRemoteItem(id, index) => {
                let Some((tab_id, tab, result)) = self.retained_sftp_transfer(id) else {
                    return;
                };
                let Some((directory, name)) =
                    completed_item(&result, index).and_then(|item| remote_location(&item.remote))
                else {
                    return;
                };
                if let Some(reason) = remote_reveal_blocker(tab, id) {
                    self.window_error = Some(reason.into());
                } else if self.tabs.select(tab_id) {
                    self.file_action(
                        FileAction::Reveal(
                            id.pane,
                            id.generation,
                            directory.into(),
                            name.map(str::to_owned),
                        ),
                        ctx,
                    );
                    self.sync_selection(ctx);
                    self.activate(ctx);
                }
            }
            Action::ExpandDetails(transfer, expanded) => {
                if let Some(id) = self.tabs.active_id()
                    && let Some(Page::SftpTransfers(current)) =
                        self.tabs.get_mut(id).map(|tab| &mut tab.page)
                {
                    if expanded {
                        *current = Some(transfer);
                    } else if *current == Some(transfer) {
                        *current = None;
                    }
                }
            }
            Action::SaveHistory => {
                if self.export.pending.is_some() {
                    return;
                }
                let rows: Vec<_> = self
                    .window_sftp_history()
                    .into_iter()
                    .map(|row| report::Row::capture(row.label, row.ssh, row.result))
                    .collect();
                if !rows.is_empty() {
                    self.begin_text_export(
                        text_export::Subject::SftpTransferHistory,
                        move |output| report::write_json(&rows, output),
                        ctx,
                    );
                }
            }
        }
    }

    pub(super) fn sftp_transfers_view(
        &self,
        expanded: Option<TransferId>,
        ctx: &ViewContext<Self>,
    ) -> View {
        let mut rows: Vec<_> = self
            .window_sftp_transfers()
            .map(|(_, label, ssh, transfer)| {
                transfer_row(transfer_source(label, ssh, transfer.id.pane), transfer, ctx)
            })
            .collect();
        if rows.is_empty() {
            rows.push(KeyedView::new(
                "active-empty",
                TextBlock::new().text("No active SFTP transfers in this window"),
            ));
        }
        let recent = self.window_sftp_history();
        if !recent.is_empty() {
            rows.push(KeyedView::new(
                "recent-heading",
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .column_spacing(8.0)
                    .children((
                        TextBlock::new()
                            .text("Recent transfers")
                            .font_size(18.0)
                            .vertical_alignment(VerticalAlignment::Center),
                        Button::new()
                            .grid_column(1)
                            .automation_name(SAVE_HISTORY)
                            .is_enabled(self.export.pending.is_none())
                            .on_click(ctx.callback(|_| Message::SftpTransfers(Action::SaveHistory)))
                            .content("Save as JSON…"),
                    )),
            ));
            rows.extend(recent.into_iter().map(|row| {
                let show_details = expanded == Some(row.result.id);
                let remote_blocker = if show_details {
                    remote_reveal_blocker(row.pane, row.result.id)
                } else {
                    None
                };
                result_row(
                    transfer_source(row.label, row.ssh, row.result.id.pane),
                    row.result,
                    show_details,
                    remote_blocker,
                    ctx,
                )
            }));
        }
        Grid::new()
            .margin(16.0)
            .row_spacing(12.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                TextBlock::new().text(LABEL).font_size(24.0),
                ScrollViewer::new()
                    .grid_row(1)
                    .automation_name("SFTP transfer activity")
                    .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                    .content(StackPanel::new().spacing(12.0).keyed_children(rows)),
            ))
    }

    fn window_sftp_history(&self) -> Vec<Recent<'_>> {
        let mut recent: Vec<_> = self
            .window_sftp_panes()
            .flat_map(|(_, label, pane, ssh)| {
                pane.session
                    .events
                    .recent_sftp_transfers()
                    .into_iter()
                    .map(move |result| Recent {
                        label,
                        ssh,
                        pane,
                        result,
                    })
            })
            .collect();
        recent.sort_unstable_by_key(|row| Reverse((row.result.finished_at, row.result.id)));
        recent.truncate(RECENT_SFTP_TRANSFER_LIMIT);
        recent
    }

    fn retained_sftp_transfer(
        &self,
        id: TransferId,
    ) -> Option<(TabId, &LiveTab, RecentSftpTransfer)> {
        let tab_id = self.tab_for_pane(id.pane)?;
        let Page::Terminal(tab) = &self.tabs.get(tab_id)?.page else {
            return None;
        };
        let result = tab
            .session
            .events
            .recent_sftp_transfers()
            .into_iter()
            .find(|result| result.id == id)?;
        Some((tab_id, tab, result))
    }

    fn window_sftp_panes(&self) -> impl Iterator<Item = (TabId, &str, &LiveTab, &SshOptions)> {
        self.tabs.entries().iter().filter_map(|entry| {
            let Page::Terminal(tab) = &entry.value.page else {
                return None;
            };
            let Source::Ssh(ssh) = &tab.source else {
                return None;
            };
            Some((entry.id, entry.value.label.as_str(), tab, ssh))
        })
    }

    fn window_sftp_transfers(
        &self,
    ) -> impl Iterator<Item = (TabId, &str, &SshOptions, TransferView<'_>)> {
        self.window_sftp_panes()
            .filter_map(|(id, label, tab, ssh)| {
                let generation = tab.session.frames.load().connection_generation;
                let transfer = tab.files.active_transfer(generation)?;
                Some((id, label, ssh, transfer))
            })
    }

    pub(super) fn control_sftp_transfers(&self, pane: Option<u64>) -> Vec<SftpTransferStatus> {
        self.window_sftp_transfers()
            .filter(|(_, _, _, transfer)| pane.is_none_or(|pane| transfer.id.pane == pane))
            .filter_map(|(tab, label, _, transfer)| {
                Some(SftpTransferStatus {
                    transfer_id: transfer.id,
                    pane: transfer.id.pane,
                    window_id: self.registry.window_id(),
                    tab_id: self.tabs.group_id(tab)?,
                    label: label.into(),
                    operation: transfer.operation,
                    state: transfer.state,
                    remote: transfer.remote.map(str::to_owned),
                    progress: transfer.progress,
                    batch: transfer.batch,
                })
            })
            .collect()
    }
}

fn transfer_source(label: &str, ssh: &SshOptions, pane: u64) -> String {
    format!(
        "{label} · Pane {pane} · {}@{}:{}",
        ssh.user, ssh.host, ssh.port
    )
}

fn transfer_row(
    source: String,
    current: TransferView<'_>,
    ctx: &ViewContext<TerminalApp>,
) -> KeyedView {
    let id = current.id;
    let pane = id.pane;
    let mut title = match current.operation {
        TransferOperation::PrepareUpload => "Preparing upload",
        TransferOperation::UploadFile => "Uploading",
        TransferOperation::UploadFolder => "Uploading folder",
        TransferOperation::DownloadFile => "Downloading",
        TransferOperation::DownloadFolder => "Downloading folder",
    }
    .to_owned();
    if let Some(remote) = current.remote {
        title.push(' ');
        title.push_str(remote);
    }
    if let Some(batch) = current.batch {
        title.push_str(&format!(" · Item {} of {}", batch.item, batch.total));
    }
    let mut details = vec![
        KeyedView::new(
            "source",
            TextBlock::new()
                .text(source)
                .text_wrapping(TextWrapping::Wrap),
        ),
        KeyedView::new(
            "transfer",
            TextBlock::new()
                .text(title)
                .text_wrapping(TextWrapping::Wrap),
        ),
    ];
    if let Some(progress) = current.progress {
        let (bar, bytes) = transfer_progress_views(progress);
        details.push(KeyedView::new("progress", bar));
        details.push(KeyedView::new("bytes", bytes));
    }
    let cancelling = current.state == TransferPhase::Cancelling;
    let cancel_label = if cancelling {
        "Cancelling transfer…"
    } else {
        "Cancel transfer"
    };
    details.push(KeyedView::new(
        "actions",
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children((
                Button::new()
                    .automation_name(format!("Open SFTP drawer for Pane {pane}"))
                    .on_click(ctx.callback(move |_| Message::SftpTransfers(Action::Visit(id))))
                    .content("Open SFTP drawer"),
                Button::new()
                    .automation_name(format!("Cancel transfer for Pane {pane}"))
                    .is_enabled(!cancelling)
                    .on_click(ctx.callback(move |_| Message::SftpTransfers(Action::Cancel(id))))
                    .content(cancel_label),
            )),
    ));
    KeyedView::new(
        pane.to_string(),
        StackPanel::new().spacing(6.0).keyed_children(details),
    )
}

fn result_row(
    source: String,
    result: RecentSftpTransfer,
    expanded: bool,
    remote_blocker: Option<&str>,
    ctx: &ViewContext<TerminalApp>,
) -> KeyedView {
    let id = result.id;
    let batch_download = matches!(result.saved_download, Some(SavedDownload::BatchFolder(_)));
    let outcome = outcome_label(result.outcome);
    let mut details = vec![
        KeyedView::new(
            "source",
            TextBlock::new()
                .text(source)
                .text_wrapping(TextWrapping::Wrap),
        ),
        KeyedView::new(
            "summary",
            TextBlock::new()
                .text(format!("{outcome} · {}", result.message))
                .text_wrapping(TextWrapping::Wrap),
        ),
    ];
    if let Some(saved) = result.saved_download {
        let label = explorer::download_label(&saved);
        details.push(KeyedView::new(
            "download",
            Button::new()
                .automation_name(format!("{label} for transfer {id}"))
                .on_click(ctx.callback(move |_| Message::SftpTransfers(Action::ShowDownload(id))))
                .content(label),
        ));
    }
    if let Some(items) = result.details {
        let mut slots = vec![SlotView::new(
            ExpanderSlot::Header,
            TextBlock::new().text("Transfer item details"),
        )];
        if expanded {
            slots.push(SlotView::new(
                ExpanderSlot::Content,
                item_details(&items, id, batch_download, remote_blocker, ctx),
            ));
        }
        details.push(KeyedView::new(
            "item-details",
            Expander::new()
                .automation_name(format!("Transfer item details for {id}"))
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .is_expanded(expanded)
                .on_is_expanded_changed(
                    ctx.callback(move |open| {
                        Message::SftpTransfers(Action::ExpandDetails(id, open))
                    }),
                )
                .slots(slots),
        ));
    }
    KeyedView::new(
        format!("recent-{id}"),
        StackPanel::new().spacing(6.0).keyed_children(details),
    )
}

fn outcome_label(outcome: TransferOutcome) -> &'static str {
    match outcome {
        TransferOutcome::Completed => "Completed",
        TransferOutcome::Cancelled => "Cancelled",
        TransferOutcome::Failed => "Failed",
        TransferOutcome::Interrupted => "Interrupted",
    }
}

fn item_details(
    details: &SftpTransferDetails,
    transfer: TransferId,
    batch_download: bool,
    remote_blocker: Option<&str>,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    let mut rows: Vec<_> = details
        .completed
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let suffix = format!("for item {} of transfer {transfer}", index + 1);
            let mut actions = Vec::new();
            if remote_location(&item.remote).is_some() {
                let mut button = Button::new()
                    .automation_name(format!("Show remote entry {suffix}"))
                    .is_enabled(remote_blocker.is_none())
                    .on_click(ctx.callback(move |_| {
                        Message::SftpTransfers(Action::ShowRemoteItem(transfer, index))
                    }))
                    .content("Show remote entry");
                if let Some(reason) = remote_blocker {
                    button = button.tooltip_with(title_tools::below(reason));
                }
                actions.push(KeyedView::new("remote", button));
            }
            if batch_download && download_item(item) {
                actions.push(KeyedView::new(
                    "local",
                    Button::new()
                        .automation_name(format!("Show in File Explorer {suffix}"))
                        .on_click(ctx.callback(move |_| {
                            Message::SftpTransfers(Action::ShowDownloadedItem(transfer, index))
                        }))
                        .content("Show in File Explorer"),
                ));
            }
            KeyedView::new(index.to_string(), item_row(item, actions))
        })
        .collect();
    if details.omitted_completed != 0 {
        rows.push(KeyedView::new(
            "omitted",
            TextBlock::new().text(format!(
                "{} additional completed items",
                details.omitted_completed
            )),
        ));
    }
    if let Some(item) = &details.stopped {
        rows.push(KeyedView::new("stopped", item_row(item, Vec::new())));
    }
    if details.not_started != 0 {
        let label = if details.not_started == 1 {
            "item"
        } else {
            "items"
        };
        rows.push(KeyedView::new(
            "not-started",
            TextBlock::new().text(format!("{} {label} did not start", details.not_started)),
        ));
    }
    StackPanel::new().spacing(10.0).keyed_children(rows)
}

fn item_row(item: &SftpTransferItem, actions: Vec<KeyedView>) -> View {
    let operation = match item.operation {
        TransferOperation::UploadFile => "File upload",
        TransferOperation::UploadFolder => "Folder upload",
        TransferOperation::DownloadFile => "File download",
        TransferOperation::DownloadFolder => "Folder download",
        TransferOperation::PrepareUpload => "Upload preparation",
    };
    let mut title = format!("{} · {operation}", outcome_label(item.outcome));
    if let Some(bytes) = item.bytes_transferred {
        use std::fmt::Write;
        let _ = write!(title, " · {bytes} bytes");
    }
    let mut children = vec![
        KeyedView::new(
            "title",
            TextBlock::new()
                .text(title)
                .text_wrapping(TextWrapping::Wrap),
        ),
        KeyedView::new("remote-path", path_text("Remote", &item.remote)),
        KeyedView::new(
            "windows-path",
            path_text("Windows", &item.local.to_string_lossy()),
        ),
    ];
    if !actions.is_empty() {
        children.push(KeyedView::new(
            "actions",
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .keyed_children(actions),
        ));
    }
    StackPanel::new().spacing(2.0).keyed_children(children)
}

fn completed_item(result: &RecentSftpTransfer, index: usize) -> Option<&SftpTransferItem> {
    let item = result.details.as_ref()?.completed.get(index)?;
    (item.outcome == TransferOutcome::Completed).then_some(item)
}

fn download_item(item: &SftpTransferItem) -> bool {
    matches!(
        item.operation,
        TransferOperation::DownloadFile | TransferOperation::DownloadFolder
    )
}

fn save_location(item: &SftpTransferItem) -> Option<SavedDownload> {
    match item.operation {
        TransferOperation::DownloadFile => {
            Some(SavedDownload::File(Arc::from(item.local.as_path())))
        }
        TransferOperation::DownloadFolder => {
            Some(SavedDownload::Folder(Arc::from(item.local.as_path())))
        }
        _ => None,
    }
}

fn remote_location(path: &str) -> Option<(&str, Option<&str>)> {
    let path = bookmark_directory(path)?;
    if path == "/" {
        return Some(("/", None));
    }
    let (parent, name) = path.rsplit_once('/')?;
    Some((if parent.is_empty() { "/" } else { parent }, Some(name)))
}

fn remote_reveal_blocker(tab: &LiveTab, id: TransferId) -> Option<&'static str> {
    if tab.session.ssh_connection_generation() != Some(id.generation) {
        Some("The original SSH connection is unavailable.")
    } else if !tab.files.can_reveal_path() {
        Some("Finish the active operation in the SFTP drawer first.")
    } else {
        None
    }
}

fn path_text(label: &str, path: &str) -> TextBlock {
    use std::fmt::Write;
    let mut line = format!("{label} · ");
    for ch in path.chars() {
        if ch.is_control()
            || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            let _ = write!(line, "{}", ch.escape_unicode());
        } else {
            line.push(ch);
        }
    }
    TextBlock::new()
        .text(line)
        .text_wrapping(TextWrapping::Wrap)
        .is_text_selection_enabled(true)
}

#[cfg(test)]
mod tests {
    use super::remote_location;

    #[test]
    fn transfer_reveal_keeps_literal_names_and_requires_an_absolute_path() {
        assert_eq!(remote_location("/"), Some(("/", None)));
        assert_eq!(remote_location("/report"), Some(("/", Some("report"))));
        assert_eq!(
            remote_location("/some  folder/Ångström $batch/"),
            Some(("/some  folder", Some("Ångström $batch")))
        );
        for invalid in [
            "",
            "relative/file",
            "/a/../b",
            "/a//file",
            "/a/./file",
            "/a/line\nname",
        ] {
            assert_eq!(remote_location(invalid), None, "{invalid:?}");
        }
    }
}
