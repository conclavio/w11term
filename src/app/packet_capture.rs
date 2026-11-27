use super::*;
use std::{
    ops::{Range, RangeInclusive},
    path::{Path, PathBuf},
};
use w11term::{
    control::PacketCaptureOptions,
    ssh::{
        CaptureStop, LocalPacketCapture, PacketCapture, PacketCaptureComparison,
        PacketCaptureExcerpt, PacketCaptureExcerptPlan, PacketCaptureInterfaces, PacketCapturePlan,
        PacketConversationEndpoints, ip_protocol_label, open_capture_in_wireshark,
        open_capture_interval_in_wireshark, open_capture_pair_in_wireshark,
    },
};

mod compare;
mod conversations;

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
    SelectSavedInterface(PathBuf, usize),
    TimelineExpanded(bool),
    SelectTimelineInterval(PathBuf, usize),
    ProtocolsExpanded(bool),
    LocalConversations(
        Arc<LocalPacketCapture>,
        compare::ConversationView,
        conversations::Change,
    ),
    LimitConversationInterval(PathBuf, bool),
    Tmpfs(bool),
    Review,
    Save,
    Stop(Arc<()>),
    OpenCaptureFile(Option<PathBuf>),
    CompareCapture,
    Comparison(Arc<()>, compare::Change),
    BackToCapture,
    ChooseWireshark,
    OpenWireshark,
    OpenTimelineInterval(PathBuf),
    SaveTimelineExcerpt(PathBuf),
    Finished(Arc<()>, Result<Option<Outcome>, String>),
}

#[derive(Clone, Copy)]
pub enum PairDestination {
    Wireshark,
    Excerpt,
}

pub enum Outcome {
    Interfaces(flame_graph::Connection, PacketCaptureInterfaces),
    Reviewed(u64, Box<PacketCapturePlan>),
    Destination(PathBuf),
    Saved(PacketCapture),
    ExcerptSaved(PacketCaptureExcerpt),
    Loaded(Arc<LocalPacketCapture>),
    Compared {
        comparison: Arc<PacketCaptureComparison>,
        baseline: Option<Arc<LocalPacketCapture>>,
        paths: [PathBuf; 2],
    },
    ComparedElapsed(Arc<()>, RangeInclusive<usize>, Arc<PacketCaptureComparison>),
    Wireshark(PathBuf),
    Opened,
}

#[derive(Clone, Copy)]
enum FileChoice {
    Capture,
    Compare,
    Save,
    Wireshark,
}

enum WiresharkFilter {
    Interval(Range<i128>),
    Conversation(PacketConversationEndpoints, Option<Range<i128>>),
}

enum LoadedCapture {
    Captured(PacketCapture),
    Local(Arc<LocalPacketCapture>),
}

impl LoadedCapture {
    fn path(&self) -> &Path {
        match self {
            Self::Captured(capture) => &capture.path,
            Self::Local(capture) => &capture.path,
        }
    }
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
    loaded: Option<LoadedCapture>,
    comparison: Option<compare::Comparison>,
    saved_interface: usize,
    timeline_expanded: bool,
    timeline_interval: Option<usize>,
    protocols_expanded: bool,
    ip_conversations: conversations::List,
    transport_conversations: conversations::List,
    limit_conversation_interval: bool,
    wireshark: Option<PathBuf>,
    pub(super) pending: Option<Arc<()>>,
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

    fn install_local(&mut self, capture: Arc<LocalPacketCapture>) {
        self.loaded = Some(LoadedCapture::Local(capture));
        self.comparison = None;
        self.saved_interface = 0;
        self.timeline_expanded = false;
        self.timeline_interval = None;
        self.protocols_expanded = false;
        self.ip_conversations = conversations::List::default();
        self.transport_conversations = conversations::List::default();
        self.limit_conversation_interval = false;
        self.activity = None;
    }

