//! Named workspace recipes. Slot IDs are scoped to a single saved layout.

use crate::durable::DurableReference;
use crate::options::{Authentication, Source, SshOptions};
use crate::pane_layout::{Axis, Layout};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

const VERSION: u32 = 1;
const MAX_DEPTH: usize = 32;
const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
mod startup;
pub use startup::SshStartup;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum SavedSource {
    SavedProfile(String),
    /// An explicit session browser on a saved SSH account. Reopening an
    /// unselected browser lists sessions again without creating a shell.
    Discovery(String),
    HerdrDiscovery(String),
    Local {
        name: Option<String>,
        shell: crate::options::LocalOptions,
    },
    Demo,
    Replay(PathBuf),
    ViewRecording(PathBuf),
    TimedReplay(PathBuf),
    PausedReplay(PathBuf),
    /// Connection options contain authentication methods and key paths; live
    /// passwords, private key material and authentication answers live elsewhere.
    Ssh(Box<SshOptions>),
    Startup {
        connection: Box<SavedSource>,
        launch: SshStartup,
    },
    Durable {
        connection: Box<SavedSource>,
        session: DurableReference,
    },
}

impl SavedSource {
    pub fn with_session(self, reference: Option<DurableReference>) -> Result<Self> {
        let Some(session) = reference else {
            return Ok(self);
        };
        let saved = Self::Durable {
            connection: Box::new(self),
            session,
        };
        saved.validate()?;
        Ok(saved)
    }

    pub fn session(&self) -> Option<&DurableReference> {
        match self {
            Self::Durable { session, .. } => Some(session),
            _ => None,
        }
    }

    pub fn profile_name(&self) -> Option<&str> {
        match self {
            Self::SavedProfile(name) | Self::Discovery(name) | Self::HerdrDiscovery(name) => {
                Some(name)
            }
            Self::Local { name, .. } => name.as_deref(),
            Self::Durable { connection, .. } | Self::Startup { connection, .. } => {
                connection.profile_name()
            }
            _ => None,
        }
    }
    pub fn is_herdr_browser(&self) -> bool {
        match self {
            Self::HerdrDiscovery(_) => true,
            Self::Durable { connection, .. } => connection.is_herdr_browser(),
            _ => false,
        }
    }
    pub fn is_discovery(&self) -> bool {
        match self {
            Self::Discovery(_) | Self::HerdrDiscovery(_) => true,
            Self::Durable { connection, .. } => connection.is_discovery(),
            _ => false,
        }
    }
    /// Supply a profile name only when the pane was opened from that profile.
    pub fn capture(source: &Source, profile: Option<&str>) -> Result<Self> {
        let saved = match (source, profile) {
            (Source::Local(shell), name) => {
                let mut shell = shell.clone();
                if let Some(cwd) = &mut shell.cwd {
                    *cwd = std::path::absolute(&*cwd)?;
                }
                if shell.executable.is_relative() && !shell.is_command_name() {
                    shell.executable = std::path::absolute(&shell.executable)?;
                }
                Self::Local {
                    name: name.map(str::to_owned),
                    shell,
                }
            }
            (Source::Demo, _) => Self::Demo,
            (Source::ViewRecording(path), _) => {
                ensure!(!path.as_os_str().is_empty(), "Empty recording path");
                Self::ViewRecording(std::path::absolute(path)?)
            }
            (Source::Replay(path), _) => {
                ensure!(!path.as_os_str().is_empty(), "Empty replay path");
                Self::Replay(std::path::absolute(path)?)
            }
            (Source::TimedReplay(path) | Source::PausedReplay(path), _) => {
                ensure!(!path.as_os_str().is_empty(), "Empty replay path");
                let path = std::path::absolute(path)?;
                if matches!(source, Source::PausedReplay(_)) {
                    Self::PausedReplay(path)
                } else {
                    Self::TimedReplay(path)
                }
            }
            (Source::Ssh(_), Some(name)) => Self::SavedProfile(name.into()),
            (Source::Ssh(options), None) => {
                let mut options = options.clone();
                for auth in options.authentications_mut() {
                    let Authentication::Identity { identity } = auth else {
                        continue;
                    };
                    ensure!(!identity.as_os_str().is_empty(), "Empty identity path");
                    *identity = std::path::absolute(&*identity)?;
                }
                if let Some(path) = &mut options.player.device_key {
                    ensure!(!path.as_os_str().is_empty(), "Empty device key path");
                    *path = std::path::absolute(&*path)?;
                }
                Self::Ssh(Box::new(options))
            }
        };
        saved.validate()?;
        Ok(saved)
    }

