use super::*;

const ROWS_PER_PAGE: usize = 12;

pub(super) struct Comparison {
    model: Box<PacketCaptureComparison>,
    page: usize,
}

impl Comparison {
    pub(super) fn new(model: Box<PacketCaptureComparison>) -> Self {
        Self { model, page: 0 }
    }

    fn page_count(&self) -> usize {
        self.model
            .conversation_changes
            .len()
            .div_ceil(ROWS_PER_PAGE)
            .max(1)
    }

    pub(super) fn select_page(&mut self, page: usize) {
        if page < self.page_count() {
            self.page = page;
        }
    }

    pub(super) fn view(&self, ctx: &ViewContext<TerminalApp>) -> View {
        let model = &self.model;
        let mut fields = vec![KeyedView::new(
            "back",
            Button::new()
                .on_click(ctx.callback(|_| message(Action::BackToCapture)))
                .content("Back to capture"),
        )];
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
                            .is_enabled(self.page != 0)
                            .on_click(ctx.callback(move |_| message(Action::ComparePage(previous))))
                            .content("Previous page"),
                        TextBlock::new().text(format!(
                            "Page {} of {}",
                            self.page + 1,
                            self.page_count()
                        )),
                        Button::new()
                            .is_enabled(next < self.page_count())
                            .on_click(ctx.callback(move |_| message(Action::ComparePage(next))))
                            .content("Next page"),
                    )),
            ));
        }
        fields.extend(model.conversation_changes.iter().skip(self.page * ROWS_PER_PAGE).take(ROWS_PER_PAGE).map(|change| {
            let label = format!("{} ↔ {}", change.a, change.b);
            let mut details = vec![text("addresses", label.clone())];
            for (side, traffic) in [("Before", change.before), ("After", change.after)] {
                let label = traffic.map_or_else(
                    || format!("{side} · Pair coverage unavailable"),
                    |traffic| format!(
                        "{side} · Left to right {} packets, {} bytes · Right to left {} packets, {} bytes",
                        traffic.a_to_b.packets, traffic.a_to_b.captured_bytes,
                        traffic.b_to_a.packets, traffic.b_to_a.captured_bytes
                    ),
                );
                details.push(text(side, label));
            }
            if let Some(delta) = &change.delta {
                details.push(text("delta", format!(
                    "Change · {:+} packets · {:+} captured bytes",
                    delta.packets, delta.captured_bytes,
                )));
            }
            KeyedView::new(label, StackPanel::new().spacing(3.0).keyed_children(details))
        }));
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
