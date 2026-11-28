use super::*;
use w11term::profiles::{ToolRestoreStatus, ToolRollbackProfile};

#[derive(Clone)]
pub(super) enum Source {
    Applied(Arc<ToolRolloutReport>),
    Receipt(PathBuf),
}

pub(super) enum Stage {
    Ready,
    Reviewing,
    Reviewed(Arc<ToolRollbackReport>),
    Applying,
}

pub(super) fn receipt_picker(current: PathBuf, save: bool) -> file_picker::Purpose {
    file_picker::Purpose::RolloutReceipt {
        current,
        kind: file_picker::RolloutReceiptKind::Tools,
        save,
    }
}

impl Panel {
    pub(super) fn request_rollback(
        &mut self,
        id: TabId,
        catalog: PathBuf,
        source: Source,
        review: Option<String>,
        ctx: &ComponentContext<TerminalApp>,
    ) {
        let stage = if review.is_some() {
            Stage::Applying
        } else {
            Stage::Reviewing
        };
        self.work = Work::Rollback(source.clone(), stage);
        self.error = None;
        let ticket = self.ticket.clone();
        let rejected = message(
            id,
            Action::RollbackCompleted(ticket.clone(), Err("SSH tool rollback unavailable".into())),
        );
        self.task = Some(ctx.spawn_background_with_rejection(
            move |_| {
                let review = review.as_deref();
                let report = match source {
                    Source::Applied(report) => report.rollback(review),
                    Source::Receipt(path) => ProfileStore::new(catalog)
                        .and_then(|store| store.rollback_tool_receipt(&path, review)),
                };
                let result = report.map(Arc::new).map_err(|error| format!("{error:#}"));
                message(id, Action::RollbackCompleted(ticket, result))
            },
            rejected,
        ));
    }

    pub(super) fn save_receipt(
        &mut self,
        id: TabId,
        owner: usize,
        report: Arc<ToolRolloutReport>,
        ctx: &ComponentContext<TerminalApp>,
    ) {
        self.work = Work::SavingReceipt(report.clone());
        self.error = None;
        let ticket = self.ticket.clone();
        let rejected = message(
            id,
            Action::ReceiptSaved(
                ticket.clone(),
                Err("Tool rollout receipt save unavailable".into()),
            ),
        );
        self.task = Some(ctx.spawn_background_with_rejection(
            move |cancel| {
                let result = (|| -> anyhow::Result<Option<PathBuf>> {
                    let purpose = receipt_picker(report.candidate_path.clone(), true);
                    let Some(file) = file_picker::pick(owner, purpose)? else {
                        return Ok(None);
                    };
                    if cancel.is_cancelled() {
                        return Ok(None);
                    }
                    report.save_rollback_receipt(&file.path)?;
                    Ok(Some(file.path))
                })()
                .map_err(|error| format!("{error:#}"));
                message(id, Action::ReceiptSaved(ticket, result))
            },
            rejected,
        ));
    }
}

pub(super) fn describe(
    fields: &mut Vec<KeyedView>,
    source: &Source,
    stage: &Stage,
    ticket: &Arc<()>,
    send: impl Fn(Action) -> Callback<()>,
) {
    if let Source::Receipt(path) = source {
        fields.push(note(
            "receipt",
            format!("Rollout receipt · {}", path.display()),
        ));
    }
    match stage {
        Stage::Ready => fields.push(KeyedView::new(
            "review-rollback",
            Button::new()
                .on_click(send(Action::ReviewRollback(ticket.clone())))
                .content("Review tool rollback"),
        )),
        Stage::Reviewing => fields.push(note("status", "Checking the saved tool approvals…")),
        Stage::Applying => fields.push(note("status", "Restoring the reviewed tool approvals…")),
        Stage::Reviewed(report) => {
            fields.push(note(
                "status",
                format!(
                    "Restore saved approvals for {} SSH profiles. They take effect on the next SSH connection.",
                    report.changed_profiles
                ),
            ));
            fields.extend(report.profiles.iter().map(restore_row));
            fields.push(KeyedView::new(
                "apply-rollback",
                Button::new()
                    .on_click(send(Action::ApplyRollback(ticket.clone())))
                    .content("Apply reviewed tool rollback"),
            ));
        }
    }
    if matches!(source, Source::Applied(_)) && matches!(stage, Stage::Ready | Stage::Reviewed(_)) {
        fields.push(KeyedView::new(
            "back-to-rollout",
            Button::new()
                .on_click(send(Action::Back(ticket.clone())))
                .content("Back to rollout result"),
        ));
    }
}

fn restore_row(entry: &ToolRollbackProfile) -> KeyedView {
    let current = &entry.current;
    let address = host_groups::ssh_address(&current.user, &current.host, current.port);
    let restore_status = match entry.restore_status {
        ToolRestoreStatus::ManifestUnavailable => "Previous manifest is unavailable",
        ToolRestoreStatus::FollowsFile => "Follows the previous manifest file",
        ToolRestoreStatus::MatchesPin => "Previous manifest matches its pin",
        ToolRestoreStatus::PinMismatch => "Previous manifest differs from its pin",
        ToolRestoreStatus::ScopeMismatch => {
            "Previous manifest destination differs from its approval"
        }
    };
    let mut fields = vec![
        note("name", format!("{} · {address}", entry.profile)),
        note(
            "current",
            format!("Current manifest · {}", current.bundle.display()),
        ),
        note(
            "restored",
            format!("Restore manifest · {}", entry.restore.bundle.display()),
        ),
        note("restore-status", restore_status),
    ];
    if let Some(pin) = &entry.restore.manifest_sha256 {
        fields.push(note("pin", format!("Restore pin SHA-256 {pin}")));
    }
    if let Some(digest) = &entry.current_manifest_sha256 {
        fields.push(note(
            "current-digest",
            format!("Current manifest SHA-256 {digest}"),
        ));
    }
    if let Some(digest) = &entry.restored_manifest_sha256 {
        fields.push(note(
            "restore-digest",
            format!("Previous manifest SHA-256 {digest}"),
        ));
    }
    KeyedView::new(
        format!("restore-{}", entry.profile),
        StackPanel::new().spacing(4.0).keyed_children(fields),
    )
}