    fn validate(&self) -> Result<()> {
        match self {
            Self::Startup { connection, launch } => {
                ensure!(
                    matches!(connection.as_ref(), Self::SavedProfile(_) | Self::Ssh(_)),
                    "Workspace startup requires an SSH connection"
                );
                connection.validate()?;
                launch.validate()?;
            }
            Self::Durable {
                connection,
                session,
            } => {
                ensure!(
                    matches!(
                        connection.as_ref(),
                        Self::Ssh(_)
                            | Self::SavedProfile(_)
                            | Self::Discovery(_)
                            | Self::HerdrDiscovery(_)
                    ),
                    "A durable reference requires an SSH connection"
                );
                connection.validate()?;
                session.validate()?;
            }
            Self::SavedProfile(name) | Self::Discovery(name) | Self::HerdrDiscovery(name) => {
                ensure!(!name.trim().is_empty(), "Missing profile name")
            }
            Self::Demo => {}
            Self::Local { name, shell } => {
                ensure!(
                    name.as_ref().is_none_or(|name| !name.trim().is_empty()),
                    "Missing local profile name"
                );
                shell.validate()?;
                shell.validate_resolved_paths()?;
            }
            Self::Replay(path)
            | Self::TimedReplay(path)
            | Self::PausedReplay(path)
            | Self::ViewRecording(path) => {
                ensure!(path.is_absolute(), "Expected an absolute replay path")
            }
            Self::Ssh(options) => {
                options.validate_route()?;
                ensure!(
                    !options.host.trim().is_empty()
                        && !options.user.trim().is_empty()
                        && options.port != 0,
                    "Missing host, user or port for an SSH pane"
                );
                ensure!(
                    crate::assets::name(&options.player.bell),
                    "Invalid bell name"
                );
                for auth in std::iter::once(&options.authentication)
                    .chain(options.proxy_jump.iter().map(|hop| &hop.authentication))
                {
                    let Authentication::Identity { identity } = auth else {
                        continue;
                    };
                    ensure!(identity.is_absolute(), "Expected an absolute identity path");
                }
                if let Some(path) = &options.player.device_key {
                    ensure!(path.is_absolute(), "Expected an absolute device key path");
                }
            }
        }
        Ok(())
    }

