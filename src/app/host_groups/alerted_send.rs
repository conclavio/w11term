use super::*;
use live_output::OutputSearch;
use regex::Regex;
use w11term::events::{GroupOutputAlert, GroupOutputAlertRule};

#[derive(Default)]
pub(super) struct State {
    review: Option<Box<Review>>,
    feedback: Option<&'static str>,
}

struct Review {
    offer: CommandOffer,
    search: OutputSearch,
    record: bool,
}

pub(super) struct Alert {
    pattern: Regex,
    rule: Arc<GroupOutputAlertRule>,
}

pub enum Action {
    Open(CommandOffer),
    Query(String),
    Regex(bool),
    MatchCase(bool),
    Record(bool),
    Confirm,
    Cancel,
}

impl State {
    pub(super) fn is_open(&self) -> bool {
        self.review.is_some()
    }

    pub(super) fn clear_feedback(&mut self) {
        self.feedback = None;
    }

    pub(super) fn feedback(&self) -> View {
        self.feedback.map_or_else(View::empty, |message| {
            TextBlock::new()
                .text(message)
                .text_wrapping(TextWrapping::Wrap)
                .into()
        })
    }

    pub(super) fn view(&self, id: TabId, ctx: &ViewContext<TerminalApp>) -> Option<View> {
        let review = self.review.as_ref()?;
        let send = move |action| Message::HostGroup(super::Action::AlertedSend(id, action));
        let search = &review.search;
        let targets = review.offer.targets.iter().map(|target| {
            KeyedView::new(
                target.pane.to_string(),
                TextBlock::new()
                    .text(format!("{} · {}", target.name, target.status_caption()))
                    .text_wrapping(TextWrapping::Wrap),
            )
        });
        Some(
            Grid::new()
                .margin(16.0)
                .row_spacing(8.0)
                .rows([GridLength::Auto, GridLength::STAR])
                .children((
                    StackPanel::new().spacing(8.0).children((
                        TextBlock::new()
                            .font_size(24.0)
                            .text("Send with output alert"),
                        TextBlock::new()
                            .text(review.offer.text.as_ref())
                            .text_wrapping(TextWrapping::Wrap),
                        TextBlock::new()
                            .text(review.offer.dispatch_policy().caption())
                            .text_wrapping(TextWrapping::Wrap),
                        TextBox::new()
                            .automation_name("Search for the output alert")
                            .placeholder_text("Find in new output")
                            .text(&search.query)
                            .on_text_changed(ctx.callback(move |query| send(Action::Query(query)))),
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .children((
                                CheckBox::new()
                                    .is_checked(search.regex)
                                    .on_is_checked_changed(
                                        ctx.callback(move |on| send(Action::Regex(on))),
                                    )
                                    .content("Regular expression"),
                                CheckBox::new()
                                    .is_checked(search.sensitive)
                                    .on_is_checked_changed(
                                        ctx.callback(move |on| send(Action::MatchCase(on))),
                                    )
                                    .content("Match case"),
                            )),
                        search.error().map_or_else(View::empty, |error| {
                            TextBlock::new()
                                .text(error)
                                .text_wrapping(TextWrapping::Wrap)
                                .into()
                        }),
                        CheckBox::new()
                            .is_checked(review.record)
                            .on_is_checked_changed(ctx.callback(move |on| send(Action::Record(on))))
                            .content("Record full output"),
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .children((
                                Button::new()
                                    .style(ButtonStyle::Accent)
                                    .is_enabled(search.matcher.as_ref().is_some_and(Result::is_ok))
                                    .on_click(ctx.callback(move |_| send(Action::Confirm)))
                                    .content(if review.record {
                                        "Choose recording file…"
                                    } else {
                                        "Send with alert"
                                    }),
                                Button::new()
                                    .on_click(ctx.callback(move |_| send(Action::Cancel)))
                                    .content("Cancel"),
                            )),
                    )),
                    ScrollViewer::new()
                        .grid_row(1)
                        .content(StackPanel::new().spacing(6.0).keyed_children(targets)),
                )),
        )
    }
}

pub(super) fn button(
    id: TabId,
    offer: CommandOffer,
    enabled: bool,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    Button::new()
        .is_enabled(enabled)
        .on_click(ctx.callback(move |_| {
            Message::HostGroup(super::Action::AlertedSend(id, Action::Open(offer.clone())))
        }))
        .content("Send with alert…")
}

impl TerminalApp {
    pub(super) fn handle_alerted_send(
        &mut self,
        id: TabId,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        if self.tabs.active_id() != Some(id) {
            return;
        }
        match action {
            Action::Open(offer) => {
                if self.group_send_matches(id, &offer, true)
                    && let Some(page) = self
                        .host_group_mut(id)
                        .filter(|page| !page.recorded_send.pending())
                {
                    page.alerted_send.clear_feedback();
                    page.alerted_send.review = Some(Box::new(Review {
                        offer,
                        search: OutputSearch::default(),
                        record: false,
                    }));
                }
            }
            Action::Confirm => self.submit_alerted_send(id, ctx),
            Action::Cancel => {
                if let Some(page) = self.host_group_mut(id) {
                    page.alerted_send.review = None;
                }
            }
            action => {
                let Some(review) = self
                    .host_group_mut(id)
                    .and_then(|page| page.alerted_send.review.as_mut())
                else {
                    return;
                };
                let search = &mut review.search;
                match action {
                    Action::Query(query) => search.set_query(query),
                    Action::Regex(on) => {
                        search.configure(search.query.clone(), on, search.sensitive)
                    }
                    Action::MatchCase(on) => {
                        search.configure(search.query.clone(), search.regex, on)
                    }
                    Action::Record(on) => review.record = on,
                    _ => {}
                }
            }
        }
    }

    fn submit_alerted_send(&mut self, id: TabId, ctx: &ComponentContext<Self>) {
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        let Some(review) = page
            .alerted_send
            .review
            .take_if(|review| review.search.matcher.as_ref().is_some_and(Result::is_ok))
        else {
            return;
        };
        let alert = Alert {
            rule: review.search.alert_rule(None),
            pattern: review
                .search
                .matcher
                .expect("review checked")
                .expect("search validated"),
        };
        if review.record {
            self.pick_recorded_send(id, review.offer, Some(alert), ctx);
            return;
        }
        if !self.group_send_matches(id, &review.offer, true) {
            if let Some(page) = self.host_group_mut(id) {
                page.alerted_send.feedback = Some(REVIEW_CHANGED);
            }
            return;
        }
        let transcript = GroupTranscript::default();
        self.arm_group_send_alert(id, &review.offer, alert, &transcript);
        self.submit_group_command(id, review.offer, transcript, Default::default());
    }

    pub(super) fn arm_group_send_alert(
        &self,
        id: TabId,
        offer: &CommandOffer,
        alert: Alert,
        transcript: &GroupTranscript,
    ) {
        let Some(AppTab {
            label,
            page: Page::HostGroup(page),
        }) = self.tabs.get(id)
        else {
            return;
        };
        transcript.arm_output_alert(GroupOutputAlert {
            inbox: self.inbox.clone(),
            tab: id,
            submission: page.next_submission + 1,
            source: label.clone(),
            hosts: offer
                .targets
                .iter()
                .map(|target| (target.pane, target.name.clone()))
                .collect(),
            rule: alert.rule,
            pattern: alert.pattern,
        });
    }
}
