use super::*;
use std::io::{self, Write};

const REFERENCE: &str = "Use output as reference";
const COMPARE: &str = "Compare with reference";
const CLEAR: &str = "Clear reference";
const IDENTICAL: &str = "No output changes.";

#[derive(Clone)]
pub enum Request {
    SetReference(OutputCapture),
    ClearReference(OutputCapture),
    Show {
        reference: OutputCapture,
        selected: OutputCapture,
    },
    Back(Arc<()>),
    Save(Arc<()>),
}

pub(super) struct Panel {
    request: Arc<()>,
    reference: OutputCapture,
    selected: OutputCapture,
    before: String,
    after: String,
    result: Option<Result<text_diff::Difference, &'static str>>,
}

impl TerminalApp {
    fn capture_output(&self, capture: OutputCapture) -> Option<Arc<CompletedCommand>> {
        self.history_frame(capture.pane)?
            .command_for_link(capture.link)
            .filter(|command| command.output.is_ok())
            .cloned()
    }

    fn capture_heading(&self, capture: OutputCapture, entry: &CompletedCommand) -> String {
        let label = self
            .tab_for_pane(capture.pane)
            .and_then(|id| self.tabs.get(id))
            .map(|tab| {
                tab.pane_name()
                    .map_or(tab.label.as_str(), |title| title.as_str())
            })
            .unwrap_or_default();
        format!(
            "{label} · Pane {} · Capture {} · {} · {}",
            capture.pane,
            capture.link.id,
            command_status(entry),
            command_name(entry),
        )
    }

    pub(super) fn history_compare(
        &mut self,
        page: TabId,
        pane: u64,
        request: Request,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(Page::CommandHistory(history)) = self.tabs.get(page).map(|tab| &tab.page) else {
            return;
        };
        let selected = history.selected.map(|(generation, id)| OutputCapture {
            pane,
            link: CommandLink { generation, id },
        });
        match request {
            Request::SetReference(capture)
                if history.comparison.is_none() && selected == Some(capture) =>
            {
                if self.capture_output(capture).is_some() {
                    self.history_reference = Some(capture);
                }
            }
            Request::ClearReference(capture) if self.history_reference == Some(capture) => {
                self.history_reference = None;
            }
            Request::Show {
                reference,
                selected: capture,
            } if selected == Some(capture)
                && reference != capture
                && history.comparison.is_none()
                && self.history_reference == Some(reference) =>
            {
                let Some(before) = self.capture_output(reference) else {
                    return;
                };
                let Some(after) = self.capture_output(capture) else {
                    return;
                };
                let request = Arc::new(());
                let panel = Panel {
                    request: Arc::clone(&request),
                    reference,
                    selected: capture,
                    before: self.capture_heading(reference, &before),
                    after: self.capture_heading(capture, &after),
                    result: None,
                };
                let Page::CommandHistory(history) = &mut self.tabs.get_mut(page).unwrap().page
                else {
                    return;
                };
                history.comparison = Some(panel);
                let ready = move |result| {
                    Message::History(Action::ComparisonReady {
                        page,
                        request: Arc::clone(&request),
                        result,
                    })
                };
                let rejected = ready(Err("Could not start the output comparison."));
                ctx.spawn_background_with_rejection(
                    move |cancel| {
                        if cancel.is_cancelled() {
                            return ready(Err("Output comparison cancelled."));
                        }
                        ready(Ok(text_diff::Difference::build(
                            before.output.as_ref().unwrap(),
                            after.output.as_ref().unwrap(),
                        )))
                    },
                    rejected,
                );
            }
            Request::Back(request) => {
                if let Some(history) = self.current_history_comparison(page, &request) {
                    history.comparison = None;
                }
            }
            Request::Save(request) => self.save_history_comparison(page, &request, ctx),
            _ => {}
        }
    }

    fn current_history_comparison(
        &mut self,
        page: TabId,
        request: &Arc<()>,
    ) -> Option<&mut HistoryPage> {
        let Page::CommandHistory(history) = &mut self.tabs.get_mut(page)?.page else {
            return None;
        };
        Arc::ptr_eq(&history.comparison.as_ref()?.request, request).then_some(history)
    }

    pub(super) fn finish_history_comparison(
        &mut self,
        page: TabId,
        request: &Arc<()>,
        result: Result<text_diff::Difference, &'static str>,
    ) {
        if let Some(panel) = self
            .current_history_comparison(page, request)
            .and_then(|history| history.comparison.as_mut())
        {
            panel.result = Some(result);
        }
    }

