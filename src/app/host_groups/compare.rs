use super::*;
use std::{cmp::Reverse, collections::HashMap};
use wezterm_term::CompletedCommand;

mod diff;

const PREVIEW_BYTES: usize = 1024;
const PREVIEW_LINES: usize = 6;

pub enum Action {
    Open,
    Refresh,
    Back,
    PrepareFollowUp(FollowUp),
    OpenDiff {
        revision: u64,
        pane: u64,
    },
    CloseDiff,
    SetReference {
        revision: u64,
        pane: Option<u64>,
    },
    DiffReady {
        revision: u64,
        result: Result<diff::Difference, &'static str>,
    },
    Ready {
        revision: u64,
        preferred: Option<u64>,
        comparison: Result<Comparison, &'static str>,
    },
}

#[derive(Default)]
pub(super) struct State {
    revision: u64,
    open: Option<Box<Open>>,
}

struct Open {
    submission: u64,
    comparison: Option<Result<Comparison, &'static str>>,
    feedback: Option<&'static str>,
    diff: Option<diff::Panel>,
    updating_reference: bool,
}

impl State {
    pub(super) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub(super) fn close(&mut self) {
        self.open = None;
    }
}

#[derive(Clone)]
struct Member {
    pane: u64,
    name: String,
    address: String,
    link: CommandLink,
}

struct Sample {
    member: Member,
    exit_code: i32,
    output: report::Output,
}

#[derive(Clone)]
struct Unavailable {
    pane: u64,
    label: String,
}

#[derive(Clone)]
struct Group {
    members: Vec<Member>,
    exit_code: i32,
    size: usize,
    preview: String,
    note: Option<String>,
    output: report::Output,
}

impl Group {
    fn visible_members(&self, visible: &BTreeMap<u64, &str>) -> BTreeSet<u64> {
        self.members
            .iter()
            .filter(|member| visible.get(&member.pane) == Some(&member.address.as_str()))
            .map(|member| member.pane)
            .collect()
    }
}

#[derive(Clone)]
pub struct FollowUp {
    revision: u64,
    group: u64,
    panes: BTreeSet<u64>,
}

#[derive(Clone)]
pub struct Comparison {
    compared: usize,
    largest: Option<u64>,
    reference: Option<u64>,
    preferred: Option<u64>,
    groups: Vec<Group>,
    unavailable: Vec<Unavailable>,
}

fn sample(
    delivery: &Delivery,
    retained: impl FnOnce(CommandLink) -> Option<Arc<CompletedCommand>>,
) -> Result<Sample, String> {
    let Some(execution) = delivery.execution() else {
        return Err(delivery.description());
    };
    let host = &delivery.target.name;
    let GroupExecution::Finished(result) = execution else {
        return Err(format!("{host} · {}", execution_caption(&execution)));
    };
    let Some(exit_code) = result.exit_code else {
        return Err(format!("{host} · Exit status unavailable"));
    };
    if let GroupOutput::Unavailable(reason) = &result.output {
        return Err(format!("{host} · Output unavailable · {reason}"));
    }
    let link = result.link;
    let Some(output) = report::Output::capture(result, retained) else {
        return Err(format!("{host} · Output unavailable"));
    };
    if output.complete().is_none() {
        return Err(format!("{host} · Full output unavailable · Preview only"));
    }
    Ok(Sample {
        member: Member {
            pane: delivery.target.pane,
            name: host.clone(),
            address: delivery.target.names.address().into(),
            link,
        },
        exit_code,
        output,
    })
}

fn preview(text: &str) -> String {
    if text.is_empty() {
        return "No output captured".into();
    }
    let bytes = text.floor_char_boundary(text.len().min(PREVIEW_BYTES));
    let end = text[..bytes]
        .match_indices('\n')
        .nth(PREVIEW_LINES - 1)
        .map_or(bytes, |(index, _)| index + 1);
    let mut preview = text[..end].trim_end_matches(['\r', '\n']).to_owned();
    if end < text.len() {
        preview.push_str("\n…");
    }
    preview
}

fn first_difference(baseline: &str, text: &str) -> Option<usize> {
    if baseline == text {
        return None;
    }
    let shared = baseline
        .split_inclusive('\n')
        .zip(text.split_inclusive('\n'))
        .take_while(|(left, right)| left == right)
        .count();
    Some(shared + 1)
}

