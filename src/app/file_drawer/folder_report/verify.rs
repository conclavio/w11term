use super::*;
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};
use w11term::sftp::{FILE_VERIFICATION_LIMIT, VerifiedFolderFile, valid_comparison_file_path};

mod batch;

#[derive(Clone)]
pub enum Action {
    Open,
    Row {
        revision: u64,
        row: comparison::Row,
    },
    Page {
        revision: u64,
        viewport_revision: u64,
    },
    Path(String),
    Start,
    StartPage,
    Stop,
    Back,
}

#[derive(Default)]
pub(super) struct Verification {
    pub(super) open: bool,
    path: String,
    result: Option<Result<VerifiedFolderFile, String>>,
    cancel: Option<TransferCancel>,
    batch: Option<batch::Batch>,
    pub(super) checks: Arc<Checks>,
}

pub(super) type Checks = BTreeMap<String, CompletedCheck>;

#[derive(Clone)]
pub(super) struct CompletedCheck {
    pub(super) completed_at_unix_seconds: Option<u64>,
    pub(super) digests: VerifiedFolderFile,
}

impl CompletedCheck {
    pub(super) fn label(&self) -> &'static str {
        if self.digests.contents_match() {
            "Last SHA-256 check found matching contents"
        } else {
            "Last SHA-256 check found different contents"
        }
    }
}

impl Drop for Verification {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
    }
}

impl Verification {
    pub(super) fn dialog(
        &self,
        context: &ViewContext<TerminalApp>,
        message: impl Fn(super::Action) -> Message + Copy + 'static,
    ) -> View {
        if let Some(batch) = &self.batch {
            let stopping = self
                .cancel
                .as_ref()
                .is_some_and(TransferCancel::is_cancelled);
            return batch.dialog(context, message, stopping);
        }
        let event = move |action| message(super::Action::Verify(action));
        let busy = self.cancel.is_some();
        let button = |label: &'static str, enabled, action: Action| {
            Button::new()
                .is_enabled(enabled)
                .on_click(context.callback(move |_| event(action.clone())))
                .content(label)
        };
        let mut fields = vec![
            KeyedView::new(
                "scope",
                TextBlock::new()
                    .text(format!(
                        "Compares the live files with SHA-256. Each file can be up to {} MiB.",
                        FILE_VERIFICATION_LIMIT / (1024 * 1024),
                    ))
                    .text_wrapping(TextWrapping::Wrap),
            ),
            KeyedView::new(
                "path",
                TextBox::new()
                    .automation_name("Relative file path")
                    .placeholder_text("Relative file path")
                    .text(&self.path)
                    .is_enabled(!busy)
                    .on_text_changed(context.callback(move |path| event(Action::Path(path)))),
            ),
            KeyedView::new(
                "verify",
                button(
                    "Compare SHA-256",
                    !busy && !self.path.is_empty(),
                    Action::Start,
                ),
            ),
        ];
        if let Some(cancel) = &self.cancel {
            fields.push(KeyedView::new(
                "progress",
                TextBlock::new().text(if cancel.is_cancelled() {
                    "Stopping file comparison…"
                } else {
                    "Calculating SHA-256…"
                }),
            ));
            fields.push(KeyedView::new(
                "stop",
                button("Stop verification", !cancel.is_cancelled(), Action::Stop),
            ));
        }
        match &self.result {
            Some(Ok(result)) => {
                let status = if result.contents_match() {
                    "File contents match"
                } else {
                    "File contents differ"
                };
                fields.push(KeyedView::new("result", TextBlock::new().text(status)));
                for (key, label, digest) in [
                    (
                        "reference-digest",
                        "Reference SHA-256",
                        &result.reference_sha256,
                    ),
                    ("current-digest", "Current SHA-256", &result.current_sha256),
                ] {
                    fields.push(KeyedView::new(
                        key,
                        TextBlock::new()
                            .text(format!("{label}  {digest}"))
                            .text_wrapping(TextWrapping::Wrap),
                    ));
                }
            }
            Some(Err(error)) => fields.push(KeyedView::new(
                "error",
                TextBlock::new()
                    .text(error)
                    .text_wrapping(TextWrapping::Wrap),
            )),
            None => {}
        }
        fields.push(KeyedView::new(
            "back",
            button("Back to comparison", true, Action::Back),
        ));
        ContentDialog::new()
            .title("Verify file contents")
            .close_button_text("Close folder comparison")
            .is_open(true)
            .on_closed(context.callback(move |_| message(super::Action::Close)))
            .content(
                StackPanel::new()
                    .min_width(290.0)
                    .spacing(8.0)
                    .keyed_children(fields),
            )
    }

    fn next_batch_operation(&mut self, report: &FolderComparison) -> Option<Operation> {
        let path = self.batch.as_ref()?.current()?;
        self.cancel = Some(TransferCancel::default());
        Some(compared_operation(report, path))
    }

    fn record(&mut self, path: String, digests: &VerifiedFolderFile) {
        Arc::make_mut(&mut self.checks).insert(
            path,
            CompletedCheck {
                completed_at_unix_seconds: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|elapsed| elapsed.as_secs()),
                digests: digests.clone(),
            },
        );
    }
}

