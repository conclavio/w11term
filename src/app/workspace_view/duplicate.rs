use super::*;
use anyhow::{Result, bail, ensure};

const CLOSED_TAB_LIMIT: usize = 10;

/// Duplicating or reopening a tab starts ordinary shells. Keep this check cheap enough for
/// disabled menu/palette state; filesystem preflight happens on invocation.
fn eligible(source: &Source, discovery: bool, startup: Option<&SshStartup>) -> Result<()> {
    if let Some(startup) = startup {
        ensure!(
            startup.command.is_empty(),
            "This tab contains an SSH workspace startup command"
        );
        let Source::Ssh(options) = source else {
            bail!("A remote starting directory requires an SSH pane")
        };
        startup.check_target(options)?;
    }
    if discovery {
        bail!("This tab contains a remote session browser or viewer");
    }
    match source {
        Source::Ssh(ssh) if ssh.durable => bail!("This tab contains a durable SSH session"),
        Source::Ssh(ssh) if ssh.player.rustmush => {
            bail!("This tab contains a RustMUSH player session")
        }
        Source::Ssh(_) => Ok(()),
        Source::Local(shell) if interactive_local(shell) => Ok(()),
        Source::Local(_) => bail!("This tab contains a local command or unsupported shell launch"),
        Source::Demo
        | Source::Replay(_)
        | Source::ViewRecording(_)
        | Source::TimedReplay(_)
        | Source::PausedReplay(_) => {
            bail!("Only tabs containing interactive SSH and local shells can be duplicated")
        }
    }
}

fn interactive_local(shell: &LocalOptions) -> bool {
    if LocalOptions::supports_powershell_integration(&shell.executable, &shell.args)
        || shell.wsl_distribution().is_some()
    {
        return true;
    }
    let Some(name) = shell.executable.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    shell.args.is_empty()
        && matches!(
            name.strip_suffix(".exe").unwrap_or(&name),
            "cmd" | "wsl" | "bash" | "sh" | "zsh" | "fish" | "nu" | "elvish" | "ksh"
        )
}

fn eligible_pane(tab: &AppTab) -> Result<&LiveTab> {
    let Page::Terminal(live) = &tab.page else {
        bail!("Select a terminal tab to open its layout")
    };
    eligible(
        &live.source,
        live.discovery.is_some(),
        live.startup.as_deref(),
    )?;
    Ok(live)
}

pub(in crate::app) struct ClosedTab {
    pub(in crate::app) group: TabId,
    layout: w11term::tabs::TabLayoutRecipe<NewShell>,
}

pub(in crate::app) struct ClosedPane {
    pub(in crate::app) id: TabId,
    location: w11term::tabs::ClosedPaneLocation,
    shell: NewShell,
}

#[derive(Clone)]
struct NewShell {
    source: Source,
    label: String,
    pane_name: Option<TabTitle>,
    profile: Option<String>,
    size: TerminalSize,
    read_only: bool,
    startup: Option<Box<SshStartup>>,
}

impl NewShell {
    fn capture(tab: &AppTab) -> Result<Self> {
        let live = eligible_pane(tab)?;
        let mut source = live.source.clone();
        if let Source::Ssh(ssh) = &mut source {
            ssh.player = live.session.player.read().unwrap().clone();
            ensure!(
                !ssh.player.rustmush,
                "This tab contains a RustMUSH player session"
            );
        }
        Ok(Self {
            source,
            profile: live.profile_name.clone(),
            label: tab.label.clone(),
            pane_name: live.pane_name.clone(),
            size: live.last_size.get().unwrap_or_default(),
            read_only: live.read_only.get(),
            startup: live.startup.clone(),
        })
    }

    fn prepare(mut self, catalog: Option<&Profiles>) -> Result<Self> {
        if let Source::Local(shell) = &self.source {
            shell
                .check_launch_paths()
                .with_context(|| format!("Cannot open {}", self.label))?;
        }
        self.profile =
            catalog.and_then(|c| matching_profile(c, self.profile.as_deref(), &self.source));
        self.label = self
            .profile
            .clone()
            .unwrap_or_else(|| source_label(&self.source));
        Ok(self)
    }

