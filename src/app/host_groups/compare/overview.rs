use super::runs::Side;
use super::*;

pub enum Action {
    Open,
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
        result: Result<Vec<Row>, &'static str>,
    },
    ViewDiff {
        revision: u64,
        pane: u64,
    },
    ShowUnchanged {
        revision: u64,
        show: bool,
    },
    Back {
        revision: u64,
    },
}

pub(super) struct Open {
    run: u64,
    baseline: Option<u64>,
    show_unchanged: bool,
    comparison: ComparisonState,
}

enum ComparisonState {
    Idle,
    Pending,
    Ready(Result<Vec<Row>, &'static str>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Change {
    Unchanged,
    Output,
    ExitCode,
    OutputAndExit,
    EarlierOnly,
    CurrentOnly,
    Unavailable,
}

impl Change {
    fn label(self) -> &'static str {
        match self {
            Self::Unchanged => "Unchanged",
            Self::Output => "Output changed",
            Self::ExitCode => "Exit code changed",
            Self::OutputAndExit => "Output and exit code changed",
            Self::EarlierOnly => "Earlier run only",
            Self::CurrentOnly => "Current run only",
            Self::Unavailable => "Comparison unavailable",
        }
    }
}

struct Candidate {
    pane: u64,
    name: String,
    sides: Contents,
}

enum Contents {
    Both(Result<(Side, Side), String>),
    EarlierOnly,
    CurrentOnly,
}

pub struct Row {
    pane: u64,
    name: String,
    change: Change,
    sides: Option<(Side, Side)>,
    detail: Option<String>,
}

fn identity(delivery: &Delivery) -> (u64, &str) {
    (delivery.target.pane, delivery.target.names.address())
}

fn pair_hosts<'a>(
    earlier: &'a [Delivery],
    current: &'a [Delivery],
) -> Vec<(Option<&'a Delivery>, Option<&'a Delivery>)> {
    let mut before: BTreeMap<_, _> = earlier.iter().map(|row| (identity(row), row)).collect();
    let mut rows: Vec<_> = current
        .iter()
        .map(|row| (before.remove(&identity(row)), Some(row)))
        .collect();
    rows.extend(
        earlier
            .iter()
            .filter(|row| before.contains_key(&identity(row)))
            .map(|row| (Some(row), None)),
    );
    rows
}

fn classify(previous: &str, current: &str, before: i32, after: i32) -> Change {
    match (previous == current, before == after) {
        (true, true) => Change::Unchanged,
        (false, true) => Change::Output,
        (true, false) => Change::ExitCode,
        (false, false) => Change::OutputAndExit,
    }
}

impl From<Candidate> for Row {
    fn from(candidate: Candidate) -> Self {
        let (change, sides, detail) = match candidate.sides {
            Contents::Both(Ok((before, after))) => {
                let change = classify(
                    before.output.text(),
                    after.output.text(),
                    before.exit_code,
                    after.exit_code,
                );
                let detail = format!("Exit {} → {}", before.exit_code, after.exit_code);
                (change, Some((before, after)), Some(detail))
            }
            Contents::Both(Err(reason)) => (Change::Unavailable, None, Some(reason)),
            Contents::EarlierOnly => (Change::EarlierOnly, None, None),
            Contents::CurrentOnly => (Change::CurrentOnly, None, None),
        };
        Self {
            pane: candidate.pane,
            name: candidate.name,
            change,
            sides,
            detail,
        }
    }
}

fn summary(rows: &[Row]) -> String {
    let mut totals = [0; 5];
    for row in rows {
        let index = match row.change {
            Change::Output | Change::ExitCode | Change::OutputAndExit => 0,
            Change::Unchanged => 1,
            Change::EarlierOnly => 2,
            Change::CurrentOnly => 3,
            Change::Unavailable => 4,
        };
        totals[index] += 1;
    }
    let [changed, unchanged, earlier, current, unavailable] = totals;
    format!(
        "{changed} changed · {unchanged} unchanged · {earlier} earlier only · {current} current only · {unavailable} unavailable"
    )
}

fn message(tab: TabId, run: u64, action: Action) -> Message {
    Message::HostGroup(super::super::Action::Compare(
        tab,
        run,
        super::Action::RunOverview(action),
    ))
}

