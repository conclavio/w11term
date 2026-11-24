use super::*;
use std::io::{self, Write};

pub enum Action {
    Open(u64),
    Choose {
        revision: u64,
        baseline: Option<u64>,
    },
    Compare {
        revision: u64,
        baseline: u64,
    },
    Ready {
        revision: u64,
        result: Result<diff::Difference, &'static str>,
    },
    Save {
        revision: u64,
    },
    Back {
        revision: u64,
    },
}

pub(super) struct Open {
    run: u64,
    pane: u64,
    baseline: Option<u64>,
    comparison: Option<Pair>,
    feedback: Option<String>,
}

struct Pair {
    earlier: Side,
    current: Side,
    result: Option<Result<diff::Difference, &'static str>>,
}

#[derive(Clone)]
pub(super) struct Side {
    heading: String,
    pub(super) exit_code: i32,
    pub(super) output: report::Output,
}

fn earlier_runs<'a>(
    page: &'a HostGroup,
    current: &'a Submission,
    pane: u64,
) -> impl Iterator<Item = (&'a Submission, &'a Delivery)> {
    let address = current
        .results
        .iter()
        .find(|row| row.target.pane == pane)
        .map(|row| row.target.names.address());
    page.submissions
        .iter()
        .rev()
        .filter(|run| run.id < current.id)
        .filter_map(move |run| {
            let address = address?;
            let row = run
                .results
                .iter()
                .find(|row| row.target.pane == pane && row.target.names.address() == address)?;
            Some((run, row))
        })
}

fn identical(earlier: i32, current: i32) -> &'static str {
    if earlier == current {
        "Output and exit codes are identical."
    } else {
        "Output is identical. The exit codes differ."
    }
}

fn message(tab: TabId, run: u64, action: Action) -> Message {
    Message::HostGroup(super::super::Action::Compare(
        tab,
        run,
        super::Action::Runs(action),
    ))
}

impl TerminalApp {
    pub(in crate::app::host_groups) fn compare_run_button(
        &self,
        tab: TabId,
        current: &Submission,
        pane: u64,
        ctx: &ViewContext<Self>,
    ) -> Option<View> {
        let Page::HostGroup(page) = &self.tabs.get(tab)?.page else {
            return None;
        };
        earlier_runs(page, current, pane).next()?;
        let run = current.id;
        Some(
            Button::new()
                .automation_name(format!("Compare earlier run for pane {pane}"))
                .on_click(ctx.callback(move |_| message(tab, run, Action::Open(pane))))
                .content("Compare with earlier run…"),
        )
    }

    pub(super) fn handle_group_run_diff(
        &mut self,
        tab: TabId,
        run: u64,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        match action {
            Action::Open(pane) => {
                if self.host_group_command_target() == Some(tab)
                    && let Some(page) = self.host_group_mut(tab)
                    && let Some(current) = page.viewed_submission(run)
                    && earlier_runs(page, current, pane).next().is_some()
                {
                    page.comparison.revision += 1;
                    page.comparison.runs = Some(Box::new(Open {
                        run,
                        pane,
                        baseline: None,
                        comparison: None,
                        feedback: None,
                    }));
                }
            }
            Action::Choose { revision, baseline } => {
                let Some(page) = self.host_group_mut(tab) else {
                    return;
                };
                let Some(open) = page.comparison.runs.as_ref().filter(|open| {
                    open.run == run
                        && page.comparison.revision == revision
                        && open.baseline != baseline
                }) else {
                    return;
                };
                if let Some(baseline) = baseline
                    && !page.viewed_submission(run).is_some_and(|current| {
                        earlier_runs(page, current, open.pane)
                            .any(|(earlier, _)| earlier.id == baseline)
                    })
                {
                    return;
                }
                let open = page.comparison.runs.as_mut().expect("reviewed comparison");
                open.baseline = baseline;
                open.comparison = None;
                open.feedback = None;
                page.comparison.revision += 1;
            }
            Action::Compare { revision, baseline } => {
                self.compare_group_run_outputs(tab, run, revision, baseline, ctx);
            }
            Action::Ready { revision, result } => {
                if let Some(Page::HostGroup(page)) = self.tabs.get_mut(tab).map(|tab| &mut tab.page)
                    && page.comparison.revision == revision
                    && let Some(pair) = page
                        .comparison
                        .runs
                        .as_mut()
                        .filter(|open| open.run == run)
                        .and_then(|open| open.comparison.as_mut())
                {
                    pair.result = Some(result);
                }
            }
            Action::Save { revision } => self.save_group_run_diff(tab, run, revision, ctx),
            Action::Back { revision } => {
                if let Some(page) = self.host_group_mut(tab)
                    && page.comparison.revision == revision
                    && page
                        .comparison
                        .runs
                        .as_ref()
                        .is_some_and(|open| open.run == run)
                {
                    page.comparison.runs = None;
                }
            }
        }
    }