    fn launch(self, preferences: impl Into<PanePreferences> + Copy) -> AppTab {
        let mut tab = if let (Some(startup), Source::Ssh(options)) = (self.startup, &self.source) {
            // A directory-only recipe opens an interactive shell. Its directory
            // was bound to this endpoint when the user saved it in w11term.
            let session = Session::start_workspace_ssh(
                options.clone(),
                self.size,
                preferences,
                (*startup).clone(),
            );
            let mut tab = live_tab_for_session(self.label, self.source, session);
            if let Page::Terminal(live) = &mut tab.page {
                live.startup = Some(startup);
            }
            tab
        } else {
            live_tab_sized(self.label, self.source, self.size, preferences, None, None)
        }
        .with_profile(self.profile);
        if let Page::Terminal(live) = &mut tab.page {
            live.pane_name = self.pane_name;
            KeyboardBridge::set_read_only(&live.session.input, &live.read_only, self.read_only);
        }
        tab
    }
}

impl TerminalApp {
    pub(in crate::app) fn remember_closed_pane(&mut self, id: TabId) {
        if let Some(location) = self.tabs.closed_pane_location(id)
            && let Some(tab) = self.tabs.get(id)
            && let Ok(shell) = NewShell::capture(tab)
        {
            self.closed_pane = Some(ClosedPane {
                id,
                location,
                shell,
            });
        }
    }

    pub(in crate::app) fn reopen_closed_pane(
        &mut self,
        id: TabId,
        context: &ComponentContext<Self>,
    ) -> Result<()> {
        let Some(closed) = self.closed_pane.as_ref().filter(|closed| closed.id == id) else {
            return Ok(());
        };
        let catalog = self.store.catalog().ok();
        let shell = closed.shell.clone().prepare(catalog.as_ref())?;
        let preferences = self.appearance.read().unwrap().pane_preferences();
        self.tabs
            .reopen_pane(&closed.location, shell.launch(preferences));
        self.closed_pane = None;
        self.sync_selection(context);
        self.activate(context);
        Ok(())
    }

    pub(in crate::app) fn close_user_tab(&mut self, id: TabId, context: &ComponentContext<Self>) {
        let Some(group) = self.tabs.group_id(id) else {
            return;
        };
        if self.tabs.group_is_pinned(group) {
            return;
        }
        self.remember_closed_tab(group);
        self.close_tab(id, context);
    }

    pub(in crate::app) fn remember_closed_tab(&mut self, group: TabId) {
        if let Ok(Some(layout)) = self.tabs.capture_group(group, NewShell::capture) {
            if self.closed_tabs.len() == CLOSED_TAB_LIMIT {
                self.closed_tabs.pop_front();
            }
            self.closed_tabs.push_back(ClosedTab { group, layout });
        }
    }

    pub(in crate::app) fn reopen_closed_tab(
        &mut self,
        group: TabId,
        context: &ComponentContext<Self>,
    ) -> Result<()> {
        let Some(closed) = self
            .closed_tabs
            .back()
            .filter(|closed| closed.group == group)
        else {
            return Ok(());
        };
        let catalog = self.store.catalog().ok();
        let preferences = self.appearance.read().unwrap().pane_preferences();
        self.tabs.reopen_group(
            &closed.layout,
            |recipe| recipe.clone().prepare(catalog.as_ref()),
            |shell| shell.launch(preferences),
        )?;
        self.closed_tabs.pop_back();
        self.sync_selection(context);
        self.activate(context);
        Ok(())
    }

    pub(in crate::app) fn can_duplicate_tab_layout(&self, group: TabId) -> Result<()> {
        ensure!(
            self.tabs.active_group_id() == Some(group),
            "The original tab is no longer active"
        );
        let layout = self
            .tabs
            .active_layout()
            .context("The original tab was closed")?;
        for id in layout.leaves() {
            let tab = self.tabs.get(id).context("The original pane was closed")?;
            let live = eligible_pane(tab)?;
            if let Source::Ssh(_) = &live.source
                && live.session.player.read().unwrap().rustmush
            {
                bail!("This tab contains a RustMUSH player session");
            }
        }
        Ok(())
    }