impl Comparison {
    fn build(samples: Vec<Sample>, unavailable: Vec<Unavailable>, preferred: Option<u64>) -> Self {
        let compared = samples.len();
        let mut grouped: Vec<Vec<usize>> = Vec::new();
        let mut exact = HashMap::new();
        for (index, sample) in samples.iter().enumerate() {
            let key = (sample.exit_code, sample.output.text());
            let group = exact.entry(key).or_insert_with(|| {
                grouped.push(Vec::new());
                grouped.len() - 1
            });
            grouped[*group].push(index);
        }
        let baseline = grouped
            .iter()
            .enumerate()
            .max_by_key(|(index, items)| (items.len(), Reverse(*index)))
            .map(|(index, _)| index);
        let largest = baseline.map(|index| samples[grouped[index][0]].member.pane);
        let mut groups = Vec::with_capacity(grouped.len());
        for members in &grouped {
            let sample = &samples[members[0]];
            let text = sample.output.text();
            groups.push(Group {
                members: members
                    .iter()
                    .map(|&member| samples[member].member.clone())
                    .collect(),
                exit_code: sample.exit_code,
                size: text.len(),
                preview: preview(text),
                note: None,
                output: sample.output.clone(),
            });
        }
        groups.sort_by_key(|group| group.members.len());
        let mut comparison = Self {
            compared,
            largest,
            reference: None,
            preferred: None,
            groups,
            unavailable,
        };
        comparison.set_reference(preferred);
        comparison
    }

    fn set_reference(&mut self, preferred: Option<u64>) {
        let selected = preferred.and_then(|pane| {
            let group = self
                .groups
                .iter()
                .find(|group| group.members.iter().any(|member| member.pane == pane))?;
            Some((pane, group.members[0].pane))
        });
        self.preferred = selected.map(|(pane, _)| pane);
        self.reference = selected.map(|(_, group)| group).or(self.largest);
        let Some(reference) = self.reference.and_then(|pane| self.group(pane)) else {
            return;
        };
        let pane = reference.members[0].pane;
        let output = reference.output.clone();
        let manual = self.preferred.is_some();
        let reference_label = if manual {
            "selected reference"
        } else {
            "largest group"
        };
        let show_notes = self.groups.len() > 1 || manual;
        for group in &mut self.groups {
            group.note = show_notes.then(|| {
                if group.members[0].pane == pane {
                    return if manual {
                        "Selected reference group".into()
                    } else {
                        "Largest result group".into()
                    };
                }
                match first_difference(output.text(), group.output.text()) {
                    Some(line) => format!("First different line from {reference_label} · {line}"),
                    None => format!("Same output as {reference_label}"),
                }
            });
        }
    }

    fn group(&self, pane: u64) -> Option<&Group> {
        self.groups
            .iter()
            .find(|group| group.members[0].pane == pane)
    }

    fn caption(&self) -> String {
        let count = self.groups.len();
        let result = if count == 1 { "result" } else { "results" };
        let host = if self.compared == 1 { "host" } else { "hosts" };
        format!(
            "{count} distinct {result} · {} {host} compared · {} pending or unavailable",
            self.compared,
            self.unavailable.len()
        )
    }
}

impl HostGroup {
    fn visible_comparison_addresses<'a>(&self, roster: &'a [Target]) -> BTreeMap<u64, &'a str> {
        self.shown(roster)
            .map(|target| (target.pane, target.names.address()))
            .collect()
    }

    fn prepare_comparison_follow_up(
        &mut self,
        submission: u64,
        offer: FollowUp,
        roster: &[Target],
    ) {
        if self.comparison.revision != offer.revision || offer.panes.is_empty() {
            return;
        }
        let Some(command) = self
            .viewed_submission(submission)
            .and_then(|item| item.content.command())
        else {
            return;
        };
        let Some(open) = self
            .comparison
            .open
            .as_ref()
            .filter(|open| open.submission == submission)
        else {
            return;
        };
        let Some(Ok(comparison)) = open.comparison.as_ref() else {
            return;
        };
        let Some(group) = comparison
            .groups
            .iter()
            .find(|group| group.members[0].pane == offer.group)
        else {
            return;
        };
        if group.visible_members(&self.visible_comparison_addresses(roster)) != offer.panes {
            if let Some(open) = &mut self.comparison.open {
                open.feedback = Some("The available hosts changed. Review the result group again.");
            }
            return;
        }
        self.load_scoped_draft(command.to_owned(), offer.panes);
        self.comparison.open = None;
        self.focus_loaded_command = true;
    }
}

