use super::*;

const CHOICES: [(&str, SshGroupParallelLimit); 6] = [
    ("Unlimited", SshGroupParallelLimit::Unlimited),
    ("1 at a time", SshGroupParallelLimit::One),
    ("2 at a time", SshGroupParallelLimit::Two),
    ("4 at a time", SshGroupParallelLimit::Four),
    ("8 at a time", SshGroupParallelLimit::Eight),
    ("16 at a time", SshGroupParallelLimit::Sixteen),
];

pub(super) fn picker(
    label: &'static str,
    current: SshGroupParallelLimit,
    enabled: bool,
    select: impl Fn(SshGroupParallelLimit) -> Message + 'static,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(8.0)
        .children((
            TextBlock::new()
                .vertical_alignment(VerticalAlignment::Center)
                .text(label),
            ComboBox::new()
                .automation_name(label)
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