    fn conversation_interval_checkbox(
        &self,
        capture: &LocalPacketCapture,
        label: &str,
        idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> Option<KeyedView> {
        let timeline = capture.summary.timeline.as_ref()?;
        let start = timeline.start_unix_ns()?;
        let interval = timeline.selected_interval(self.timeline_interval)?;
        let path = capture.path.clone();
        Some(KeyedView::new(
            "conversation-interval",
            CheckBox::new()
                .is_checked(self.limit_conversation_interval)
                .is_enabled(idle)
                .on_is_checked_changed(ctx.callback(move |checked| {
                    message(Action::LimitConversationInterval(path.clone(), checked))
                }))
                .content(format!(
                    "{label} · +{} to +{} s",
                    seconds_text(interval.start.abs_diff(start)),
                    seconds_text(interval.end.abs_diff(start))
                )),
        ))
    }

    fn view(
        &self,
        active: Option<flame_graph::Connection>,
        export_idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let idle = self.pending.is_none();
        if let Some(comparison) = &self.comparison {
            return comparison.view(self.status.as_deref(), idle, export_idle, ctx);
        }
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
        let mut files = vec![KeyedView::new(
            "open-capture-file",
            button("Open capture file…", idle, || {
                Action::OpenCaptureFile(None)
            }),
        )];
        if let Some(status) = &self.status {
            files.push(text("status", status));
        }
        if let Some(LoadedCapture::Captured(saved)) = &self.loaded {
            let reason = stop_reason(saved);
            let packets = if saved.rotation.is_some() {
                "Packets retained"
            } else {
                "Packets captured"
            };
            files.extend([
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
                files.push(text(
                    "drops",
                    format!("Packets dropped by kernel {dropped}"),
                ));
            }
            if let Some(rotation) = &saved.rotation {
                files.push(text(
                    "rotation-saved",
                    format!(
                        "Rolling capture · {} captured · {} files kept · {} overwritten",
                        rotation.captured_packets,
                        rotation.retained_files,
                        rotation.overwritten_files
                    ),
                ));
            }
        } else if let Some(LoadedCapture::Local(capture)) = &self.loaded {
            let summary = &capture.summary;
            let stats = &summary.stats;
            files.extend([
                text("local-capture-path", capture.path.display().to_string()),
                text(
                    "local-capture-details",
                    format!("{} · {} bytes", capture.format_label(), capture.bytes),
                ),
                text(
                    "local-packet-counts",
                    format!(
                        "Packets {} · {} captured bytes · {} truncated",
                        stats.packets, stats.captured_bytes, stats.truncated_packets
                    ),
                ),
            ]);
            if let Some(span) = stats.span_ns() {
                files.push(text(
                    "local-packet-span",
                    format!(
                        "Timestamp span {} s · {} of {} packets timestamped",
                        seconds_text(span),
                        stats.timestamped_packets,
                        stats.packets
                    ),
                ));
            } else if stats.packets != 0 {
                files.push(text("local-packet-span", "No usable packet timestamps"));
            }
            if let Some(timeline) = &summary.timeline {
                let buckets: Vec<_> = timeline.buckets().collect();
                if let Some((peak_index, peak)) = timeline.peak() {
                    const CHART_HEIGHT: f64 = 56.0;
                    let width = timeline.bucket_width_ns();
                    let selected = self
                        .timeline_interval
                        .filter(|&index| {
                            buckets.get(index).is_some_and(|bucket| bucket.packets != 0)
                        })
                        .unwrap_or(peak_index);
                    let occupied: Vec<_> = buckets
                        .iter()
                        .enumerate()
                        .filter(|(_, bucket)| bucket.packets != 0)
                        .map(|(index, _)| index)
                        .collect();
                    let labels = occupied
                        .iter()
                        .map(|&index| {
                            let bucket = &buckets[index];
                            let unit = if bucket.packets == 1 {
                                "packet"
                            } else {
                                "packets"
                            };
                            format!(
                                "+{} s · {} {unit}",
                                seconds_text(width * index as u128),
                                bucket.packets
                            )
                        })
                        .collect::<Vec<_>>();
                    let selected_option = occupied.iter().position(|&index| index == selected);
                    let timeline_path = capture.path.clone();
                    let open_path = capture.path.clone();
                    let excerpt_path = capture.path.clone();
                    let selection = ComboBox::new()
                        .automation_name("Packet timeline intervals")
                        .items_source(labels)
                        .selected_index(selected_option)
                        .is_enabled(idle)
                        .on_selection_changed(ctx.callback(move |option: Option<usize>| {
                            option.and_then(|index| occupied.get(index)).map_or(
                                Message::ControlIdle,
                                |&index| {
                                    message(Action::SelectTimelineInterval(
                                        timeline_path.clone(),
                                        index,
                                    ))
                                },
                            )
                        }));
                    let bars = Grid::new()
                        .columns((0..buckets.len()).map(|_| GridLength::Star(1.0)))
                        .column_spacing(1.0)
                        .keyed_children(
                            buckets
                                .iter()
                                .enumerate()
                                .filter(|(_, bucket)| bucket.packets > 0)
                                .map(|(column, bucket)| {
                                    KeyedView::new(
                                        column.to_string(),
                                        Border::new()
                                            .grid_column(column as i32)
                                            .vertical_alignment(VerticalAlignment::Bottom)
                                            .height(
                                                (f64::from(bucket.packets)
                                                    / f64::from(peak.packets)
                                                    * CHART_HEIGHT)
                                                    .max(1.0),
                                            )
                                            .background(if column == selected {
                                                Color::rgb(161, 205, 246)
                                            } else {
                                                Color::rgb(96, 165, 230)
                                            }),
                                    )
                                }),
                        );
                    let unit = if peak.packets == 1 {
                        "packet"
                    } else {
                        "packets"
                    };
                    files.push(KeyedView::new("local-timeline",
                        Expander::new().automation_name("Packet timeline").automation_id("packet-timeline")
                            .is_expanded(self.timeline_expanded)
                            .on_is_expanded_changed(ctx.callback(|expanded| message(Action::TimelineExpanded(expanded))))
                            .slots([
                            SlotView::new(ExpanderSlot::Header, TextBlock::new().text("Packet timeline")),
                            SlotView::new(ExpanderSlot::Content, StackPanel::new().spacing(6.0).children((
                                TextBlock::new().text(format!("Bucket width {} s", seconds_text(width))),
                                Border::new().height(CHART_HEIGHT).background(Color::rgb(44, 44, 48))
                                    .automation_name("Packet timeline chart").content(bars),
                                TextBlock::new().text(format!("Peak {} {unit} · {} bytes · +{} s from timeline start",
                                    peak.packets, peak.captured_bytes, seconds_text(width * peak_index as u128)))
                                    .text_wrapping(TextWrapping::Wrap),
                                selection,
                                Button::new().is_enabled(idle)
                                    .on_click(ctx.callback(move |_| message(Action::OpenTimelineInterval(open_path.clone()))))
                                    .content("Open interval in Wireshark"),
                                Button::new().is_enabled(idle)
                                    .on_click(ctx.callback(move |_| message(Action::SaveTimelineExcerpt(excerpt_path.clone()))))
                                    .content("Save capture excerpt…"),
                            ))),
                        ])));
                }
            }
            if stats.packets != 0 {
                let protocols = &summary.ip_protocols;
                let mut rows = Vec::new();
                if self.protocols_expanded {
                    rows.push(text(
                        "coverage",
                        format!(
                            "Unclassified packets {} · Unresolved IP protocols {} · Fragmented IP packets {}",
                            protocols.unclassified_packets,
                            protocols.unresolved_protocol_packets,
                            protocols.fragmented_packets,
                        ),
                    ));
                    let mut ranked: Vec<_> = protocols.by_number.iter().collect();
                    ranked.sort_unstable_by_key(|&(number, traffic)| {
                        (std::cmp::Reverse(traffic.captured_bytes), *number)
                    });
                    rows.extend(ranked.into_iter().map(|(&number, traffic)| {
                        let name = ip_protocol_label(number);
                        text(
                            &format!("protocol-{number}"),
                            format!(
                                "{name} · {} packets · {} captured bytes",
                                traffic.packets, traffic.captured_bytes
                            ),
                        )
                    }));
                    let tcp_packets = protocols.tcp_packets();
                    if tcp_packets != 0 {
                        let flags = &protocols.tcp_flags;
                        rows.extend([
                            text(
                                "tcp-coverage",
                                format!(
                                    "TCP flags available on {} of {} TCP packets",
                                    flags.decoded_headers, tcp_packets
                                ),
                            ),
                            text(
                                "tcp-counts",
                                format!(
                                    "SYN without ACK {} · SYN-ACK {} · RST {} · FIN {}",
                                    flags.syn_without_ack, flags.syn_ack, flags.rst, flags.fin
                                ),
                            ),
                        ]);
                    }
                }
                files.push(KeyedView::new(
                    "local-protocols",
                    Expander::new()
                        .automation_name("IP protocols")
                        .automation_id("packet-ip-protocols")
                        .is_expanded(self.protocols_expanded)
                        .on_is_expanded_changed(
                            ctx.callback(|expanded| message(Action::ProtocolsExpanded(expanded))),
                        )
                        .slots([
                            SlotView::new(
                                ExpanderSlot::Header,
                                TextBlock::new().text("IP protocols"),
                            ),
                            SlotView::new(
                                ExpanderSlot::Content,
                                StackPanel::new().spacing(6.0).keyed_children(rows),
                            ),
                        ]),
                ));
            }
            files.extend(self.conversation_browser(
                capture,
                compare::ConversationView::Ip,
                idle,
                ctx,
            ));
            files.extend(self.conversation_browser(
                capture,
                compare::ConversationView::Transport,
                idle,
                ctx,
            ));
            if !summary.interfaces.is_empty() {
                let mut types: Vec<_> = summary
                    .link_types
                    .iter()
                    .copied()
                    .take(8)
                    .map(link_type)
                    .collect();
                let more = summary.link_types.len().saturating_sub(types.len());
                if more != 0 {
                    types.push(format!("{more} more"));
                }
                files.push(text(
                    "local-link-types",
                    format!(
                        "Interfaces {} · {}",
                        summary.interfaces.len(),
                        types.join(", ")
                    ),
                ));
            }
            let multiple_interfaces = summary.interfaces.len() > 1;
            if multiple_interfaces {
                let labels = summary.interfaces.iter().map(|interface| {
                    let mut label = format!("Interface {}", interface.interface_id);
                    if let Some(section) = interface.section {
                        label = format!("Section {section} · {label}");
                    }
                    if let Some(name) = &interface.name {
                        label.push_str(" · ");
                        label.push_str(name);
                    }
                    label
                });
                let path = capture.path.clone();
                files.push(KeyedView::new(
                    "saved-interfaces",
                    ComboBox::new()
                        .automation_name("Saved capture interfaces")
                        .items_source(labels)
                        .selected_index(Some(self.saved_interface))
                        .is_enabled(idle)
                        .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                            index.map_or(Message::ControlIdle, |index| {
                                message(Action::SelectSavedInterface(path.clone(), index))
                            })
                        })),
                ));
            }
            if let Some(interface) = summary.interfaces.get(self.saved_interface) {
                let snap = interface.snap_length.map_or_else(
                    || "Unlimited snap length".into(),
                    |length| format!("Snap length {length} bytes"),
                );
                let details = if multiple_interfaces {
                    format!("{} · {snap}", link_type(interface.link_type))
                } else {
                    snap
                };
                let label = match &interface.name {
                    Some(name) => format!("{name} · {details}"),
                    None => details,
                };
                files.push(text("saved-interface-link", label));
                if multiple_interfaces {
                    let stats = &interface.stats;
                    files.push(text(
                        "saved-interface-counts",
                        format!(
                            "Interface packets {} · {} captured bytes · {} truncated",
                            stats.packets, stats.captured_bytes, stats.truncated_packets
                        ),
                    ));
                    if let Some(span) = stats.span_ns() {
                        files.push(text(
                            "saved-interface-span",
                            format!(
                                "Interface span {} s · {} of {} packets timestamped",
                                seconds_text(span),
                                stats.timestamped_packets,
                                stats.packets
                            ),
                        ));
                    } else if stats.packets != 0 {
                        files.push(text(
                            "saved-interface-span",
                            "No usable timestamps on this interface",
                        ));
                    }
                }
                if let Some(snapshot) = &interface.capture_statistics {
                    let counters = &snapshot.counters;
                    let reported = [
                        ("Received", counters.received),
                        ("Filter accepted", counters.filter_accepted),
                        ("Delivered", counters.delivered),
                    ];
                    let drops = [
                        ("Interface drops", counters.interface_dropped),
                        ("OS drops", counters.os_dropped),
                    ];
                    for (key, values) in [
                        ("capture-reported-counts", reported.as_slice()),
                        ("capture-reported-drops", drops.as_slice()),
                    ] {
                        let counters: Vec<_> = values
                            .iter()
                            .filter_map(|(label, value)| {
                                value.map(|value| format!("{label} {value}"))
                            })
                            .collect();
                        if !counters.is_empty() {
                            files.push(text(key, format!("Reported · {}", counters.join(" · "))));
                        }
                    }
                }
            }
            if let Some(issue) = &summary.issue {
                files.push(text(
                    "local-summary-issue",
                    format!("Partial packet summary · {issue}"),
                ));
            }
        }
        if self.loaded.is_some() {
            files.extend([
                KeyedView::new(
                    "compare-captures",
                    button("Compare captures…", idle, || Action::CompareCapture),
                ),
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
        files.append(&mut fields);
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
                        .content(StackPanel::new().spacing(8.0).keyed_children(files)),
                ),
            ))
    }
}

