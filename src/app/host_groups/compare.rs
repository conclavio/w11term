use super::*;
use std::{cmp::Reverse, collections::HashMap};
use wezterm_term::CompletedCommand;

const PREVIEW_BYTES: usize = 1024;
const PREVIEW_LINES: usize = 6;

pub enum Action {
    Open,
    Refresh,
    Back,
    Ready {
        revision: u64,
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
}

impl State {
    pub(super) fn is_open(&self) -> bool {
        self.open.is_some()
    }
}

#[derive(Clone)]
struct Member {
    pane: u64,
    name: String,
    link: CommandLink,
}

struct Sample {
    member: Member,
    exit_code: i32,
    output: report::Output,
}

struct Unavailable {
    pane: u64,
    label: String,
}

struct Group {
    members: Vec<Member>,
    exit_code: i32,
    size: usize,
    preview: String,
    note: Option<String>,
}

pub struct Comparison {
    compared: usize,
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
    fn build(samples: Vec<Sample>, unavailable: Vec<Unavailable>) -> Self {
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
        let mut groups = Vec::with_capacity(grouped.len());
        for (index, members) in grouped.iter().enumerate() {
            let sample = &samples[members[0]];
            let text = sample.output.text();
            let note = match baseline {
                Some(_) if grouped.len() == 1 => None,
                Some(baseline) if baseline == index => Some("Largest result group".into()),
                Some(baseline) => {
                    let reference = samples[grouped[baseline][0]].output.text();
                    Some(match first_difference(reference, text) {
                        Some(line) => format!("First different line from largest group · {line}"),
                        None => "Same output as largest group".into(),
                    })
                }
                None => None,
            };
            groups.push(Group {
                members: members
                    .iter()
                    .map(|&member| samples[member].member.clone())
                    .collect(),
                exit_code: sample.exit_code,
                size: text.len(),
                preview: preview(text),
                note,
            });
        }
        groups.sort_by_key(|group| group.members.len());
        Self {
            compared,
            groups,
            unavailable,
        }
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
                        open.submission == submission_id && open.comparison.is_some()
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
            Action::Ready {
                revision,
                comparison,
            } => {
                if let Some(Page::HostGroup(page)) = self.tabs.get_mut(id).map(|tab| &mut tab.page)
                    && page.comparison.revision == revision
                    && let Some(open) = &mut page.comparison.open
                    && open.submission == submission_id
                {
                    open.comparison = Some(comparison);
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
        }));
        let revision = state.revision;
        let ready = move |comparison| {
            Message::HostGroup(super::Action::Compare(
                id,
                submission_id,
                Action::Ready {
                    revision,
                    comparison,
                },
            ))
        };
        ctx.spawn_background_with_rejection(
            move |cancel| {
                let comparison = if cancel.is_cancelled() {
                    Err("Comparison cancelled.")
                } else {
                    Ok(Comparison::build(samples, unavailable))
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
        let mut rows = Vec::new();
        let status = match &open.comparison {
            Some(Ok(comparison)) => {
                for group in &comparison.groups {
                    rows.push(KeyedView::new(
                        format!("group-{}", group.members[0].pane),
                        self.comparison_group(id, group, ctx),
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
                comparison.caption()
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
                        StackPanel::new()
                            .spacing(8.0)
                            .orientation(Orientation::Horizontal)
                            .children((
                                Button::new()
                                    .on_click(ctx.callback(move |_| action(Action::Back)))
                                    .content("Back to results"),
                                Button::new()
                                    .is_enabled(open.comparison.is_some())
                                    .on_click(ctx.callback(move |_| action(Action::Refresh)))
                                    .content("Refresh comparison"),
                            )),
                    )),
                    ScrollViewer::new()
                        .grid_row(1)
                        .content(StackPanel::new().spacing(12.0).keyed_children(rows)),
                )),
        )
    }

    fn comparison_group(&self, id: TabId, group: &Group, ctx: &ViewContext<Self>) -> View {
        let count = group.members.len();
        let host = if count == 1 { "host" } else { "hosts" };
        let first = group.members[0].pane;
        let history = group.members.iter().find(|member| {
            self.history_frame(member.pane)
                .is_some_and(|frame| frame.has_command(member.link))
        });
        let details = history.map(|member| (member.pane, member.link));
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
            output,
        };
        Delivery {
            target: Target {
                pane,
                name: format!("Host {pane}"),
                names: Default::default(),
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
        let results = Comparison::build(samples, Vec::new());
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
        );
        assert_eq!(results.groups.len(), 2);
        assert_eq!(
            results.groups[1].note.as_deref(),
            Some("First different line from largest group · 2")
        );
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
