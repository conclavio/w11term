use super::*;
use std::path::PathBuf;
use w11term::{
    control::PacketCaptureOptions,
    ssh::{PacketCapture, PacketCapturePlan, open_capture_in_wireshark},
};

pub(super) const LABEL: &str = "Packet capture";

#[derive(Clone, Copy)]
pub enum Field {
    Interface,
    Filter,
    Seconds,
    Snapshot,
    Budget,
    PacketLimit,
    Scratch,
}

pub enum Action {
    Open,
    Close,
    Edit(Field, String),
    Tmpfs(bool),
    Review,
    Save,
    ChooseWireshark,
    OpenWireshark,
    Finished(Arc<()>, Result<Option<Outcome>, String>),
}

pub enum Outcome {
    Reviewed(u64, Box<PacketCapturePlan>),
    Destination(PathBuf),
    Saved(PacketCapture),
    Wireshark(PathBuf),
    Opened,
}

struct Inputs {
    interface: String,
    filter: String,
    seconds: String,
    snapshot: String,
    budget: String,
    packet_limit: String,
    scratch: String,
    tmpfs: bool,
}

impl Default for Inputs {
    fn default() -> Self {
        Self {
            interface: String::new(),
            filter: String::new(),
            seconds: PacketCaptureOptions::DEFAULT_SECONDS.to_string(),
            snapshot: PacketCaptureOptions::DEFAULT_SNAP_LENGTH.to_string(),
            budget: PacketCaptureOptions::DEFAULT_MAX_MIB.to_string(),
            packet_limit: String::new(),
            scratch: String::new(),
            tmpfs: false,
        }
    }
}

impl Inputs {
    fn options(&self) -> anyhow::Result<PacketCaptureOptions> {
        PacketCaptureOptions {
            interface: self.interface.trim().into(),
            filter: self.filter.clone(),
            seconds: self
                .seconds
                .trim()
                .parse()
                .context("Enter a duration from 1 to 60 seconds")?,
            snap_length: self
                .snapshot
                .trim()
                .parse()
                .context("Enter a snap length from 64 to 262144 bytes")?,
            max_mib: self
                .budget
                .trim()
                .parse()
                .context("Enter a capture budget from 1 to 64 MiB")?,
            packet_limit: if self.packet_limit.trim().is_empty() {
                None
            } else {
                Some(
                    self.packet_limit
                        .trim()
                        .parse()
                        .context("Enter a positive packet limit")?,
                )
            },
            scratch_directory: if self.scratch.is_empty() {
                PacketCaptureOptions::default_scratch_directory(self.tmpfs).into()
            } else {
                self.scratch.clone()
            },
            require_tmpfs: self.tmpfs,
        }
        .validate()
    }
}

struct Review {
    pane: u64,
    plan: PacketCapturePlan,
}

impl Review {
    fn matches(&self, active: Option<flame_graph::Connection>) -> bool {
        active
            == Some(flame_graph::Connection {
                pane: self.pane,
                generation: self.plan.connection_generation,
            })
    }
}

#[derive(Default)]
pub(super) struct Panel {
    pub(super) open: bool,
    inputs: Inputs,
    review: Option<Review>,
    saved: Option<PacketCapture>,
    wireshark: Option<PathBuf>,
    pending: Option<Arc<()>>,
    status: Option<String>,
}

fn message(action: Action) -> Message {
    Message::PacketCapture(action)
}

fn text(key: &str, value: impl Into<String>) -> KeyedView {
    KeyedView::new(
        key,
        TextBlock::new()
            .text(value.into())
            .text_wrapping(TextWrapping::Wrap),
    )
}

impl Panel {
    fn begin(&mut self, status: &str) -> Arc<()> {
        let ticket = Arc::new(());
        self.pending = Some(ticket.clone());
        self.status = Some(status.into());
        ticket
    }

