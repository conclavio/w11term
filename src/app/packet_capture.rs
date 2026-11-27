use super::*;
use std::path::PathBuf;
use w11term::{
    control::PacketCaptureOptions,
    ssh::{
        CaptureStop, PacketCapture, PacketCaptureInterfaces, PacketCapturePlan,
        open_capture_in_wireshark,
    },
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
    RotateFiles,
    Scratch,
}

pub enum Action {
    Open,
    Close,
    Edit(Field, String),
    ListInterfaces,
    SelectInterface(flame_graph::Connection, String),
    Tmpfs(bool),
    Review,
    Save,
    Stop(Arc<()>),
    ChooseWireshark,
    OpenWireshark,
    Finished(Arc<()>, Result<Option<Outcome>, String>),
}

pub enum Outcome {
    Interfaces(flame_graph::Connection, PacketCaptureInterfaces),
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
    rotate_files: String,
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
            rotate_files: String::new(),
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
            rotate_files: if self.rotate_files.trim().is_empty() {
                None
            } else {
                Some(
                    self.rotate_files
                        .trim()
                        .parse()
                        .context("Enter 2 to 16 rolling capture files")?,
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
    pub(super) activity: Option<diagnostics::Activity>,
    inputs: Inputs,
    interfaces: Option<(flame_graph::Connection, PacketCaptureInterfaces)>,
    review: Option<Review>,
    saved: Option<PacketCapture>,
    wireshark: Option<PathBuf>,
    pending: Option<Arc<()>>,
    stop: Option<CaptureStop>,
    status: Option<String>,
}

fn message(action: Action) -> Message {
    Message::PacketCapture(action)
}

fn stop_reason(saved: &PacketCapture) -> &'static str {
    match saved.stop_reason {
        "stopped" => "Stopped by request",
        "time-limit" => "Time limit reached",
        "packet-limit" => "Packet limit reached",
        "byte-budget" => "Packet ceiling for the byte budget reached",
        _ => "Capture ended",
    }
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
            KeyedView::new(
                "list-interfaces",
                button("List capture interfaces", idle && active.is_some(), || {
                    Action::ListInterfaces
                }),
            ),
        ];
        if let Some((source, list)) = &self.interfaces
            && Some(*source) == active
            && !list.interfaces.is_empty()
        {
            let source = *source;
            let selected = list
                .interfaces
                .iter()
                .position(|entry| entry.name == inputs.interface);
            let names: Vec<_> = list
                .interfaces
                .iter()
                .map(|entry| entry.name.clone())
                .collect();
            let labels = list.interfaces.iter().map(|entry| {
                if entry.detail.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{} · {}", entry.name, entry.detail)
                }
            });
            fields.push(KeyedView::new(
                "interfaces",
                ComboBox::new()
                    .automation_name("Capture interfaces")
                    .placeholder_text("Choose an interface")
                    .is_enabled(idle)
                    .items_source(labels)
                    .selected_index(selected)
                    .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                        index
                            .and_then(|index| names.get(index))
                            .map_or(Message::ControlIdle, |name| {
                                message(Action::SelectInterface(source, name.clone()))
                            })
                    })),
            ));
        }
        fields.extend([
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
            input(
                "rotate-files",
                Field::RotateFiles,
                "Rolling capture files (optional)",
                &inputs.rotate_files,
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
        ]);
        if let Some((stop, ticket)) = self.stop.as_ref().zip(self.pending.as_ref()) {
            let ticket = ticket.clone();
            fields.insert(
                0,
                KeyedView::new(
                    "stop",
                    Button::new()
                        .is_enabled(!stop.is_requested())
                        .on_click(ctx.callback(move |_| message(Action::Stop(ticket.clone()))))
                        .content("Stop and save"),
                ),
            );
        }
        if active.is_none() {
            fields.push(text("choose-pane", "Select a connected Linux SSH pane."));
        }
        if let Some(review) = &self.review {
            let plan = &review.plan;
            let ready = review.matches(active);
            let limit = plan.effective_packet_limit.map_or_else(
                || "No packet limit".into(),
                |count| format!("Packet ceiling {count}"),
            );
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
                        "{} seconds · {} MiB · {limit}",
                        plan.options.seconds, plan.options.max_mib
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
            if let Some(rotation) = plan.rotation {
                fields.push(text("rotation", format!(
                    "Keep up to {} files · {} bytes per file. The oldest file is replaced when the buffer fills.",
                    rotation.files, rotation.bytes_per_file
                )));
            }
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
            let reason = stop_reason(saved);
            let packets = if saved.rotation.is_some() {
                "Packets retained"
            } else {
                "Packets captured"
            };
            fields.extend([
                text("saved-path", saved.path.display().to_string()),
                text(
                    "saved-counts",
                    format!(
                        "{reason}\n{packets} {} · {} bytes",
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
            if let Some(rotation) = &saved.rotation {
                fields.push(text(
                    "rotation-saved",
                    format!(
                        "Rolling capture · {} captured · {} files kept · {} overwritten",
                        rotation.captured_packets,
                        rotation.retained_files,
                        rotation.overwritten_files
                    ),
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
                Border::new().margin(12.0).content(diagnostics::header(
                    LABEL,
                    "Close packet capture",
                    ctx.callback(|_| message(Action::Close)),
                    ctx,
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
                self.set_diagnostics_drawer(Some(diagnostics::Drawer::PacketCapture));
            }
            Action::Close => self.packet_capture.open = false,
            Action::Stop(ticket) => {
                let panel = &mut self.packet_capture;
                if panel
                    .pending
                    .as_ref()
                    .is_some_and(|pending| Arc::ptr_eq(pending, &ticket))
                    && let Some(stop) = &panel.stop
                {
                    stop.request();
                    if let Some(activity) = &mut panel.activity {
                        activity.stopping();
                    }
                    panel.status = Some("Stop requested. Saving the completed capture…".into());
                }
            }
            Action::Finished(ticket, result) => self.packet_capture_finished(ticket, result, ctx),
            action if self.packet_capture.open && self.packet_capture.pending.is_none() => {
                self.packet_capture_input(action, ctx);
            }
            _ => (),
        }
    }

    fn packet_capture_input(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        let active = self.profile_connection();
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
                    Field::RotateFiles => &mut panel.inputs.rotate_files,
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
            Action::ListInterfaces => self.list_packet_interfaces(ctx),
            Action::SelectInterface(source, name) if active == Some(source) => {
                if panel.inputs.interface != name
                    && panel.interfaces.as_ref().is_some_and(|(bound, list)| {
                        *bound == source && list.interfaces.iter().any(|entry| entry.name == name)
                    })
                {
                    panel.inputs.interface = name;
                    panel.review = None;
                    panel.status = None;
                }
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

    fn list_packet_interfaces(&mut self, ctx: &ComponentContext<Self>) {
        let result = (|| -> anyhow::Result<_> {
            let session = self
                .triage_session()
                .context("Select a connected Linux SSH pane")?;
            let target = session.triage_target()?;
            let source = flame_graph::Connection {
                pane: session.pane_id,
                generation: target.connection_generation(),
            };
            Ok((source, target))
        })();
        let panel = &mut self.packet_capture;
        let (source, target) = match result {
            Ok(value) => value,
            Err(error) => {
                panel.status = Some(format!("{error:#}"));
                return;
            }
        };
        let ticket = panel.begin("Listing capture interfaces…");
        work(ctx, ticket, move || {
            target
                .list_packet_interfaces_blocking()
                .map(|list| Some(Outcome::Interfaces(source, list)))
        });
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
        let active = self.profile_connection();
        let panel = &mut self.packet_capture;
        if panel
            .pending
            .take_if(|pending| Arc::ptr_eq(pending, &ticket))
            .is_none()
        {
            return;
        }
        panel.status = None;
        panel.stop = None;
        if let Some(activity) = &mut panel.activity {
            activity.finish(&ticket, &result, |outcome| match outcome {
                Outcome::Saved(capture) => Some(format!(
                    "Saved packet capture\n{}\n{}",
                    stop_reason(capture),
                    capture.path.display(),
                )),
                _ => None,
            });
        }
        match result {
            Ok(Some(Outcome::Interfaces(source, list))) => {
                if active == Some(source) {
                    panel.status = if list.interfaces.is_empty() {
                        Some(
                            "No capture interfaces reported. Enter a name directly to review it."
                                .into(),
                        )
                    } else if list.truncated {
                        Some("Showing the first 512 capture interfaces. You can also enter a name directly.".into())
                    } else {
                        None
                    };
                }
                panel.interfaces = Some((source, list));
            }
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
            Ok((target, review.pane, review.plan.clone()))
        })();
        let panel = &mut self.packet_capture;
        let (target, pane, plan) = match result {
            Ok(job) => job,
            Err(error) => {
                panel.status = Some(format!("{error:#}"));
                return;
            }
        };
        let ticket = panel.begin("Capturing packets and transferring the completed file…");
        panel.activity = Some(diagnostics::Activity::start(
            &ticket, &plan.user, &plan.host, plan.port,
        ));
        let stop = CaptureStop::default();
        panel.stop = Some(stop.clone());
        work(ctx, ticket, move || {
            target
                .save_packet_capture_with_stop_blocking(pane, plan, path, stop)
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
