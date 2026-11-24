use super::*;

pub(super) const LABEL: &str = "Export group variables";
const SYNTAXES: &[&str] = &["POSIX (sh, Bash, Zsh)", "Fish"];

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Syntax {
    #[default]
    Posix,
    Fish,
}

fn shell_literal(value: &str) -> String {
    // Keep backslashes outside quotes, where POSIX shells and Fish agree on
    // escaping. An incorrect dialect selection must still send literal data.
    let mut quoted = String::from("'");
    for ch in value.chars() {
        match ch {
            '\\' => quoted.push_str(r"'\\'"),
            '\'' => quoted.push_str(r"'\''"),
            _ => quoted.push(ch),
        }
    }
    quoted.push('\'');
    quoted
}

impl Syntax {
    fn command(self, names: &filter::Names, rank: usize, total: usize) -> Option<Arc<str>> {
        let host = shell_literal(&names.host);
        let label = shell_literal(&names.label);
        let command = match self {
            Self::Posix => format!(
                "export POLYSH_NR_SHELLS={total} POLYSH_RANK={rank} POLYSH_NAME={host} POLYSH_DISPLAY_NAME={label}"
            ),
            Self::Fish => format!(
                "set -gx POLYSH_NR_SHELLS {total} && set -gx POLYSH_RANK {rank} && set -gx POLYSH_NAME {host} && set -gx POLYSH_DISPLAY_NAME {label}"
            ),
        };
        w11term::saved_commands::valid_command_text(&command).then(|| command.into())
    }
}

struct Item {
    target: Target,
    command: Option<Arc<str>>,
}

struct Review {
    syntax: Syntax,
    items: Vec<Item>,
}

fn checked<'a>(page: &'a HostGroup, roster: &'a [Target]) -> impl Iterator<Item = &'a Target> {
    page.shown(roster)
        .filter(|target| page.selected.contains(&target.pane))
}

fn identity(target: &Target) -> (u64, Option<PromptToken>, &str, &str) {
    (
        target.pane,
        target.state.prompt(),
        &target.names.host,
        &target.names.label,
    )
}

impl Review {
    fn change_syntax(&mut self, syntax: Syntax) {
        self.syntax = syntax;
        let total = self.items.len();
        for (rank, item) in self.items.iter_mut().enumerate() {
            item.command = syntax.command(&item.target.names, rank, total);
        }
    }

    fn issue(&self, page: &HostGroup, roster: &[Target]) -> Option<&'static str> {
        if self.items.iter().any(|item| item.command.is_none()) {
            Some("A host name or label cannot be sent as a single shell line.")
        } else if page.sending()
            || !checked(page, roster)
                .map(identity)
                .eq(self.items.iter().map(|item| identity(&item.target)))
        {
            Some("The hosts or their prompts changed. Return to the roster and review again.")
        } else {
            None
        }
    }
}

#[derive(Default)]
pub(super) struct State {
    revision: u64,
    review: Option<Review>,
}

pub enum Action {
    Open,
    SetSyntax(u64, usize),
    Send(u64),
    Close(u64),
}

impl State {
    pub(super) fn is_open(&self) -> bool {
        self.review.is_some()
    }

    pub(super) fn view(
        &self,
        id: TabId,
        page: &HostGroup,
        roster: &[Target],
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let Some(review) = &self.review else {
            return View::empty();
        };
        let revision = self.revision;
        let issue = review.issue(page, roster);
        let action = move |action| Message::HostGroup(super::Action::Environment(id, action));
        let count = review.items.len();
        let hosts = if count == 1 { "host" } else { "hosts" };
        let rows = review.items.iter().map(|item| {
            KeyedView::new(
                item.target.pane.to_string(),
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new()
                        .text(&item.target.name)
                        .text_wrapping(TextWrapping::Wrap),
                    TextBlock::new()
                        .text(
                            item.command
                                .as_deref()
                                .unwrap_or("This name cannot be sent."),
                        )
                        .text_wrapping(TextWrapping::Wrap),
                )),
            )
        });
        Grid::new()
            .margin(16.0)
            .row_spacing(8.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                StackPanel::new().spacing(8.0).children((
                    TextBlock::new().font_size(24.0).text(LABEL),
                    TextBlock::new()
                        .text("Set the variables in each selected shell. Choose the syntax used by all these shells.")
                        .text_wrapping(TextWrapping::Wrap),
                    ComboBox::new()
                        .automation_name("Shell syntax")
                        .items_source(SYNTAXES.iter().copied())
                        .selected_index(Some(usize::from(review.syntax == Syntax::Fish)))
                        .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                            index.map_or(Message::ControlIdle, |index| action(Action::SetSyntax(revision, index)))
                        })),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .style(ButtonStyle::Accent)
                                .is_enabled(issue.is_none())
                                .on_click(ctx.callback(move |_| action(Action::Send(revision))))
                                .content(format!("Export to {count} {hosts}")),
                            Button::new()
                                .on_click(ctx.callback(move |_| action(Action::Close(revision))))
                                .content("Back to host group"),
                        )),
                    issue.map_or_else(View::empty, |message| {
                        TextBlock::new().text(message).text_wrapping(TextWrapping::Wrap).into()
                    }),
                )),
                ScrollViewer::new().grid_row(1).content(
                    StackPanel::new().spacing(10.0).keyed_children(rows),
                ),
            ))
    }
}