    pub(in crate::app) fn duplicate_tab_layout(
        &mut self,
        group: TabId,
        context: &ComponentContext<Self>,
    ) -> Result<()> {
        self.can_duplicate_tab_layout(group)?;
        // Only keep a profile association if the on-disk catalog still agrees.
        // A moved/deleted catalog cannot change a live pane's launch recipe.
        let catalog = self.store.catalog().ok();
        let preferences = self.appearance.read().unwrap().pane_preferences();
        self.tabs.duplicate_active_group(
            |tab| NewShell::capture(tab)?.prepare(catalog.as_ref()),
            |recipe| recipe.launch(preferences),
        )?;
        self.sync_selection(context);
        self.activate(context);
        Ok(())
    }
}

fn matching_profile(catalog: &Profiles, name: Option<&str>, source: &Source) -> Option<String> {
    let name = name?;
    let matches = match source {
        Source::Ssh(ssh) => catalog.profiles.get(name) == Some(ssh),
        Source::Local(shell) => catalog
            .local
            .get(name)
            .is_some_and(|saved| saved == shell || shell.matches_wsl_profile(saved)),
        _ => false,
    };
    matches.then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use w11term::clipboard::RemoteClipboardPolicy;

    fn ssh() -> SshOptions {
        serde_json::from_value(
            serde_json::json!({"user":"dev", "host":"build.example", "auth":"none"}),
        )
        .unwrap()
    }

    fn local(executable: &str, args: &[&str]) -> Source {
        Source::Local(LocalOptions {
            executable: executable.into(),
            args: args.iter().map(|s| (*s).to_string()).collect(),
            cwd: None,
            powershell_integration: false,
            scrollback_lines: Default::default(),
            color_scheme: Default::default(),
            tab_color: Default::default(),
        })
    }

    #[test]
    fn duplicate_accepts_ordinary_shells_and_refuses_startup_or_attachment_replay() {
        let standard = Source::Ssh(ssh());
        assert!(eligible(&standard, false, None).is_ok());
        let directory =
            SshStartup::new(&ssh(), "/srv/crew's project".into(), String::new()).unwrap();
        assert!(eligible(&standard, false, Some(&directory)).is_ok());
        let mut changed = ssh();
        changed.host = "different.test".into();
        assert!(eligible(&Source::Ssh(changed), false, Some(&directory)).is_err());
        let command = SshStartup::new(&ssh(), String::new(), "echo hello".into()).unwrap();
        assert!(
            eligible(&standard, true, None)
                .unwrap_err()
                .to_string()
                .contains("viewer")
        );
        assert!(
            eligible(&standard, false, Some(&command))
                .unwrap_err()
                .to_string()
                .contains("workspace startup")
        );
        let mut durable = ssh();
        durable.durable = true;
        assert!(
            eligible(&Source::Ssh(durable), false, None)
                .unwrap_err()
                .to_string()
                .contains("durable")
        );
        let mut game = ssh();
        game.player.rustmush = true;
        assert!(
            eligible(&Source::Ssh(game), false, None)
                .unwrap_err()
                .to_string()
                .contains("RustMUSH")
        );
        for source in [
            local("PowerShell.exe", &["-NoProfile"]),
            local("pwsh", &[]),
            local("cmd.exe", &[]),
            local("wsl.exe", &[]),
            local("wsl.exe", &["-d", "Build Linux", "--cd", "/work"]),
        ] {
            assert!(eligible(&source, false, None).is_ok(), "{source:?}");
        }
        for source in [
            Source::Demo,
            Source::Replay("last.vt".into()),
            local("cmd.exe", &["/c", "echo ran"]),
            local("pwsh.exe", &["-File", "job.ps1"]),
            local("maintenance.cmd", &[]),
            local("wsl.exe", &["--exec", "make"]),
            local(
                "wsl.exe",
                &["--distribution", "Build Linux", "--exec", "make"],
            ),
        ] {
            assert!(eligible(&source, false, None).is_err(), "{source:?}");
        }
    }

    #[test]
    fn duplicated_and_reopened_panes_keep_their_own_display_names() {
        let mut tabs = Tabs::default();
        let shell = local("powershell.exe", &["-NoProfile"]);
        let original = tabs.insert(live_tab(source_label(&shell), shell, false));
        let name: TabTitle = "Deploy 雪".to_owned().try_into().unwrap();
        let Page::Terminal(pane) = &mut tabs.get_mut(original).unwrap().page else {
            unreachable!()
        };
        pane.pane_name = Some(name.clone());
        let group = tabs.group_id(original).unwrap();
        let closed_recipe = tabs
            .capture_group(group, NewShell::capture)
            .unwrap()
            .unwrap();
        tabs.duplicate_active_group(
            |tab| NewShell::capture(tab)?.prepare(None),
            |recipe| recipe.launch(RemoteClipboardPolicy::Disabled),
        )
        .unwrap();
        let duplicate = tabs.active_id().unwrap();
        assert_ne!(original, duplicate);
        let Page::Terminal(pane) = &mut tabs.get_mut(duplicate).unwrap().page else {
            unreachable!()
        };
        assert_eq!(pane.pane_name.take().as_ref(), Some(&name));
        assert_eq!(tabs.get(original).unwrap().pane_name(), Some(&name));
        assert!(tabs.remove_group(original));
        tabs.reopen_group(
            &closed_recipe,
            |recipe| recipe.clone().prepare(None),
            |recipe| recipe.launch(RemoteClipboardPolicy::Disabled),
        )
        .unwrap();
        let reopened = tabs.active_id().unwrap();
        assert_ne!(reopened, original);
        assert_eq!(tabs.get(reopened).unwrap().pane_name(), Some(&name));
        assert!(tabs.get(duplicate).unwrap().pane_name().is_none());
    }

    #[test]
    fn reopen_checks_the_current_directory_without_retargeting_a_saved_shell() {
        let mut catalog = Profiles::default();
        let mut changed = ssh();
        changed.host = "another.example".into();
        catalog.profiles.insert("test".into(), changed);
        let mut recipe = NewShell {
            source: Source::Ssh(ssh()),
            profile: Some("test".into()),
            label: "test".into(),
            pane_name: Some("Deploy console".to_owned().try_into().unwrap()),
            size: TerminalSize::default(),
            read_only: true,
            startup: None,
        };
        let prepared = recipe.clone().prepare(Some(&catalog)).unwrap();
        assert!(prepared.profile.is_none());
        assert_eq!(prepared.pane_name, recipe.pane_name);
        assert_eq!(prepared.label, source_label(&prepared.source));
        assert!(matches!(prepared.source, Source::Ssh(ref options) if options == &ssh()));
        assert!(prepared.read_only);

        let folder = tempfile::tempdir().unwrap();
        let Source::Local(mut shell) = local("powershell.exe", &[]) else {
            unreachable!()
        };
        shell.cwd = Some(folder.path().join("build"));
        recipe.source = Source::Local(shell);
        assert!(recipe.clone().prepare(None).is_err());
        std::fs::create_dir(folder.path().join("build")).unwrap();
        assert!(recipe.prepare(None).is_ok());
    }

    #[test]
    fn duplicate_binds_profile_features_only_to_the_same_live_launch_options() {
        let mut catalog = Profiles::default();
        let original = Source::Ssh(ssh());
        catalog.profiles.insert("renamed".into(), ssh());
        assert_eq!(
            matching_profile(&catalog, Some("renamed"), &original).as_deref(),
            Some("renamed")
        );
        assert_eq!(matching_profile(&catalog, Some("deleted"), &original), None);
        catalog.profiles.get_mut("renamed").unwrap().host = "other.example".into();
        assert_eq!(matching_profile(&catalog, Some("renamed"), &original), None);
        let original = local("pwsh.exe", &["-NoProfile"]);
        let Source::Local(changed) = local("pwsh.exe", &["-File", "job.ps1"]) else {
            unreachable!()
        };
        catalog.local.insert("local".into(), changed);
        assert_eq!(matching_profile(&catalog, Some("local"), &original), None);
    }
}
