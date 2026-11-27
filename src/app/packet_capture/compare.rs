use super::*;
use w11term::ssh::{PacketConversationChange, PacketConversationEndpoints};

const ROWS_PER_PAGE: usize = 12;
const SOURCE_LABELS: [&str; 2] = ["Before capture", "After capture"];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConversationView {
    Ip,
    Transport,
}

impl ConversationView {
    const ALL: [Self; 2] = [Self::Ip, Self::Transport];

    fn selector_label(self) -> &'static str {
        match self {
            Self::Ip => "IP addresses",
            Self::Transport => "TCP and UDP ports",
        }
    }

    fn category(self) -> &'static str {
        match self {
            Self::Ip => "IP",
            Self::Transport => "TCP and UDP",
        }
    }
}

#[derive(Clone, Copy)]
pub enum Change {
    View(ConversationView),
    Page(ConversationView, usize),
    Pair {
        view: ConversationView,
        page: usize,
        row: Option<usize>,
    },
    Source(usize),
    ProtocolsExpanded(bool),
    Open(ConversationView, usize),
    Export,
}

pub(super) struct Comparison {
    model: Arc<PacketCaptureComparison>,
    paths: [PathBuf; 2],
    revision: Arc<()>,
    conversation_view: ConversationView,
    page: usize,
    selected: Option<usize>,
    source: usize,
    protocols_expanded: bool,
}

impl Comparison {
    pub(super) fn new(model: Arc<PacketCaptureComparison>, paths: [PathBuf; 2]) -> Self {
        Self {
            model,
            paths,
            revision: Arc::new(()),
            conversation_view: ConversationView::Ip,
            page: 0,
            selected: None,
            source: 1,
            protocols_expanded: false,
        }
    }

    fn page_count(&self) -> usize {
        self.row_count().div_ceil(ROWS_PER_PAGE).max(1)
    }

    fn row_count(&self) -> usize {
        self.row_groups().iter().map(|group| group.len()).sum()
    }

    fn rows(&self) -> impl Iterator<Item = &PacketConversationChange> {
        self.row_groups().into_iter().flatten()
    }

    fn row_groups(&self) -> [&[PacketConversationChange]; 2] {
        match self.conversation_view {
            ConversationView::Ip => [
                &self.model.conversation_changes,
                &self.model.rate_only_changes.ip_conversations,
            ],
            ConversationView::Transport => [
                &self.model.transport_changes,
                &self.model.rate_only_changes.transport_conversations,
            ],
        }
    }

    pub(super) fn export_model(&self, revision: &Arc<()>) -> Option<Arc<PacketCaptureComparison>> {
        Arc::ptr_eq(revision, &self.revision).then(|| Arc::clone(&self.model))
    }

    pub(super) fn update(
        &mut self,
        revision: &Arc<()>,
        change: Change,
    ) -> Option<(PathBuf, WiresharkFilter)> {
        if !Arc::ptr_eq(revision, &self.revision) {
            return None;
        }
        let start = self.page * ROWS_PER_PAGE;
        match change {
            Change::View(view) if view != self.conversation_view => {
                self.conversation_view = view;
                self.page = 0;
                self.selected = None;
            }
            Change::Page(view, page)
                if view == self.conversation_view
                    && page < self.page_count()
                    && page != self.page =>
            {
                self.page = page;
                self.selected = None;
            }
            Change::Pair { view, page, row }
                if view == self.conversation_view && page == self.page =>
            {
                self.selected =
                    row.filter(|&row| row < ROWS_PER_PAGE && start + row < self.row_count());
            }
            Change::Source(source) if source < self.paths.len() => self.source = source,
            Change::ProtocolsExpanded(expanded) => self.protocols_expanded = expanded,
            Change::Open(view, page) if view == self.conversation_view && page == self.page => {
                let pair = self.rows().nth(start + self.selected?)?;
                let filter = match pair.endpoints {
                    PacketConversationEndpoints::Ip { a, b } => {
                        WiresharkFilter::Conversation(a, b, None)
                    }
                    PacketConversationEndpoints::Transport { protocol, a, b } => {
                        WiresharkFilter::Transport(protocol, a, b)
                    }
                };
                return Some((self.paths[self.source].clone(), filter));
            }
            _ => (),
        }
        None
    }