pub(super) fn button(
    id: TabId,
    page: &HostGroup,
    selected: &[Target],
    ctx: &ViewContext<TerminalApp>,
) -> View {
    Button::new()
        .is_enabled(
            !page.sending()
                && !selected.is_empty()
                && selected.iter().all(|target| target.state.is_ready()),
        )
        .on_click(
            ctx.callback(move |_| Message::HostGroup(super::Action::Environment(id, Action::Open))),
        )
        .content("Export group variables…")
}

impl TerminalApp {
    pub(super) fn handle_host_group_environment(&mut self, id: TabId, action: Action) {
        match action {
            Action::Open => {
                if self.host_group_command_target() != Some(id) {
                    return;
                }
                let roster = self.host_group_targets();
                let Some(page) = self.host_group_mut(id) else {
                    return;
                };
                let items: Vec<_> = checked(page, &roster)
                    .cloned()
                    .map(|target| Item {
                        target,
                        command: None,
                    })
                    .collect();
                if items.is_empty()
                    || page.sending()
                    || items.iter().any(|item| !item.target.state.is_ready())
                {
                    return;
                }
                let mut review = Review {
                    syntax: Syntax::Posix,
                    items,
                };
                review.change_syntax(Syntax::Posix);
                page.environment.revision += 1;
                page.environment.review = Some(review);
            }
            Action::SetSyntax(revision, index) => {
                let syntax = match index {
                    0 => Syntax::Posix,
                    1 => Syntax::Fish,
                    _ => return,
                };
                let Some(page) = self
                    .host_group_mut(id)
                    .filter(|page| page.environment.revision == revision)
                else {
                    return;
                };
                if let Some(review) = &mut page.environment.review
                    && review.syntax != syntax
                {
                    review.change_syntax(syntax);
                    page.environment.revision += 1;
                }
            }
            Action::Send(revision) => self.send_host_group_environment(id, revision),
            Action::Close(revision) => {
                if let Some(page) = self
                    .host_group_mut(id)
                    .filter(|page| page.environment.revision == revision)
                {
                    page.environment.review = None;
                }
            }
        }
    }

    fn send_host_group_environment(&mut self, id: TabId, revision: u64) {
        let roster = self.host_group_targets();
        let Some(page) = self
            .host_group_mut(id)
            .filter(|page| page.environment.revision == revision)
        else {
            return;
        };
        let Some(review) = &page.environment.review else {
            return;
        };
        if review.issue(page, &roster).is_some() {
            return;
        }
        let review = page
            .environment
            .review
            .take()
            .expect("the checked review is open");
        let transcript = GroupTranscript::default();
        let mut commands = BTreeMap::new();
        let results = review
            .items
            .into_iter()
            .map(|item| {
                let command = item.command.expect("the checked command is valid");
                commands.insert(item.target.pane, command.clone());
                let host = transcript.host(item.target.pane);
                self.dispatch_to_host(item.target, command, host)
            })
            .collect();
        if let Some(page) = self.host_group_mut(id) {
            page.submit(
                SubmissionContent::Environment(commands),
                results,
                transcript,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_commands_quote_values_and_reject_terminal_controls() {
        let names = filter::Names::new(r"Pilot's $(run) \\雪", "2001:db8::9", "ops", 22);
        assert_eq!(
            Syntax::Posix.command(&names, 1, 3).as_deref(),
            Some(
                r"export POLYSH_NR_SHELLS=3 POLYSH_RANK=1 POLYSH_NAME='2001:db8::9' POLYSH_DISPLAY_NAME='Pilot'\''s $(run) '\\''\\'雪'"
            )
        );
        assert_eq!(
            Syntax::Fish.command(&names, 1, 3).as_deref(),
            Some(
                r"set -gx POLYSH_NR_SHELLS 3 && set -gx POLYSH_RANK 1 && set -gx POLYSH_NAME '2001:db8::9' && set -gx POLYSH_DISPLAY_NAME 'Pilot'\''s $(run) '\\''\\'雪'"
            )
        );
        for label in [
            "line\rbreak",
            "escape\x1bsequence",
            "unicode\u{2028}separator",
        ] {
            let names = filter::Names::new(label, "web", "ops", 22);
            assert!(Syntax::Posix.command(&names, 0, 1).is_none());
            assert!(Syntax::Fish.command(&names, 0, 1).is_none());
        }
    }
}