    pub(super) fn capture_run_side(
        &self,
        run: &Submission,
        row: &Delivery,
    ) -> Result<Side, String> {
        let (exit_code, _, output) = capture_output(row, |link| {
            self.history_frame(row.target.pane)?
                .command_for_link(link)
                .cloned()
        })
        .map_err(|error| format!("Run {} · {error}", run.id))?;
        let command = run
            .content
            .host_command(row.target.pane)
            .unwrap_or_else(|| run.content.label());
        Ok(Side {
            heading: format!("Run {} · {command} · Exit {exit_code}", run.id),
            exit_code,
            output,
        })
    }

    fn compare_group_run_outputs(
        &mut self,
        tab: TabId,
        run: u64,
        revision: u64,
        baseline: u64,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(Page::HostGroup(page)) = self.tabs.get(tab).map(|tab| &tab.page) else {
            return;
        };
        let Some(open) = page.comparison.runs.as_ref().filter(|open| {
            open.run == run
                && page.comparison.revision == revision
                && open.baseline == Some(baseline)
                && open
                    .comparison
                    .as_ref()
                    .is_none_or(|pair| pair.result.is_some())
        }) else {
            return;
        };
        let Some(current) = page.viewed_submission(run) else {
            return;
        };
        let Some((earlier, previous)) =
            earlier_runs(page, current, open.pane).find(|(earlier, _)| earlier.id == baseline)
        else {
            return;
        };
        let Some(row) = current
            .results
            .iter()
            .find(|row| row.target.pane == open.pane)
        else {
            return;
        };
        let pair = self
            .capture_run_side(earlier, previous)
            .and_then(|earlier| {
                self.capture_run_side(current, row).map(|current| Pair {
                    earlier,
                    current,
                    result: None,
                })
            });
        let Some(page) = self.host_group_mut(tab) else {
            return;
        };
        page.comparison.revision += 1;
        let revision = page.comparison.revision;
        let open = page.comparison.runs.as_mut().expect("reviewed comparison");
        open.feedback = None;
        open.comparison = None;
        let pair = match pair {
            Ok(pair) => pair,
            Err(error) => {
                open.feedback = Some(error);
                return;
            }
        };
        build_diff(tab, run, revision, &pair, ctx);
        open.comparison = Some(pair);
    }

    pub(super) fn open_overview_run_diff(
        &mut self,
        tab: TabId,
        run: u64,
        baseline: u64,
        pane: u64,
        (earlier, current): (Side, Side),
        ctx: &ComponentContext<Self>,
    ) {
        let Some(page) = self.host_group_mut(tab) else {
            return;
        };
        page.comparison.revision += 1;
        let revision = page.comparison.revision;
        let comparison = Pair {
            earlier,
            current,
            result: None,
        };
        build_diff(tab, run, revision, &comparison, ctx);
        page.comparison.runs = Some(Box::new(Open {
            run,
            pane,
            baseline: Some(baseline),
            comparison: Some(comparison),
            feedback: None,
        }));
    }