fn seconds_text(ns: u128) -> String {
    let fraction = format!("{:09}", ns % 1_000_000_000);
    let digits = fraction.trim_end_matches('0').len().max(3);
    format!("{}.{}", ns / 1_000_000_000, &fraction[..digits])
}

fn link_type(value: u16) -> String {
    match value {
        0 => "BSD loopback",
        1 => "Ethernet",
        101 => "Raw IP",
        105 => "IEEE 802.11",
        108 => "OpenBSD loopback",
        113 => "Linux cooked",
        127 => "IEEE 802.11 with Radiotap",
        276 => "Linux cooked v2",
        _ => return format!("Link type {value}"),
    }
    .into()
}

fn default_wireshark() -> Option<PathBuf> {
    ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(|root| PathBuf::from(root).join("Wireshark").join("Wireshark.exe"))
        .find(|path| path.is_absolute() && path.is_file())
}

impl TerminalApp {
    pub(super) fn compare_saved_packet_captures(
        &mut self,
        before: PathBuf,
        after: PathBuf,
        ctx: &ComponentContext<Self>,
    ) -> bool {
        if self.packet_capture.pending.is_some() {
            return false;
        }
        self.set_diagnostics_drawer(Some(diagnostics::Drawer::PacketCapture));
        let ticket = self
            .packet_capture
            .begin("Comparing saved packet captures…");
        work(ctx, ticket, move || {
            let before =
                Arc::new(LocalPacketCapture::open(before).context("Opening the before capture")?);
            compare_captures(before, after, true).map(Some)
        });
        true
    }