    pub(super) fn view(
        &self,
        status: Option<&str>,
        idle: bool,
        export_idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let model = &self.model;
        let view = self.conversation_view;
        let category = view.category();
        let conversation_count = self.row_count();
        let action = |change| {
            let revision = Arc::clone(&self.revision);
            ctx.callback(move |_| message(Action::Comparison(revision.clone(), change)))
        };
        let mut fields = vec![
            KeyedView::new(
                "back",
                Button::new()
                    .is_enabled(idle)
                    .on_click(ctx.callback(|_| message(Action::BackToCapture)))
                    .content("Back to capture"),
            ),
            KeyedView::new(
                "export",
                Button::new()
                    .is_enabled(idle && export_idle)
                    .on_click(action(Change::Export))
                    .content("Export comparison…"),
            ),
        ];
        if let Some(status) = status {
            fields.push(text("status", status));
        }
        for (side, capture) in [("Before", &model.before), ("After", &model.after)] {
            let span = capture.timestamp_span_ns.map_or_else(
                || "No usable timestamps".into(),
                |ns| format!("Timestamp span {} s", seconds_text(ns)),
            );
            let mut details = vec![
                text(
                    "source",
                    format!("{side} · {} · {}", capture.file_name, capture.format),
                ),
                text(
                    "counts",
                    format!(
                        "Packets {} · {} captured bytes · {} truncated",
                        capture.packets, capture.captured_bytes, capture.truncated_packets,
                    ),
                ),
                text("time", span),
                text(
                    "coverage",
                    format!(
                        "Unclassified packets {} · Untracked packets {}",
                        capture.unclassified_packets, capture.untracked_packets,
                    ),
                ),
                text(
                    "protocol-coverage",
                    format!(
                        "Unresolved IP protocols {} · Fragmented IP packets {}",
                        capture.unresolved_protocol_packets, capture.fragmented_packets
                    ),
                ),
            ];
            if view == ConversationView::Transport {
                details.push(text(
                    "transport-coverage",
                    format!(
                        "TCP/UDP packets without ports {} · Untracked TCP/UDP packets {}",
                        capture.packets_without_ports, capture.transport_untracked_packets,
                    ),
                ));
            }
            if let Some(issue) = &capture.issue {
                details.push(text("partial", format!("Partial scan · {issue}")));
            }
            fields.push(KeyedView::new(
                side,
                StackPanel::new().spacing(3.0).keyed_children(details),
            ));
        }
        if let Some(delta) = &model.totals_delta {
            fields.push(text(
                "total-delta",
                format!(
                    "Capture change · {:+} packets · {:+} captured bytes · {:+} truncated",
                    delta.packets, delta.captured_bytes, delta.truncated_packets,
                ),
            ));
        } else {
            fields.push(text(
                "total-delta",
                "Capture deltas unavailable for a partial scan",
            ));
        }
        fields.push(KeyedView::new(
            "average-rates",
            StackPanel::new().spacing(3.0).keyed_children(
                std::iter::once(text(
                    "heading",
                    "Average rates over each recorded timestamp span",
                ))
                .chain(
                    model
                        .average_rates
                        .report_lines()
                        .into_iter()
                        .enumerate()
                        .map(|(index, line)| text(&format!("rate-{index}"), line)),
                ),
            ),
        ));
        if !model.protocol_changes.is_empty() || model.tcp_flag_comparison.is_some() {
            let mut rows = Vec::new();
            if self.protocols_expanded {
                rows.extend(
                    model.protocol_changes.iter().map(|change| {
                        text(&format!("protocol-{}", change.number), change.summary())
                    }),
                );
                if let Some(flags) = &model.tcp_flag_comparison {
                    rows.extend(
                        flags
                            .report_lines()
                            .into_iter()
                            .enumerate()
                            .map(|(index, line)| text(&format!("tcp-{index}"), line)),
                    );
                }
            }
            let revision = Arc::clone(&self.revision);
            fields.push(KeyedView::new(
                "protocol-changes",
                Expander::new()
                    .automation_name("IP protocol changes")
                    .automation_id("comparison-ip-protocols")
                    .is_expanded(self.protocols_expanded)
                    .on_is_expanded_changed(ctx.callback(move |expanded| {
                        message(Action::Comparison(
                            revision.clone(),
                            Change::ProtocolsExpanded(expanded),
                        ))
                    }))
                    .slots([
                        SlotView::new(
                            ExpanderSlot::Header,
                            TextBlock::new().text("IP protocol changes"),
                        ),
                        SlotView::new(
                            ExpanderSlot::Content,
                            StackPanel::new().spacing(6.0).keyed_children(rows),
                        ),
                    ]),
            ));
        }
        let type_revision = Arc::clone(&self.revision);
        fields.extend([
            KeyedView::new(
                "conversation-type",
                ComboBox::new()
                    .automation_name("Conversation type")
                    .items_source(
                        ConversationView::ALL.map(|view| view.selector_label().to_owned()),
                    )
                    .selected_index(Some(view as usize))
                    .is_enabled(idle)
                    .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                        index
                            .and_then(|index| ConversationView::ALL.get(index))
                            .map_or(Message::ControlIdle, |&view| {
                                message(Action::Comparison(
                                    type_revision.clone(),
                                    Change::View(view),
                                ))
                            })
                    })),
            ),
            text(
                "changes",
                format!("{category} conversation changes {conversation_count}"),
            ),
        ]);
        if conversation_count == 0 {
            fields.push(text(
                "empty",
                format!("No changes in observed {category} conversations"),
            ));
        }
        if self.page_count() > 1 {
            let previous = self.page.saturating_sub(1);
            let next = self.page + 1;
            fields.push(KeyedView::new(
                "pages",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(6.0)
                    .children((
                        Button::new()
                            .is_enabled(idle && self.page != 0)
                            .on_click(action(Change::Page(view, previous)))
                            .content("Previous page"),
                        TextBlock::new().text(format!(
                            "Page {} of {}",
                            self.page + 1,
                            self.page_count()
                        )),
                        Button::new()
                            .is_enabled(idle && next < self.page_count())
                            .on_click(action(Change::Page(view, next)))
                            .content("Next page"),
                    )),
            ));
        }
        if conversation_count != 0 {
            let page = self.page;
            let row_revision = Arc::clone(&self.revision);
            let source_revision = Arc::clone(&self.revision);
            fields.extend([
                KeyedView::new(
                    format!("conversations-{category}-{page}"),
                    ListView::new()
                        .automation_name(format!("Changed {category} conversations"))
                        .max_height(320.0)
                        .selected_index(self.selected)
                        .on_selection_changed(ctx.callback(move |row| {
                            message(Action::Comparison(row_revision.clone(), Change::Pair { view, page, row }))
                        }))
                        .collection_slot(
                            ListViewSlot::Items,
                            self.rows().skip(page * ROWS_PER_PAGE).take(ROWS_PER_PAGE).map(|change| {
                                let label = change.endpoints.to_string();
                                let mut details = vec![text("endpoints", label.clone())];
                                for (side, traffic) in [("Before", change.before), ("After", change.after)] {
                                    let detail = traffic.map_or_else(
                                        || format!("{side} · Pair coverage unavailable"),
                                        |traffic| format!(
                                            "{side} · Left to right {} packets, {} bytes · Right to left {} packets, {} bytes",
                                            traffic.a_to_b.packets, traffic.a_to_b.captured_bytes,
                                            traffic.b_to_a.packets, traffic.b_to_a.captured_bytes
                                        ),
                                    );
                                    details.push(text(side, detail));
                                }
                                let delta = change.delta.as_ref().map_or_else(
                                    || "Change · Unavailable".into(),
                                    |delta| format!(
                                        "Change · {:+} packets · {:+} captured bytes",
                                        delta.packets, delta.captured_bytes,
                                    ),
                                );
                                details.push(text("delta", delta));
                                if let Some(rates) = change.average_rates {
                                    details.extend(
                                        rates.report_lines().into_iter().enumerate().map(|(index, line)| {
                                            text(&format!("rate-{index}"), line)
                                        }),
                                    );
                                }
                                KeyedView::new(
                                    label.as_str(),
                                    ListViewItem::new().automation_name(&label).content(
                                        StackPanel::new().spacing(3.0).keyed_children(details)
                                    ),
                                )
                            }),
                        ),
                ),
                KeyedView::new(
                    "wireshark-source",
                    ComboBox::new()
                        .automation_name("Capture to open in Wireshark")
                        .items_source(SOURCE_LABELS.map(str::to_owned))
                        .selected_index(Some(self.source))
                        .is_enabled(idle)
                        .on_selection_changed(ctx.callback(move |source: Option<usize>| {
                            source.map_or(Message::ControlIdle, |source| {
                                message(Action::Comparison(source_revision.clone(), Change::Source(source)))
                            })
                        })),
                ),
                KeyedView::new(
                    "open-conversation",
                    Button::new()
                        .is_enabled(idle && self.selected.is_some())
                        .on_click(action(Change::Open(view, page)))
                        .content("Open conversation in Wireshark"),
                ),
                KeyedView::new(
                    "choose-wireshark",
                    HyperlinkButton::new()
                        .is_enabled(idle)
                        .on_click(ctx.callback(|_| message(Action::ChooseWireshark)))
                        .content("Choose Wireshark executable…"),
                ),
            ]);
        }
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .children((
                Border::new().margin(12.0).content(diagnostics::header(
                    "Packet capture comparison",
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