impl TerminalApp {
    pub(super) fn handle_host_group_comparison(
        &mut self,
        id: TabId,
        submission_id: u64,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        match action {
            Action::Open if self.host_group_command_target() == Some(id) => {
                self.begin_host_group_comparison(id, submission_id, ctx);
            }
            Action::Refresh => {
                if self.host_group_mut(id).is_some_and(|page| {
                    page.comparison.open.as_ref().is_some_and(|open| {
                        open.submission == submission_id
                            && open.comparison.is_some()
                            && !open.updating_reference
                    })
                }) {
                    self.begin_host_group_comparison(id, submission_id, ctx);
                }
            }
            Action::Back => {
                if let Some(page) = self.host_group_mut(id)
                    && page
                        .comparison
                        .open
                        .as_ref()
                        .is_some_and(|open| open.submission == submission_id)
                {
                    page.comparison.open = None;
                }
            }
            Action::PrepareFollowUp(offer) => {
                let roster = self.host_group_targets();
                if let Some(page) = self.host_group_mut(id) {
                    page.prepare_comparison_follow_up(submission_id, offer, &roster);
                }
            }
            Action::OpenDiff { revision, pane } => {
                self.open_group_diff(id, submission_id, revision, pane, ctx);
            }
            Action::SetReference { revision, pane } => {
                self.set_group_comparison_reference(id, submission_id, revision, pane, ctx);
            }
            Action::CloseDiff => {
                if let Some(page) = self.host_group_mut(id)
                    && let Some(open) = page.comparison.open.as_mut()
                    && open.submission == submission_id
                {
                    open.diff = None;
                }
            }
            Action::DiffReady { revision, result } => {
                if let Some(Page::HostGroup(page)) = self.tabs.get_mut(id).map(|tab| &mut tab.page)
                    && page.comparison.revision == revision
                    && let Some(open) = &mut page.comparison.open
                    && open.submission == submission_id
                    && let Some(diff) = &mut open.diff
                {
                    diff.result = Some(result);
                }
            }
            Action::Ready {
                revision,
                preferred,
                comparison,
            } => {
                if let Some(Page::HostGroup(page)) = self.tabs.get_mut(id).map(|tab| &mut tab.page)
                    && page.comparison.revision == revision
                    && let Some(open) = &mut page.comparison.open
                    && open.submission == submission_id
                {
                    let changing = std::mem::take(&mut open.updating_reference);
                    open.feedback = match &comparison {
                        Err(message) if changing => Some(*message),
                        Ok(comparison) if preferred.is_some() && comparison.preferred.is_none() => {
                            Some(if comparison.reference.is_some() {
                                "The selected host no longer has a comparable result. Using the largest group."
                            } else {
                                "The selected host no longer has a comparable result."
                            })
                        }
                        _ => None,
                    };
                    if !changing || comparison.is_ok() {
                        open.comparison = Some(comparison);
                    }
                }
            }
            _ => {}
        }
    }

    fn begin_host_group_comparison(
        &mut self,
        id: TabId,
        submission_id: u64,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(Page::HostGroup(page)) = self.tabs.get(id).map(|tab| &tab.page) else {
            return;
        };
        let Some(submission) = page.viewed_submission(submission_id) else {
            return;
        };
        let preferred = page
            .comparison
            .open
            .as_ref()
            .filter(|open| open.submission == submission_id)
            .and_then(|open| open.comparison.as_ref()?.as_ref().ok()?.preferred);
        let mut samples = Vec::new();
        let mut unavailable = Vec::new();
        for delivery in &submission.results {
            let pane = delivery.target.pane;
            match sample(delivery, |link| {
                self.history_frame(pane)?.command_for_link(link).cloned()
            }) {
                Ok(sample) => samples.push(sample),
                Err(label) => unavailable.push(Unavailable { pane, label }),
            }
        }
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        let state = &mut page.comparison;
        state.revision += 1;
        state.open = Some(Box::new(Open {
            submission: submission_id,
            comparison: None,
            feedback: None,
            diff: None,
            updating_reference: false,
        }));
        let revision = state.revision;
        Self::queue_comparison(id, submission_id, revision, preferred, ctx, move || {
            Comparison::build(samples, unavailable, preferred)
        });
    }

