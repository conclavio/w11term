use super::*;
use w11term::windows_release::{self, LATEST_PAGE, Latest};

pub enum Action {
    Expand(bool),
    Check,
    Completed(Result<Latest, String>),
    OpenPage,
}

#[derive(Default)]
enum Check {
    #[default]
    Idle,
    Running,
    Ready(Latest),
    Failed(String),
}

#[derive(Default)]
pub(super) struct Panel {
    expanded: bool,
    state: Check,
}

impl Panel {
    pub(super) fn view(&self, ctx: &ViewContext<TerminalApp>) -> View {
        let version = windows_release::VERSION;
        let current = windows_release::build_commit();
        let running = match current {
            Some(commit) => format!("Running w11term {version} · build-{}", &commit[..12]),
            None => format!("Running w11term {version} · local build"),
        };
        let mut fields = vec![KeyedView::new("current", TextBlock::new().text(running))];
        match &self.state {
            Check::Idle => {}
            Check::Running => fields.push(KeyedView::new(
                "status",
                TextBlock::new().text("Checking GitHub for the latest Windows release…"),
            )),
            Check::Failed(error) => fields.push(KeyedView::new(
                "status",
                TextBlock::new()
                    .text(format!("Release check failed · {error}"))
                    .text_wrapping(TextWrapping::Wrap),
            )),
            Check::Ready(release) => {
                fields.push(KeyedView::new(
                    "status",
                    TextBlock::new()
                        .text(if current == Some(release.commit.as_str()) {
                            "This is the latest published Windows build".to_owned()
                        } else {
                            format!("Latest Windows build · {}", release.tag)
                        })
                        .text_wrapping(TextWrapping::Wrap),
                ));
                fields.push(KeyedView::new(
                    "published",
                    TextBlock::new().text(format!(
                        "Published {} · {:.1} MiB ZIP",
                        release.published,
                        release.archive_bytes as f64 / (1024.0 * 1024.0)
                    )),
                ));
            }
        }
        let page_label = match &self.state {
            Check::Ready(_) => "Open release page on GitHub",
            _ => "Open Latest on GitHub",
        };
        fields.push(KeyedView::new(
            "actions",
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    Button::new()
                        .is_enabled(!matches!(self.state, Check::Running))
                        .on_click(ctx.callback(|_| Message::WindowsRelease(Action::Check)))
                        .content("Check latest Windows release"),
                    HyperlinkButton::new()
                        .on_click(ctx.callback(|_| Message::WindowsRelease(Action::OpenPage)))
                        .content(page_label),
                )),
        ));
        Expander::new()
            .automation_name("Windows release")
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .is_expanded(self.expanded)
            .on_is_expanded_changed(
                ctx.callback(|expanded| Message::WindowsRelease(Action::Expand(expanded))),
            )
            .slots([
                SlotView::new(
                    ExpanderSlot::Header,
                    TextBlock::new().text("Windows release"),
                ),
                SlotView::new(
                    ExpanderSlot::Content,
                    StackPanel::new().spacing(6.0).keyed_children(fields),
                ),
            ])
    }

    pub(super) fn update(&mut self, action: Action, ctx: &ComponentContext<TerminalApp>) {
        match action {
            Action::Expand(expanded) => self.expanded = expanded,
            Action::Check if !matches!(self.state, Check::Running) => {
                self.state = Check::Running;
                ctx.spawn_background_with_rejection(
                    |scope| {
                        let result = windows_release::check(|| scope.is_cancelled())
                            .map_err(|error| format!("{error:#}"));
                        Message::WindowsRelease(Action::Completed(result))
                    },
                    Message::WindowsRelease(Action::Completed(Err(
                        "Release check worker unavailable".into(),
                    ))),
                );
            }
            Action::Completed(result) if matches!(self.state, Check::Running) => {
                self.state = match result {
                    Ok(release) => Check::Ready(release),
                    Err(error) => Check::Failed(error),
                };
            }
            Action::OpenPage => match &self.state {
                Check::Ready(release) => TerminalApp::open_link(&release.page()),
                _ => TerminalApp::open_link(LATEST_PAGE),
            },
            _ => {}
        }
    }
}
