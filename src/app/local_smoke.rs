use super::split_smoke::split_control;
use super::*;
use anyhow::{Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::path::Path;
use w11term::control::Request;

async fn raw(pane: u64, bytes: &[u8]) -> Result<()> {
    split_control(Request::SendText {
        pane,
        data_base64: STANDARD.encode(bytes),
        no_paste: true,
    })
    .await?;
    Ok(())
}

async fn seen(pane: u64, marker: &str) -> Result<()> {
    split_control(Request::WaitForText {
        pane,
        pattern: marker.into(),
        regex: false,
        timeout_seconds: 15,
    })
    .await?;
    Ok(())
}

async fn capture(pane: u64, path: std::path::PathBuf) -> Result<()> {
    split_control(Request::Screenshot { pane, path }).await?;
    Ok(())
}

impl TerminalApp {
    pub(super) fn local_smoke_step(
        &mut self,
        step: u8,
        directory: &Path,
        context: &ComponentContext<Self>,
    ) {
        let remote = self.tabs.get(1).unwrap().pane_id().unwrap();
        let dir = directory.to_owned();
        let next = match step {
            0 => {
                let Page::Terminal(tab) = &self.tabs.get(1).unwrap().page else {
                    unreachable!()
                };
                if tab.session.ssh_connection().is_none() {
                    0
                } else {
                    context.spawn_background(move |_| run_step(1, async {
                        let pixels = STANDARD.encode([255u8, 0, 0].repeat(16 * 16));
                        let fixture = format!("\x1b[2J\x1b[H\x1b_Ga=T,f=24,s=16,v=16,i=1,q=2;{pixels}\x1b\\\x1b[5;1Hdirect-marker");
                        raw(remote, fixture.as_bytes()).await?;
                        seen(remote, "direct-marker").await?;
                        capture(remote, dir.join("direct.png")).await
                    }));
                    return;
                }
            }
            1 => {
                let mut shell = self
                    .local_profiles
                    .get("Windows PowerShell")
                    .unwrap()
                    .clone();
                shell.args = vec!["-NoLogo".into(), "-NoProfile".into()];
                shell.cwd = Some(directory.to_owned());
                std::fs::write(directory.join("fixture.txt"), "local-cwd-verified").unwrap();
                self.store
                    .save_local(None, "Fixture PowerShell", &shell)
                    .unwrap();
                self.local_profiles = self.store.catalog().unwrap().local;
                self.update(
                    tab_menu_message("Split right with Fixture PowerShell".into()),
                    context,
                );
                2
            }
            2 => {
                let id = self.tabs.active_id().unwrap();
                let Page::Terminal(local) = &self.tabs.get(id).unwrap().page else {
                    unreachable!()
                };
                assert!(matches!(local.source, Source::Local(_)));
                let pane = local.session.pane_id;
                if local.session.frames.load().connection_generation == 0 {
                    2
                } else {
                    context.spawn_background(move |_| run_step(3, async {
                            raw(pane, b"[IO.File]::WriteAllText((Join-Path $PWD 'previous-pid'),[string]$PID); Get-Content fixture.txt\r").await?;
                        seen(pane,"local-cwd-verified").await?;
                        raw(pane, b"1..20 | ForEach-Object { Write-Output $_; Start-Sleep -Milliseconds 30 }\r").await?;
                        raw(remote,b"\x1b[8;1Hbackground-ssh").await?;
                        seen(remote,"background-ssh").await?;
                        capture(remote,dir.join("mixed.png")).await?;
                        let listing = split_control(Request::List).await?;
                        ensure!(listing["sessions"].as_array().unwrap().iter().any(|s| s["active"] == true && s["id"] == pane), "local shell lost focus");
                        Ok(())
                    }));
                    return;
                }
            }
            3 => {
                let local = self.tabs.active_id().unwrap();
                use super::workspace_view::WorkspaceAction;
                self.workspace_action(WorkspaceAction::Show, context);
                let manager = self.tabs.active_id().unwrap();
                self.workspace_action(WorkspaceAction::Name(manager, "Mixed test".into()), context);
                self.workspace_action(WorkspaceAction::Save(manager), context);
                self.tabs.select(local);
                self.activate(context);
                self.update(Message::ClosePane, context);
                context.spawn_background(move |_| {
                    run_step(4, async {
                        raw(remote, b"\r\nafter-local-close").await?;
                        seen(remote, "after-local-close").await
                    })
                });
                return;
            }
            4 => {
                use super::workspace_view::WorkspaceAction;
                self.workspace_action(WorkspaceAction::Show, context);
                self.workspace_action(
                    WorkspaceAction::Open(self.tabs.active_id().unwrap(), "Mixed test".into()),
                    context,
                );
                5
            }
            5 => {
                let Some(AppTab {
                    page: Page::Terminal(local),
                    ..
                }) = self.tabs.active()
                else {
                    panic!("Restored focus did not return to the local shell");
                };
                assert!(matches!(local.source, Source::Local(_)));
                let pane = local.session.pane_id;
                if local.session.frames.load().connection_generation == 0 {
                    5
                } else {
                    context.spawn_background(move |_| run_step(6, async {
                        raw(pane, b"if ([int](gc previous-pid) -ne $PID) { 'restored-' + (gc fixture.txt) }\r").await?;
                        seen(pane, "restored-local-cwd-verified").await
                    }));
                    return;
                }
            }
            6 => {
                eprintln!("Mixed SSH and local shell smoke passed");
                self.update(Message::CloseWindow, context);
                return;
            }
            _ => panic!("Mixed local/SSH control smoke failed"),
        };
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(120));
            Message::Smoke(next)
        });
    }
}

fn run_step(next: u8, work: impl Future<Output = Result<()>>) -> Message {
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(work)
    {
        Ok(()) => Message::Smoke(next),
        Err(e) => {
            eprintln!("Mixed shell smoke: {e:#}");
            Message::Smoke(u8::MAX)
        }
    }
}
