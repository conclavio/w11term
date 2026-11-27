use super::*;
use w11term::ssh::PacketConversationChange;

mod timeline;

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

#[derive(Clone)]
pub enum Change {
    View(ConversationView),
    Filter(String),
    Page { list: u64, page: usize },
    Pair { list: u64, row: Option<usize> },
    Source(usize),
    ProtocolsExpanded(bool),
    TimelineExpanded(bool),
    TimelineInterval(usize),
    TimelineEnd { first: usize, last: usize },
    OpenTimelineInterval(usize, RangeInclusive<usize>),
    SaveTimelineExcerpt(usize, RangeInclusive<usize>),
    CompareElapsed(RangeInclusive<usize>),
    CompareWholeCaptures,
    LimitElapsedInterval(bool),
    Open(ConversationSelection),
    SaveConversationExcerpt(ConversationSelection),
    Export,
}

#[derive(Clone)]
pub struct ConversationSelection {
    list: u64,
    row: Option<usize>,
    source: usize,
    elapsed: Option<RangeInclusive<usize>>,
}

pub(super) struct Comparison {
    model: Arc<PacketCaptureComparison>,
    elapsed_model: Option<(RangeInclusive<usize>, Arc<PacketCaptureComparison>)>,
    paths: [PathBuf; 2],
    revision: Arc<()>,
    conversation_view: ConversationView,
    conversation_query: String,
    matching_rows: Option<Vec<usize>>,
    list_generation: u64,
    page: usize,
    selected: Option<usize>,
    source: usize,
    protocols_expanded: bool,
    timeline_expanded: bool,
    timeline_range: Option<RangeInclusive<usize>>,
    limit_elapsed_interval: bool,
}

impl Comparison {
    pub(super) fn new(model: Arc<PacketCaptureComparison>, paths: [PathBuf; 2]) -> Self {
        Self {
            model,
            elapsed_model: None,
            paths,
            revision: Arc::new(()),
            conversation_view: ConversationView::Ip,
            conversation_query: String::new(),
            matching_rows: None,
            list_generation: 0,
            page: 0,
            selected: None,
            source: 1,
            protocols_expanded: false,
            timeline_expanded: false,
            timeline_range: None,
            limit_elapsed_interval: false,
        }
    }

    fn page_count(&self) -> usize {
        self.row_count().div_ceil(ROWS_PER_PAGE).max(1)
    }

    fn row_count(&self) -> usize {
        self.matching_rows
            .as_ref()
            .map_or_else(|| self.unfiltered_count(), Vec::len)
    }

    fn unfiltered_count(&self) -> usize {
        self.row_groups().iter().map(|group| group.len()).sum()
    }

    fn row(&self, index: usize) -> Option<&PacketConversationChange> {
        let index = match &self.matching_rows {
            Some(rows) => *rows.get(index)?,
            None => index,
        };
        let [counts, rates] = self.row_groups();
        if index < counts.len() {
            counts.get(index)
        } else {
            rates.get(index - counts.len())
        }
    }

    fn row_groups(&self) -> [&[PacketConversationChange]; 2] {
        let model = self.visible_model();
        match self.conversation_view {
            ConversationView::Ip => [
                &model.conversation_changes,
                &model.rate_only_changes.ip_conversations,
            ],
            ConversationView::Transport => [
                &model.transport_changes,
                &model.rate_only_changes.transport_conversations,
            ],
        }
    }

    fn apply_filter(&mut self) {
        let query = search::Query::new(&self.conversation_query);
        self.matching_rows = (!query.is_empty()).then(|| {
            self.row_groups()
                .into_iter()
                .flatten()
                .enumerate()
                .filter_map(|(index, change)| query.matches(change.endpoints).then_some(index))
                .collect()
        });
        self.set_page(0);
    }

    fn set_page(&mut self, page: usize) {
        self.page = page;
        self.selected = None;
        self.list_generation = self.list_generation.wrapping_add(1);
    }

    fn visible_model(&self) -> &Arc<PacketCaptureComparison> {
        self.elapsed_model
            .as_ref()
            .map_or(&self.model, |(_, model)| model)
    }

