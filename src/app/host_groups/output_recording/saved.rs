use super::*;

#[derive(Default)]
pub(in crate::app::host_groups) struct State {
    paths: Vec<PathBuf>,
    selected: usize,
    revealing: Option<PathBuf>,
    feedback: Option<String>,
}

pub enum Action {
    Select(PathBuf),
    CopyPath(PathBuf),
    Reveal(PathBuf),
    Revealed(PathBuf, Result<(), String>),
}

fn message(id: TabId, run: u64, action: Action) -> Message {
    Message::HostGroup(super::super::Action::RecordOutput(
        id,
        run,
        super::Action::Saved(action),
    ))
}

impl State {
    pub(in crate::app::host_groups) fn add(&mut self, path: PathBuf) {
        self.paths.retain(|previous| previous != &path);
        self.paths.insert(0, path);
        self.selected = 0;
        self.feedback = None;
    }

    fn selected(&self) -> Option<&PathBuf> {
        self.paths.get(self.selected)
    }

    fn select(&mut self, path: &Path) {
        if let Some(index) = self.paths.iter().position(|entry| entry == path)
            && index != self.selected
        {
            self.selected = index;
            self.feedback = None;
        }
    }

    pub(super) fn view(&self, id: TabId, run: u64, ctx: &ViewContext<TerminalApp>) -> View {
        let Some(path) = self.selected() else {
            return View::empty();
        };
        let caption = format!("Saved live output to {}", path.display());
        let copy = path.clone();
        let reveal = path.clone();
        let selection = if self.paths.len() > 1 {
            let paths = self.paths.clone();
            let labels: Vec<_> = paths
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            ComboBox::new()
                .automation_name("Saved recordings")
                .max_width(480.0)
                .items_source(labels)
                .selected_index(Some(self.selected))
                .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                    index
                        .and_then(|index| paths.get(index))
                        .map_or(Message::ControlIdle, |path| {
                            message(id, run, Action::Select(path.clone()))
                        })
                }))
                .into()
        } else {
            View::empty()
        };
        let feedback = if self.revealing.as_ref() == Some(path) {
            Some("Opening File Explorer…")
        } else {
            self.feedback.as_deref()
        };
        StackPanel::new().spacing(4.0).children((
            selection,
            TextBlock::new()
                .text(caption)
                .text_wrapping(TextWrapping::Wrap),
            StackPanel::new()
                .spacing(6.0)
                .orientation(Orientation::Horizontal)
                .children((
                    Button::new()
                        .is_enabled(self.revealing.is_none())
                        .on_click(
                            ctx.callback(move |_| message(id, run, Action::Reveal(reveal.clone()))),
                        )
                        .content("Show in File Explorer"),
                    Button::new()
                        .on_click(
                            ctx.callback(move |_| message(id, run, Action::CopyPath(copy.clone()))),
                        )
                        .content("Copy recording path"),
                )),
            feedback.map_or_else(View::empty, |text| {
                TextBlock::new()
                    .text(text)
                    .text_wrapping(TextWrapping::Wrap)
                    .into()
            }),
        ))
    }
}

impl TerminalApp {
    fn viewed_saved_recordings(&mut self, id: TabId, run: u64) -> Option<&mut State> {
        let page = self.host_group_mut(id)?;
        if page.viewing != run {
            return None;
        }
        page.submissions
            .iter_mut()
            .find(|submission| submission.id == run)
            .map(|submission| &mut submission.saved_recordings)
    }

    pub(super) fn handle_saved_recording(
        &mut self,
        id: TabId,
        run: u64,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        match action {
            Action::Select(path) => {
                if let Some(saved) = self.viewed_saved_recordings(id, run) {
                    saved.select(&path);
                }
            }
            Action::CopyPath(path) => {
                let Some(saved) = self
                    .viewed_saved_recordings(id, run)
                    .filter(|saved| saved.selected() == Some(&path))
                else {
                    return;
                };
                let result = path
                    .to_str()
                    .context("This path cannot be represented as text")
                    .and_then(w11term::clipboard::write);
                saved.feedback = Some(match result {
                    Ok(()) => "Copied recording path".into(),
                    Err(error) => format!("Could not copy the recording path · {error:#}"),
                });
            }
            Action::Reveal(path) => {
                let Some(saved) = self
                    .viewed_saved_recordings(id, run)
                    .filter(|saved| saved.selected() == Some(&path) && saved.revealing.is_none())
                else {
                    return;
                };
                saved.feedback = None;
                saved.revealing = Some(path.clone());
                let rejected = message(
                    id,
                    run,
                    Action::Revealed(path.clone(), Err("File Explorer is unavailable".into())),
                );
                ctx.spawn_background_with_rejection(
                    move |cancel| {
                        let result = if cancel.is_cancelled() {
                            Err("The Explorer request was cancelled".into())
                        } else {
                            explorer::reveal_saved_file(&path).map_err(|error| format!("{error:#}"))
                        };
                        message(id, run, Action::Revealed(path, result))
                    },
                    rejected,
                );
            }
            Action::Revealed(path, result) => {
                let Some(saved) = self
                    .output_recording_submission(id, run)
                    .map(|submission| &mut submission.saved_recordings)
                    .filter(|saved| saved.revealing.as_ref() == Some(&path))
                else {
                    return;
                };
                saved.revealing = None;
                if saved.selected() == Some(&path) {
                    saved.feedback = Some(match result {
                        Ok(()) => "Selected in File Explorer".into(),
                        Err(error) => {
                            format!("Could not show the recording in File Explorer · {error}")
                        }
                    });
                }
            }
        }
    }
}