    fn save_history_comparison(
        &mut self,
        page: TabId,
        request: &Arc<()>,
        ctx: &ComponentContext<Self>,
    ) {
        if self.export.pending.is_some() {
            return;
        }
        let Some(Page::CommandHistory(history)) = self.tabs.get(page).map(|tab| &tab.page) else {
            return;
        };
        let Some(panel) = history.comparison.as_ref().filter(|panel| {
            Arc::ptr_eq(&panel.request, request) && matches!(panel.result, Some(Ok(_)))
        }) else {
            return;
        };
        let (reference, selected) = (panel.reference, panel.selected);
        let Some(before) = self.capture_output(reference) else {
            return;
        };
        let Some(after) = self.capture_output(selected) else {
            return;
        };
        let (before_heading, after_heading) = (panel.before.clone(), panel.after.clone());
        self.begin_text_export(
            text_export::Subject::CommandOutputDiff {
                reference_pane: reference.pane,
                reference: reference.link.id,
                selected_pane: selected.pane,
                selected: selected.link.id,
            },
            move |file| {
                let mut file = io::BufWriter::new(file);
                writeln!(file, "Reference · {before_heading}\r")?;
                writeln!(file, "Selected · {after_heading}\r")?;
                text_diff::write_changes(
                    &mut file,
                    before.output.as_ref().unwrap(),
                    after.output.as_ref().unwrap(),
                    IDENTICAL,
                )?;
                file.flush()
            },
            ctx,
        );
    }

    pub(super) fn history_reference_view(
        &self,
        pane: u64,
        generation: u64,
        selected: Option<&CompletedCommand>,
        ctx: &ViewContext<Self>,
    ) -> View {
        let selected = selected
            .filter(|entry| entry.output.is_ok())
            .map(|entry| OutputCapture {
                pane,
                link: CommandLink {
                    generation,
                    id: entry.id,
                },
            });
        let reference = self.history_reference;
        if selected.is_none() && reference.is_none() {
            return View::empty();
        }
        let retained = reference
            .and_then(|capture| self.capture_output(capture).map(|entry| (capture, entry)));
        let reference_caption = if let Some((capture, entry)) = &retained {
            format!("Reference · {}", self.capture_heading(*capture, entry))
        } else if reference.is_some() {
            "The reference capture is no longer retained.".into()
        } else {
            String::new()
        };
        let compare = retained
            .as_ref()
            .map(|(capture, _)| *capture)
            .zip(selected)
            .filter(|(reference, selected)| reference != selected);
        let button = |label: &'static str, request: Option<Request>| {
            Button::new()
                .is_enabled(request.is_some())
                .on_click(ctx.callback(move |_| {
                    request.clone().map_or(Message::ControlIdle, |request| {
                        Message::History(Action::InPane(pane, HistoryAction::Compare(request)))
                    })
                }))
                .content(label)
        };
        StackPanel::new().spacing(4.0).children((
            if reference_caption.is_empty() {
                View::empty()
            } else {
                TextBlock::new()
                    .text(reference_caption)
                    .text_wrapping(TextWrapping::Wrap)
                    .into()
            },
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children((
                    button(
                        REFERENCE,
                        selected
                            .filter(|capture| Some(*capture) != reference)
                            .map(Request::SetReference),
                    ),
                    if let Some(reference) = reference {
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(6.0)
                            .children((
                                button(
                                    COMPARE,
                                    compare.map(|(reference, selected)| Request::Show {
                                        reference,
                                        selected,
                                    }),
                                ),
                                button(CLEAR, Some(Request::ClearReference(reference))),
                            ))
                    } else {
                        View::empty()
                    },
                )),
        ))
    }

    pub(super) fn history_comparison_view(
        &self,
        pane: u64,
        panel: &Panel,
        ctx: &ViewContext<Self>,
    ) -> View {
        let request = Arc::clone(&panel.request);
        let save_request = Arc::clone(&panel.request);
        let retained = self.capture_output(panel.reference).is_some()
            && self.capture_output(panel.selected).is_some();
        let can_save =
            retained && self.export.pending.is_none() && matches!(panel.result, Some(Ok(_)));
        Grid::new()
            .margin(16.0)
            .row_spacing(8.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                StackPanel::new().spacing(6.0).children((
                    TextBlock::new()
                        .font_size(24.0)
                        .text("Command output differences"),
                    TextBlock::new()
                        .text(format!("Reference · {}", panel.before))
                        .text_wrapping(TextWrapping::Wrap),
                    TextBlock::new()
                        .text(format!("Selected · {}", panel.after))
                        .text_wrapping(TextWrapping::Wrap),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .on_click(ctx.callback(move |_| {
                                    Message::History(Action::InPane(
                                        pane,
                                        HistoryAction::Compare(Request::Back(Arc::clone(&request))),
                                    ))
                                }))
                                .content("Back to command history"),
                            Button::new()
                                .is_enabled(can_save)
                                .on_click(ctx.callback(move |_| {
                                    Message::History(Action::InPane(
                                        pane,
                                        HistoryAction::Compare(Request::Save(Arc::clone(
                                            &save_request,
                                        ))),
                                    ))
                                }))
                                .content("Save full differences…"),
                        )),
                )),
                ScrollViewer::new()
                    .grid_row(1)
                    .content(text_diff::output_view(panel.result.as_ref(), IDENTICAL)),
            ))
    }
}