    fn save_group_run_diff(
        &mut self,
        tab: TabId,
        run: u64,
        revision: u64,
        ctx: &ComponentContext<Self>,
    ) {
        if self.export.pending.is_some() {
            return;
        }
        let Some(page) = self.host_group_mut(tab) else {
            return;
        };
        let Some(open) = page
            .comparison
            .runs
            .as_ref()
            .filter(|open| open.run == run && page.comparison.revision == revision)
        else {
            return;
        };
        let Some(baseline) = open.baseline else {
            return;
        };
        let Some(pair) = open
            .comparison
            .as_ref()
            .filter(|pair| matches!(pair.result, Some(Ok(_))))
        else {
            return;
        };
        let (earlier, current) = (pair.earlier.clone(), pair.current.clone());
        let pane = open.pane;
        self.begin_text_export(
            text_export::Subject::HostGroupRunDiff {
                baseline,
                run,
                pane,
            },
            move |file| {
                let mut file = io::BufWriter::new(file);
                writeln!(file, "SSH output differences · Pane {pane}\r")?;
                writeln!(file, "{}\r\n{}\r", earlier.heading, current.heading)?;
                diff::write_changes(
                    &mut file,
                    earlier.output.text(),
                    current.output.text(),
                    identical(earlier.exit_code, current.exit_code),
                )?;
                file.flush()
            },
            ctx,
        );
    }
}

fn build_diff(
    tab: TabId,
    run: u64,
    revision: u64,
    pair: &Pair,
    ctx: &ComponentContext<TerminalApp>,
) {
    let before = pair.earlier.output.clone();
    let after = pair.current.output.clone();
    let ready = move |result| message(tab, run, Action::Ready { revision, result });
    ctx.spawn_background_with_rejection(
        move |cancel| {
            if cancel.is_cancelled() {
                ready(Err("Output comparison cancelled."))
            } else {
                ready(Ok(diff::Difference::build(before.text(), after.text())))
            }
        },
        ready(Err("Could not start the output comparison.")),
    );
}

pub(super) fn baseline_picker(
    choices: &[&Submission],
    baseline: Option<u64>,
    choose: impl Fn(Option<u64>) -> Message + 'static,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    let chosen = baseline.and_then(|id| choices.iter().position(|run| run.id == id));
    let ids: Vec<_> = choices.iter().map(|run| run.id).collect();
    ComboBox::new()
        .automation_name("Earlier group run")
        .items_source(
            std::iter::once("Choose an earlier run".to_owned()).chain(
                choices
                    .iter()
                    .map(|run| format!("Run {} · {}", run.id, run.content.label())),
            ),
        )
        .selected_index(Some(chosen.map_or(0, |index| index + 1)))
        .on_selection_changed(ctx.callback(move |index: Option<usize>| {
            let baseline = index
                .and_then(|i| i.checked_sub(1))
                .and_then(|i| ids.get(i))
                .copied();
            choose(baseline)
        }))
        .into()
}