    pub fn resolve(&self, profiles: &BTreeMap<String, SshOptions>) -> Result<Source> {
        self.validate()?;
        Ok(match self {
            Self::Startup { connection, launch } => {
                let source = connection.resolve(profiles)?;
                let Source::Ssh(options) = &source else {
                    anyhow::bail!("Workspace startup requires SSH")
                };
                launch.check_target(options)?;
                source
            }
            Self::Durable {
                connection,
                session,
            } => {
                let source = connection.resolve(profiles)?;
                ensure!(
                    matches!(&source, Source::Ssh(options) if options.durable && session.matches_endpoint(options)),
                    "The saved durable session belongs to a different host, account, host key, or a profile with durability disabled"
                );
                source
            }
            Self::SavedProfile(name) | Self::Discovery(name) | Self::HerdrDiscovery(name) => {
                let mut options = profiles
                    .get(name)
                    .with_context(|| format!("Unknown workspace profile {name}"))?
                    .clone();
                if matches!(self, Self::Discovery(_) | Self::HerdrDiscovery(_)) {
                    ensure!(
                        !options.player.rustmush,
                        "Remote shell discovery requires an SSH shell profile"
                    );
                    options.durable = true;
                }
                Source::Ssh(options)
            }
            Self::Demo => Source::Demo,
            Self::ViewRecording(path) => {
                crate::recorder::check_raw_file(path)?;
                crate::recorder::raw_grid(path)?;
                Source::ViewRecording(path.clone())
            }
            Self::Local { shell, .. } => {
                shell.check_launch_paths()?;
                Source::Local(shell.clone())
            }
            Self::Replay(path) | Self::TimedReplay(path) | Self::PausedReplay(path) => {
                ensure!(
                    path.is_file(),
                    "Workspace replay file is unavailable: {}",
                    path.display()
                );
                if matches!(self, Self::TimedReplay(_) | Self::PausedReplay(_)) {
                    crate::recorder::TimedReplay::read_initial_size(path)?;
                    if matches!(self, Self::PausedReplay(_)) {
                        Source::PausedReplay(path.clone())
                    } else {
                        Source::TimedReplay(path.clone())
                    }
                } else {
                    Source::Replay(path.clone())
                }
            }
            Self::Ssh(options) => Source::Ssh(options.as_ref().clone()),
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Axis", rename_all = "snake_case")]
enum SavedAxis {
    Columns,
    Rows,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SavedLayout {
    Leaf {
        slot: u64,
        source: SavedSource,
    },
    Split {
        #[serde(with = "SavedAxis")]
        axis: Axis,
        ratio: f64,
        first: Box<Self>,
        second: Box<Self>,
    },
}

impl SavedLayout {
    fn capture(
        layout: &Layout,
        depth: usize,
        slots: &mut BTreeMap<u64, u64>,
        source: &mut impl FnMut(u64) -> Result<SavedSource>,
    ) -> Result<Self> {
        ensure!(depth <= MAX_DEPTH, "Workspace split tree is too deep");
        Ok(match layout {
            Layout::Leaf(id) => {
                let slot = slots.len() as u64 + 1;
                ensure!(
                    slots.insert(*id, slot).is_none(),
                    "Pane occurs more than once"
                );
                Self::Leaf {
                    slot,
                    source: source(*id)?,
                }
            }
            Layout::Split {
                axis,
                ratio,
                first,
                second,
            } => Self::Split {
                axis: *axis,
                ratio: *ratio,
                first: Box::new(Self::capture(first, depth + 1, slots, source)?),
                second: Box::new(Self::capture(second, depth + 1, slots, source)?),
            },
        })
    }

    fn visit(
        &self,
        depth: usize,
        leaf: &mut impl FnMut(u64, &SavedSource) -> Result<()>,
    ) -> Result<()> {
        ensure!(depth <= MAX_DEPTH, "Workspace split tree is too deep");
        match self {
            Self::Leaf { slot, source } => leaf(*slot, source),
            Self::Split {
                ratio,
                first,
                second,
                ..
            } => {
                ensure!(
                    *ratio > 0.0 && *ratio < 1.0,
                    "Invalid workspace divider proportion"
                );
                first.visit(depth + 1, leaf)?;
                second.visit(depth + 1, leaf)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTab {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<crate::tabs::TabTitle>,
    pub focused: u64,
    pub layout: SavedLayout,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    /// Index in the ordered terminal tabs.
    pub active_tab: usize,
    pub tabs: Vec<WorkspaceTab>,
}

/// Fully resolved before a launch starts any sessions or creates a window.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceLaunch {
    pub layout: Workspace,
    pub sources: BTreeMap<u64, Source>,
}

impl WorkspaceLaunch {
    pub fn load(store: &WorkspaceStore, name: &str, profiles_path: &Path) -> Result<Self> {
        let layout = store
            .load()?
            .remove(name)
            .with_context(|| format!("Unknown workspace {name}"))?;
        let mut needs_profiles = false;
        for tab in &layout.tabs {
            tab.layout.visit(0, &mut |_, source| {
                needs_profiles |=
                    !matches!(source, SavedSource::Local { .. }) && source.profile_name().is_some();
                Ok(())
            })?;
        }
        let profiles = if needs_profiles && profiles_path.try_exists()? {
            crate::profiles::ProfileStore::new(profiles_path.to_owned())?.load()?
        } else {
            BTreeMap::new()
        };
        let sources = layout.resolve_sources(&profiles)?;
        Ok(Self { layout, sources })
    }
}

impl Workspace {
    pub fn capture<'a>(
        tabs: impl IntoIterator<Item = (&'a Layout, u64)>,
        active_tab: usize,
        mut source: impl FnMut(u64) -> Result<SavedSource>,
    ) -> Result<Self> {
        let mut slots = BTreeMap::new();
        let mut saved = Self {
            active_tab,
            tabs: vec![],
        };
        for (tree, focused) in tabs {
            let layout = SavedLayout::capture(tree, 0, &mut slots, &mut source)?;
            let focused = *slots
                .get(&focused)
                .context("Focused pane missing from workspace")?;
            saved.tabs.push(WorkspaceTab {
                title: None,
                focused,
                layout,
            });
        }
        saved.validate()?;
        Ok(saved)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.tabs.is_empty(),
            "A workspace needs at least one terminal tab"
        );
        ensure!(
            self.active_tab < self.tabs.len(),
            "Active tab missing from workspace"
        );
        let mut seen = BTreeSet::new();
        for tab in &self.tabs {
            let mut has_focus = false;
            tab.layout.visit(0, &mut |slot, source| {
                ensure!(
                    slot > 0 && seen.insert(slot),
                    "Invalid or duplicate workspace slot {slot}"
                );
                has_focus |= slot == tab.focused;
                source.validate()
            })?;
            ensure!(has_focus, "Focused pane missing from its workspace tab");
        }
        Ok(())
    }

    /// Resolve all references before the caller creates any live sessions.
    pub fn resolve_sources(
        &self,
        profiles: &BTreeMap<String, SshOptions>,
    ) -> Result<BTreeMap<u64, Source>> {
        self.validate()?;
        let mut sources = BTreeMap::new();
        for tab in &self.tabs {
            tab.layout.visit(0, &mut |slot, source| {
                sources.insert(slot, source.resolve(profiles)?);
                Ok(())
            })?;
        }
        Ok(sources)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    version: u32,
    workspaces: BTreeMap<String, Workspace>,
}

pub struct WorkspaceStore {
    path: PathBuf,
}

impl WorkspaceStore {
    pub fn open() -> Result<Self> {
        Ok(Self::new(crate::keys::data_dir()?.join("workspaces.json")))
    }

    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Result<BTreeMap<String, Workspace>> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(e).with_context(|| format!("Reading {}", self.path.display())),
        };
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= MAX_FILE_BYTES,
            "Workspace catalog is too large"
        );
        let catalog: Catalog = serde_json::from_slice(&bytes)
            .with_context(|| format!("Parsing {}", self.path.display()))?;
        ensure!(
            catalog.version == VERSION,
            "Unsupported workspace catalog version {}",
            catalog.version
        );
        for (name, workspace) in &catalog.workspaces {
            validate_name(name)?;
            workspace
                .validate()
                .with_context(|| format!("Invalid workspace {name}"))?;
        }
        Ok(catalog.workspaces)
    }

    pub fn save(&self, name: &str, workspace: &Workspace) -> Result<()> {
        validate_name(name)?;
        workspace.validate()?;
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        // All GUI processes lock this stable sibling through the entire
        // read/modify/replace, so saving one workspace preserves other names.
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path.with_extension("lock"))?;
        lock.try_lock()
            .context("Another window may be saving workspaces; try again")?;
        let mut workspaces = self.load()?;
        workspaces.insert(name.to_owned(), workspace.clone());
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(
            &mut file,
            &Catalog {
                version: VERSION,
                workspaces,
            },
        )?;
        file.write_all(b"\n")?;
        ensure!(
            file.as_file().metadata()?.len() <= MAX_FILE_BYTES as u64,
            "Workspace catalog is too large"
        );
        file.as_file().sync_all()?;
        file.persist(&self.path).context("Saving workspaces")?;
        Ok(())
    }
}

fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.trim() == name && !name.chars().any(char::is_control),
        "Enter a workspace name without leading spaces, trailing spaces or control characters"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> Workspace {
        Workspace::capture([(&Layout::Leaf(42), 42)], 0, |_| Ok(SavedSource::Demo)).unwrap()
    }

    fn ssh(host: &str) -> SshOptions {
        SshOptions {
            host: host.into(),
            user: "tester".into(),
            scrollback_lines: Default::default(),
            color_scheme: Default::default(),
            terminal_type: Default::default(),
            port: 22,
            authentication: Authentication::Agent,
            host_key: None,
            host_key_alias: None,
            proxy_jump: Vec::new(),
            local_forwards: Vec::new(),
            durable: false,
            player: Default::default(),
        }
    }

    #[test]
    fn startup_exec_preserves_the_command_and_quotes_directory_arguments() {
        let opts = ssh("dev.example");
        let build = "printf '%s' \"$PWD\"";
        let recipe = |dir: &str, cmd: &str| SshStartup::new(&opts, dir.into(), cmd.into());
        assert_eq!(recipe("", build).unwrap().exec_request(), build);
        assert_eq!(
            recipe("/a'b/$user", "").unwrap().exec_request(),
            r#"sh -c 'cd "$1" && exec "${SHELL:-/bin/sh}" -i' w11term '/a'\''b/$user'"#
        );
        assert_eq!(
            recipe("/a b", build).unwrap().exec_request(),
            r#"sh -c 'cd "$1" && exec "${SHELL:-/bin/sh}" -c "$2"' w11term '/a b' 'printf '\''%s'\'' "$PWD"'"#
        );
        for (dir, command) in [
            ("", ""),
            ("relative/dir", build),
            ("/dir\0", ""),
            ("", "a\0b"),
        ] {
            assert!(recipe(dir, command).is_err());
        }
    }

    #[test]
    fn workspace_startup_remains_bound_to_its_ordinary_ssh_account() {
        let opts = ssh("dev.example");
        let launch = SshStartup::new(&opts, "/work".into(), "make".into()).unwrap();
        assert!(
            serde_json::to_value(&launch)
                .unwrap()
                .get("host_key_alias")
                .is_none()
        );
        let mut aliased = opts.clone();
        aliased.host_key_alias = Some("deployment".into());
        let recipe = SshStartup::new(&aliased, "/work".into(), "make".into()).unwrap();
        let recipe: SshStartup =
            serde_json::from_value(serde_json::to_value(&recipe).unwrap()).unwrap();
        assert!(recipe.check_target(&aliased).is_ok());
        assert!(recipe.check_target(&opts).is_err());
        let source = SavedSource::Startup {
            connection: Box::new(SavedSource::SavedProfile("dev".into())),
            launch: launch.clone(),
        };
        let saved: SavedSource =
            serde_json::from_slice(&serde_json::to_vec(&source).unwrap()).unwrap();
        assert_eq!(saved, source);
        assert_eq!(saved.profile_name(), Some("dev"));
        let resolve = |opts| saved.resolve(&BTreeMap::from([("dev".into(), opts)]));
        assert_eq!(resolve(opts.clone()).unwrap(), Source::Ssh(opts.clone()));
        let mut game = opts.clone();
        game.player.rustmush = true;
        for changed in [
            ssh("other.example"),
            aliased,
            SshOptions {
                port: 2222,
                ..opts.clone()
            },
            SshOptions {
                user: "other".into(),
                ..opts.clone()
            },
            SshOptions {
                durable: true,
                ..opts
            },
            game,
        ] {
            assert!(resolve(changed).is_err());
        }
        for connection in [
            SavedSource::Demo,
            SavedSource::Discovery("dev".into()),
            saved,
        ] {
            assert!(
                SavedSource::Startup {
                    connection: Box::new(connection),
                    launch: launch.clone()
                }
                .validate()
                .is_err()
            );
        }
    }

    #[test]
    fn named_layout_round_trips_with_workspace_local_slots_and_current_profiles() {
        let temp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::new(temp.path().join("workspaces.json"));
        let replay = temp.path().join("capture.vt");
        std::fs::write(&replay, b"output").unwrap();
        let mut split = Layout::Leaf(51);
        split.split(51, 244, Axis::Rows);
        split.set_ratio(&[], 0.6875);
        let (path, ratio) = split
            .keyboard_resize(244, crate::pane_layout::Direction::Down, 72.0, 6.0, |_| {
                Some(800.0)
            })
            .unwrap();
        assert!(split.set_ratio(&path, ratio));
        let captured = Workspace::capture([(&Layout::Leaf(71), 71), (&split, 244)], 1, |id| {
            Ok(match id {
                51 => SavedSource::SavedProfile("admin".into()),
                244 => SavedSource::Replay(replay.clone()),
                _ => SavedSource::Demo,
            })
        })
        .unwrap();
        assert_eq!(captured.tabs[1].focused, 3);
        store.save("ops", &captured).unwrap();
        store.save("lab", &demo()).unwrap();
        let saved = store.load().unwrap();
        assert_eq!(saved["ops"], captured);
        let SavedLayout::Split { ratio, .. } = &saved["ops"].tabs[1].layout else {
            panic!("saved split missing");
        };
        assert!((*ratio - 0.7375).abs() < 1e-9);
        assert_eq!(saved["lab"], demo());
        assert!(captured.resolve_sources(&BTreeMap::new()).is_err());
        let profiles = BTreeMap::from([("admin".to_owned(), ssh("new-host.example"))]);
        let sources = captured.resolve_sources(&profiles).unwrap();
        assert_eq!(sources[&2], Source::Ssh(ssh("new-host.example")));
        assert_eq!(sources[&3], Source::Replay(replay));
    }

    #[test]
    fn saved_host_browser_reopens_unselected_or_pinned_without_changing_profile_defaults() {
        let options = ssh("host.example");
        let source = SavedSource::Discovery("work".into());
        let profiles = BTreeMap::from([("work".into(), options.clone())]);
        assert!(source.is_discovery() && source.session().is_none());
        assert!(matches!(source.resolve(&profiles).unwrap(), Source::Ssh(s) if s.durable));
        assert!(!profiles["work"].durable);
        let reference = DurableReference::new(&options, &format!("SHA256:{}", "A".repeat(43)));
        let saved = source.with_session(Some(reference.clone())).unwrap();
        let saved: SavedSource =
            serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
        assert!(saved.is_discovery() && saved.session() == Some(&reference));
        assert!(saved.resolve(&profiles).is_ok());
        let other = BTreeMap::from([("work".into(), ssh("other.example"))]);
        assert!(saved.resolve(&other).is_err());
    }

    #[test]
    fn saved_herdr_browser_reopens_in_the_same_discovery_scope_with_or_without_a_choice() {
        let profile = ssh("dev.example");
        let profiles = BTreeMap::from([("work".into(), profile.clone())]);
        let mut reference = DurableReference::new(&profile, &format!("SHA256:{}", "A".repeat(43)));
        reference.backend = crate::durable::Backend::Herdr;
        reference.name = "t7".into();
        reference.generation = format!(
            "{}/123/112/d4156dc9-a3ad-4d94-ad77-0303577e25cc/34",
            "a".repeat(32)
        );
        for session in [None, Some(reference)] {
            let saved = SavedSource::HerdrDiscovery("work".into())
                .with_session(session.clone())
                .unwrap();
            let saved: SavedSource =
                serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
            assert!(saved.is_herdr_browser() && saved.is_discovery());
            assert_eq!(saved.session(), session.as_ref());
            assert!(saved.resolve(&profiles).is_ok());
        }
    }

    #[test]
    fn durable_workspace_checks_the_current_profile_before_reattaching() {
        let mut options = ssh("dev.example");
        options.durable = true;
        let pin = format!("SHA256:{}", "A".repeat(43));
        let reference = DurableReference::new(&options, &pin);
        let profile = SavedSource::SavedProfile("dev".into());
        let saved = profile
            .clone()
            .with_session(Some(reference.clone()))
            .unwrap();
        let encoded = serde_json::to_vec(&saved).unwrap();
        let restored: SavedSource = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored.session(), Some(&reference));
        assert_eq!(restored.profile_name(), Some("dev"));
        let resolve = |opts| restored.resolve(&BTreeMap::from([("dev".into(), opts)]));
        assert_eq!(
            resolve(options.clone()).unwrap(),
            Source::Ssh(options.clone())
        );
        for changed in [
            SshOptions {
                host: "other.example".into(),
                ..options.clone()
            },
            SshOptions {
                port: 2222,
                ..options.clone()
            },
            SshOptions {
                user: "someone-else".into(),
                ..options.clone()
            },
            SshOptions {
                host_key: Some(format!("SHA256:{}", "B".repeat(43))),
                ..options.clone()
            },
            SshOptions {
                durable: false,
                ..options
            },
        ] {
            assert!(resolve(changed).is_err());
        }
        assert!(
            SavedSource::Demo
                .with_session(Some(reference.clone()))
                .is_err()
        );
        let mut invalid = reference;
        invalid.generation.clear();
        assert!(profile.with_session(Some(invalid)).is_err());
    }

    #[test]
    fn local_workspace_preserves_executable_arguments_and_directory_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let shell = crate::options::LocalOptions {
            executable: "powershell.exe".into(),
            args: vec!["-NoProfile".into()],
            cwd: Some(dir.path().to_owned()),
            scrollback_lines: "420".parse().unwrap(),
            color_scheme: crate::options::ColorScheme::OneHalfLight,
            powershell_integration: true,
        };
        let store = WorkspaceStore::new(dir.path().join("workspaces.json"));
        let mut layout = Layout::Leaf(41);
        layout.split(41, 42, Axis::Columns);
        let workspace = Workspace::capture([(&layout, 42)], 0, |id| {
            if id == 42 {
                SavedSource::capture(&Source::Local(shell.clone()), Some("Studio"))
            } else {
                Ok(SavedSource::SavedProfile("ssh".into()))
            }
        })
        .unwrap();
        store.save("mixed", &workspace).unwrap();
        let restored = store.load().unwrap().remove("mixed").unwrap();
        assert_eq!(workspace, restored);
        let profiles = BTreeMap::from([("ssh".into(), ssh("remote"))]);
        let sources = restored.resolve_sources(&profiles).unwrap();
        assert!(matches!(&sources[&1], Source::Ssh(_)));
        assert_eq!(sources[&2], Source::Local(shell));
    }

    #[test]
    fn restored_local_paths_do_not_depend_on_the_launching_directory() {
        let dir = tempfile::tempdir().unwrap();
        let recipe = |exe: &Path, cwd: &Path| SavedSource::Local {
            name: None,
            shell: crate::options::LocalOptions {
                executable: exe.into(),
                cwd: Some(cwd.into()),
                scrollback_lines: Default::default(),
                color_scheme: Default::default(),
                powershell_integration: false,
                args: Vec::new(),
            },
        };
        let profiles = BTreeMap::new();
        for (exe, cwd) in [
            (Path::new("bin/tool.exe"), dir.path()),
            (Path::new("tool.exe"), Path::new("local-cwd")),
        ] {
            assert!(recipe(exe, cwd).resolve(&profiles).is_err());
        }
        assert!(
            recipe(&dir.path().join("missing.exe"), dir.path())
                .resolve(&profiles)
                .is_err()
        );
        assert!(
            recipe(Path::new("tool.exe"), &dir.path().join("missing"))
                .resolve(&profiles)
                .is_err()
        );
        assert!(
            recipe(Path::new("tool.exe"), dir.path())
                .resolve(&profiles)
                .is_ok()
        );
    }

    #[test]
    fn invalid_focus_slots_ratios_and_tree_depth_are_rejected() {
        let left = Layout::Leaf(100);
        let right = Layout::Leaf(202);
        assert!(
            Workspace::capture([(&left, 100), (&left, 100)], 0, |_| Ok(SavedSource::Demo)).is_err()
        );
        // The focused leaf exists, but belongs to the other tab.
        assert!(
            Workspace::capture([(&left, 100), (&right, 100)], 0, |_| Ok(SavedSource::Demo))
                .is_err()
        );
        assert!(Workspace::capture([(&left, 100)], 1, |_| Ok(SavedSource::Demo)).is_err());
        assert!(Workspace::capture([], 0, |_| Ok(SavedSource::Demo)).is_err());
        let mut nested = left;
        for id in 200..(200 + MAX_DEPTH as u64 + 1) {
            nested.split(100, id, Axis::Rows);
        }
        assert!(Workspace::capture([(&nested, 100)], 0, |_| Ok(SavedSource::Demo)).is_err());
        let mut workspace = demo();
        for ratio in [0.0, 1.0, f64::NAN, f64::INFINITY] {
            workspace.tabs[0].layout = SavedLayout::Split {
                axis: Axis::Columns,
                ratio,
                first: Box::new(SavedLayout::Leaf {
                    slot: 1,
                    source: SavedSource::Demo,
                }),
                second: Box::new(SavedLayout::Leaf {
                    slot: 2,
                    source: SavedSource::Demo,
                }),
            };
            assert!(workspace.validate().is_err());
        }
    }

    #[test]
    fn save_preserves_unreadable_or_future_workspaces_and_respects_a_competing_writer() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("workspaces.json");
        let store = WorkspaceStore::new(path.clone());
        assert!(store.load().unwrap().is_empty());
        for bad in ["malformed", r#"{"version":2,"workspaces":{}}"#] {
            std::fs::write(&path, bad).unwrap();
            assert!(store.save("ops", &demo()).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), bad);
        }
        std::fs::write(&path, r#"{"version":1,"workspaces":{}}"#).unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))
            .unwrap();
        lock.try_lock().unwrap();
        assert!(store.save("ops", &demo()).is_err());
        drop(lock);
        assert!(store.save("   ", &demo()).is_err());
        store.save("ops", &demo()).unwrap();
        assert_eq!(store.load().unwrap()["ops"], demo());
    }