    pub(super) fn with_packet_capture(&self, content: View, ctx: &ViewContext<Self>) -> View {
        let width = (self.width * 0.4).clamp(285.0, 520.0);
        let panel = self.packet_capture.view(
            self.profile_connection(),
            self.export.pending.is_none(),
            ctx,
        );
        self.diagnostics_layout(content, "packet-capture", panel, width)
    }

    pub(super) fn packet_capture_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        match action {
            Action::Open => {
                self.command_reference.close();
                self.set_diagnostics_drawer(Some(diagnostics::Drawer::PacketCapture));
            }
            Action::Close => {
                self.packet_capture.open = false;
                self.close_empty_diagnostics_window(ctx);
            }
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
            action
                if self.packet_capture.open
                    && (self.packet_capture.pending.is_none()
                        || matches!(
                            action,
                            Action::LocalConversations(
                                _,
                                _,
                                conversations::Change::Expanded(_)
                                    | conversations::Change::Select { .. }
                            ) | Action::ProtocolsExpanded(_)
                                | Action::Comparison(_, compare::Change::Pair { .. })
                                | Action::Comparison(_, compare::Change::ProtocolsExpanded(_))
                                | Action::Comparison(_, compare::Change::TimelineExpanded(_))
                        )) =>
            {
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
            Action::SelectSavedInterface(path, index) => {
                if let Some(LoadedCapture::Local(capture)) = &panel.loaded
                    && capture.path == path
                    && index < capture.summary.interfaces.len()
                {
                    panel.saved_interface = index;
                }
            }
            Action::TimelineExpanded(expanded) => panel.timeline_expanded = expanded,
            Action::ProtocolsExpanded(expanded) => panel.protocols_expanded = expanded,
            Action::LocalConversations(capture, view, change) => {
                if panel.comparison.is_some()
                    || !matches!(&panel.loaded, Some(LoadedCapture::Local(current)) if Arc::ptr_eq(current, &capture))
                {
                    return;
                }
                if let Some((endpoints, destination)) = panel
                    .conversation_list_mut(view)
                    .update(&capture, view, change)
                {
                    self.local_conversation(capture, endpoints, destination, ctx);
                }
            }
            Action::LimitConversationInterval(path, limit) => {
                if let Some(LoadedCapture::Local(capture)) = &panel.loaded
                    && capture.path == path
                    && capture.summary.timeline.is_some()
                {
                    panel.limit_conversation_interval = limit;
                }
            }
            Action::SelectTimelineInterval(path, index) => {
                if let Some(LoadedCapture::Local(capture)) = &panel.loaded
                    && capture.path == path
                    && capture
                        .summary
                        .timeline
                        .as_ref()
                        .and_then(|timeline| timeline.occupied_interval(index))
                        .is_some()
                {
                    panel.timeline_interval = Some(index);
                }
            }
            Action::Review => self.review_packet_capture(ctx),
            Action::Save => self.choose_capture_file(FileChoice::Save, ctx),
            Action::OpenCaptureFile(None) => self.choose_capture_file(FileChoice::Capture, ctx),
            Action::OpenCaptureFile(Some(path)) => {
                let ticket = panel.begin("Opening packet capture…");
                work(ctx, ticket, move || {
                    Ok(Some(Outcome::Loaded(Arc::new(LocalPacketCapture::open(
                        path,
                    )?))))
                });
            }
            Action::CompareCapture => self.choose_capture_file(FileChoice::Compare, ctx),
            Action::Comparison(revision, compare::Change::Export) => {
                if let Some(comparison) = panel
                    .comparison
                    .as_ref()
                    .and_then(|comparison| comparison.export_model(&revision))
                {
                    self.begin_comparison_export(
                        text_export::Subject::PacketComparison,
                        move |output, format| match format {
                            file_picker::ComparisonFormat::Text => comparison.write_report(output),
                            file_picker::ComparisonFormat::Json => comparison.write_json(output),
                        },
                        ctx,
                    );
                }
            }
            Action::Comparison(revision, compare::Change::CompareElapsed(range)) => {
                let Some((paths, interval, model)) = panel
                    .comparison
                    .as_ref()
                    .and_then(|comparison| comparison.elapsed_request(&revision, &range))
                else {
                    return;
                };
                let ticket = panel.begin("Comparing the selected elapsed interval…");
                work(ctx, ticket, move || {
                    let comparison = Arc::new(model.rescan_elapsed(paths, interval)?);
                    Ok(Some(Outcome::ComparedElapsed(revision, range, comparison)))
                });
            }
            Action::Comparison(revision, compare::Change::SaveTimelineExcerpt(side, range)) => {
                let Some((path, bounds, model)) = panel
                    .comparison
                    .as_ref()
                    .and_then(|comparison| comparison.excerpt_request(&revision, side, &range))
                else {
                    return;
                };
                self.save_capture_excerpt(move || model.prepare_excerpt(side, path, bounds), ctx);
            }
            Action::Comparison(revision, compare::Change::SaveConversationExcerpt(selection)) => {
                let Some(prepare) = panel
                    .comparison
                    .as_ref()
                    .and_then(|comparison| comparison.pair_excerpt_job(&revision, &selection))
                else {
                    return;
                };
                self.save_capture_excerpt(prepare, ctx);
            }
            Action::Comparison(revision, change) => {
                if let Some((capture, filter)) = panel
                    .comparison
                    .as_mut()
                    .and_then(|comparison| comparison.update(&revision, change))
                {
                    self.open_packet_viewer(capture, Some(filter), ctx);
                }
            }
            Action::BackToCapture => panel.comparison = None,
            Action::ChooseWireshark => self.choose_capture_file(FileChoice::Wireshark, ctx),
            Action::SaveTimelineExcerpt(path) => {
                let Some(LoadedCapture::Local(capture)) = &panel.loaded else {
                    return;
                };
                if !panel.timeline_expanded || capture.path != path {
                    return;
                }
                let Some(bounds) = capture
                    .summary
                    .timeline
                    .as_ref()
                    .and_then(|timeline| timeline.selected_interval(panel.timeline_interval))
                else {
                    return;
                };
                let capture = Arc::clone(capture);
                self.save_capture_excerpt(move || capture.prepare_excerpt(bounds), ctx);
            }
            Action::OpenWireshark | Action::OpenTimelineInterval(..) => {
                let Some(capture) = &panel.loaded else { return };
                let filter = match (action, capture) {
                    (Action::OpenWireshark, _) => None,
                    (Action::OpenTimelineInterval(path), LoadedCapture::Local(local))
                        if panel.timeline_expanded && local.path == path =>
                    {
                        let Some(interval) = local.summary.timeline.as_ref().and_then(|timeline| {
                            timeline.selected_interval(panel.timeline_interval)
                        }) else {
                            return;
                        };
                        Some(WiresharkFilter::Interval(interval))
                    }
                    _ => return,
                };
                let capture = capture.path().to_owned();
                self.open_packet_viewer(capture, filter, ctx);
            }
            _ => (),
        }
    }

