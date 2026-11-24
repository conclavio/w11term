use super::*;
use std::fmt::Write as _;
use w11term::events::{GroupQueueActivity, GroupQueueEvent, GroupQueueEventKind};

fn label(kind: GroupQueueEventKind) -> Cow<'static, str> {
    match kind {
        GroupQueueEventKind::Paused => "Paused queue".into(),
        GroupQueueEventKind::Resumed => "Resumed queue".into(),
        GroupQueueEventKind::CanaryReady => "Canary ready for approval".into(),
        GroupQueueEventKind::CanaryApproved => "Approved remaining hosts".into(),
        GroupQueueEventKind::CanaryFailed => {
            "Stopped after the canary did not confirm success".into()
        }
        GroupQueueEventKind::StoppedOnFailure => "Stopped after an unsuccessful host".into(),
        GroupQueueEventKind::Cancelled => "Cancelled queued commands".into(),
        GroupQueueEventKind::HostSkipped { pane_id } => {
            format!("Skipped queued host · Pane {pane_id}").into()
        }
    }
}

fn caption(event: &GroupQueueEvent) -> String {
    format!(
        "{}.{:03} s · {}",
        event.elapsed_ms / 1000,
        event.elapsed_ms % 1000,
        label(event.kind),
    )
}

fn omitted(count: u64) -> Option<String> {
    (count != 0).then(|| {
        if count == 1 {
            "1 earlier queue event omitted".into()
        } else {
            format!("{count} earlier queue events omitted")
        }
    })
}

pub(super) fn text(activity: &GroupQueueActivity) -> String {
    if activity.is_empty() {
        return String::new();
    }
    let mut block = String::from("[Queue activity]\r\n");
    if let Some(omitted) = omitted(activity.omitted) {
        let _ = writeln!(block, "{omitted}\r");
    }
    for event in &activity.events {
        let _ = writeln!(block, "{}\r", caption(event));
    }
    block
}

pub(super) fn view(id: TabId, submission: &Submission, ctx: &ViewContext<TerminalApp>) -> View {
    let activity = submission.transcript.queue_activity();
    if activity.is_empty() {
        return View::empty();
    }
    let run = submission.id;
    let show = submission.show_queue_activity;
    let button = Button::new()
        .on_click(
            ctx.callback(move |_| Message::HostGroup(Action::ShowQueueActivity(id, run, !show))),
        )
        .content(if show {
            "Hide queue activity"
        } else {
            "Show queue activity"
        });
    if !show {
        return button;
    }
    let rows = activity.events.iter().enumerate().map(|(index, event)| {
        KeyedView::new(
            index.to_string(),
            TextBlock::new()
                .automation_name(label(event.kind).as_ref())
                .text(caption(event))
                .text_wrapping(TextWrapping::Wrap),
        )
    });
    StackPanel::new().spacing(4.0).children((
        button,
        omitted(activity.omitted)
            .map_or_else(View::empty, |text| TextBlock::new().text(text).into()),
        ScrollViewer::new()
            .max_height(132.0)
            .content(StackPanel::new().spacing(2.0).keyed_children(rows)),
    ))
}
