use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Ready,
    Running,
    Complete,
    Stopped,
}

pub(super) struct Batch {
    paths: Vec<String>,
    phase: Phase,
    matching: usize,
    different: usize,
    errors: Vec<(String, String)>,
}

impl Batch {
    pub(super) fn new(paths: Vec<String>) -> Self {
        Self {
            paths,
            phase: Phase::Ready,
            matching: 0,
            different: 0,
            errors: Vec::new(),
        }
    }

    fn processed(&self) -> usize {
        self.matching + self.different + self.errors.len()
    }

    pub(super) fn start(&mut self) {
        if self.phase == Phase::Ready {
            self.phase = Phase::Running;
        }
    }

    pub(super) fn current(&self) -> Option<&str> {
        if self.phase == Phase::Running {
            self.paths.get(self.processed()).map(String::as_str)
        } else {
            None
        }
    }

    pub(super) fn stop(&mut self) {
        self.phase = Phase::Stopped;
    }

    pub(super) fn finish_file(&mut self, result: Result<&VerifiedFolderFile, &str>) {
        let Some(path) = self.current() else {
            return;
        };
        match result {
            Ok(result) if result.contents_match() => self.matching += 1,
            Ok(_) => self.different += 1,
            Err(error) => {
                let path = path.to_owned();
                self.errors.push((path, error.into()));
            }
        }
        if self.processed() == self.paths.len() {
            self.phase = Phase::Complete;
        }
    }

    pub(super) fn dialog(
        &self,
        context: &ViewContext<TerminalApp>,
        message: impl Fn(super::super::Action) -> Message + Copy + 'static,
        stopping: bool,
    ) -> View {
        let event = move |action| message(super::super::Action::Verify(action));
        let button = |label: &'static str, enabled, action: Action| {
            Button::new()
                .is_enabled(enabled)
                .on_click(context.callback(move |_| event(action.clone())))
                .content(label)
        };
        let count = self.paths.len();
        let mut fields = vec![KeyedView::new(
            "scope",
            TextBlock::new()
                .text(format!(
                    "{count} paired files on this page. SHA-256 compares the live files, up to {} MiB per file.",
                    FILE_VERIFICATION_LIMIT / (1024 * 1024)
                ))
                .text_wrapping(TextWrapping::Wrap),
        )];
        if self.phase == Phase::Ready {
            fields.push(KeyedView::new(
                "start",
                button("Start page verification", count > 0, Action::StartPage),
            ));
        } else {
            let status = match self.phase {
                Phase::Ready => unreachable!(),
                Phase::Running if stopping => "Stopping page verification…",
                Phase::Running => "Verifying the page…",
                Phase::Complete => "Page verification complete",
                Phase::Stopped => "Page verification stopped",
            };
            for (key, line) in [
                ("status", status.to_owned()),
                (
                    "progress",
                    format!("Processed {} of {count} files", self.processed()),
                ),
                (
                    "counts",
                    format!(
                        "{} matched · {} differed · {} failed",
                        self.matching,
                        self.different,
                        self.errors.len()
                    ),
                ),
            ] {
                fields.push(KeyedView::new(
                    key,
                    TextBlock::new()
                        .text(line)
                        .text_wrapping(TextWrapping::Wrap),
                ));
            }
            if let Some(path) = self.current() {
                fields.push(KeyedView::new(
                    "current",
                    TextBlock::new()
                        .text(path)
                        .text_wrapping(TextWrapping::Wrap),
                ));
                fields.push(KeyedView::new(
                    "stop",
                    button("Stop page verification", !stopping, Action::Stop),
                ));
            }
            if !self.errors.is_empty() {
                fields.push(KeyedView::new(
                    "errors",
                    ScrollViewer::new()
                        .automation_name("Page verification errors")
                        .max_height(130.0)
                        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                        .content(StackPanel::new().spacing(6.0).keyed_children(
                            self.errors.iter().map(|(path, error)| {
                                KeyedView::new(
                                    path.as_str(),
                                    TextBlock::new()
                                        .text(format!("{path}\n{error}"))
                                        .text_wrapping(TextWrapping::Wrap),
                                )
                            }),
                        )),
                ));
            }
        }
        fields.push(KeyedView::new(
            "back",
            button("Back to comparison", true, Action::Back),
        ));
        ContentDialog::new()
            .title("Verify files on this page")
            .close_button_text("Close folder comparison")
            .is_open(true)
            .on_closed(context.callback(move |_| message(super::super::Action::Close)))
            .content(
                StackPanel::new()
                    .min_width(290.0)
                    .spacing(8.0)
                    .keyed_children(fields),
            )
    }
}