    fn view(
        &self,
        active: Option<flame_graph::Connection>,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let idle = self.pending.is_none();
        let button = |label: &str, enabled, action: fn() -> Action| {
            Button::new()
                .is_enabled(enabled)
                .on_click(ctx.callback(move |_| message(action())))
                .content(label)
        };
        let input = |key, field, label, value: &str| {
            KeyedView::new(
                key,
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new()
                        .text(label)
                        .text_wrapping(TextWrapping::Wrap),
                    TextBox::new()
                        .automation_name(label)
                        .is_enabled(idle)
                        .text(value)
                        .on_text_changed(
                            ctx.callback(move |value| message(Action::Edit(field, value))),
                        ),
                )),
            )
        };
        let inputs = &self.inputs;
        let destination = PacketCaptureOptions::default_scratch_directory(inputs.tmpfs);
        let mut fields = vec![
            input(
                "interface",
                Field::Interface,
                "Linux network interface",
                &inputs.interface,
            ),
            input(
                "filter",
                Field::Filter,
                "Capture filter (optional)",
                &inputs.filter,
            ),
            input(
                "seconds",
                Field::Seconds,
                "Duration in seconds",
                &inputs.seconds,
            ),
            input(
                "snapshot",
                Field::Snapshot,
                "Snap length in bytes",
                &inputs.snapshot,
            ),
            input(
                "budget",
                Field::Budget,
                "Maximum capture size in MiB",
                &inputs.budget,
            ),
            input(
                "packet-limit",
                Field::PacketLimit,
                "Packet limit (optional)",
                &inputs.packet_limit,
            ),
            KeyedView::new(
                "tmpfs",
                CheckBox::new()
                    .is_checked(inputs.tmpfs)
                    .is_enabled(idle)
                    .on_is_checked_changed(ctx.callback(|checked| message(Action::Tmpfs(checked))))
                    .content("Use RAM-backed scratch storage"),
            ),
            input(
                "scratch",
                Field::Scratch,
                "Remote scratch directory (optional)",
                &inputs.scratch,
            ),
            text(
                "scratch-default",
                format!("Default scratch directory {destination}"),
            ),
            KeyedView::new(
                "review",
                button(
                    "Review packet capture",
                    idle && active.is_some() && !inputs.interface.trim().is_empty(),
                    || Action::Review,
                ),
            ),
        ];
        if active.is_none() {
            fields.push(text("choose-pane", "Select a connected Linux SSH pane."));
        }
        if let Some(review) = &self.review {
            let plan = &review.plan;
            let ready = review.matches(active);
            fields.extend([
                text(
                    "endpoint",
                    format!(
                        "{}@{}:{}\nHost key {}",
                        plan.user, plan.host, plan.port, plan.host_key_fingerprint
                    ),
                ),
                text(
                    "reviewed-scratch",
                    format!("{} ({})", plan.scratch_directory, plan.scratch_filesystem),
                ),
                text(
                    "limits",
                    format!(
                        "{} seconds · {} MiB · Packet ceiling {}",
                        plan.options.seconds, plan.options.max_mib, plan.effective_packet_limit
                    ),
                ),
                text(
                    "arguments",
                    format!("tcpdump {}", serde_json::json!(&plan.tcpdump_arguments)),
                ),
                KeyedView::new(
                    "save",
                    button("Capture and save…", idle && ready, || Action::Save),
                ),
            ]);
            if !ready {
                fields.push(text(
                    "stale",
                    "Return to the reviewed SSH connection or review the selected pane.",
                ));
            }
        }
        if let Some(status) = &self.status {
            fields.push(text("status", status));
        }
        if let Some(saved) = &self.saved {
            let reason = match saved.stop_reason {
                "time-limit" => "Time limit reached",
                "packet-limit" => "Packet limit reached",
                "byte-budget" => "Packet ceiling for the byte budget reached",
                _ => "Capture ended",
            };
            fields.extend([
                text("saved-path", saved.path.display().to_string()),
                text(
                    "saved-counts",
                    format!(
                        "{reason}\nPackets captured {} · {} bytes",
                        saved.packets, saved.bytes
                    ),
                ),
            ]);
            if let Some(dropped) = saved.dropped_by_kernel {
                fields.push(text(
                    "drops",
                    format!("Packets dropped by kernel {dropped}"),
                ));
            }
            fields.extend([
                KeyedView::new(
                    "open",
                    button("Open in Wireshark", idle, || Action::OpenWireshark),
                ),
                KeyedView::new(
                    "choose-wireshark",
                    button("Choose Wireshark executable…", idle, || {
                        Action::ChooseWireshark
                    }),
                ),
            ]);
        }
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .children((
                Grid::new()
                    .columns([GridLength::Star(1.0), GridLength::Auto])
                    .column_spacing(8.0)
                    .margin(12.0)
                    .children((
                        TextBlock::new().text(LABEL).font_size(18.0),
                        Button::new()
                            .grid_column(1)
                            .automation_name("Close packet capture")
                            .on_click(ctx.callback(|_| message(Action::Close)))
                            .content("Close"),
                    )),
                Border::new().grid_row(1).padding(12.0).content(
                    ScrollViewer::new()
                        .content(StackPanel::new().spacing(8.0).keyed_children(fields)),
                ),
            ))
    }
}

