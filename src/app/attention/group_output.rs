use super::*;
use crate::app::host_groups::displayed_line;
use w11term::events::GroupTranscriptLine;

pub(in crate::app) struct MatchView {
    pub(super) target: NoticeTarget,
    excerpt: String,
    context: Option<(String, Vec<GroupTranscriptLine>)>,
}

impl TerminalApp {
    pub(super) fn open_group_output_match(&mut self, target: NoticeTarget) {
        let NoticeTarget::GroupOutput {
            tab,
            submission,
            pane,
            line_id,
        } = target
        else {
            return;
        };
        if !matches!(self.active_attention(), Some(AttentionPage::Inbox)) {
            return;
        }
        let Some(excerpt) = self.inbox.read_group_output_notice(target) else {
            return;
        };
        let context = self.group_output_context(tab, submission, pane, line_id);
        self.set_attention(AttentionPage::GroupOutput(MatchView {
            target,
            excerpt,
            context,
        }));
    }

    pub(super) fn group_output_match_view(
        &self,
        output: &MatchView,
        ctx: &ViewContext<Self>,
    ) -> View {
        let target = output.target;
        let NoticeTarget::GroupOutput {
            tab,
            submission,
            line_id,
            ..
        } = target
        else {
            unreachable!("the group output page requires an output notice");
        };
        let (subtitle, body) = match &output.context {
            Some((host, lines)) => {
                let rows: Vec<_> = lines
                    .iter()
                    .map(|line| {
                        KeyedView::new(
                            line.id.to_string(),
                            StackPanel::new().spacing(3.0).children((
                                if line.id == line_id {
                                    TextBlock::new().text("Matched line").into()
                                } else {
                                    View::empty()
                                },
                                TextBlock::new()
                                    .text(displayed_line(line).as_ref())
                                    .text_wrapping(TextWrapping::Wrap),
                            )),
                        )
                    })
                    .collect();
                (
                    format!("Run {submission} · {host}"),
                    StackPanel::new().spacing(8.0).keyed_children(rows),
                )
            }
            None => (
                format!("Run {submission} · Saved alert excerpt"),
                TextBlock::new()
                    .text(&output.excerpt)
                    .text_wrapping(TextWrapping::Wrap)
                    .into(),
            ),
        };
        Grid::new()
            .margin(16.0)
            .row_spacing(8.0)
            .rows([
                GridLength::Auto,
                GridLength::Auto,
                GridLength::Auto,
                GridLength::STAR,
            ])
            .children((
                TextBlock::new()
                    .font_size(24.0)
                    .text("SSH group output match"),
                TextBlock::new()
                    .grid_row(1)
                    .text(subtitle)
                    .text_wrapping(TextWrapping::Wrap),
                StackPanel::new()
                    .grid_row(2)
                    .spacing(8.0)
                    .orientation(Orientation::Horizontal)
                    .children((
                        Button::new()
                            .on_click(ctx.callback(|_| Message::Attention(Action::Open)))
                            .content("Back to inbox"),
                        Button::new()
                            .is_enabled(self.group_run_retained(tab, submission))
                            .on_click(
                                ctx.callback(move |_| {
                                    Message::Attention(Action::VisitGroup(target))
                                }),
                            )
                            .content("Open run results"),
                    )),
                ScrollViewer::new().grid_row(3).content(body),
            ))
    }
}
