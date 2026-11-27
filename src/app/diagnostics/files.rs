use super::file_picker::DiagnosticStage;
use super::*;
use anyhow::{Result, bail, ensure};
use std::path::PathBuf;

pub enum Action {
    Browse,
    BrowseComparison,
    Picked(Arc<()>, Result<Option<(Pick, Artifact)>, String>),
    Dropped(DroppedData),
    #[cfg(feature = "ui-smoke")]
    DropFixture(PathBuf),
}

pub enum Pick {
    Open,
    Baseline,
    Candidate(Artifact),
}

fn message(action: Action) -> Message {
    Message::Diagnostics(super::Action::Files(action))
}

fn artifact(paths: Vec<PathBuf>) -> Result<Artifact> {
    ensure!(
        paths.iter().all(|path| path.is_absolute()),
        "Choose saved files with local filesystem paths"
    );
    let kind = match paths.as_slice() {
        [] => bail!("Choose a saved diagnostic file"),
        [path] => {
            let extension = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_ascii_lowercase);
            match extension.as_deref() {
                Some("folded" | "collapsed") => Kind::CpuProfile,
                Some("json") => Kind::TriageReport,
                Some("pcap" | "pcapng" | "cap") => Kind::PacketCapture,
                Some("strace") => Kind::SyscallTrace,
                _ if w11term::syscall_trace::file_pids(&paths)?[0].is_some() => Kind::SyscallTrace,
                _ => bail!(
                    "Choose folded stacks, a Linux triage report, a packet capture, or a syscall trace"
                ),
            }
        }
        _ => {
            w11term::syscall_trace::file_pids(&paths).context(
                "Choose one diagnostic or the matching parts of one split syscall trace",
            )?;
            Kind::SyscallTrace
        }
    };
    Ok(Artifact { kind, paths })
}

fn picked_artifact(paths: Vec<PathBuf>, stage: DiagnosticStage) -> Result<Option<Artifact>> {
    if paths.is_empty() {
        return Ok(None);
    }
    let artifact = artifact(paths)?;
    match stage {
        DiagnosticStage::Open => (),
        DiagnosticStage::Baseline => ensure!(
            artifact.kind != Kind::PacketCapture,
            "Choose a CPU profile, Linux triage report, or syscall trace as the baseline"
        ),
        DiagnosticStage::Candidate(kind) => ensure!(
            artifact.kind == kind,
            "Choose a diagnostic of the same type as the baseline"
        ),
    }
    Ok(Some(artifact))
}

impl TerminalApp {
    pub(super) fn diagnostics_file_actions(&self, ctx: &ViewContext<Self>) -> Vec<KeyedView> {
        let idle = self.diagnostics_recent.file_picker.is_none();
        let open = KeyedView::new(
            "open-diagnostic-file",
            Border::new()
                .background(Color::rgb(45, 45, 45))
                .padding(10.0)
                .drop_policy(idle.then(|| {
                    DragDropPolicy::new().storage_items(
                        DragDropAction::new(DragDropOperation::Copy)
                            .caption("Open saved diagnostic"),
                    )
                }))
                .on_drop(ctx.callback(|data| message(Action::Dropped(data))))
                .content(
                    StackPanel::new().spacing(6.0).children((
                        Button::new()
                            .is_enabled(idle)
                            .on_click(ctx.callback(|_| message(Action::Browse)))
                            .content("Open saved diagnostic…"),
                        TextBlock::new()
                            .text("Drop a saved diagnostic here")
                            .text_wrapping(TextWrapping::Wrap),
                    )),
                ),
        );
        let compare = KeyedView::new(
            "compare-diagnostic-files",
            Button::new()
                .is_enabled(idle)
                .on_click(ctx.callback(|_| message(Action::BrowseComparison)))
                .content("Compare saved diagnostics…"),
        );
        #[cfg(feature = "ui-smoke")]
        if let Some(path) = std::env::var_os("W11TERM_TEST_DIAGNOSTIC_DROP").map(PathBuf::from) {
            return vec![
                open,
                compare,
                KeyedView::new(
                    "test-drop-diagnostic",
                    Button::new()
                        .is_enabled(idle)
                        .on_click(ctx.callback(move |_| message(Action::DropFixture(path.clone()))))
                        .content("Drop fixture diagnostic"),
                ),
            ];
        }
        vec![open, compare]
    }

