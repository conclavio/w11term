use super::{compare::ConversationView, *};
use w11term::ssh::PacketConversationQuery;

const PAGE_SIZE: usize = 10;

pub enum Change {
    Expanded(bool),
    Filter(String),
    Page {
        generation: u64,
        page: usize,
    },
    Select {
        generation: u64,
        row: Option<usize>,
    },
    Launch {
        generation: u64,
        row: Option<usize>,
        destination: PairDestination,
    },
}

#[derive(Default)]
pub(super) struct List {
    expanded: bool,
    query: String,
    matches: Option<Vec<usize>>,
    page: usize,
    selected: Option<usize>,
    generation: u64,
}

struct Row {
    endpoints: PacketConversationEndpoints,
    a_to_b: (u32, u64),
    b_to_a: (u32, u64),
}

impl Row {
    fn at(capture: &LocalPacketCapture, view: ConversationView, index: usize) -> Option<Self> {
        match view {
            ConversationView::Ip => {
                let pair = capture.summary.conversations.pair(index)?;
                Some(Self {
                    endpoints: PacketConversationEndpoints::Ip {
                        a: pair.a,
                        b: pair.b,
                    },
                    a_to_b: (pair.a_to_b.packets, pair.a_to_b.captured_bytes),
                    b_to_a: (pair.b_to_a.packets, pair.b_to_a.captured_bytes),
                })
            }
            ConversationView::Transport => {
                let pair = capture.summary.transport_conversations.pair(index)?;
                Some(Self {
                    endpoints: PacketConversationEndpoints::Transport {
                        protocol: pair.protocol,
                        a: pair.a,
                        b: pair.b,
                    },
                    a_to_b: (pair.a_to_b.packets, pair.a_to_b.captured_bytes),
                    b_to_a: (pair.b_to_a.packets, pair.b_to_a.captured_bytes),
                })
            }
        }
    }

    fn view(self, view: ConversationView) -> KeyedView {
        let (forward_packets, forward_bytes) = self.a_to_b;
        let (reverse_packets, reverse_bytes) = self.b_to_a;
        let packets = forward_packets + reverse_packets;
        let bytes = forward_bytes + reverse_bytes;
        let label = self.endpoints.to_string();
        let (totals, directions) = match view {
            ConversationView::Ip => (
                format!("{packets} packets · {bytes} captured bytes"),
                format!(
                    "Left to right {forward_bytes} bytes · Right to left {reverse_bytes} bytes"
                ),
            ),
            ConversationView::Transport => (
                format!("Packets {packets} · Captured bytes {bytes}"),
                format!(
                    "Left to right {forward_packets} packets, {forward_bytes} bytes · Right to left {reverse_packets} packets, {reverse_bytes} bytes"
                ),
            ),
        };
        KeyedView::new(
            label.as_str(),
            ListViewItem::new().automation_name(&label).content(
                StackPanel::new().spacing(3.0).keyed_children([
                    text("endpoints", label.clone()),
                    text("totals", totals),
                    text("directions", directions),
                ]),
            ),
        )
    }
}

impl List {
    fn count(&self, total: usize) -> usize {
        self.matches.as_ref().map_or(total, Vec::len)
    }

    fn row(
        &self,
        capture: &LocalPacketCapture,
        view: ConversationView,
        index: usize,
    ) -> Option<Row> {
        let rank = match &self.matches {
            Some(matches) => *matches.get(index)?,
            None => index,
        };
        Row::at(capture, view, rank)
    }

    fn set_page(&mut self, page: usize) {
        self.page = page;
        self.selected = None;
        self.generation = self.generation.wrapping_add(1);
    }