impl TerminalApp {
    pub(super) fn handle_group_run_overview(
        &mut self,
        tab: TabId,
        run: u64,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        match action {
            Action::Open => {
                if self.host_group_command_target() == Some(tab)
                    && let Some(page) = self.host_group_mut(tab)
                    && page.viewed_submission(run).is_some()
                    && page.submissions.iter().any(|item| item.id < run)
                {
                    page.comparison.revision += 1;
                    page.comparison.overview = Some(Box::new(Open {
                        run,
                        baseline: None,
                        show_unchanged: false,
                        comparison: ComparisonState::Idle,
                    }));
                }
            }
            Action::Choose { revision, baseline } => {
                let Some(page) = self.host_group_mut(tab) else {
                    return;
                };
                if baseline.is_some_and(|id| {
                    id >= run || !page.submissions.iter().any(|item| item.id == id)
                }) {
                    return;
                }
                if let Some(open) = page.comparison.overview_mut(run, revision)
                    && open.baseline != baseline
                {
                    open.baseline = baseline;
                    open.comparison = ComparisonState::Idle;
                    page.comparison.revision += 1;
                }
            }
            Action::Compare { revision, baseline } => {
                self.compare_group_runs(tab, run, baseline, revision, ctx)
            }
            Action::Ready { revision, result } => {
                if let Some(Page::HostGroup(page)) = self.tabs.get_mut(tab).map(|tab| &mut tab.page)
                    && let Some(open) = page.comparison.overview_mut(run, revision)
                    && matches!(open.comparison, ComparisonState::Pending)
                {
                    open.comparison = ComparisonState::Ready(result);
                }
            }
            Action::ViewDiff { revision, pane } => {
                let Some(page) = self.host_group_mut(tab) else {
                    return;
                };
                let Some(open) = page.comparison.overview_mut(run, revision) else {
                    return;
                };
                let Some(baseline) = open.baseline else {
                    return;
                };
                let ComparisonState::Ready(Ok(rows)) = &open.comparison else {
                    return;
                };
                let sides = rows
                    .iter()
                    .filter(|row| row.pane == pane)
                    .find_map(|row| row.sides.clone());
                if let Some(sides) = sides {
                    self.open_overview_run_diff(tab, run, baseline, pane, sides, ctx);
                }
            }
            Action::ShowUnchanged { revision, show } => {
                if let Some(page) = self.host_group_mut(tab)
                    && let Some(open) = page.comparison.overview_mut(run, revision)
                {
                    open.show_unchanged = show;
                }
            }
            Action::Back { revision } => {
                if let Some(page) = self.host_group_mut(tab)
                    && page.comparison.overview_mut(run, revision).is_some()
                {
                    page.comparison.overview = None;
                }
            }
        }
    }

    fn compare_group_runs(
        &mut self,
        tab: TabId,
        run: u64,
        baseline: u64,
        revision: u64,
        ctx: &ComponentContext<Self>,
    ) {
        if self.tabs.active_id() != Some(tab) {
            return;
        }
        let Some(Page::HostGroup(page)) = self.tabs.get(tab).map(|tab| &tab.page) else {
            return;
        };
        if !page.comparison.overview().is_some_and(|open| {
            open.run == run
                && page.comparison.revision == revision
                && open.baseline == Some(baseline)
                && !matches!(open.comparison, ComparisonState::Pending)
        }) {
            return;
        }
        let Some(current) = page.viewed_submission(run) else {
            return;
        };
        let Some(earlier) = page
            .submissions
            .iter()
            .find(|item| item.id == baseline && baseline < run)
        else {
            return;
        };
        let candidates: Vec<_> = pair_hosts(&earlier.results, &current.results)
            .into_iter()
            .map(|(before, after)| {
                let row = after
                    .or(before)
                    .expect("every comparison row has a recipient");
                let sides = match (before, after) {
                    (Some(before), Some(after)) => Contents::Both(
                        self.capture_run_side(earlier, before).and_then(|previous| {
                            self.capture_run_side(current, after)
                                .map(|current| (previous, current))
                        }),
                    ),
                    (Some(_), None) => Contents::EarlierOnly,
                    (None, Some(_)) => Contents::CurrentOnly,
                    (None, None) => unreachable!(),
                };
                Candidate {
                    pane: row.target.pane,
                    name: row.target.name.clone(),
                    sides,
                }
            })
            .collect();
        let Some(page) = self.host_group_mut(tab) else {
            return;
        };
        let Some(open) = page.comparison.overview_mut(run, revision) else {
            return;
        };
        open.comparison = ComparisonState::Pending;
        page.comparison.revision += 1;
        let revision = page.comparison.revision;
        let ready = move |result| message(tab, run, Action::Ready { revision, result });
        ctx.spawn_background_with_rejection(
            move |cancel| {
                if cancel.is_cancelled() {
                    ready(Err("Run comparison cancelled."))
                } else {
                    ready(Ok(candidates.into_iter().map(Row::from).collect()))
                }
            },
            ready(Err("Could not start the group run comparison.")),
        );
    }
}

impl State {
    fn overview(&self) -> Option<&Open> {
        self.overview.as_deref().filter(|_| self.runs.is_none())
    }

    fn overview_mut(&mut self, run: u64, revision: u64) -> Option<&mut Open> {
        if self.revision != revision || self.runs.is_some() {
            return None;
        }
        self.overview.as_deref_mut().filter(|open| open.run == run)
    }
}