impl Open {
    pub(super) fn view(
        &self,
        tab: TabId,
        page: &HostGroup,
        can_save: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> Option<View> {
        let current = page.viewed_submission(self.run)?;
        let row = current
            .results
            .iter()
            .find(|row| row.target.pane == self.pane)?;
        let run = self.run;
        let revision = page.comparison.revision;
        let choices: Vec<_> = earlier_runs(page, current, self.pane)
            .map(|(earlier, _)| earlier)
            .collect();
        let baseline = self
            .baseline
            .filter(|id| choices.iter().any(|run| run.id == *id));
        let picker = baseline_picker(
            &choices,
            baseline,
            move |baseline| message(tab, run, Action::Choose { revision, baseline }),
            ctx,
        );
        let heading = |value| {
            TextBlock::new()
                .text(value)
                .text_trimming(TextTrimming::CharacterEllipsis)
        };
        let (details, changes, ready) = if let Some(pair) = &self.comparison {
            (
                StackPanel::new().spacing(4.0).children((
                    heading(&pair.earlier.heading),
                    heading(&pair.current.heading),
                )),
                diff::result_view(
                    pair.result.as_ref(),
                    identical(pair.earlier.exit_code, pair.current.exit_code),
                ),
                matches!(pair.result, Some(Ok(_))),
            )
        } else {
            (
                View::empty(),
                self.feedback.as_ref().map_or_else(View::empty, |error| {
                    TextBlock::new()
                        .text(error)
                        .text_wrapping(TextWrapping::Wrap)
                        .into()
                }),
                false,
            )
        };
        let back = if page.comparison.overview.is_some() {
            "Back to run overview"
        } else {
            "Back to results"
        };
        let comparing = self
            .comparison
            .as_ref()
            .is_some_and(|pair| pair.result.is_none());
        Some(
            Grid::new()
                .margin(16.0)
                .row_spacing(8.0)
                .rows([GridLength::Auto, GridLength::STAR])
                .children((
                    StackPanel::new().spacing(8.0).children((
                        TextBlock::new()
                            .font_size(24.0)
                            .text("SSH output across runs"),
                        TextBlock::new()
                            .text(&row.target.name)
                            .text_wrapping(TextWrapping::Wrap),
                        picker,
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .children((
                                Button::new()
                                    .on_click(ctx.callback(move |_| {
                                        message(tab, run, Action::Back { revision })
                                    }))
                                    .content(back),
                                Button::new()
                                    .is_enabled(baseline.is_some() && !comparing)
                                    .on_click(ctx.callback(move |_| {
                                        baseline.map_or(Message::ControlIdle, |baseline| {
                                            message(
                                                tab,
                                                run,
                                                Action::Compare { revision, baseline },
                                            )
                                        })
                                    }))
                                    .content("Compare output"),
                                Button::new()
                                    .is_enabled(can_save && ready)
                                    .on_click(ctx.callback(move |_| {
                                        message(tab, run, Action::Save { revision })
                                    }))
                                    .content("Save full run differences…"),
                            )),
                        details,
                    )),
                    ScrollViewer::new().grid_row(1).content(changes),
                )),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_runs_require_the_same_pane_and_endpoint_and_must_be_earlier() {
        let sample = |pane, host| Delivery {
            target: Target {
                pane,
                name: "fixture".into(),
                names: filter::Names::new("fixture", host, "ops", 22),
                profile: None,
                connected: true,
                state: TargetState::Unverified,
            },
            status: DeliveryStatus::Sent,
            execution: None,
            interrupt: None,
        };
        let mut page = HostGroup::default();
        for result in [
            sample(7, "original"),
            sample(8, "original"),
            sample(7, "other"),
            sample(7, "original"),
            sample(7, "original"),
        ] {
            page.submit(
                SubmissionContent::Command("show".into()),
                vec![result],
                GroupTranscript::default(),
            );
        }
        let baseline: Vec<_> = earlier_runs(&page, &page.submissions[3], 7)
            .map(|(run, _)| run.id)
            .collect();
        assert_eq!(baseline, [1]);
        assert!(
            earlier_runs(&page, &page.submissions[3], 8)
                .next()
                .is_none()
        );
    }

    #[test]
    fn identical_diff_exports_explain_whether_exit_codes_changed() {
        for (code, expected) in [
            (0, "Output and exit codes are identical.\r\n"),
            (17, "Output is identical. The exit codes differ.\r\n"),
        ] {
            let mut bytes = Vec::new();
            diff::write_changes(&mut bytes, "same", "same", identical(0, code)).unwrap();
            assert_eq!(String::from_utf8(bytes).unwrap(), expected);
        }
    }
}
