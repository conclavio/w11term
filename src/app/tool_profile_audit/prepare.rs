use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct Work {
    pub(super) ticket: Arc<()>,
    pub(super) profile: String,
    stop: Arc<AtomicBool>,
    task: ComponentTask,
}

impl Work {
    pub(super) fn start(
        id: TabId,
        catalog: PathBuf,
        entry: &ToolProfileAuditRow,
        digest: String,
        architecture: Option<UtilityArchitecture>,
        ctx: &ComponentContext<TerminalApp>,
    ) -> Self {
        let ticket = Arc::new(());
        let request = ticket.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_requested = stop.clone();
        let profile = entry.profile.clone();
        let name = profile.clone();
        let rejected = Message::ToolProfileAudit(
            id,
            Action::Prepared(
                ticket.clone(),
                Err("SSH tool preparation unavailable".into()),
            ),
        );
        let task = ctx.spawn_background_with_rejection(
            move |cancel| {
                let result = ProfileStore::new(catalog)
                    .and_then(|store| {
                        store.prepare_tool_profile(&name, &digest, architecture, || {
                            cancel.is_cancelled() || stop_requested.load(Ordering::Relaxed)
                        })
                    })
                    .map_err(|error| format!("{error:#}"));
                Message::ToolProfileAudit(id, Action::Prepared(request, result))
            },
            rejected,
        );
        Self {
            ticket,
            profile,
            stop,
            task,
        }
    }

    pub(super) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub(super) fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        self.task.cancel();
    }
}

pub(super) fn has_downloads(entry: &ToolProfileAuditRow) -> bool {
    entry
        .build_checks
        .iter()
        .flatten()
        .flat_map(|check| &check.commands)
        .flat_map(|command| &command.builds)
        .any(|build| matches!(build.source, ToolExecutableSource::DownloadCache))
}