impl Open {
    pub(super) fn view(
        &self,
        tab: TabId,
        page: &HostGroup,
        ctx: &ViewContext<TerminalApp>,
    ) -> Option<View> {
        let current = page.viewed_submission(self.run)?;
        let run = self.run;
        let revision = page.comparison.revision;
        let choices: Vec<_> = page
            .submissions
            .iter()
            .rev()
            .filter(|item| item.id < run)
            .collect();
        let baseline = self
            .baseline
            .filter(|id| choices.iter().any(|run| run.id == *id));
        let picker = runs::baseline_picker(
            &choices,
            baseline,
            move |baseline| message(tab, run, Action::Choose { revision, baseline }),
            ctx,
        );
        let send = move |action| message(tab, run, action);
        let (status, rows) = match &self.comparison {
            ComparisonState::Idle => (View::empty(), View::empty()),
            ComparisonState::Pending => (
                TextBlock::new().text("Comparing group runs…").into(),
                View::empty(),
            ),
            ComparisonState::Ready(Err(error)) => {
                (TextBlock::new().text(*error).into(), View::empty())
            }
            ComparisonState::Ready(Ok(rows)) => {
                let status =
                    StackPanel::new().spacing(4.0).children((
                        TextBlock::new()
                            .text(summary(rows))
                            .text_wrapping(TextWrapping::Wrap),
                        CheckBox::new()
                            .automation_name("Show unchanged hosts")
                            .is_checked(self.show_unchanged)
                            .on_is_checked_changed(ctx.callback(move |show| {
                                send(Action::ShowUnchanged { revision, show })
                            }))
                            .content("Show unchanged hosts"),
                    ));
                let shown = rows
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| self.show_unchanged || row.change != Change::Unchanged);
                let empty = shown.clone().next().is_none();
                let entries =
                    shown.map(|(index, row)| KeyedView::new(index, row.view(revision, send, ctx)));
                let entries = if empty {
                    TextBlock::new()
                        .text("No changed or unavailable hosts")
                        .into()
                } else {
                    StackPanel::new().spacing(10.0).keyed_children(entries)
                };
                (status, entries)
            }
        };
        Some(
            Grid::new()
                .margin(16.0)
                .row_spacing(8.0)
                .rows([GridLength::Auto, GridLength::STAR])
                .children((
                    StackPanel::new().spacing(8.0).children((
                        TextBlock::new()
                            .font_size(24.0)
                            .text("SSH group run comparison"),
                        TextBlock::new()
                            .text(format!("Run {run} · {}", current.content.label()))
                            .text_trimming(TextTrimming::CharacterEllipsis),
                        picker,
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .children((
                                Button::new()
                                    .on_click(
                                        ctx.callback(move |_| send(Action::Back { revision })),
                                    )
                                    .content("Back to results"),
                                Button::new()
                                    .is_enabled(
                                        baseline.is_some()
                                            && !matches!(self.comparison, ComparisonState::Pending),
                                    )
                                    .on_click(ctx.callback(move |_| {
                                        baseline.map_or(Message::ControlIdle, |baseline| {
                                            send(Action::Compare { revision, baseline })
                                        })
                                    }))
                                    .content("Compare runs"),
                            )),
                        status,
                    )),
                    ScrollViewer::new().grid_row(1).content(rows),
                )),
        )
    }
}

impl Row {
    fn view(
        &self,
        revision: u64,
        action: impl Fn(Action) -> Message + Copy + 'static,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let pane = self.pane;
        StackPanel::new().spacing(4.0).children((
            TextBlock::new()
                .text(format!("{} · {}", self.name, self.change.label()))
                .text_wrapping(TextWrapping::Wrap),
            self.detail.as_ref().map_or_else(View::empty, |detail| {
                TextBlock::new()
                    .text(detail)
                    .text_wrapping(TextWrapping::Wrap)
                    .into()
            }),
            if self.sides.is_some() {
                Button::new()
                    .automation_name(format!("View run differences for pane {pane}"))
                    .on_click(ctx.callback(move |_| action(Action::ViewDiff { revision, pane })))
                    .content("View differences")
            } else {
                View::empty()
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_recipients_by_pane_and_address_in_current_then_earlier_order() {
        let delivery = |pane, address| Delivery {
            target: Target {
                pane,
                name: format!("Pane {pane}"),
                names: filter::Names::new("Fixture", address, "ops", 22),
                profile: None,
                connected: true,
                state: TargetState::Unverified,
            },
            status: DeliveryStatus::Sent,
            execution: None,
            interrupt: None,
        };
        let before = [
            delivery(1, "old"),
            delivery(2, "shared"),
            delivery(3, "left"),
        ];
        let after = [
            delivery(2, "shared"),
            delivery(1, "new"),
            delivery(4, "joined"),
        ];
        let panes: Vec<_> = pair_hosts(&before, &after)
            .into_iter()
            .map(|(before, after)| {
                (
                    before.map(|row| row.target.pane),
                    after.map(|row| row.target.pane),
                )
            })
            .collect();
        assert_eq!(
            panes,
            [
                (Some(2), Some(2)),
                (None, Some(1)),
                (None, Some(4)),
                (Some(1), None),
                (Some(3), None)
            ]
        );
    }

    #[test]
    fn classifies_changes_in_output_and_exit_status_independently() {
        assert_eq!(classify("same", "same", 0, 0), Change::Unchanged);
        assert_eq!(classify("old", "new", 0, 0), Change::Output);
        assert_eq!(classify("same", "same", 0, 17), Change::ExitCode);
        assert_eq!(classify("old", "new", 0, 17), Change::OutputAndExit);
    }
}