    fn local_conversation(
        &mut self,
        local: Arc<LocalPacketCapture>,
        endpoints: PacketConversationEndpoints,
        destination: PairDestination,
        ctx: &ComponentContext<Self>,
    ) {
        let panel = &self.packet_capture;
        let interval = if panel.limit_conversation_interval {
            let Some(interval) = local
                .summary
                .timeline
                .as_ref()
                .and_then(|timeline| timeline.selected_interval(panel.timeline_interval))
            else {
                return;
            };
            Some(interval)
        } else {
            None
        };
        match destination {
            PairDestination::Wireshark => self.open_packet_viewer(
                local.path.clone(),
                Some(WiresharkFilter::Conversation(endpoints, interval)),
                ctx,
            ),
            PairDestination::Excerpt => {
                self.save_capture_excerpt(
                    move || local.prepare_conversation_excerpt(endpoints, interval),
                    ctx,
                );
            }
        }
    }

    fn open_packet_viewer(
        &mut self,
        capture: PathBuf,
        filter: Option<WiresharkFilter>,
        ctx: &ComponentContext<Self>,
    ) {
        let configured = self.packet_capture.wireshark.clone();
        let ticket = self.packet_capture.begin("Opening Wireshark…");
        work(ctx, ticket, move || {
            let executable = configured
                .or_else(default_wireshark)
                .context("Wireshark was not found. Choose its executable to open the capture.")?;
            match filter {
                None => open_capture_in_wireshark(&capture, &executable),
                Some(WiresharkFilter::Interval(interval)) => {
                    open_capture_interval_in_wireshark(&capture, &executable, interval)
                }
                Some(WiresharkFilter::Conversation(endpoints, interval)) => {
                    open_capture_pair_in_wireshark(&capture, &executable, endpoints, interval)
                }
            }?;
            Ok(Some(Outcome::Opened))
        });
    }

