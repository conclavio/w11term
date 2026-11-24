use super::*;

const CHOICES: [(&str, SshGroupLoginLimit); 6] = [
    ("Unlimited", SshGroupLoginLimit::Unlimited),
    ("1 at a time", SshGroupLoginLimit::One),
    ("2 at a time", SshGroupLoginLimit::Two),
    ("4 at a time", SshGroupLoginLimit::Four),
    ("8 at a time", SshGroupLoginLimit::Eight),
    ("16 at a time", SshGroupLoginLimit::Sixteen),
];

pub(super) fn picker(
    current: SshGroupLoginLimit,
    enabled: bool,
    select: impl Fn(SshGroupLoginLimit) -> Message + 'static,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(8.0)
        .children((
            TextBlock::new()
                .vertical_alignment(VerticalAlignment::Center)
                .text("Parallel SSH logins"),
            ComboBox::new()
                .automation_name("Parallel SSH logins")
                .is_enabled(enabled)
                .items_source(CHOICES.map(|(label, _)| label))
                .selected_index(CHOICES.iter().position(|(_, limit)| *limit == current))
                .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                    index
                        .and_then(|index| CHOICES.get(index))
                        .map_or(Message::ControlIdle, |&(_, limit)| select(limit))
                })),
        ))
}