fn compared_operation(report: &FolderComparison, path: &str) -> Operation {
    Operation::VerifyComparedFile {
        reference_root: report.reference_root.clone(),
        reference_origin: report.reference_origin,
        current_root: report.current_root.clone(),
        relative: path.into(),
    }
}

impl FileDrawer {
    pub(in crate::app::file_drawer) fn comparison_verification_cancel(
        &self,
    ) -> Option<TransferCancel> {
        self.folder_report.as_ref()?.verification.cancel.clone()
    }

    pub(in crate::app::file_drawer) fn comparison_verification_action(
        &mut self,
        serial: u64,
        action: Action,
    ) -> Option<Operation> {
        let panel = self
            .folder_report
            .as_mut()
            .filter(|panel| panel.serial == serial)?;
        let super::State::Loaded(Ok(Report::Comparison(report))) = &panel.state else {
            return None;
        };
        let view = &mut panel.verification;
        let action = match action {
            Action::Open => {
                if self.pending.is_none() && !panel.reveal.checking() {
                    view.open = true;
                }
                return None;
            }
            Action::Row { revision, row } => {
                if panel.revision != revision
                    || self.pending.is_some()
                    || panel.reveal.checking()
                    || view.cancel.is_some()
                {
                    return None;
                }
                let row = row.get(report).filter(|row| row.can_verify())?;
                view.path = row.path.into();
                view.open = true;
                Action::Start
            }
            Action::Page {
                revision,
                viewport_revision,
            } => {
                if panel.revision == revision
                    && self.pending.is_none()
                    && !panel.reveal.checking()
                    && !view.open
                {
                    let paths = panel
                        .comparison
                        .verification_paths(report, viewport_revision);
                    if !paths.is_empty() {
                        view.batch = Some(batch::Batch::new(paths));
                        view.open = true;
                    }
                }
                return None;
            }
            action => action,
        };
        if !view.open {
            return None;
        }
        match action {
            Action::Path(path) if view.cancel.is_none() && view.path != path => {
                view.path = path;
                view.result = None;
            }
            Action::Stop => {
                if let Some(cancel) = &view.cancel {
                    cancel.cancel();
                }
            }
            Action::Back => {
                if let Some(cancel) = view.cancel.take() {
                    cancel.cancel();
                }
                view.open = false;
                view.batch = None;
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|job| matches!(job.step, Step::VerifyCompared(id) if id == serial))
                {
                    self.pending.take();
                }
            }
            Action::Start
                if view.batch.is_none()
                    && view.cancel.is_none()
                    && self.pending.is_none()
                    && self.open
                    && self.directory.as_ref() == Some(&report.current_root) =>
            {
                let reference = self.folder_reference.as_ref().filter(|reference| {
                    reference.root == report.reference_root
                        && reference.origin == report.reference_origin
                })?;
                if !valid_comparison_file_path(&view.path)
                    || !reference
                        .entries
                        .get(&view.path)
                        .is_some_and(|entry| entry.is_file())
                {
                    view.result = Some(Err(
                        "Enter the exact relative path of a regular file in the reference.".into(),
                    ));
                    return None;
                }
                view.result = None;
                view.cancel = Some(TransferCancel::default());
                return Some(compared_operation(report, &view.path));
            }
            Action::StartPage
                if view.cancel.is_none()
                    && self.pending.is_none()
                    && self.open
                    && self.directory.as_ref() == Some(&report.current_root) =>
            {
                view.batch.as_mut()?.start();
                return view.next_batch_operation(report);
            }
            _ => {}
        }
        None
    }

    pub(in crate::app::file_drawer) fn finish_compared_verification(
        &mut self,
        serial: u64,
        reply: Result<Value, String>,
    ) -> Option<Operation> {
        let panel = self
            .folder_report
            .as_mut()
            .filter(|panel| panel.serial == serial)?;
        let super::State::Loaded(Ok(Report::Comparison(report))) = &panel.state else {
            return None;
        };
        let view = &mut panel.verification;
        let cancel = view.cancel.take()?;
        let stopped = cancel.is_cancelled();
        cancel.cancel();
        let result = if stopped {
            Err("File comparison cancelled".into())
        } else {
            reply
                .and_then(|value| {
                    serde_json::from_value::<VerifiedFolderFile>(value)
                        .map_err(|error| error.to_string())
                })
                .map_err(|error| format!("Could not compare file contents. {error}"))
        };
        if let Some(batch) = &mut view.batch {
            if stopped {
                batch.stop();
                return None;
            }
            let path = batch.current()?.to_owned();
            batch.finish_file(result.as_ref().map_err(String::as_str));
            if let Ok(digests) = &result {
                view.record(path, digests);
            }
            return view.next_batch_operation(report);
        }
        if let Ok(digests) = &result {
            view.record(view.path.clone(), digests);
        }
        view.result = Some(result);
        None
    }

    pub(in crate::app::file_drawer) fn queue_compared_verification(
        &mut self,
        pane: u64,
        session: &Session,
        serial: u64,
        operation: Operation,
        context: &ComponentContext<TerminalApp>,
    ) {
        if let Err(error) = self.job(
            pane,
            session,
            operation,
            Step::VerifyCompared(serial),
            context,
        ) && let Some(panel) = self
            .folder_report
            .as_mut()
            .filter(|panel| panel.serial == serial)
        {
            let view = &mut panel.verification;
            if let Some(cancel) = view.cancel.take() {
                cancel.cancel();
            }
            let error = format!("Could not start file verification. {error:#}");
            if let Some(batch) = &mut view.batch {
                batch.finish_file(Err(&error));
                batch.stop();
            } else {
                view.result = Some(Err(error));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn page_verification_advances_after_errors_and_stops_without_losing_results() {
        let mut drawer = FileDrawer {
            open: true,
            generation: 7,
            directory: Some("/live".into()),
            ..Default::default()
        };
        let serial = drawer.begin_folder_report(Kind::Comparison, "/live".into());
        let panel = drawer.folder_report.as_mut().unwrap();
        panel.state =
            super::super::State::Loaded(Ok(Report::Comparison(Arc::new(FolderComparison {
                current_root: "/live".into(),
                ..Default::default()
            }))));
        panel.verification.open = true;
        panel.verification.batch = Some(batch::Batch::new(
            ["a.txt", "b.txt", "c.txt", "d.txt"]
                .map(String::from)
                .to_vec(),
        ));
        assert!(matches!(
            drawer.comparison_verification_action(serial, Action::StartPage),
            Some(Operation::VerifyComparedFile { relative, .. }) if relative == "a.txt"
        ));
        let result = json!({"reference_sha256":"a", "current_sha256":"b"});
        assert!(matches!(
            drawer.finish_compared_verification(serial, Ok(result.clone())),
            Some(Operation::VerifyComparedFile { relative, .. }) if relative == "b.txt"
        ));
        assert!(matches!(
            drawer.finish_compared_verification(serial, Err("Missing file".into())),
            Some(Operation::VerifyComparedFile { relative, .. }) if relative == "c.txt"
        ));
        drawer.comparison_verification_action(serial, Action::Stop);
        assert!(
            drawer
                .finish_compared_verification(serial, Ok(result))
                .is_none()
        );
        let verification = &drawer.folder_report.as_ref().unwrap().verification;
        assert_eq!(verification.checks.len(), 1);
        assert!(verification.checks.contains_key("a.txt"));
        assert!(verification.batch.as_ref().unwrap().current().is_none());
    }

    #[test]
    fn only_completed_checks_replace_results_and_refresh_starts_a_new_history() {
        let reference = Arc::new(FolderInventory {
            root: "/reference".into(),
            origin: FolderOrigin::Remote,
            entries: Default::default(),
        });
        let mut drawer = FileDrawer {
            open: true,
            generation: 7,
            directory: Some("/live".into()),
            folder_reference: Some(reference),
            ..Default::default()
        };
        let serial = drawer.begin_folder_report(Kind::Comparison, "/live".into());
        let panel = drawer.folder_report.as_mut().unwrap();
        panel.state = super::super::State::Loaded(Ok(Report::Comparison(Arc::new(
            FolderComparison::default(),
        ))));
        panel.verification.path = "test.txt".into();
        panel.verification.cancel = Some(TransferCancel::default());
        let first = json!({"reference_sha256":"a", "current_sha256":"b"});
        let second = json!({"reference_sha256":"a", "current_sha256":"a"});
        drawer.finish_compared_verification(serial, Ok(first));
        let saved = drawer
            .folder_report
            .as_ref()
            .unwrap()
            .verification
            .checks
            .clone();
        assert!(!saved["test.txt"].digests.contents_match());

        let cancel = TransferCancel::default();
        cancel.cancel();
        drawer.folder_report.as_mut().unwrap().verification.cancel = Some(cancel);
        drawer.finish_compared_verification(serial, Ok(second.clone()));
        let active = &drawer.folder_report.as_ref().unwrap().verification.checks;
        assert!(Arc::ptr_eq(active, &saved));

        drawer.folder_report.as_mut().unwrap().verification.cancel =
            Some(TransferCancel::default());
        drawer.finish_compared_verification(serial, Ok(second));
        assert!(
            drawer.folder_report.as_ref().unwrap().verification.checks["test.txt"]
                .digests
                .contents_match()
        );
        assert!(!saved["test.txt"].digests.contents_match());
        assert!(drawer.refresh_folder_report(serial).is_some());
        assert!(
            drawer
                .folder_report
                .as_ref()
                .unwrap()
                .verification
                .checks
                .is_empty()
        );
    }
}
