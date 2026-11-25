use super::{live_output::OutputSearch, *};
use w11term::saved_output_rules::{Edit, MAX_RULES, Rule, Rules};

#[derive(Default)]
pub(super) struct Panel {
    rules: Rules,
    name: String,
    feedback: Option<String>,
}

#[derive(Clone)]
pub enum Action {
    Toggle,
    Name(String),
    Save,
    Load(Rule),
    Remove(Rule),
}

impl OutputSearch {
    pub(super) fn update_presets(&mut self, action: Action) {
        if matches!(action, Action::Toggle) {
            if self.presets.is_some() {
                self.presets = None;
            } else {
                let mut panel = Panel::default();
                match Rules::path().and_then(|path| Rules::load(&path)) {
                    Ok(rules) => panel.rules = rules,
                    Err(error) => panel.feedback = Some(format!("{error:#}")),
                }
                self.presets = Some(Box::new(panel));
            }
            return;
        }
        let Some(panel) = self.presets.as_mut() else {
            return;
        };
        let edit = match action {
            Action::Name(name) => {
                panel.name = name;
                panel.feedback = None;
                return;
            }
            Action::Load(rule) if panel.rules.entries().contains(&rule) => {
                self.configure(rule.query, rule.regex, rule.match_case);
                self.presets = None;
                return;
            }
            Action::Save if self.matcher.as_ref().is_some_and(Result::is_ok) => Edit::Save(Rule {
                name: panel.name.clone(),
                query: self.query.clone(),
                regex: self.regex,
                match_case: self.sensitive,
            }),
            Action::Remove(rule) => Edit::Remove(rule),
            _ => return,
        };
        match Rules::path().and_then(|path| Rules::update(&path, edit)) {
            Ok(rules) => {
                panel.rules = rules;
                panel.name.clear();
                panel.feedback = None;
            }
            Err(error) => panel.feedback = Some(format!("{error:#}")),
        }
    }

    pub(super) fn preset_view(
        &self,
        send: impl Fn(Action) -> Message + Copy + 'static,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let toggle = Button::new()
            .on_click(ctx.callback(move |_| send(Action::Toggle)))
            .content(if self.presets.is_some() {
                "Close saved rules"
            } else {
                "Saved alert rules…"
            });
        let Some(panel) = &self.presets else {
            return toggle;
        };
        let rows = panel.rules.entries().iter().map(|rule| {
            let load = rule.clone();
            let remove = rule.clone();
            KeyedView::new(
                rule.name.clone(),
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .column_spacing(8.0)
                    .children((
                        Button::new()
                            .automation_name(format!("Use alert rule {}", rule.name))
                            .on_click(ctx.callback(move |_| send(Action::Load(load.clone()))))
                            .content(rule.name.as_str()),
                        Button::new()
                            .grid_column(1)
                            .automation_name(format!("Remove alert rule {}", rule.name))
                            .on_click(ctx.callback(move |_| send(Action::Remove(remove.clone()))))
                            .content("Remove"),
                    )),
            )
        });
        StackPanel::new().spacing(6.0).children((
            toggle,
            if panel.rules.entries().is_empty() {
                TextBlock::new().text("No saved alert rules").into()
            } else {
                ScrollViewer::new()
                    .max_height(128.0)
                    .content(StackPanel::new().spacing(4.0).keyed_children(rows))
            },
            Grid::new()
                .columns([GridLength::STAR, GridLength::Auto])
                .column_spacing(8.0)
                .children((
                    TextBox::new()
                        .automation_name("Output alert rule name")
                        .placeholder_text("Name for this rule")
                        .text(&panel.name)
                        .on_text_changed(ctx.callback(move |name| send(Action::Name(name)))),
                    Button::new()
                        .grid_column(1)
                        .is_enabled(
                            !panel.name.trim().is_empty()
                                && panel.rules.entries().len() < MAX_RULES
                                && self.matcher.as_ref().is_some_and(Result::is_ok),
                        )
                        .on_click(ctx.callback(move |_| send(Action::Save)))
                        .content("Save current rule"),
                )),
            panel
                .feedback
                .as_ref()
                .map_or_else(View::empty, |feedback| {
                    TextBlock::new()
                        .text(feedback)
                        .text_wrapping(TextWrapping::Wrap)
                        .into()
                }),
        ))
    }
}