    pub(super) fn diagnostic_files_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        if !self.diagnostics_open {
            return;
        }
        let selection = match action {
            Action::Browse | Action::BrowseComparison => {
                let pick = if matches!(action, Action::BrowseComparison) {
                    Pick::Baseline
                } else {
                    Pick::Open
                };
                let path = self
                    .diagnostics_recent
                    .entries
                    .first()
                    .map(|row| row.entry.artifact.paths[0].clone())
                    .unwrap_or_else(|| self.store.path().into());
                self.browse_diagnostic(path, pick, ctx);
                return;
            }
            Action::Picked(ticket, result) => {
                if self
                    .diagnostics_recent
                    .file_picker
                    .take_if(|pending| Arc::ptr_eq(pending, &ticket))
                    .is_none()
                {
                    return;
                }
                result
            }
            Action::Dropped(DroppedData::StorageItems(items))
                if self.diagnostics_recent.file_picker.is_none() =>
            {
                let paths = items
                    .into_iter()
                    .map(|item| PathBuf::from(item.path))
                    .collect();
                picked_artifact(paths, DiagnosticStage::Open)
                    .map(|artifact| artifact.map(|artifact| (Pick::Open, artifact)))
                    .map_err(|error| format!("{error:#}"))
            }
            Action::Dropped(_) => return,
            #[cfg(feature = "ui-smoke")]
            Action::DropFixture(path) => {
                let result = std::fs::read(path)
                    .map_err(anyhow::Error::from)
                    .and_then(|bytes| {
                        serde_json::from_slice::<Vec<String>>(&bytes).map_err(Into::into)
                    });
                match result {
                    Ok(paths) => {
                        let items = paths
                            .into_iter()
                            .map(|path| DroppedStorageItem {
                                name: String::new(),
                                path,
                            })
                            .collect();
                        ctx.sender()
                            .send(message(Action::Dropped(DroppedData::StorageItems(items))));
                    }
                    Err(error) => {
                        self.diagnostics_recent.status =
                            Some(format!("Reading diagnostic drop fixture: {error:#}"))
                    }
                }
                return;
            }
        };
        match selection {
            Ok(Some((Pick::Open, artifact))) => {
                if artifact.kind != Kind::SyscallTrace
                    && self.diagnostic_ticket(artifact.kind).is_some()
                {
                    self.diagnostics_recent.status =
                        Some("Wait for the current diagnostics job to finish.".into());
                    return;
                }
                self.diagnostics_recent.status = None;
                self.open_diagnostic_artifact(&artifact, ctx);
            }
            Ok(Some((Pick::Baseline, before))) => {
                self.browse_diagnostic(before.paths[0].clone(), Pick::Candidate(before), ctx);
            }
            Ok(Some((Pick::Candidate(before), after))) => {
                self.diagnostics_recent.status = None;
                self.compare_saved_diagnostics(&before, &after, ctx);
            }
            Ok(None) => (),
            Err(error) => self.diagnostics_recent.status = Some(error),
        }
    }

    fn browse_diagnostic(&mut self, path: PathBuf, pick: Pick, ctx: &ComponentContext<Self>) {
        let Some(owner) = self.keyboard.hwnd().map(|hwnd| hwnd.0 as usize) else {
            return;
        };
        let state = &mut self.diagnostics_recent;
        if state.file_picker.is_some() {
            return;
        }
        let ticket = Arc::new(());
        state.file_picker = Some(ticket.clone());
        state.status = None;
        let rejected = message(Action::Picked(
            ticket.clone(),
            Err("Diagnostic file picker unavailable".into()),
        ));
        ctx.spawn_background_with_rejection(
            move |cancel| {
                let result = if cancel.is_cancelled() {
                    Ok(None)
                } else {
                    let stage = match &pick {
                        Pick::Open => DiagnosticStage::Open,
                        Pick::Baseline => DiagnosticStage::Baseline,
                        Pick::Candidate(before) => DiagnosticStage::Candidate(before.kind),
                    };
                    file_picker::pick_paths(
                        owner,
                        file_picker::Purpose::OpenDiagnostic { path, stage },
                    )
                    .and_then(|paths| picked_artifact(paths, stage))
                    .map(|artifact| artifact.map(|artifact| (pick, artifact)))
                    .map_err(|error| format!("{error:#}"))
                };
                message(Action::Picked(ticket, result))
            },
            rejected,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_file_types_and_validated_split_groups_to_their_viewers() {
        let root = std::path::absolute("saved diagnostics").unwrap();
        for (name, expected) in [
            ("cpu.FOLDED", Kind::CpuProfile),
            ("cpu.collapsed", Kind::CpuProfile),
            ("triage.json", Kind::TriageReport),
            ("network.PCAPNG", Kind::PacketCapture),
            ("network.cap", Kind::PacketCapture),
            ("trace.strace", Kind::SyscallTrace),
            ("run.94", Kind::SyscallTrace),
        ] {
            assert_eq!(artifact(vec![root.join(name)]).unwrap().kind, expected);
        }
        let split = vec![root.join("run.94"), root.join("run.12")];
        let picked = artifact(split.clone()).unwrap();
        assert_eq!(picked.kind, Kind::SyscallTrace);
        assert_eq!(picked.paths, split);

        for invalid in [
            vec![PathBuf::from("relative.folded")],
            vec![PathBuf::new()],
            vec![root.join("run.0")],
            vec![root.join("settings.toml")],
            vec![root.join("run.12"), root.join("other.13")],
            vec![root.join("run.12"), root.join("run.12")],
            vec![root.join("cpu.folded"), root.join("network.pcap")],
        ] {
            assert!(artifact(invalid.clone()).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn comparison_picker_validates_each_run_and_preserves_split_members() {
        let root = std::path::absolute("saved diagnostics").unwrap();
        let baseline = picked_artifact(
            vec![root.join("working.42"), root.join("working.43")],
            DiagnosticStage::Baseline,
        )
        .unwrap()
        .unwrap();
        assert_eq!(baseline.kind, Kind::SyscallTrace);
        let stage = DiagnosticStage::Candidate(baseline.kind);
        let failing = vec![root.join("failing.500"), root.join("failing.501")];
        assert_eq!(
            picked_artifact(failing.clone(), stage)
                .unwrap()
                .unwrap()
                .paths,
            failing
        );
        assert!(picked_artifact(Vec::new(), stage).unwrap().is_none());
        assert!(picked_artifact(vec![root.join("profile.folded")], stage).is_err());
        assert!(
            picked_artifact(vec![root.join("network.pcap")], DiagnosticStage::Baseline).is_err()
        );
    }
}
