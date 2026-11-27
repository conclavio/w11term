use super::{Registry, TIMEOUT, UiRequest, WindowId};
use crate::workspace::{SavedLayout, SavedSource, Workspace, WorkspaceStore};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use tokio::sync::oneshot;

#[derive(Serialize)]
struct WorkspacePreview<'a> {
    name: &'a str,
    active_tab: usize,
    tabs: Vec<Value>,
    required_ssh_profiles: BTreeSet<&'a str>,
    profile_check: Option<ProfileCheck>,
}

#[derive(Serialize)]
struct ProfileCheck {
    window_id: WindowId,
    missing_ssh_profiles: Vec<String>,
}

impl Registry {
    pub(super) async fn get_workspace(
        &self,
        name: &str,
        window_id: Option<WindowId>,
    ) -> Result<Value> {
        let layout = WorkspaceStore::open()?.entry(name)?.layout;
        let mut preview = WorkspacePreview::new(name, &layout);
        if let Some(window_id) = window_id {
            let ui = self.window_ui(window_id)?;
            let (reply, response) = oneshot::channel();
            ui(UiRequest::CheckWorkspaceProfiles {
                names: preview
                    .required_ssh_profiles
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect(),
                reply,
            });
            let missing_ssh_profiles = tokio::time::timeout(TIMEOUT, response)
                .await
                .context("checking workspace profile availability timed out")?
                .context("GUI is closed")?
                .map_err(anyhow::Error::msg)?;
            preview.profile_check = Some(ProfileCheck {
                window_id,
                missing_ssh_profiles,
            });
        }
        Ok(serde_json::to_value(preview)?)
    }
}

impl<'a> WorkspacePreview<'a> {
    fn new(name: &'a str, workspace: &'a Workspace) -> Self {
        let mut required_ssh_profiles = BTreeSet::new();
        let tabs = workspace
            .tabs
            .iter()
            .map(|tab| {
                json!({
                    "title": tab.title,
                    "color_override": tab.tab_color,
                    "pinned": tab.pinned,
                    "focused_slot": tab.focused,
                    "layout": preview_layout(&tab.layout, &mut required_ssh_profiles),
                })
            })
            .collect();
        Self {
            name,
            active_tab: workspace.active_tab,
            tabs,
            required_ssh_profiles,
            profile_check: None,
        }
    }
}

fn preview_layout<'a>(root: &'a SavedLayout, profiles: &mut BTreeSet<&'a str>) -> Vec<Value> {
    let mut pending = vec![root];
    let mut nodes = Vec::new();
    while let Some(&node) = pending.get(nodes.len()) {
        nodes.push(match node {
            SavedLayout::Leaf(pane) => {
                profiles.extend(pane.source.ssh_profile());
                // Saved source recipes contain authentication options, paths and
                // startup commands. Only expose the fields listed here.
                json!({
                    "kind": "pane",
                    "slot": pane.slot,
                    "pane_name": pane.pane_name,
                    "font_size": pane.font_size,
                    "source": {
                        "kind": source_kind(&pane.source),
                        "profile": pane.source.profile_name(),
                        "startup": matches!(pane.source, SavedSource::Startup { .. }),
                        "durable": pane.source.session().is_some(),
                    },
                })
            }
            SavedLayout::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let first_index = pending.len();
                pending.extend([first.as_ref(), second.as_ref()]);
                json!({
                    "kind": "split", "axis": axis, "ratio": ratio,
                    "first": first_index, "second": first_index + 1,
                })
            }
        });
    }
    nodes
}