    pub(super) fn elapsed_request(
        &self,
        revision: &Arc<()>,
        range: &RangeInclusive<usize>,
    ) -> Option<([PathBuf; 2], Range<i128>, Arc<PacketCaptureComparison>)> {
        if !Arc::ptr_eq(revision, &self.revision)
            || !self.timeline_expanded
            || self.selected_timeline_range().as_ref() != Some(range)
        {
            return None;
        }
        let timeline = self.model.timeline.as_ref()?;
        timeline.before_origin_unix_ns?;
        timeline.after_origin_unix_ns?;
        Some((
            self.paths.clone(),
            timeline.offset_range(range.clone())?,
            self.model.clone(),
        ))
    }

    pub(super) fn excerpt_request(
        &self,
        revision: &Arc<()>,
        side: usize,
        range: &RangeInclusive<usize>,
    ) -> Option<(PathBuf, Range<i128>, Arc<PacketCaptureComparison>)> {
        if !Arc::ptr_eq(revision, &self.revision)
            || !self.timeline_expanded
            || self.selected_timeline_range().as_ref() != Some(range)
        {
            return None;
        }
        let bounds = self
            .model
            .timeline
            .as_ref()?
            .occupied_range(side, range.clone())?;
        Some((self.paths.get(side)?.clone(), bounds, self.model.clone()))
    }

    pub(super) fn install_elapsed(
        &mut self,
        revision: &Arc<()>,
        range: RangeInclusive<usize>,
        model: Arc<PacketCaptureComparison>,
    ) {
        if Arc::ptr_eq(revision, &self.revision) {
            self.elapsed_model = Some((range, model));
            self.apply_filter();
        }
    }

    fn elapsed_scope(&self) -> Option<RangeInclusive<usize>> {
        if let Some((range, _)) = &self.elapsed_model {
            return Some(range.clone());
        }
        if self.limit_elapsed_interval {
            self.selected_timeline_range()
        } else {
            None
        }
    }

    fn source_interval(
        &self,
        source: usize,
        elapsed: RangeInclusive<usize>,
    ) -> Option<Range<i128>> {
        if let Some((range, model)) = &self.elapsed_model
            && *range == elapsed
        {
            let window = model.window.as_ref()?;
            let side = *[&window.before, &window.after].get(source)?;
            return Some(side.start_unix_ns..side.end_unix_ns);
        }
        self.model.timeline.as_ref()?.source_range(source, elapsed)
    }

    pub(super) fn export_model(&self, revision: &Arc<()>) -> Option<Arc<PacketCaptureComparison>> {
        Arc::ptr_eq(revision, &self.revision).then(|| Arc::clone(self.visible_model()))
    }

    fn requested_pair(
        &self,
        revision: &Arc<()>,
        selection: &ConversationSelection,
    ) -> Option<(PathBuf, PacketConversationEndpoints, Option<Range<i128>>)> {
        if !Arc::ptr_eq(revision, &self.revision)
            || selection.list != self.list_generation
            || selection.row != self.selected
            || selection.source != self.source
            || selection.elapsed != self.elapsed_scope()
        {
            return None;
        }
        let pair = self.row(self.page * ROWS_PER_PAGE + selection.row?)?;
        let bounds = match &selection.elapsed {
            Some(range) => Some(self.source_interval(selection.source, range.clone())?),
            None => None,
        };
        Some((
            self.paths.get(selection.source)?.clone(),
            pair.endpoints,
            bounds,
        ))
    }

