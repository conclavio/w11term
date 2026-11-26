use super::{
    file_drawer::{TransferId, TransferView, transfer_progress_views},
    *,
};
use std::cmp::Reverse;
use w11term::{
    control::SftpTransferStatus,
    events::{
        RECENT_SFTP_TRANSFER_LIMIT, RecentSftpTransfer, SftpTransferDetails, SftpTransferItem,
    },
    options::SshOptions,
    sftp::{TransferOperation, TransferOutcome, TransferPhase},
};

pub(super) const LABEL: &str = "SFTP transfers";

pub enum Action {
    Open,
    Visit(TransferId),
    Cancel(TransferId),
    ShowDownload(TransferId),
    ExpandDetails(TransferId, bool),
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
                if let Some(tab) = self.tab_for_pane(id.pane)
                    && let Some(Page::Terminal(tab)) = self.tabs.get(tab).map(|tab| &tab.page)
                    && let Some(saved) = tab
                        .session
                        .events
                        .recent_sftp_transfers()
                        .into_iter()
                        .find(|result| result.id == id)
                        .and_then(|result| result.saved_download)
                {
                    explorer::show_download(saved, ctx);
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
        let mut recent: Vec<_> = self
            .window_sftp_panes()
            .flat_map(|(_, label, tab, ssh)| {
                tab.session
                    .events
                    .recent_sftp_transfers()
                    .into_iter()
                    .map(move |result| (label, ssh, result))
            })
            .collect();
        recent.sort_unstable_by_key(|(_, _, result)| Reverse((result.finished_at, result.id)));
        if !recent.is_empty() {
            rows.push(KeyedView::new(
                "recent-heading",
                TextBlock::new().text("Recent transfers").font_size(18.0),
            ));
            rows.extend(recent.into_iter().take(RECENT_SFTP_TRANSFER_LIMIT).map(
                |(label, ssh, result)| {
                    let show_details = expanded == Some(result.id);
                    result_row(
                        transfer_source(label, ssh, result.id.pane),
                        result,
                        show_details,
                        ctx,
                    )
                },
            ));
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
    ctx: &ViewContext<TerminalApp>,
) -> KeyedView {
    let id = result.id;
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
            slots.push(SlotView::new(ExpanderSlot::Content, item_details(&items)));
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

fn item_details(details: &SftpTransferDetails) -> View {
    let mut rows: Vec<_> = details
        .completed
        .iter()
        .enumerate()
        .map(|(index, item)| KeyedView::new(index.to_string(), item_row(item)))
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
        rows.push(KeyedView::new("stopped", item_row(item)));
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

fn item_row(item: &SftpTransferItem) -> View {
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
    StackPanel::new().spacing(2.0).children((
        TextBlock::new()
            .text(title)
            .text_wrapping(TextWrapping::Wrap),
        path_text("Remote", &item.remote),
        path_text("Windows", &item.local.to_string_lossy()),
    ))
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
