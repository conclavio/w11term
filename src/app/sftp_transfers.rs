use super::{
    file_drawer::{TransferId, TransferView, transfer_progress_views},
    *,
};
use w11term::{
    control::SftpTransferStatus,
    options::SshOptions,
    sftp::{TransferOperation, TransferPhase},
};

pub(super) const LABEL: &str = "SFTP transfers";

pub enum Action {
    Open,
    Visit(TransferId),
    Cancel(TransferId),
}

impl TerminalApp {
    pub(super) fn handle_sftp_transfers(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        match action {
            Action::Open => {
                let existing = self
                    .tabs
                    .entries()
                    .iter()
                    .find(|entry| matches!(entry.value.page, Page::SftpTransfers))
                    .map(|entry| entry.id);
                if let Some(id) = existing {
                    self.tabs.select(id);
                    self.sync_selection(ctx);
                    self.activate(ctx);
                } else {
                    self.insert_tab(
                        AppTab {
                            label: LABEL.into(),
                            page: Page::SftpTransfers,
                        },
                        ctx,
                    );
                }
            }
            Action::Visit(transfer) | Action::Cancel(transfer) => {
                if !matches!(
                    self.tabs.active().map(|tab| &tab.page),
                    Some(Page::SftpTransfers)
                ) {
                    return;
                }
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
        }
    }

    pub(super) fn sftp_transfers_view(&self, ctx: &ViewContext<Self>) -> View {
        let rows: Vec<_> = self
            .window_sftp_transfers()
            .map(|(_, label, ssh, transfer)| {
                let source = format!(
                    "{label} · Pane {} · {}@{}:{}",
                    transfer.id.pane, ssh.user, ssh.host, ssh.port
                );
                transfer_row(source, transfer, ctx)
            })
            .collect();
        let content: View = if rows.is_empty() {
            TextBlock::new()
                .text("No active SFTP transfers in this window")
                .into()
        } else {
            ScrollViewer::new()
                .automation_name("Active SFTP transfers")
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .content(StackPanel::new().spacing(12.0).keyed_children(rows))
        };
        Grid::new()
            .margin(16.0)
            .row_spacing(12.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                TextBlock::new().text(LABEL).font_size(24.0),
                Border::new().grid_row(1).content(content),
            ))
    }

    fn window_sftp_transfers(
        &self,
    ) -> impl Iterator<Item = (TabId, &str, &SshOptions, TransferView<'_>)> {
        self.tabs.entries().iter().filter_map(|entry| {
            let Page::Terminal(tab) = &entry.value.page else {
                return None;
            };
            let Source::Ssh(ssh) = &tab.source else {
                return None;
            };
            let generation = tab.session.frames.load().connection_generation;
            let transfer = tab.files.active_transfer(generation)?;
            Some((entry.id, entry.value.label.as_str(), ssh, transfer))
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