fn source_kind(source: &SavedSource) -> &'static str {
    match source {
        SavedSource::SavedProfile(_) => "ssh_profile",
        SavedSource::Discovery(_) => "discovery",
        SavedSource::HerdrDiscovery(_) => "herdr_discovery",
        SavedSource::Local { .. } => "local",
        SavedSource::Demo => "demo",
        SavedSource::Replay(_) => "replay",
        SavedSource::ViewRecording(_) => "view_recording",
        SavedSource::TimedReplay(_) => "timed_replay",
        SavedSource::PausedReplay(_) => "paused_replay",
        SavedSource::Ssh(_) => "ssh",
        SavedSource::Startup { connection, .. } | SavedSource::Durable { connection, .. } => {
            source_kind(connection)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        durable::DurableReference,
        options::{LocalOptions, SshOptions, TabColor},
        pane_layout::{Axis, Layout},
        workspace::SshStartup,
    };

    #[test]
    fn workspace_preview_keeps_semantics_without_serializing_launch_details() {
        let ssh: SshOptions = serde_json::from_value(json!({
            "host":"private.example.test",
            "user":"private-user",
            "auth":"identity",
            "identity":r"C:\private\key",
        }))
        .unwrap();
        let startup =
            SshStartup::new(&ssh, "/private/directory".into(), "private-command".into()).unwrap();
        let durable =
            DurableReference::new(&ssh, "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        let local: LocalOptions = serde_json::from_value(json!({
            "executable":r"C:\private\shell.exe",
            "args":["private-local-arg"],
            "cwd":r"C:\private\working-directory",
        }))
        .unwrap();
        let sources = [
            SavedSource::Startup {
                connection: Box::new(SavedSource::Ssh(Box::new(ssh))),
                launch: startup,
            },
            SavedSource::Durable {
                connection: Box::new(SavedSource::Discovery("Fleet".into())),
                session: durable,
            },
            SavedSource::Local {
                name: Some("PowerShell".into()),
                shell: local,
            },
            SavedSource::Replay(r"C:\private\recording.vt".into()),
        ];
        let mut split = Layout::Leaf(1);
        assert!(split.split(1, 2, Axis::Columns));
        assert!(split.set_ratio(&[], 0.625));
        assert!(split.split(2, 3, Axis::Rows));
        let mut workspace =
            Workspace::capture_panes([(&split, 2), (&Layout::Leaf(4), 4)], 1, |id| {
                Ok((
                    sources[(id - 1) as usize].clone(),
                    Some(format!("Label {id}").try_into().unwrap()),
                    Some(14.0),
                ))
            })
            .unwrap();
        workspace.tabs[0].pinned = true;
        workspace.tabs[0].tab_color = Some(TabColor::None);
        let preview =
            serde_json::to_value(WorkspacePreview::new("Operations", &workspace)).unwrap();
        assert_eq!(preview["name"], "Operations");
        assert_eq!(preview["active_tab"], 1);
        assert_eq!(preview["required_ssh_profiles"], json!(["Fleet"]));
        assert_eq!(preview["profile_check"], Value::Null);
        let primary = &preview["tabs"][0];
        assert_eq!(primary["focused_slot"], 2);
        assert_eq!(primary["pinned"], true);
        assert_eq!(primary["color_override"], "none");
        assert_eq!(
            primary["layout"][0],
            json!({"kind":"split","axis":"columns","ratio":0.625,"first":1,"second":2})
        );
        assert_eq!(
            primary["layout"][1],
            json!({
                "kind":"pane","slot":1,"pane_name":"Label 1","font_size":14.0,
                "source":{"kind":"ssh","profile":null,"startup":true,"durable":false},
            })
        );
        assert_eq!(
            primary["layout"][3]["source"],
            json!({"kind":"discovery","profile":"Fleet","startup":false,"durable":true})
        );
        assert_eq!(
            primary["layout"][4]["source"],
            json!({"kind":"local","profile":"PowerShell","startup":false,"durable":false})
        );
        assert_eq!(
            preview["tabs"][1]["layout"][0]["source"],
            json!({"kind":"replay","profile":null,"startup":false,"durable":false})
        );
        let reply = preview.to_string();
        for excluded in ["private", "auth", "generation", "host_key"] {
            assert!(!reply.contains(excluded), "{reply}");
        }
    }
}