fn default_wireshark() -> Option<PathBuf> {
    ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(|root| PathBuf::from(root).join("Wireshark").join("Wireshark.exe"))
        .find(|path| path.is_absolute() && path.is_file())
}

impl TerminalApp {
    pub(super) fn with_packet_capture(&self, content: View, ctx: &ViewContext<Self>) -> View {
        let width = (self.width * 0.4).clamp(285.0, 520.0);
        Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Pixel(width)])
            .keyed_children([
                KeyedView::new("terminal", Border::new().content(content)),
                KeyedView::new(
                    "packet-capture",
                    Border::new()
                        .grid_column(1)
                        .background(Color::rgb(35, 35, 35))
                        .content(self.packet_capture.view(self.profile_connection(), ctx)),
                ),
            ])
    }

    pub(super) fn packet_capture_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        match action {
            Action::Open => {
                self.command_reference.close();
                self.triage.open = false;
                self.flame_graph.open = false;
                self.packet_capture.open = true;
            }
            Action::Close => self.packet_capture.open = false,
            Action::Finished(ticket, result) => self.packet_capture_finished(ticket, result, ctx),
            action if self.packet_capture.open && self.packet_capture.pending.is_none() => {
                self.packet_capture_input(action, ctx);
            }
            _ => (),
        }
    }

    fn packet_capture_input(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        let panel = &mut self.packet_capture;
        match action {
            Action::Edit(field, value) => {
                let current = match field {
                    Field::Interface => &mut panel.inputs.interface,
                    Field::Filter => &mut panel.inputs.filter,
                    Field::Seconds => &mut panel.inputs.seconds,
                    Field::Snapshot => &mut panel.inputs.snapshot,
                    Field::Budget => &mut panel.inputs.budget,
                    Field::PacketLimit => &mut panel.inputs.packet_limit,
                    Field::Scratch => &mut panel.inputs.scratch,
                };
                if *current != value {
                    *current = value;
                    panel.review = None;
                    panel.status = None;
                }
            }
            Action::Tmpfs(checked) if checked != panel.inputs.tmpfs => {
                panel.inputs.tmpfs = checked;
                panel.review = None;
                panel.status = None;
            }
            Action::Review => self.review_packet_capture(ctx),
            Action::Save => self.choose_capture_file(false, ctx),
            Action::ChooseWireshark => self.choose_capture_file(true, ctx),
            Action::OpenWireshark => {
                let Some(saved) = &panel.saved else { return };
                let capture = saved.path.clone();
                let configured = panel.wireshark.clone();
                let ticket = panel.begin("Opening Wireshark…");
                work(ctx, ticket, move || {
                    let executable = configured.or_else(default_wireshark).context(
                        "Wireshark was not found. Choose its executable to open the saved capture.",
                    )?;
                    open_capture_in_wireshark(&capture, &executable)?;
                    Ok(Some(Outcome::Opened))
                });
            }
            _ => (),
        }
    }

    fn review_packet_capture(&mut self, ctx: &ComponentContext<Self>) {
        let result = self.packet_capture.inputs.options().and_then(|options| {
            let session = self
                .triage_session()
                .context("Select a connected Linux SSH pane")?;
            Ok((session.pane_id, session.triage_target()?, options))
        });
        let panel = &mut self.packet_capture;
        panel.review = None;
        let (pane, target, options) = match result {
            Ok(review) => review,
            Err(error) => {
                panel.status = Some(format!("{error:#}"));
                return;
            }
        };
        let ticket = panel.begin("Reviewing the packet capture…");
        work(ctx, ticket, move || {
            target
                .preview_packet_capture_blocking(options)
                .map(|plan| Some(Outcome::Reviewed(pane, Box::new(plan))))
        });
    }

    fn choose_capture_file(&mut self, wireshark: bool, ctx: &ComponentContext<Self>) {
        let active = self.profile_connection();
        let panel = &mut self.packet_capture;
        if wireshark {
            if panel.saved.is_none() {
                return;
            }
        } else if !panel
            .review
            .as_ref()
            .is_some_and(|review| review.matches(active))
        {
            return;
        }
        let Some(owner) = self.keyboard.hwnd().map(|window| window.0 as usize) else {
            return;
        };
        let initial = self.store.path();
        let purpose = if wireshark {
            file_picker::Purpose::WiresharkExecutable(
                panel.wireshark.clone().unwrap_or_else(|| initial.into()),
            )
        } else {
            file_picker::Purpose::SavePacketCapture(
                panel
                    .saved
                    .as_ref()
                    .map_or_else(|| initial.into(), |saved| saved.path.clone()),
            )
        };
        let ticket = panel.begin("Choosing a Windows file…");
        work(ctx, ticket, move || {
            file_picker::pick(owner, purpose).map(|selected| {
                selected.map(|file| {
                    if wireshark {
                        Outcome::Wireshark(file.path)
                    } else {
                        Outcome::Destination(file.path)
                    }
                })
            })
        });
    }

    fn packet_capture_finished(
        &mut self,
        ticket: Arc<()>,
        result: Result<Option<Outcome>, String>,
        ctx: &ComponentContext<Self>,
    ) {
        let panel = &mut self.packet_capture;
        if panel
            .pending
            .take_if(|pending| Arc::ptr_eq(pending, &ticket))
            .is_none()
        {
            return;
        }
        panel.status = None;
        match result {
            Ok(Some(Outcome::Reviewed(pane, plan))) => {
                panel.review = Some(Review { pane, plan: *plan })
            }
            Ok(Some(Outcome::Destination(path))) if panel.open => {
                self.save_packet_capture(path, ctx)
            }
            Ok(Some(Outcome::Saved(capture))) => {
                panel.saved = Some(capture);
                panel.status = Some("Saved packet capture".into());
            }
            Ok(Some(Outcome::Wireshark(path))) => {
                panel.status = Some(format!("Wireshark executable {}", path.display()));
                panel.wireshark = Some(path);
            }
            Ok(Some(Outcome::Opened)) => panel.status = Some("Opened capture in Wireshark".into()),
            Ok(_) => (),
            Err(error) => panel.status = Some(error),
        }
    }

    fn save_packet_capture(&mut self, path: PathBuf, ctx: &ComponentContext<Self>) {
        let result = (|| -> anyhow::Result<_> {
            let review = self
                .packet_capture
                .review
                .as_ref()
                .filter(|review| review.matches(self.profile_connection()))
                .context("The reviewed SSH connection changed. Review again.")?;
            let target = self
                .triage_session()
                .context("Select the reviewed SSH pane")?
                .triage_target()?;
            Ok((target, review.plan.clone()))
        })();
        let panel = &mut self.packet_capture;
        let (target, plan) = match result {
            Ok(job) => job,
            Err(error) => {
                panel.status = Some(format!("{error:#}"));
                return;
            }
        };
        let ticket = panel.begin("Capturing packets and transferring the completed file…");
        work(ctx, ticket, move || {
            target
                .save_packet_capture_blocking(plan, path)
                .map(|capture| Some(Outcome::Saved(capture)))
        });
    }
}

fn work(
    ctx: &ComponentContext<TerminalApp>,
    ticket: Arc<()>,
    task: impl FnOnce() -> anyhow::Result<Option<Outcome>> + Send + 'static,
) {
    let rejected = message(Action::Finished(
        ticket.clone(),
        Err("Packet capture worker unavailable".into()),
    ));
    ctx.spawn_background_with_rejection(
        move |scope| {
            let result = if scope.is_cancelled() {
                Ok(None)
            } else {
                task()
            };
            message(Action::Finished(
                ticket,
                result.map_err(|error| format!("{error:#}")),
            ))
        },
        rejected,
    );
}