    fn set_group_comparison_reference(
        &mut self,
        id: TabId,
        submission: u64,
        revision: u64,
        preferred: Option<u64>,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        let state = &mut page.comparison;
        if state.revision != revision {
            return;
        }
        let Some(open) = state.open.as_mut().filter(|open| {
            open.submission == submission && !open.updating_reference && open.diff.is_none()
        }) else {
            return;
        };
        let Some(Ok(comparison)) = &open.comparison else {
            return;
        };
        if preferred == comparison.preferred
            || preferred.is_some_and(|pane| comparison.group(pane).is_none())
        {
            return;
        }
        let mut comparison = comparison.clone();
        open.updating_reference = true;
        open.feedback = None;
        state.revision += 1;
        Self::queue_comparison(id, submission, state.revision, preferred, ctx, move || {
            comparison.set_reference(preferred);
            comparison
        });
    }

    fn queue_comparison(
        id: TabId,
        submission_id: u64,
        revision: u64,
        preferred: Option<u64>,
        ctx: &ComponentContext<Self>,
        build: impl FnOnce() -> Comparison + Send + 'static,
    ) {
        let ready = move |comparison| {
            Message::HostGroup(super::Action::Compare(
                id,
                submission_id,
                Action::Ready {
                    revision,
                    preferred,
                    comparison,
                },
            ))
        };
        ctx.spawn_background_with_rejection(
            move |cancel| {
                let comparison = if cancel.is_cancelled() {
                    Err("Comparison cancelled.")
                } else {
                    Ok(build())
                };
                ready(comparison)
            },
            ready(Err("Could not start the comparison.")),
        );
    }

    pub(super) fn host_group_comparison(
        &self,
        id: TabId,
        page: &HostGroup,
        ctx: &ViewContext<Self>,
    ) -> Option<View> {
        let open = page.comparison.open.as_ref()?;
        let submission_id = open.submission;
        let submission = page.viewed_submission(submission_id)?;
        let action =
            move |action| Message::HostGroup(super::Action::Compare(id, submission_id, action));
        if let Some(diff) = &open.diff {
            return Some(diff.view(submission.content.label(), action, ctx));
        }
        let mut rows = Vec::new();
        let mut reset_reference = View::empty();
        let status = match &open.comparison {
            Some(Ok(comparison)) => {
                let targets = self.host_group_targets();
                let visible = page.visible_comparison_addresses(&targets);
                let revision = page.comparison.revision;
                if comparison.preferred.is_some() {
                    reset_reference = Button::new()
                        .is_enabled(!open.updating_reference)
                        .on_click(ctx.callback(move |_| {
                            action(Action::SetReference {
                                revision,
                                pane: None,
                            })
                        }))
                        .content("Use largest group");
                }
                for group in &comparison.groups {
                    let pane = group.members[0].pane;
                    let difference = if comparison
                        .reference
                        .is_some_and(|reference| reference != pane)
                    {
                        Button::new()
                            .automation_name(format!("View differences for pane {pane}"))
                            .is_enabled(!open.updating_reference)
                            .on_click(
                                ctx.callback(move |_| action(Action::OpenDiff { revision, pane })),
                            )
                            .content("View differences")
                    } else {
                        View::empty()
                    };
                    let choose_reference =
                        if comparison.preferred.is_none() || comparison.reference != Some(pane) {
                            Button::new()
                                .automation_name(format!("Use pane {pane} as comparison reference"))
                                .is_enabled(!open.updating_reference)
                                .on_click(ctx.callback(move |_| {
                                    action(Action::SetReference {
                                        revision,
                                        pane: Some(pane),
                                    })
                                }))
                                .content("Use as reference")
                        } else {
                            View::empty()
                        };
                    let follow_up = submission.content.command().map(|_| FollowUp {
                        revision,
                        group: pane,
                        panes: group.visible_members(&visible),
                    });
                    rows.push(KeyedView::new(
                        format!("group-{pane}"),
                        self.comparison_group(
                            id,
                            submission_id,
                            group,
                            follow_up,
                            StackPanel::new()
                                .orientation(Orientation::Horizontal)
                                .spacing(8.0)
                                .children((difference, choose_reference)),
                            ctx,
                        ),
                    ));
                }
                if !comparison.unavailable.is_empty() {
                    rows.push(KeyedView::new(
                        "unavailable",
                        TextBlock::new()
                            .font_size(18.0)
                            .text("Pending or unavailable"),
                    ));
                    for unavailable in &comparison.unavailable {
                        rows.push(KeyedView::new(
                            format!("unavailable-{}", unavailable.pane),
                            TextBlock::new()
                                .text(&unavailable.label)
                                .text_wrapping(TextWrapping::Wrap),
                        ));
                    }
                }
                if open.updating_reference {
                    "Updating comparison reference…".into()
                } else {
                    comparison.caption()
                }
            }
            Some(Err(message)) => (*message).into(),
            None => "Comparing command results…".into(),
        };
        Some(
            Grid::new()
                .margin(16.0)
                .row_spacing(8.0)
                .rows([GridLength::Auto, GridLength::STAR])
                .children((
                    StackPanel::new().spacing(6.0).children((
                        TextBlock::new()
                            .font_size(24.0)
                            .text("SSH group result comparison"),
                        TextBlock::new()
                            .text(format!("Submitted · {}", submission.content.label()))
                            .text_trimming(TextTrimming::CharacterEllipsis),
                        TextBlock::new()
                            .text(status)
                            .text_wrapping(TextWrapping::Wrap),
                        open.feedback.map_or_else(View::empty, |feedback| {
                            TextBlock::new()
                                .text(feedback)
                                .text_wrapping(TextWrapping::Wrap)
                                .into()
                        }),
                        StackPanel::new()
                            .spacing(8.0)
                            .orientation(Orientation::Horizontal)
                            .children((
                                Button::new()
                                    .on_click(ctx.callback(move |_| action(Action::Back)))
                                    .content("Back to results"),
                                Button::new()
                                    .is_enabled(
                                        open.comparison.is_some() && !open.updating_reference,
                                    )
                                    .on_click(ctx.callback(move |_| action(Action::Refresh)))
                                    .content("Refresh comparison"),
                                reset_reference,
                            )),
                    )),
                    ScrollViewer::new()
                        .grid_row(1)
                        .content(StackPanel::new().spacing(12.0).keyed_children(rows)),
                )),
        )
    }

