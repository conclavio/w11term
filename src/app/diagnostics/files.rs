use super::*;
use anyhow::{Result, bail, ensure};
use std::path::PathBuf;

pub enum Action {
    Browse,
    Picked(Arc<()>, Result<Vec<PathBuf>, String>),
    Dropped(DroppedData),
    #[cfg(feature = "ui-smoke")]
    DropFixture(PathBuf),
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
        #[cfg(feature = "ui-smoke")]
        if let Some(path) = std::env::var_os("W11TERM_TEST_DIAGNOSTIC_DROP").map(PathBuf::from) {
            return vec![
                open,
                KeyedView::new(
                    "test-drop-diagnostic",
                    Button::new()
                        .is_enabled(idle)
                        .on_click(ctx.callback(move |_| message(Action::DropFixture(path.clone()))))
                        .content("Drop fixture diagnostic"),
                ),
            ];
        }
        vec![open]
    }

    pub(super) fn diagnostic_files_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        if !self.diagnostics_open {
            return;
        }
        let paths = match action {
            Action::Browse => {
                let Some(owner) = self.keyboard.hwnd().map(|hwnd| hwnd.0 as usize) else {
                    return;
                };
                let state = &mut self.diagnostics_recent;
                if state.file_picker.is_some() {
                    return;
                }
                let path = state
                    .entries
                    .first()
                    .map(|row| row.entry.artifact.paths[0].clone())
                    .unwrap_or_else(|| self.store.path().into());
                let ticket = Arc::new(());
                state.file_picker = Some(ticket.clone());
                state.status = None;
                let rejected = message(Action::Picked(
                    ticket.clone(),
                    Err("Diagnostic file picker unavailable".into()),
                ));
                ctx.spawn_background_with_rejection(
                    move |_| {
                        let outcome = file_picker::pick_paths(
                            owner,
                            file_picker::Purpose::OpenDiagnostic(path),
                        )
                        .map_err(|error| format!("{error:#}"));
                        message(Action::Picked(ticket, outcome))
                    },
                    rejected,
                );
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
                match result {
                    Ok(paths) => paths,
                    Err(error) => {
                        self.diagnostics_recent.status = Some(error);
                        return;
                    }
                }
            }
            Action::Dropped(DroppedData::StorageItems(items))
                if self.diagnostics_recent.file_picker.is_none() =>
            {
                items
                    .into_iter()
                    .map(|item| PathBuf::from(item.path))
                    .collect()
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
        if paths.is_empty() {
            return;
        }
        match artifact(paths) {
            Ok(artifact) => {
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
            Err(error) => self.diagnostics_recent.status = Some(format!("{error:#}")),
        }
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
}
