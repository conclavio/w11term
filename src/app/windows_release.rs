use super::*;
use std::path::PathBuf;
use w11term::{
    sftp::TransferCancel,
    windows_release::{self, LATEST_PAGE, Latest},
};

pub enum Action {
    Expand(bool),
    Check,
    Completed(Result<Latest, String>),
    OpenPage,
    SaveZip(String),
    CancelZip(Arc<()>),
    ZipProgress(Arc<()>, u64),
    ZipCompleted(Arc<()>, Result<Option<PathBuf>, String>),
}

#[derive(Default)]
enum Check {
    #[default]
    Idle,
    Running,
    Ready(Latest),
    Failed(String),
}

#[derive(Default)]
pub(super) struct Panel {
    expanded: bool,
    state: Check,
    download: Option<Download>,
    download_status: Option<String>,
}

struct Download {
    ticket: Arc<()>,
    cancel: TransferCancel,
    received: Option<u64>,
    _timer: Option<crate::ui_timer::RetryTimer>,
}

impl Panel {
    pub(super) fn view(&self, ctx: &ViewContext<TerminalApp>) -> View {
        let version = windows_release::VERSION;
        let current = windows_release::build_commit();
        let running = match current {
            Some(commit) => format!("Running w11term {version} · build-{}", &commit[..12]),
            None => format!("Running w11term {version} · local build"),
        };
        let mut fields = vec![KeyedView::new("current", TextBlock::new().text(running))];
        match &self.state {
            Check::Idle => {}
            Check::Running => fields.push(KeyedView::new(
                "status",
                TextBlock::new().text("Checking GitHub for the latest Windows release…"),
            )),
            Check::Failed(error) => fields.push(KeyedView::new(
                "status",
                TextBlock::new()
                    .text(format!("Release check failed · {error}"))
                    .text_wrapping(TextWrapping::Wrap),
            )),
            Check::Ready(release) => {
                fields.push(KeyedView::new(
                    "status",
                    TextBlock::new()
                        .text(if current == Some(release.commit.as_str()) {
                            "This is the latest published Windows build".to_owned()
                        } else {
                            format!("Latest Windows build · {}", release.tag())
                        })
                        .text_wrapping(TextWrapping::Wrap),
                ));
                fields.push(KeyedView::new(
                    "published",
                    TextBlock::new().text(format!(
                        "Published {} · {:.1} MiB ZIP",
                        release.published,
                        release.archive_bytes as f64 / (1024.0 * 1024.0)
                    )),
                ));
                self.download_view(&mut fields, release, ctx);
            }
        }
        let page_label = match &self.state {
            Check::Ready(_) => "Open release page on GitHub",
            _ => "Open Latest on GitHub",
        };
        fields.push(KeyedView::new(
            "actions",
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    Button::new()
                        .is_enabled(
                            self.download.is_none() && !matches!(self.state, Check::Running),
                        )
                        .on_click(ctx.callback(|_| Message::WindowsRelease(Action::Check)))
                        .content("Check latest Windows release"),
                    HyperlinkButton::new()
                        .on_click(ctx.callback(|_| Message::WindowsRelease(Action::OpenPage)))
                        .content(page_label),
                )),
        ));
        Expander::new()
            .automation_name("Windows release")
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .is_expanded(self.expanded)
            .on_is_expanded_changed(
                ctx.callback(|expanded| Message::WindowsRelease(Action::Expand(expanded))),
            )
            .slots([
                SlotView::new(
                    ExpanderSlot::Header,
                    TextBlock::new().text("Windows release"),
                ),
                SlotView::new(
                    ExpanderSlot::Content,
                    StackPanel::new().spacing(6.0).keyed_children(fields),
                ),
            ])
    }

    fn download_view(
        &self,
        fields: &mut Vec<KeyedView>,
        release: &Latest,
        ctx: &ViewContext<TerminalApp>,
    ) {
        let tag = release.tag().to_owned();
        let total = release.archive_bytes;
        fields.push(KeyedView::new(
            "save-zip",
            Button::new()
                .is_enabled(self.download.is_none())
                .on_click(
                    ctx.callback(move |_| Message::WindowsRelease(Action::SaveZip(tag.clone()))),
                )
                .content("Save verified ZIP"),
        ));
        if let Some(download) = &self.download {
            let cancelling = download.cancel.is_cancelled();
            let ticket = download.ticket.clone();
            let status = if cancelling {
                "Cancelling the ZIP download…".to_owned()
            } else if let Some(received) = download.received {
                let mib = 1024.0 * 1024.0;
                format!(
                    "Downloading and verifying · {:.1} / {:.1} MiB",
                    received as f64 / mib,
                    total as f64 / mib
                )
            } else {
                "Preparing ZIP download…".into()
            };
            fields.push(KeyedView::new(
                "download-progress",
                TextBlock::new().text(status),
            ));
            fields.push(KeyedView::new(
                "cancel-zip",
                Button::new()
                    .is_enabled(!cancelling)
                    .on_click(ctx.callback(move |_| {
                        Message::WindowsRelease(Action::CancelZip(ticket.clone()))
                    }))
                    .content("Cancel ZIP download"),
            ));
        }
        if let Some(status) = &self.download_status {
            fields.push(KeyedView::new(
                "download-result",
                TextBlock::new()
                    .text(status)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
    }

    fn save_zip(&mut self, owner: usize, release: Latest, ctx: &ComponentContext<TerminalApp>) {
        let ticket = Arc::new(());
        let cancel = TransferCancel::default();
        let (progress, observed) = tokio::sync::watch::channel(None);
        let sender = ctx.sender();
        let running = ticket.clone();
        let mut delivered = None;
        let timer = crate::ui_timer::RetryTimer::new(move || {
            let received = *observed.borrow();
            if let Some(bytes) = received
                && received != delivered
                && sender.send(Message::WindowsRelease(Action::ZipProgress(
                    running.clone(),
                    bytes,
                )))
            {
                delivered = received;
            }
        })
        .ok();
        self.download_status = None;
        self.download = Some(Download {
            ticket: ticket.clone(),
            cancel: cancel.clone(),
            received: None,
            _timer: timer,
        });
        let rejected = Message::WindowsRelease(Action::ZipCompleted(
            ticket.clone(),
            Err("Download worker unavailable".into()),
        ));
        ctx.spawn_background_with_rejection(
            move |scope| {
                let result = (|| -> anyhow::Result<_> {
                    let Some(file) = file_picker::pick(
                        owner,
                        file_picker::Purpose::SaveWindowsZip(release.suggested_zip_name()),
                    )?
                    else {
                        return Ok(None);
                    };
                    progress.send_replace(Some(0));
                    let mut reported = 0;
                    release.save_zip(
                        &file.path,
                        || cancel.is_cancelled() || scope.is_cancelled(),
                        |received| {
                            if received - reported >= 512 * 1024
                                || received == release.archive_bytes
                            {
                                reported = received;
                                progress.send_replace(Some(received));
                            }
                        },
                    )?;
                    Ok(Some(file.path))
                })()
                .map_err(|error| format!("{error:#}"));
                Message::WindowsRelease(Action::ZipCompleted(ticket, result))
            },
            rejected,
        );
    }

    pub(super) fn update(
        &mut self,
        action: Action,
        owner: Option<usize>,
        ctx: &ComponentContext<TerminalApp>,
    ) {
        match action {
            Action::Expand(expanded) => self.expanded = expanded,
            Action::Check if self.download.is_none() && !matches!(self.state, Check::Running) => {
                self.state = Check::Running;
                self.download_status = None;
                ctx.spawn_background_with_rejection(
                    |scope| {
                        let result = windows_release::check(|| scope.is_cancelled())
                            .map_err(|error| format!("{error:#}"));
                        Message::WindowsRelease(Action::Completed(result))
                    },
                    Message::WindowsRelease(Action::Completed(Err(
                        "Release check worker unavailable".into(),
                    ))),
                );
            }
            Action::Completed(result) if matches!(self.state, Check::Running) => {
                self.state = match result {
                    Ok(release) => Check::Ready(release),
                    Err(error) => Check::Failed(error),
                };
            }
            Action::OpenPage => match &self.state {
                Check::Ready(release) => TerminalApp::open_link(&release.page()),
                _ => TerminalApp::open_link(LATEST_PAGE),
            },
            Action::SaveZip(tag) if self.download.is_none() => {
                if let (Some(owner), Check::Ready(release)) = (owner, &self.state)
                    && tag == release.tag()
                {
                    self.save_zip(owner, release.clone(), ctx);
                }
            }
            Action::CancelZip(ticket) => {
                if let Some(download) = self
                    .download
                    .as_ref()
                    .filter(|job| Arc::ptr_eq(&job.ticket, &ticket))
                {
                    download.cancel.cancel();
                }
            }
            Action::ZipProgress(ticket, received) => {
                if let Some(download) = self
                    .download
                    .as_mut()
                    .filter(|job| Arc::ptr_eq(&job.ticket, &ticket))
                {
                    download.received = Some(received);
                }
            }
            Action::ZipCompleted(ticket, result) => {
                if let Some(download) = self
                    .download
                    .take_if(|job| Arc::ptr_eq(&job.ticket, &ticket))
                {
                    self.download_status = match result {
                        Ok(Some(path)) => Some(format!("Verified ZIP saved to {}", path.display())),
                        Ok(None) => None,
                        Err(_) if download.cancel.is_cancelled() => {
                            Some("ZIP download cancelled".into())
                        }
                        Err(error) => Some(format!("Could not save the ZIP · {error}")),
                    };
                }
            }
            _ => {}
        }
    }
}