    fn save_capture_excerpt(
        &mut self,
        prepare: impl FnOnce() -> anyhow::Result<PacketCaptureExcerptPlan> + Send + 'static,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(owner) = self.keyboard.hwnd().map(|window| window.0 as usize) else {
            return;
        };
        let ticket = self.packet_capture.begin("Saving capture excerpt…");
        work(ctx, ticket, move || {
            let plan = prepare()?;
            let purpose =
                file_picker::Purpose::SavePacketExcerpt(plan.source().to_owned(), plan.extension());
            let Some(file) = file_picker::pick(owner, purpose)? else {
                return Ok(None);
            };
            Ok(Some(Outcome::ExcerptSaved(plan.save(file.path)?)))
        });
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

    fn choose_capture_file(&mut self, choice: FileChoice, ctx: &ComponentContext<Self>) {
        let active = self.profile_connection();
        let panel = &mut self.packet_capture;
        match choice {
            FileChoice::Wireshark | FileChoice::Compare if panel.loaded.is_none() => return,
            FileChoice::Save
                if !panel
                    .review
                    .as_ref()
                    .is_some_and(|review| review.matches(active)) =>
            {
                return;
            }
            _ => (),
        }
        let Some(owner) = self.keyboard.hwnd().map(|window| window.0 as usize) else {
            return;
        };
        let initial = self.store.path();
        let capture = || {
            panel
                .loaded
                .as_ref()
                .map_or_else(|| initial.into(), |capture| capture.path().into())
        };
        let purpose = match choice {
            FileChoice::Wireshark => file_picker::Purpose::WiresharkExecutable(
                panel.wireshark.clone().unwrap_or_else(|| initial.into()),
            ),
            FileChoice::Capture => file_picker::Purpose::OpenPacketCapture(capture()),
            FileChoice::Compare => file_picker::Purpose::ComparePacketCapture(capture()),
            FileChoice::Save => file_picker::Purpose::SavePacketCapture(capture()),
        };
        let before = if matches!(choice, FileChoice::Compare) {
            panel.loaded.as_ref().map(|capture| match capture {
                LoadedCapture::Local(capture) => Ok(Arc::clone(capture)),
                LoadedCapture::Captured(capture) => Err(capture.path.clone()),
            })
        } else {
            None
        };
        let ticket = panel.begin("Choosing a Windows file…");
        work(ctx, ticket, move || {
            let Some(file) = file_picker::pick(owner, purpose)? else {
                return Ok(None);
            };
            Ok(Some(match choice {
                FileChoice::Capture => {
                    Outcome::Loaded(Arc::new(LocalPacketCapture::open(file.path)?))
                }
                FileChoice::Compare => {
                    let before = match before.context("Open a before capture first")? {
                        Ok(before) => before,
                        Err(path) => Arc::new(
                            LocalPacketCapture::open(path).context("Opening the before capture")?,
                        ),
                    };
                    compare_captures(before, file.path, false)?
                }
                FileChoice::Save => Outcome::Destination(file.path),
                FileChoice::Wireshark => Outcome::Wireshark(file.path),
            }))
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
        self.diagnostic_control.finish(&ticket, &result);
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
                let path = capture.path.clone();
                panel.loaded = Some(LoadedCapture::Captured(capture));
                panel.comparison = None;
                panel.status = Some("Saved packet capture".into());
                self.remember_diagnostic(
                    w11term::diagnostic_history::Kind::PacketCapture,
                    [path],
                    ctx,
                );
            }
            Ok(Some(Outcome::Loaded(capture))) => {
                let path = capture.path.clone();
                panel.install_local(capture);
                self.remember_diagnostic(
                    w11term::diagnostic_history::Kind::PacketCapture,
                    [path],
                    ctx,
                );
            }
            Ok(Some(Outcome::ExcerptSaved(excerpt))) => {
                let unit = if excerpt.packets == 1 {
                    "packet"
                } else {
                    "packets"
                };
                panel.status = Some(format!(
                    "Saved capture excerpt · {} {unit} · {}",
                    excerpt.packets,
                    excerpt.path.display()
                ));
                self.remember_diagnostic(
                    w11term::diagnostic_history::Kind::PacketCapture,
                    [excerpt.path],
                    ctx,
                );
            }
            Ok(Some(Outcome::Compared {
                comparison,
                baseline,
                paths,
            })) => {
                if let Some(baseline) = baseline {
                    panel.install_local(baseline);
                }
                panel.comparison = Some(compare::Comparison::new(comparison, paths.clone()));
                for path in paths {
                    self.remember_diagnostic(
                        w11term::diagnostic_history::Kind::PacketCapture,
                        [path],
                        ctx,
                    );
                }
            }
            Ok(Some(Outcome::Wireshark(path))) => {
                panel.status = Some(format!("Wireshark executable {}", path.display()));
                panel.wireshark = Some(path);
            }
            Ok(Some(Outcome::ComparedElapsed(revision, range, model))) => {
                if let Some(comparison) = &mut panel.comparison {
                    comparison.install_elapsed(&revision, range, model);
                }
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

fn compare_captures(
    before: Arc<LocalPacketCapture>,
    after: PathBuf,
    install_baseline: bool,
) -> anyhow::Result<Outcome> {
    let after = LocalPacketCapture::open(after).context("Opening the after capture")?;
    Ok(Outcome::Compared {
        comparison: Arc::new(PacketCaptureComparison::new(&before, &after)),
        paths: [before.path.clone(), after.path],
        baseline: install_baseline.then_some(before),
    })
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