    pub(super) fn update(
        &mut self,
        capture: &LocalPacketCapture,
        view: ConversationView,
        change: Change,
    ) -> Option<(PacketConversationEndpoints, PairDestination)> {
        if let Change::Expanded(expanded) = change {
            self.expanded = expanded;
            return None;
        }
        if !self.expanded {
            return None;
        }
        let total = tracked(capture, view);
        match change {
            Change::Filter(query) if query != self.query => {
                self.query = query;
                let filter = PacketConversationQuery::new(&self.query);
                self.matches = (!filter.is_empty()).then(|| {
                    (0..total)
                        .filter(|&index| {
                            Row::at(capture, view, index)
                                .is_some_and(|row| filter.matches(row.endpoints))
                        })
                        .collect()
                });
                self.set_page(0);
            }
            Change::Page { generation, page }
                if generation == self.generation
                    && page < self.count(total).div_ceil(PAGE_SIZE)
                    && page != self.page =>
            {
                self.set_page(page);
            }
            Change::Select { generation, row } if generation == self.generation => {
                self.selected = row.filter(|&row| {
                    row < PAGE_SIZE && self.page * PAGE_SIZE + row < self.count(total)
                });
            }
            Change::Launch {
                generation,
                row,
                destination,
            } if generation == self.generation && row == self.selected => {
                let selected = self.row(capture, view, self.page * PAGE_SIZE + row?)?;
                return Some((selected.endpoints, destination));
            }
            _ => (),
        }
        None
    }
}

fn tracked(capture: &LocalPacketCapture, view: ConversationView) -> usize {
    match view {
        ConversationView::Ip => capture.summary.conversations.tracked_pairs,
        ConversationView::Transport => capture.summary.transport_conversations.tracked_pairs,
    }
}

impl Panel {
    pub(super) fn conversation_list_mut(&mut self, view: ConversationView) -> &mut List {
        match view {
            ConversationView::Ip => &mut self.ip_conversations,
            ConversationView::Transport => &mut self.transport_conversations,
        }
    }

