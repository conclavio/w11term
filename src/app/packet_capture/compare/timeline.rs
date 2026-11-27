use super::*;

const CHART_HEIGHT: f64 = 48.0;
const SIDE_LABELS: [&str; 2] = ["Before", "After"];

impl Comparison {
    pub(super) fn selected_timeline_interval(&self) -> Option<usize> {
        self.timeline_interval
            .or_else(|| self.model.timeline.as_ref()?.peak_index())
    }

    pub(super) fn timeline_view(
        &self,
        idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> Option<KeyedView> {
        let timeline = self.model.timeline.as_ref()?;
        let mut fields = Vec::new();
        if self.timeline_expanded {
            let selected = self.selected_timeline_interval()?;
            let intervals: Vec<_> = (0..timeline.buckets.len())
                .filter_map(|index| timeline.offset_interval(index))
                .map(|interval| {
                    format!(
                        "{} to {} s",
                        signed_seconds(interval.start),
                        signed_seconds(interval.end),
                    )
                })
                .collect();
            let peak = timeline
                .buckets
                .iter()
                .flat_map(|bucket| [bucket.before, bucket.after])
                .flatten()
                .map(|bucket| bucket.packets)
                .max()
                .unwrap_or(1);
            fields.extend([
                text(
                    "alignment",
                    "Elapsed time from each file's first timestamped record",
                ),
                text(
                    "bucket-width",
                    format!(
                        "Interval width {} s",
                        seconds_text(timeline.bucket_width_ns as u128)
                    ),
                ),
                text(
                    "before",
                    format!(
                        "Before · {} of {} packets timestamped",
                        self.model.before.timestamped_packets, self.model.before.packets
                    ),
                ),
                KeyedView::new(
                    "before-chart",
                    chart(
                        "Before packet timeline chart",
                        timeline
                            .buckets
                            .iter()
                            .map(|bucket| bucket.before.map(|counts| counts.packets)),
                        timeline.buckets.len(),
                        peak,
                        selected,
                        Color::rgb(96, 165, 230),
                    ),
                ),
                text(
                    "after",
                    format!(
                        "After · {} of {} packets timestamped",
                        self.model.after.timestamped_packets, self.model.after.packets
                    ),
                ),
                KeyedView::new(
                    "after-chart",
                    chart(
                        "After packet timeline chart",
                        timeline
                            .buckets
                            .iter()
                            .map(|bucket| bucket.after.map(|counts| counts.packets)),
                        timeline.buckets.len(),
                        peak,
                        selected,
                        Color::rgb(120, 193, 152),
                    ),
                ),
            ]);
            let revision = Arc::clone(&self.revision);
            fields.push(KeyedView::new(
                "interval",
                ComboBox::new()
                    .automation_name("Compared timeline intervals")
                    .items_source(intervals)
                    .selected_index(Some(selected))
                    .is_enabled(idle)
                    .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                        index.map_or(Message::ControlIdle, |index| {
                            message(Action::Comparison(
                                revision.clone(),
                                Change::TimelineInterval(index),
                            ))
                        })
                    })),
            ));
            for (side, label) in SIDE_LABELS.into_iter().enumerate() {
                let observation = timeline.buckets[selected].observation(side);
                let detail = observation.map_or_else(
                    || "Outside timestamped coverage".to_owned(),
                    |bucket| {
                        format!(
                            "{} packets · {} captured bytes",
                            bucket.packets, bucket.captured_bytes
                        )
                    },
                );
                let revision = Arc::clone(&self.revision);
                fields.push(text(
                    &format!("count-{side}"),
                    format!("{label} interval · {detail}"),
                ));
                fields.push(KeyedView::new(
                    format!("open-{side}"),
                    Button::new()
                        .is_enabled(idle && observation.is_some_and(|bucket| bucket.packets != 0))
                        .on_click(ctx.callback(move |_| {
                            message(Action::Comparison(
                                revision.clone(),
                                Change::OpenTimelineInterval(side, selected),
                            ))
                        }))
                        .content(format!("Open {label} interval in Wireshark")),
                ));
            }
        }
        let revision = Arc::clone(&self.revision);
        Some(KeyedView::new(
            "elapsed-timeline",
            Expander::new()
                .automation_name("Elapsed packet timeline")
                .automation_id("comparison-packet-timeline")
                .is_expanded(self.timeline_expanded)
                .on_is_expanded_changed(ctx.callback(move |expanded| {
                    message(Action::Comparison(
                        revision.clone(),
                        Change::TimelineExpanded(expanded),
                    ))
                }))
                .slots([
                    SlotView::new(
                        ExpanderSlot::Header,
                        TextBlock::new().text("Elapsed packet timeline"),
                    ),
                    SlotView::new(
                        ExpanderSlot::Content,
                        StackPanel::new().spacing(6.0).keyed_children(fields),
                    ),
                ]),
        ))
    }
}

fn signed_seconds(ns: i128) -> String {
    let sign = if ns < 0 { "-" } else { "+" };
    format!("{sign}{}", seconds_text(ns.unsigned_abs()))
}

fn chart(
    name: &str,
    counts: impl Iterator<Item = Option<u32>>,
    intervals: usize,
    peak: u32,
    selected: usize,
    bar_color: Color,
) -> View {
    let cells = counts.enumerate().map(|(column, count)| {
        let background = match (count.is_some(), column == selected) {
            (true, true) => Color::rgb(74, 76, 90),
            (true, false) => Color::rgb(44, 44, 48),
            (false, true) => Color::rgb(51, 51, 57),
            (false, false) => Color::rgb(29, 29, 31),
        };
        let cell = Border::new()
            .grid_column(column as i32)
            .background(background);
        let cell = if let Some(count) = count.filter(|&count| count != 0) {
            cell.content(
                Border::new()
                    .vertical_alignment(VerticalAlignment::Bottom)
                    .height((f64::from(count) / f64::from(peak) * CHART_HEIGHT).max(1.0))
                    .background(bar_color),
            )
        } else {
            cell.into()
        };
        KeyedView::new(column.to_string(), cell)
    });
    Border::new()
        .height(CHART_HEIGHT)
        .automation_name(name)
        .content(
            Grid::new()
                .columns((0..intervals).map(|_| GridLength::Star(1.0)))
                .column_spacing(1.0)
                .keyed_children(cells),
        )
}