    fn comparison_group(
        &self,
        id: TabId,
        submission: u64,
        group: &Group,
        follow_up: Option<FollowUp>,
        reference_actions: View,
        ctx: &ViewContext<Self>,
    ) -> View {
        let count = group.members.len();
        let host = if count == 1 { "host" } else { "hosts" };
        let first = group.members[0].pane;
        let history = group.members.iter().find(|member| {
            self.history_frame(member.pane)
                .is_some_and(|frame| frame.has_command(member.link))
        });
        let details = history.map(|member| (member.pane, member.link));
        let follow_up = follow_up.map_or_else(View::empty, |offer| {
            let visible = offer.panes.len();
            let caption = if visible == count {
                format!("Prepare follow-up ({count} {host})")
            } else {
                format!("Prepare follow-up ({visible} of {count} {host} in roster)")
            };
            Button::new()
                .automation_name(format!(
                    "Prepare follow-up for result group starting at pane {first} · {caption}"
                ))
                .is_enabled(visible > 0)
                .on_click(ctx.callback(move |_| {
                    Message::HostGroup(super::Action::Compare(
                        id,
                        submission,
                        Action::PrepareFollowUp(offer.clone()),
                    ))
                }))
                .content(caption)
        });
        let members = group.members.iter().map(|member| {
            let pane = member.pane;
            KeyedView::new(
                pane.to_string(),
                Button::new()
                    .automation_name(format!("Focus compared pane {pane}"))
                    .is_enabled(self.tab_for_pane(pane).is_some())
                    .on_click(
                        ctx.callback(move |_| Message::HostGroup(super::Action::Visit(id, pane))),
                    )
                    .content(member.name.as_str()),
            )
        });
        StackPanel::new().spacing(4.0).children((
            TextBlock::new().font_size(18.0).text(format!(
                "Exit {} · {count} {host} · {} bytes",
                group.exit_code, group.size
            )),
            group.note.as_ref().map_or_else(View::empty, |note| {
                TextBlock::new()
                    .text(note)
                    .text_wrapping(TextWrapping::Wrap)
                    .into()
            }),
            TextBlock::new()
                .text(&group.preview)
                .text_wrapping(TextWrapping::Wrap),
            details.map_or_else(View::empty, |(pane, link)| {
                Button::new()
                    .automation_name(format!("View comparison output for pane {first}"))
                    .on_click(ctx.callback(move |_| {
                        Message::HostGroup(super::Action::ViewOutput(id, pane, link))
                    }))
                    .content("View full output")
            }),
            reference_actions,
            follow_up,
            StackPanel::new().spacing(3.0).keyed_children(members),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use w11term::events::GroupCommandResult;

    fn completed(pane: u64, exit_code: Option<i32>, output: GroupOutput) -> Delivery {
        let result = GroupCommandResult {
            link: CommandLink {
                generation: 3,
                id: 5,
            },
            exit_code,
            duration: Duration::ZERO,
            output,
        };
        Delivery {
            target: Target {
                pane,
                name: format!("Host {pane}"),
                names: filter::Names::new(&format!("Host {pane}"), "example.test", "ops", 22),
                profile: None,
                connected: true,
                state: TargetState::InUse,
            },
            status: DeliveryStatus::Sent,
            execution: Some(watch::channel(GroupExecution::Finished(Arc::new(result))).1),
            interrupt: None,
        }
    }

    fn capture(text: &str, truncated: bool) -> GroupOutput {
        GroupOutput::Captured {
            text: text.into(),
            truncated,
        }
    }

    #[test]
    fn follow_up_only_loads_the_reviewed_result_members_still_in_the_roster() {
        let deliveries = vec![
            completed(1, Some(0), capture("different\n", false)),
            completed(2, Some(0), capture("matching\n", false)),
            completed(3, Some(0), capture("matching\n", false)),
        ];
        let mut roster: Vec<_> = deliveries.iter().map(|item| item.target.clone()).collect();
        let samples = deliveries
            .iter()
            .map(|item| sample(item, |_| None).unwrap())
            .collect();
        let mut page = HostGroup {
            draft: "working draft".into(),
            selected: BTreeSet::from([1, 3]),
            roster: Some(Box::new(MemberRoster {
                panes: BTreeSet::from([1, 2]),
                ..Default::default()
            })),
            ..Default::default()
        };
        page.filter.set_query("Host".into());
        page.submit(
            SubmissionContent::Command("original  command".into()),
            deliveries,
            GroupTranscript::default(),
        );
        page.comparison = State {
            revision: 2,
            open: Some(Box::new(Open {
                submission: 1,
                comparison: Some(Ok(Comparison::build(samples, Vec::new(), None))),
                feedback: None,
                diff: None,
                updating_reference: false,
            })),
        };
        let offer = FollowUp {
            revision: 2,
            group: 2,
            panes: BTreeSet::from([2]),
        };
        page.prepare_comparison_follow_up(
            1,
            FollowUp {
                revision: 1,
                ..offer.clone()
            },
            &roster,
        );
        assert!(page.comparison.is_open());
        assert_eq!(page.draft, "working draft");

        let old_names = roster[1].names.clone();
        roster[1].names = filter::Names::new("Host 2", "example.test", "someone", 22);
        page.prepare_comparison_follow_up(1, offer.clone(), &roster);
        assert!(page.comparison.open.as_ref().unwrap().feedback.is_some());
        assert_eq!(page.selected, BTreeSet::from([1, 3]));
        assert!(page.previous_draft.is_none());

        roster[1].names = old_names;
        page.prepare_comparison_follow_up(1, offer, &roster);
        assert!(!page.comparison.is_open());
        assert_eq!(page.selected, BTreeSet::from([2]));
        assert_eq!(page.draft, "original  command");
        assert_eq!(page.submissions.len(), 1);
        page.filter.clear();
        assert!(page.restore_draft());
        assert_eq!(page.filter.query(), "Host");
        assert_eq!(page.selected, BTreeSet::from([1, 3]));
        assert_eq!(page.draft, "working draft");
    }

    #[test]
    fn groups_by_full_text_and_exit_status_and_puts_outliers_first() {
        let inputs = [
            completed(1, Some(0), capture("line 1\r\nline 2\r\n", false)),
            completed(2, Some(0), capture("line 1\r\nline 2\r\n", false)),
            completed(3, Some(0), capture("line 1\r\nline 2 \r\n", false)),
            completed(4, Some(7), capture("line 1\r\nline 2\r\n", false)),
        ];
        let samples = inputs
            .iter()
            .map(|item| sample(item, |_| None).unwrap())
            .collect();
        let results = Comparison::build(samples, Vec::new(), None);
        assert_eq!(
            results.caption(),
            "3 distinct results · 4 hosts compared · 0 pending or unavailable"
        );
        assert_eq!(results.groups[0].members[0].pane, 3);
        assert_eq!(
            results.groups[0].note.as_deref(),
            Some("First different line from largest group · 2")
        );
        assert_eq!(results.groups[1].exit_code, 7);
        assert_eq!(
            results.groups[1].note.as_deref(),
            Some("Same output as largest group")
        );
        let largest = &results.groups[2];
        assert_eq!(
            largest
                .members
                .iter()
                .map(|member| member.pane)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(largest.note.as_deref(), Some("Largest result group"));
    }

    #[test]
    fn requires_complete_text_and_a_verified_status_before_grouping() {
        let a = completed(1, Some(0), capture("same preview\n", true));
        let b = completed(2, Some(0), capture("same preview\n", true));
        let c = completed(3, None, capture("same preview\n", false));
        assert_eq!(
            sample(&a, |_| None).err().as_deref(),
            Some("Host 1 · Full output unavailable · Preview only")
        );
        assert_eq!(
            sample(&c, |_| None).err().as_deref(),
            Some("Host 3 · Exit status unavailable")
        );
        let unavailable = completed(4, Some(0), GroupOutput::Unavailable("capture expired"));
        assert_eq!(
            sample(&unavailable, |_| None).err().as_deref(),
            Some("Host 4 · Output unavailable · capture expired")
        );
        let retained = |link: CommandLink, output: &str| {
            assert_eq!(
                link,
                CommandLink {
                    generation: 3,
                    id: 5
                }
            );
            Some(Arc::new(CompletedCommand {
                id: link.id,
                command: None,
                exit_code: Some(0),
                output: Ok(output.into()),
            }))
        };
        let results = Comparison::build(
            vec![
                sample(&a, |link| retained(link, "same preview\nfirst tail")).unwrap(),
                sample(&b, |link| retained(link, "same preview\nsecond tail")).unwrap(),
            ],
            Vec::new(),
            None,
        );
        assert_eq!(results.groups.len(), 2);
        assert_eq!(
            results.groups[1].note.as_deref(),
            Some("First different line from largest group · 2")
        );
    }

    #[test]
    fn chosen_host_remains_the_reference_when_its_group_gains_an_earlier_member() {
        let build = |entries: &[(u64, &str)], preferred| {
            let samples = entries
                .iter()
                .map(|&(pane, text)| {
                    sample(&completed(pane, Some(0), capture(text, false)), |_| None).unwrap()
                })
                .collect();
            Comparison::build(samples, Vec::new(), preferred)
        };
        let mut results = build(&[(1, "common"), (2, "common"), (3, "outlier")], None);
        assert_eq!(results.reference, Some(1));
        results.set_reference(Some(3));
        assert_eq!(results.reference, Some(3));
        assert_eq!(
            results.group(3).unwrap().note.as_deref(),
            Some("Selected reference group")
        );
        assert_eq!(
            results.group(1).unwrap().note.as_deref(),
            Some("First different line from selected reference · 1")
        );
        let mut refreshed = build(
            &[(1, "common"), (2, "common"), (4, "outlier"), (3, "outlier")],
            results.preferred,
        );
        assert_eq!(refreshed.preferred, Some(3));
        assert_eq!(refreshed.reference, Some(4));
        refreshed.set_reference(None);
        assert_eq!(refreshed.reference, Some(1));
        assert!(refreshed.preferred.is_none());

        let expired = build(&[(1, "common"), (2, "different")], Some(3));
        assert_eq!(expired.reference, Some(1));
        assert!(expired.preferred.is_none());
        let unavailable = build(&[], Some(3));
        assert!(unavailable.reference.is_none());
        assert!(unavailable.preferred.is_none());
    }

    #[test]
    fn comparison_preview_is_bounded_and_respects_utf8() {
        let unicode = "夜".repeat(PREVIEW_BYTES);
        let excerpt = preview(&unicode);
        assert!(excerpt.ends_with('\u{2026}'));
        assert!(excerpt.len() <= PREVIEW_BYTES + 4);
        assert_eq!(
            preview(&"row\n".repeat(9)).lines().count(),
            PREVIEW_LINES + 1
        );
        assert_eq!(first_difference("a\n", "a\nb"), Some(2));
        assert_eq!(first_difference("", "b"), Some(1));
    }
}