    pub(super) fn pair_excerpt_job(
        &self,
        revision: &Arc<()>,
        selection: &ConversationSelection,
    ) -> Option<impl FnOnce() -> anyhow::Result<PacketCaptureExcerptPlan> + use<>> {
        let (path, endpoints, bounds) = self.requested_pair(revision, selection)?;
        let model = Arc::clone(&self.model);
        let source = selection.source;
        Some(move || model.prepare_conversation_excerpt(source, path, endpoints, bounds))
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
                self.apply_filter();
            }
            Change::Filter(query) if self.conversation_query != query => {
                self.conversation_query = query;
                self.apply_filter();
            }
            Change::Page { list, page }
                if list == self.list_generation
                    && page < self.page_count()
                    && page != self.page =>
            {
                self.set_page(page);
            }
            Change::Pair { list, row } if list == self.list_generation => {
                self.selected =
                    row.filter(|&row| row < ROWS_PER_PAGE && start + row < self.row_count());
            }
            Change::Source(source) if source < self.paths.len() => self.source = source,
            Change::ProtocolsExpanded(expanded) => self.protocols_expanded = expanded,
            Change::TimelineExpanded(expanded) => self.timeline_expanded = expanded,
            Change::CompareWholeCaptures if self.elapsed_model.take().is_some() => {
                self.apply_filter();
            }
            Change::LimitElapsedInterval(limit)
                if !limit || self.selected_timeline_range().is_some() =>
            {
                self.limit_elapsed_interval = limit;
            }
            Change::TimelineInterval(index) if self.timeline_expanded => {
                let range = self.selected_timeline_range()?;
                let last = if range.start() == range.end() {
                    index
                } else {
                    index.max(*range.end())
                };
                let range = index..=last;
                if self
                    .model
                    .timeline
                    .as_ref()?
                    .offset_range(range.clone())
                    .is_some()
                {
                    self.timeline_range = Some(range);
                }
            }
            Change::TimelineEnd { first, last }
                if self.timeline_expanded && self.selected_timeline_range()?.start() == &first =>
            {
                let range = first..=last;
                if self
                    .model
                    .timeline
                    .as_ref()?
                    .offset_range(range.clone())
                    .is_some()
                {
                    self.timeline_range = Some(range);
                }
            }
            Change::OpenTimelineInterval(side, range)
                if self.timeline_expanded
                    && self.selected_timeline_range().as_ref() == Some(&range) =>
            {
                let interval = self.model.timeline.as_ref()?.occupied_range(side, range)?;
                return Some((
                    self.paths.get(side)?.clone(),
                    WiresharkFilter::Interval(interval),
                ));
            }
            Change::Open(selection) => {
                let (path, endpoints, bounds) = self.requested_pair(revision, &selection)?;
                return Some((path, WiresharkFilter::Conversation(endpoints, bounds)));
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
        let model = self.visible_model();
        let view = self.conversation_view;
        let category = view.category();
        let conversation_count = self.row_count();
        let total_conversations = self.unfiltered_count();
        let list = self.list_generation;
        let action = |change: Change| {
            let revision = Arc::clone(&self.revision);
            ctx.callback(move |_| message(Action::Comparison(revision.clone(), change.clone())))
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
        if let Some(window) = &model.window {
            let label = timeline::interval_label(window.start_offset_ns..window.end_offset_ns);
            fields.extend([
                text(
                    "elapsed-scope",
                    format!("Comparing elapsed interval · {label}"),
                ),
                KeyedView::new(
                    "compare-whole-captures",
                    Button::new()
                        .is_enabled(idle)
                        .on_click(action(Change::CompareWholeCaptures))
                        .content("Compare whole captures"),
                ),
            ]);
        }
        for (index, (side, capture)) in [("Before", &model.before), ("After", &model.after)]
            .into_iter()
            .enumerate()
        {
            let span = capture.timestamp_span_ns.map_or_else(
                || {
                    if model.window.is_some() {
                        "No observed packets in interval".into()
                    } else {
                        "No usable timestamps".into()
                    }
                },
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
            if let Some(window) = &model.window {
                let coverage = [&window.before, &window.after][index];
                details.push(text(
                    "source-coverage",
                    format!(
                        "{} · {} untimestamped packets in source",
                        coverage.coverage_label(),
                        coverage.untimestamped_packets
                    ),
                ));
            }
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
                if model.window.is_some() {
                    "Capture deltas unavailable for incomplete interval coverage"
                } else {
                    "Capture deltas unavailable for a partial scan"
                },
            ));
        }
        fields.push(KeyedView::new(
            "average-rates",
            StackPanel::new().spacing(3.0).keyed_children(
                std::iter::once(text("heading", model.average_rates_label())).chain(
                    model
                        .average_rates
                        .report_lines()
                        .into_iter()
                        .enumerate()
                        .map(|(index, line)| text(&format!("rate-{index}"), line)),
                ),
            ),
        ));
        fields.extend(self.timeline_view(idle, ctx));
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
        let filter_revision = Arc::clone(&self.revision);
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
                format!("{category} conversation changes {total_conversations}"),
            ),
            KeyedView::new(
                "conversation-filter",
                TextBox::new()
                    .automation_name("Filter conversations")
                    .placeholder_text(match view {
                        ConversationView::Ip => "IP address or CIDR",
                        ConversationView::Transport => "IP address, CIDR, port, or protocol",
                    })
                    .text(&self.conversation_query)
                    .is_enabled(idle)
                    .on_text_changed(ctx.callback(move |value| {
                        message(Action::Comparison(
                            filter_revision.clone(),
                            Change::Filter(value),
                        ))
                    })),
            ),
        ]);
        if self.matching_rows.is_some() {
            fields.extend([
                text(
                    "match-count",
                    format!("Matching conversations {conversation_count} of {total_conversations}"),
                ),
                KeyedView::new(
                    "clear-filter",
                    Button::new()
                        .is_enabled(idle)
                        .on_click(action(Change::Filter(String::new())))
                        .content("Clear conversation filter"),
                ),
            ]);
        }
        if conversation_count == 0 {
            fields.push(text(
                "empty",
                if self.matching_rows.is_some() {
                    "No conversations match the filter".to_owned()
                } else {
                    format!("No changes in observed {category} conversations")
                },
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
                            .on_click(action(Change::Page {
                                list,
                                page: previous,
                            }))
                            .content("Previous page"),
                        TextBlock::new().text(format!(
                            "Page {} of {}",
                            self.page + 1,
                            self.page_count()
                        )),
                        Button::new()
                            .is_enabled(idle && next < self.page_count())
                            .on_click(action(Change::Page { list, page: next }))
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
                    format!("conversations-{list}"),
                    ListView::new()
                        .automation_name(format!("Changed {category} conversations"))
                        .max_height(320.0)
                        .selected_index(self.selected)
                        .on_selection_changed(ctx.callback(move |row| {
                            message(Action::Comparison(row_revision.clone(), Change::Pair { list, row }))
                        }))
                        .collection_slot(
                            ListViewSlot::Items,
                            (page * ROWS_PER_PAGE..(page + 1) * ROWS_PER_PAGE).filter_map(|index| self.row(index)).map(|change| {
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
            ]);
            if self.elapsed_model.is_none()
                && let Some(label) = self.selected_timeline_label()
            {
                let revision = Arc::clone(&self.revision);
                fields.push(KeyedView::new(
                    "elapsed-conversation-scope",
                    CheckBox::new()
                        .is_enabled(idle)
                        .is_checked(self.limit_elapsed_interval)
                        .on_is_checked_changed(ctx.callback(move |checked| {
                            message(Action::Comparison(
                                revision.clone(),
                                Change::LimitElapsedInterval(checked),
                            ))
                        }))
                        .content(format!("Limit to elapsed interval · {label}")),
                ));
            }
            let elapsed = self.elapsed_scope();
            let can_scope = elapsed
                .as_ref()
                .is_none_or(|range| self.source_interval(self.source, range.clone()).is_some());
            if !can_scope {
                fields.push(text(
                    "no-elapsed-origin",
                    "The selected capture has no timestamped records to align the interval.",
                ));
            }
            let selection = ConversationSelection {
                list,
                row: self.selected,
                source: self.source,
                elapsed,
            };
            let can_open = idle && self.selected.is_some() && can_scope;
            fields.extend([
                KeyedView::new(
                    "open-conversation",
                    Button::new()
                        .is_enabled(can_open)
                        .on_click(action(Change::Open(selection.clone())))
                        .content("Open conversation in Wireshark"),
                ),
                KeyedView::new(
                    "save-conversation-excerpt",
                    Button::new()
                        .is_enabled(can_open)
                        .on_click(action(Change::SaveConversationExcerpt(selection)))
                        .content("Save conversation excerpt…"),
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