    #[test]
    fn workspace_round_trips_timed_replay_and_detects_a_missing_index() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cockpit.vt");
        crate::recorder::Recorder::start_timed(&path, wezterm_term::TerminalSize::default())
            .unwrap()
            .finish()
            .unwrap();
        let source = Source::TimedReplay(path.clone());
        let paused = Source::PausedReplay(path.clone());
        let paused_recipe = SavedSource::capture(&paused, None).unwrap();
        let paused_json = serde_json::to_value(&paused_recipe).unwrap();
        assert_eq!(paused_json["kind"], "paused_replay");
        let reopened: SavedSource = serde_json::from_value(paused_json).unwrap();
        assert_eq!(reopened.resolve(&BTreeMap::new()).unwrap(), paused);
        let saved = SavedSource::capture(&source, None).unwrap();
        let serialized = serde_json::to_value(&saved).unwrap();
        assert_eq!(serialized["kind"], "timed_replay");
        let restored: SavedSource = serde_json::from_value(serialized).unwrap();
        assert_eq!(restored.resolve(&BTreeMap::new()).unwrap(), source);
        std::fs::remove_file(path.with_extension("vt.jsonl")).unwrap();
        assert!(restored.resolve(&BTreeMap::new()).is_err());
        assert_eq!(
            SavedSource::Replay(path.clone())
                .resolve(&BTreeMap::new())
                .unwrap(),
            Source::Replay(path)
        );
    }

    #[test]
    fn ad_hoc_sources_keep_only_options_with_absolute_paths() {
        let mut options = ssh("dev.example");
        options.authentication = Authentication::Identity {
            identity: "keys/dev".into(),
        };
        let saved = SavedSource::capture(&Source::Ssh(options), None).unwrap();
        let Source::Ssh(options) = saved.resolve(&BTreeMap::new()).unwrap() else {
            panic!()
        };
        let Authentication::Identity { identity } = options.authentication else {
            panic!()
        };
        assert_eq!(identity, std::path::absolute("keys/dev").unwrap());
        assert!(SavedSource::capture(&Source::Replay("some-capture.vt".into()), None).is_ok());
    }
}
