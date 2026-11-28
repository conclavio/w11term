use super::*;

pub type ChecksResult = Result<Arc<Vec<ToolArchitectureAudit>>, String>;

struct Job {
    ticket: Arc<()>,
    download: Option<UtilityArchitecture>,
    task: ComponentTask,
}

#[derive(Default)]
pub(super) struct Checks {
    job: Option<Job>,
    result: Option<ChecksResult>,
}

impl Drop for Checks {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.task.cancel();
        }
    }
}

impl Checks {
    pub(super) fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub(super) fn can_prepare(&self, architecture: UtilityArchitecture) -> bool {
        matches!(&self.result, Some(Ok(checks)) if checks.iter().any(|check|
            check.architecture == architecture && can_prepare(check)))
    }

    pub(super) fn start(
        &mut self,
        id: TabId,
        panel: Arc<()>,
        report: Arc<ToolRolloutReport>,
        download: Option<UtilityArchitecture>,
        ctx: &ComponentContext<TerminalApp>,
    ) {
        let ticket = Arc::new(());
        let request = ticket.clone();
        let rejected = message(
            id,
            Action::CandidateChecked(
                panel.clone(),
                ticket.clone(),
                Err("Candidate tool check unavailable".into()),
            ),
        );
        let task = ctx.spawn_background_with_rejection(
            move |cancel| {
                let cancelled = || cancel.is_cancelled();
                let result = match download {
                    Some(architecture) => {
                        report.prepare_candidate_downloads(architecture, cancelled)
                    }
                    None => report.verify_candidate_builds(cancelled),
                }
                .map(Arc::new)
                .map_err(|error| format!("{error:#}"));
                message(id, Action::CandidateChecked(panel, request, result))
            },
            rejected,
        );
        self.job = Some(Job {
            ticket,
            download,
            task,
        });
    }

    pub(super) fn finish(&mut self, ticket: &Arc<()>, result: ChecksResult) {
        if matches!(&self.job, Some(job) if Arc::ptr_eq(&job.ticket, ticket)) {
            self.job = None;
            self.result = Some(result);
        }
    }

    pub(super) fn describe(
        &self,
        fields: &mut Vec<KeyedView>,
        ticket: &Arc<()>,
        send: impl Fn(Action) -> Callback<()>,
    ) {
        if let Some(job) = &self.job {
            let status = match job.download {
                Some(architecture) => format!(
                    "Preparing candidate {} downloads…",
                    profile_tools::architecture_label(architecture)
                ),
                None => "Checking candidate builds on Windows…".into(),
            };
            fields.push(note("candidate-check-progress", status));
            return;
        }
        match &self.result {
            Some(Ok(checks)) => {
                for check in checks.iter() {
                    let architecture = check.architecture;
                    let label = profile_tools::architecture_label(architecture);
                    let ready = check
                        .commands
                        .iter()
                        .filter(|command| command.locally_available)
                        .count();
                    fields.push(note(
                        format!("candidate-readiness-{label}"),
                        format!(
                            "Candidate {label} · {ready} of {} commands ready locally",
                            check.commands.len()
                        ),
                    ));
                    fields.push(code_details(
                        format!("candidate-build-details-{label}"),
                        &format!("Candidate {label} build details"),
                        &build_details(check),
                    ));
                    if can_prepare(check) {
                        fields.push(KeyedView::new(
                            format!("candidate-prepare-{label}"),
                            Button::new()
                                .on_click(send(Action::PrepareCandidate(
                                    ticket.clone(),
                                    architecture,
                                )))
                                .content(format!("Prepare candidate {label} downloads")),
                        ));
                    }
                }
            }
            Some(Err(error)) => fields.push(note("candidate-check-error", error)),
            None => {}
        }
        fields.push(KeyedView::new(
            "check-candidate",
            Button::new()
                .on_click(send(Action::CheckCandidate(ticket.clone())))
                .content("Check candidate builds"),
        ));
    }
}

fn can_prepare(check: &ToolArchitectureAudit) -> bool {
    check
        .commands
        .iter()
        .flat_map(|command| &command.builds)
        .any(|build| {
            matches!(build.source, ToolExecutableSource::DownloadCache)
                && build.status != ToolExecutableStatus::Verified
        })
}
