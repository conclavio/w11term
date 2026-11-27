use super::*;

const ROWS_PER_PAGE: usize = 12;
const SOURCE_LABELS: [&str; 2] = ["Before capture", "After capture"];

#[derive(Clone, Copy)]
pub enum Change {
    Page(usize),
    Pair { page: usize, row: Option<usize> },
    Source(usize),
    ProtocolsExpanded(bool),
    Open,
    Export,
}

pub(super) struct Comparison {
    model: Arc<PacketCaptureComparison>,
    paths: [PathBuf; 2],
    revision: Arc<()>,
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
            page: 0,
            selected: None,
            source: 1,
            protocols_expanded: false,
        }
    }

    fn page_count(&self) -> usize {
        self.model
            .conversation_changes
            .len()
            .div_ceil(ROWS_PER_PAGE)
            .max(1)
    }

    pub(super) fn export_model(&self, revision: &Arc<()>) -> Option<Arc<PacketCaptureComparison>> {
        Arc::ptr_eq(revision, &self.revision).then(|| Arc::clone(&self.model))
    }

    pub(super) fn update(
        &mut self,
        revision: &Arc<()>,
        change: Change,
    ) -> Option<(PathBuf, IpAddr, IpAddr)> {
        if !Arc::ptr_eq(revision, &self.revision) {
            return None;
        }
        let start = self.page * ROWS_PER_PAGE;
        match change {
            Change::Page(page) if page < self.page_count() && page != self.page => {
                self.page = page;
                self.selected = None;
            }
            Change::Pair { page, row } if page == self.page => {
                self.selected = row.filter(|&row| {
                    row < ROWS_PER_PAGE && start + row < self.model.conversation_changes.len()
                });
            }
            Change::Source(source) if source < self.paths.len() => self.source = source,
            Change::ProtocolsExpanded(expanded) => self.protocols_expanded = expanded,
            Change::Open => {
                let pair = self
                    .model
                    .conversation_changes
                    .get(start + self.selected?)?;
                return Some((self.paths[self.source].clone(), pair.a, pair.b));
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
        fields.push(text(
            "changes",
            format!(
                "IP conversation changes {}",
                model.conversation_changes.len()
            ),
        ));
        if model.conversation_changes.is_empty() {
            fields.push(text("empty", "No changes in observed IP conversations"));
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
                            .on_click(action(Change::Page(previous)))
                            .content("Previous page"),
                        TextBlock::new().text(format!(
                            "Page {} of {}",
                            self.page + 1,
                            self.page_count()
                        )),
                        Button::new()
                            .is_enabled(idle && next < self.page_count())
                            .on_click(action(Change::Page(next)))
                            .content("Next page"),
                    )),
            ));
        }
        if !model.conversation_changes.is_empty() {
            let page = self.page;
            let row_revision = Arc::clone(&self.revision);
            let source_revision = Arc::clone(&self.revision);
            fields.extend([
                KeyedView::new(
                    format!("conversations-{page}"),
                    ListView::new()
                        .automation_name("Changed IP conversations")
                        .max_height(320.0)
                        .selected_index(self.selected)
                        .on_selection_changed(ctx.callback(move |row| {
                            message(Action::Comparison(row_revision.clone(), Change::Pair { page, row }))
                        }))
                        .collection_slot(
                            ListViewSlot::Items,
                            model.conversation_changes.iter().skip(page * ROWS_PER_PAGE).take(ROWS_PER_PAGE).map(|change| {
                                let label = format!("{} ↔ {}", change.a, change.b);
                                let mut details = vec![text("addresses", label.clone())];
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
                                if let Some(delta) = &change.delta {
                                    details.push(text("delta", format!(
                                        "Change · {:+} packets · {:+} captured bytes",
                                        delta.packets, delta.captured_bytes,
                                    )));
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
                        .on_click(action(Change::Open))
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