    pub(super) fn conversation_browser(
        &self,
        capture: &Arc<LocalPacketCapture>,
        view: ConversationView,
        idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> Option<KeyedView> {
        let total = tracked(capture, view);
        let (list, title, id, coverage, omitted) = match view {
            ConversationView::Ip => {
                if capture.summary.stats.packets == 0 {
                    return None;
                }
                let counts = &capture.summary.conversations;
                (
                    &self.ip_conversations,
                    "IP conversations",
                    "packet-ip-conversations",
                    format!(
                        "Tracked IP pairs {total} · Unclassified packets {}",
                        counts.unclassified_packets
                    ),
                    counts.untracked_packets,
                )
            }
            ConversationView::Transport => {
                let counts = &capture.summary.transport_conversations;
                if total == 0 && counts.packets_without_ports == 0 {
                    return None;
                }
                (
                    &self.transport_conversations,
                    "TCP and UDP conversations",
                    "packet-transport-conversations",
                    format!(
                        "Tracked transport pairs {total} · TCP or UDP packets without ports {}",
                        counts.packets_without_ports
                    ),
                    counts.untracked_packets,
                )
            }
        };
        let mut fields = Vec::new();
        if list.expanded {
            fields.push(text("coverage", coverage));
            if omitted != 0 {
                let kind = if view == ConversationView::Ip {
                    "IP"
                } else {
                    "Transport"
                };
                fields.push(text(
                    "limit",
                    format!(
                        "{kind} pair limit reached · {omitted} packets omitted from the ranking"
                    ),
                ));
            }
            if total != 0 {
                fields.extend(list.controls(capture, view, total, idle, ctx));
            }
            if list.count(total) != 0 {
                let (scope_label, open, save) = match view {
                    ConversationView::Ip => (
                        "Limit to timeline interval",
                        "Open conversation in Wireshark",
                        "Save conversation excerpt…",
                    ),
                    ConversationView::Transport => (
                        "Limit port pair to timeline interval",
                        "Open port pair in Wireshark",
                        "Save port pair excerpt…",
                    ),
                };
                fields.extend(self.conversation_interval_checkbox(capture, scope_label, idle, ctx));
                fields.extend(
                    [
                        (open, PairDestination::Wireshark),
                        (save, PairDestination::Excerpt),
                    ]
                    .map(|(label, destination)| {
                        let capture = Arc::clone(capture);
                        let generation = list.generation;
                        let row = list.selected;
                        KeyedView::new(
                            label,
                            Button::new()
                                .is_enabled(idle && row.is_some())
                                .on_click(ctx.callback(move |_| {
                                    message(Action::LocalConversations(
                                        capture.clone(),
                                        view,
                                        Change::Launch {
                                            generation,
                                            row,
                                            destination,
                                        },
                                    ))
                                }))
                                .content(label),
                        )
                    }),
                );
            }
        }
        let capture = Arc::clone(capture);
        Some(KeyedView::new(
            id,
            Expander::new()
                .automation_name(title)
                .automation_id(id)
                .is_expanded(list.expanded)
                .on_is_expanded_changed(ctx.callback(move |expanded| {
                    message(Action::LocalConversations(
                        capture.clone(),
                        view,
                        Change::Expanded(expanded),
                    ))
                }))
                .slots([
                    SlotView::new(ExpanderSlot::Header, TextBlock::new().text(title)),
                    SlotView::new(
                        ExpanderSlot::Content,
                        StackPanel::new().spacing(6.0).keyed_children(fields),
                    ),
                ]),
        ))
    }
}

impl List {
    fn controls(
        &self,
        capture: &Arc<LocalPacketCapture>,
        view: ConversationView,
        total: usize,
        idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> Vec<KeyedView> {
        let (kind, plural, filter_name, placeholder, list_name) = match view {
            ConversationView::Ip => (
                "IP",
                "IP pairs",
                "Filter IP conversations",
                "IP address or CIDR",
                "IP conversations by captured bytes",
            ),
            ConversationView::Transport => (
                "port pair",
                "port pairs",
                "Filter port pairs",
                "IP address, CIDR, port, or protocol",
                "TCP and UDP port pairs by captured bytes",
            ),
        };
        let count = self.count(total);
        let page_count = count.div_ceil(PAGE_SIZE);
        let filter_source = Arc::clone(capture);
        let mut fields = vec![
            text("rank-order", "Ranked by captured bytes"),
            KeyedView::new(
                "filter",
                TextBox::new()
                    .automation_name(filter_name)
                    .placeholder_text(placeholder)
                    .text(&self.query)
                    .is_enabled(idle)
                    .on_text_changed(ctx.callback(move |query| {
                        message(Action::LocalConversations(
                            filter_source.clone(),
                            view,
                            Change::Filter(query),
                        ))
                    })),
            ),
        ];
        if self.matches.is_some() {
            let capture = Arc::clone(capture);
            fields.extend([
                text("matches", format!("Matching {plural} {count} of {total}")),
                KeyedView::new(
                    "clear",
                    Button::new()
                        .is_enabled(idle)
                        .on_click(ctx.callback(move |_| {
                            message(Action::LocalConversations(
                                capture.clone(),
                                view,
                                Change::Filter(String::new()),
                            ))
                        }))
                        .content(format!("Clear {kind} filter")),
                ),
            ]);
        }
        if count == 0 {
            fields.push(text("no-matches", format!("No {plural} match the filter")));
            return fields;
        }

        let generation = self.generation;
        if page_count > 1 {
            let page_button = |label, page, enabled| {
                let capture = Arc::clone(capture);
                Button::new()
                    .automation_name(format!("{label} {kind} page"))
                    .is_enabled(idle && enabled)
                    .on_click(ctx.callback(move |_| {
                        message(Action::LocalConversations(
                            capture.clone(),
                            view,
                            Change::Page { generation, page },
                        ))
                    }))
                    .content(label)
            };
            fields.push(KeyedView::new(
                "pages",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(6.0)
                    .children((
                        page_button("Previous", self.page.saturating_sub(1), self.page != 0),
                        TextBlock::new().text(format!("Page {} of {page_count}", self.page + 1)),
                        page_button("Next", self.page + 1, self.page + 1 < page_count),
                    )),
            ));
        }
        let selection_source = Arc::clone(capture);
        let start = self.page * PAGE_SIZE;
        fields.push(KeyedView::new(
            format!("ranked-{generation}"),
            ListView::new()
                .automation_name(list_name)
                .max_height(280.0)
                .selected_index(self.selected)
                .on_selection_changed(ctx.callback(move |row| {
                    message(Action::LocalConversations(
                        selection_source.clone(),
                        view,
                        Change::Select { generation, row },
                    ))
                }))
                .collection_slot(
                    ListViewSlot::Items,
                    (start..(start + PAGE_SIZE).min(count))
                        .filter_map(|index| self.row(capture, view, index))
                        .map(|row| row.view(view)),
                ),
        ));
        fields
    }
}
