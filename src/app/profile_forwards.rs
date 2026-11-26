use super::*;
use w11term::options::LocalForwardPreset;

const TITLE: &str = "Saved local forwards";

#[derive(Clone, Default)]
pub(super) struct Editor {
    expanded: bool,
    local: String,
    remote: String,
}

pub enum Edit {
    Expand(bool),
    Local(String),
    Remote(String),
    Add(LocalForwardPreset),
    Remove(LocalForwardPreset),
}

impl Editor {
    fn candidate(&self) -> Option<LocalForwardPreset> {
        Some(LocalForwardPreset {
            local_port: local_forwards::parse_port(&self.local)?,
            remote_port: local_forwards::parse_port(&self.remote)?,
        })
    }

    pub(super) fn update(&mut self, presets: &mut Vec<LocalForwardPreset>, edit: Edit) -> bool {
        match edit {
            Edit::Expand(expanded) => self.expanded = expanded,
            Edit::Local(text) => self.local = text,
            Edit::Remote(text) => self.remote = text,
            Edit::Add(preset) => {
                if self.candidate() == Some(preset) && !presets.contains(&preset) {
                    presets.push(preset);
                    self.local.clear();
                    self.remote.clear();
                    return true;
                }
            }
            Edit::Remove(preset) => {
                let count = presets.len();
                presets.retain(|saved| *saved != preset);
                return count != presets.len();
            }
        }
        false
    }

    pub(super) fn view(
        &self,
        id: TabId,
        presets: &[LocalForwardPreset],
        context: &ViewContext<TerminalApp>,
    ) -> View {
        let mut seen = std::collections::HashSet::new();
        let rows = presets
            .iter()
            .copied()
            .filter(|preset| seen.insert(*preset))
            .map(|preset| {
                let label = local_forwards::preset_label(preset);
                KeyedView::new(
                    label.clone(),
                    Grid::new()
                        .columns([GridLength::STAR, GridLength::Auto])
                        .column_spacing(8.0)
                        .children((
                            TextBlock::new()
                                .text(&label)
                                .text_wrapping(TextWrapping::Wrap)
                                .vertical_alignment(VerticalAlignment::Center),
                            Button::new()
                                .grid_column(1)
                                .automation_name(format!("Remove {label}"))
                                .on_click(context.callback(move |_| {
                                    Message::ProfileForward(id, Edit::Remove(preset))
                                }))
                                .content("Remove"),
                        )),
                )
            })
            .collect::<Vec<_>>();
        let count = rows.len();
        let candidate = self.candidate().filter(|preset| !presets.contains(preset));
        let fields =
            Grid::new()
                .columns([GridLength::STAR, GridLength::STAR])
                .column_spacing(8.0)
                .children((
                    StackPanel::new().spacing(4.0).children((
                        TextBlock::new().text("Windows loopback port"),
                        TextBox::new()
                            .automation_name("Saved forward Windows port")
                            .text(&self.local)
                            .on_text_changed(context.callback(move |text| {
                                Message::ProfileForward(id, Edit::Local(text))
                            })),
                    )),
                    StackPanel::new().grid_column(1).spacing(4.0).children((
                        TextBlock::new().text("Remote loopback port"),
                        TextBox::new()
                            .automation_name("Saved forward remote port")
                            .text(&self.remote)
                            .on_text_changed(context.callback(move |text| {
                                Message::ProfileForward(id, Edit::Remote(text))
                            })),
                    )),
                ));
        Expander::new()
            .automation_name(TITLE)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .is_expanded(self.expanded)
            .on_is_expanded_changed(
                context
                    .callback(move |expanded| Message::ProfileForward(id, Edit::Expand(expanded))),
            )
            .slots([
                SlotView::new(
                    ExpanderSlot::Header,
                    TextBlock::new().text(format!("{TITLE} ({count})")),
                ),
                SlotView::new(
                    ExpanderSlot::Content,
                    StackPanel::new().spacing(8.0).children((
                        TextBlock::new()
                            .text("Ports 1–65535. Start a saved forward from the connected pane.")
                            .text_wrapping(TextWrapping::Wrap),
                        StackPanel::new().spacing(4.0).keyed_children(rows),
                        fields,
                        Button::new()
                            .horizontal_alignment(HorizontalAlignment::Left)
                            .is_enabled(candidate.is_some())
                            .on_click(context.callback(move |_| {
                                candidate.map_or(Message::ControlIdle, |preset| {
                                    Message::ProfileForward(id, Edit::Add(preset))
                                })
                            }))
                            .content("Add saved forward"),
                    )),
                ),
            ])
    }
}
