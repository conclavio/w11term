use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
mod group_prompt;
mod local;
mod ssh_destination;
mod tab_color;
pub use group_prompt::GroupPromptPattern;
pub use local::LocalOptions;
pub use tab_color::TabColor;

/// Base colors for a new terminal pane. Applications can override them with OSC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorScheme {
    #[default]
    Default,
    OneHalfDark,
    OneHalfLight,
    FollowWindows,
}

impl ColorScheme {
    pub const ALL: [Self; 4] = [
        Self::Default,
        Self::OneHalfDark,
        Self::OneHalfLight,
        Self::FollowWindows,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "w11term (default)",
            Self::OneHalfDark => "One Half Dark",
            Self::OneHalfLight => "One Half Light",
            Self::FollowWindows => "Follow Windows",
        }
    }

    pub(crate) fn resolve(
        self,
        appearance: impl FnOnce() -> crate::windows_appearance::WindowsAppearance,
    ) -> ResolvedColorScheme {
        use crate::windows_appearance::WindowsAppearance;
        match self {
            Self::Default => ResolvedColorScheme::Default,
            Self::OneHalfDark => ResolvedColorScheme::OneHalfDark,
            Self::OneHalfLight => ResolvedColorScheme::OneHalfLight,
            Self::FollowWindows => match appearance() {
                WindowsAppearance::Light => ResolvedColorScheme::OneHalfLight,
                WindowsAppearance::Dark => ResolvedColorScheme::OneHalfDark,
            },
        }
    }

    pub(crate) fn is_default(&self) -> bool {
        *self == Self::Default
    }
}

/// Only concrete palettes enter a terminal's configuration and snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResolvedColorScheme {
    Default,
    OneHalfDark,
    OneHalfLight,
}

/// Number of physical rows a pane retains above its primary screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "u32")]
pub struct ScrollbackLines(u16);

impl ScrollbackLines {
    pub const MAX: u16 = 32_767;

    pub fn get(self) -> usize {
        usize::from(self.0)
    }

    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Default for ScrollbackLines {
    fn default() -> Self {
        Self(10_000)
    }
}

impl std::fmt::Display for ScrollbackLines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl TryFrom<u32> for ScrollbackLines {
    type Error = anyhow::Error;

    fn try_from(value: u32) -> Result<Self> {
        ensure!(
            value <= u32::from(Self::MAX),
            "Scrollback lines must be between 0 and {}",
            Self::MAX
        );
        Ok(Self(value as u16))
    }
}

impl std::str::FromStr for ScrollbackLines {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        value
            .trim()
            .parse::<u32>()
            .with_context(|| {
                format!(
                    "Enter a whole number of scrollback lines between 0 and {}",
                    Self::MAX
                )
            })?
            .try_into()
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "auth", rename_all = "kebab-case")]
pub enum Authentication {
    Agent,
    None,
    Identity { identity: PathBuf },
    Password,
    KeyboardInteractive,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlayerOptions {
    pub reconnect: bool,
    pub game_mode: bool,
    /// None follows game mode: compatible cursor-key scrolling outside games.
    pub alternate_scroll: Option<bool>,
    pub rustmush: bool,
    pub mute: bool,
    pub bell: String,
    pub sync_assets: Option<bool>,
    pub remote_sounds: Option<bool>,
    pub device_key: Option<PathBuf>,
}
impl Default for PlayerOptions {
    fn default() -> Self {
        Self {
            reconnect: true,
            game_mode: false,
            alternate_scroll: None,
            rustmush: false,
            mute: false,
            bell: "system".into(),
            sync_assets: None,
            remote_sounds: None,
            device_key: None,
        }
    }
}

impl PlayerOptions {
    pub fn sync_assets_enabled(&self) -> bool {
        self.sync_assets.unwrap_or(self.game_mode || self.rustmush)
    }
    pub fn remote_sounds_enabled(&self) -> bool {
        self.remote_sounds
            .unwrap_or(self.game_mode || self.rustmush)
    }
    pub fn alternate_scroll_enabled(&self) -> bool {
        self.alternate_scroll.unwrap_or(!self.game_mode)
    }
}

/// The terminfo name sent in SSH's PTY request.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String")]
pub struct TerminalType(Box<str>);

impl TerminalType {
    pub const DEFAULT: &str = "xterm-256color";
    pub const SIXEL: &str = "xterm-sixel";

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn is_default(&self) -> bool {
        self.as_str() == Self::DEFAULT
    }
}

impl Default for TerminalType {
    fn default() -> Self {
        Self(Self::DEFAULT.into())
    }
}

impl TryFrom<String> for TerminalType {
    type Error = anyhow::Error;

    fn try_from(name: String) -> Result<Self> {
        ensure!(
            name.len() <= 64
                && name
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b)),
            "Enter a terminal type of 1–64 ASCII characters starting with a letter or digit. Use letters, digits, dots, underscores, plus signs or hyphens."
        );
        Ok(Self(name.into_boxed_str()))
    }
}

/// The two IPv4 loopback ports offered by a profile’s forward manager.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
pub struct LocalForwardPreset {
    pub local_port: std::num::NonZeroU16,
    pub remote_port: std::num::NonZeroU16,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct SshOptions {
    pub host: String,
    pub user: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default, skip_serializing_if = "TerminalType::is_default")]
    pub terminal_type: TerminalType,
    #[serde(default, skip_serializing_if = "ScrollbackLines::is_default")]
    pub scrollback_lines: ScrollbackLines,
    #[serde(default, skip_serializing_if = "ColorScheme::is_default")]
    pub color_scheme: ColorScheme,
    #[serde(default, skip_serializing_if = "TabColor::is_default")]
    pub tab_color: TabColor,
    #[serde(flatten)]
    pub authentication: Authentication,
    pub host_key: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_host_key_alias"
    )]
    pub host_key_alias: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proxy_jump: Vec<JumpHost>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_forwards: Vec<LocalForwardPreset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_prompt: Option<GroupPromptPattern>,
    #[serde(default)]
    pub durable: bool,
    #[serde(default)]
    pub player: PlayerOptions,
}

pub const MAX_JUMPS: usize = 8;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct JumpHost {
    pub host: String,
    pub user: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(flatten)]
    pub authentication: Authentication,
    pub host_key: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_host_key_alias"
    )]
    pub host_key_alias: Option<String>,
}

impl From<JumpHost> for SshOptions {
    fn from(hop: JumpHost) -> Self {
        Self {
            host: hop.host,
            user: hop.user,
            port: hop.port,
            terminal_type: TerminalType::default(),
            scrollback_lines: ScrollbackLines::default(),
            color_scheme: ColorScheme::default(),
            tab_color: Default::default(),
            authentication: hop.authentication,
            host_key: hop.host_key,
            host_key_alias: hop.host_key_alias,
            proxy_jump: Vec::new(),
            local_forwards: Vec::new(),
            group_prompt: None,
            durable: false,
            player: PlayerOptions::default(),
        }
    }
}

/// A single literal lookup name that is safe to write to an OpenSSH known_hosts file.
pub fn validate_host_key_alias(alias: &str) -> Result<()> {
    // OpenSSH host patterns recognize only * and ? as wildcards. Brackets,
    // colons and internal punctuation remain literal, including in [host]:port.
    ensure!(
        !alias.is_empty()
            && !alias.starts_with(['!', '@', '#', '|'])
            && !alias
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || "*,?".contains(c)),
        "HostKeyAlias must be a single literal known_hosts name"
    );
    Ok(())
}

pub(crate) fn deserialize_host_key_alias<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error> {
    let mut alias = Option::<String>::deserialize(deserializer)?;
    if let Some(value) = &mut alias {
        validate_host_key_alias(value).map_err(serde::de::Error::custom)?;
        value.make_ascii_lowercase();
    }
    Ok(alias)
}

impl SshOptions {
    /// Validate a profile and resolve its key paths against the catalog location.
    pub fn prepare_profile_connection(&mut self, catalog: &Path) -> Result<()> {
        ensure!(
            crate::assets::name(&self.player.bell),
            "bell must be a sound name, system, or off"
        );
        ensure!(
            !self.host.is_empty() && !self.user.is_empty() && self.port != 0,
            "host, user, and port must be nonempty"
        );
        self.validate_route()?;
        let base = catalog.parent().unwrap_or(Path::new("."));
        if let Some(key) = &mut self.player.device_key
            && key.is_relative()
        {
            *key = base.join(&*key);
        }
        for auth in self.authentications_mut() {
            if let Authentication::Identity { identity } = auth {
                ensure!(!identity.as_os_str().is_empty(), "identity path is empty");
                if identity.is_relative() {
                    *identity = base.join(&*identity);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn validate_route(&self) -> Result<()> {
        ensure!(
            self.proxy_jump.len() <= MAX_JUMPS,
            "at most {MAX_JUMPS} SSH jump hosts are supported"
        );
        if let Some(alias) = &self.host_key_alias {
            validate_host_key_alias(alias)?;
        }
        for hop in &self.proxy_jump {
            if let Some(alias) = &hop.host_key_alias {
                validate_host_key_alias(alias)
                    .with_context(|| format!("jump host {}", hop.host))?;
            }
            ensure!(
                !hop.host.trim().is_empty() && !hop.user.trim().is_empty() && hop.port != 0,
                "an SSH jump host requires a host, user and port"
            );
        }
        Ok(())
    }

    pub(crate) fn authentications_mut(&mut self) -> impl Iterator<Item = &mut Authentication> {
        std::iter::once(&mut self.authentication).chain(
            self.proxy_jump
                .iter_mut()
                .map(|hop| &mut hop.authentication),
        )
    }
}

fn default_port() -> u16 {
    22
}

fn default_profiles_path() -> PathBuf {
    let local = PathBuf::from("profiles.json");
    if local.is_file() {
        return local;
    }
    // Also support double-clicking a development executable under target/debug or target/release.
    if let Ok(executable) = std::env::current_exe()
        && let Some(parent) = executable.parent()
        && let Some(path) = parent
            .ancestors()
            .take(3)
            .map(|dir| dir.join("profiles.json"))
            .find(|path| path.is_file())
    {
        return path;
    }
    local
}

pub type SshRecipientSets = BTreeMap<String, Vec<String>>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SshRosterOrder {
    Label,
    CheckedFirst,
    AttentionFirst,
    #[default]
    #[serde(other)]
    Tabs,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct SshRosterDefaults {
    pub order: SshRosterOrder,
    pub show_last_output: bool,
    pub require_all_ready: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Profiles {
    pub profiles: BTreeMap<String, SshOptions>,
    pub local: BTreeMap<String, LocalOptions>,
    /// Each available name once, including discovered local shells.
    pub profile_order: Vec<String>,
    pub startup_profile: Option<String>,
    pub ssh_host_groups: BTreeMap<String, Vec<String>>,
    pub ssh_recipient_sets: BTreeMap<String, SshRecipientSets>,
    pub ssh_host_group_defaults: BTreeMap<String, SshRosterDefaults>,
    pub(crate) has_explicit_order: bool,
}

impl Default for Profiles {
    fn default() -> Self {
        let local = LocalOptions::installed();
        Self {
            profile_order: local.keys().cloned().collect(),
            profiles: BTreeMap::new(),
            local,
            startup_profile: None,
            ssh_host_groups: BTreeMap::new(),
            ssh_recipient_sets: BTreeMap::new(),
            ssh_host_group_defaults: BTreeMap::new(),
            has_explicit_order: false,
        }
    }
}

impl Profiles {
    pub fn copy_name(&self, original: &str) -> String {
        let mut name = format!("{original} copy");
        let mut index = 2;
        while self.profiles.contains_key(&name) || self.local.contains_key(&name) {
            name = format!("{original} copy {index}");
            index += 1;
        }
        name
    }

    fn cli_order(&self) -> Vec<&String> {
        if self.has_explicit_order {
            self.profile_order.iter().collect()
        } else {
            // --list-profiles historically prints local shells first.
            self.local.keys().chain(self.profiles.keys()).collect()
        }
    }

    pub fn source(&self, name: &str) -> Option<Source> {
        profile_source(&self.profiles, &self.local, name)
    }

    pub fn ssh_host_group_member(&self, name: &str) -> Option<&SshOptions> {
        self.profiles
            .get(name)
            .filter(|options| !options.durable && !options.player.rustmush)
    }

    pub fn resolve_ssh_host_group_members<'a>(
        &'a self,
        members: &[String],
    ) -> Result<Vec<&'a SshOptions>> {
        ensure!(!members.is_empty(), "Choose at least one SSH profile");
        let mut seen = std::collections::HashSet::new();
        members
            .iter()
            .map(|name| {
                ensure!(
                    seen.insert(name),
                    "SSH profile {name} appears more than once"
                );
                self.ssh_host_group_member(name)
                    .with_context(|| format!("SSH profile {name} is unavailable for groups"))
            })
            .collect()
    }
}

pub fn profile_source(
    ssh: &BTreeMap<String, SshOptions>,
    local: &BTreeMap<String, LocalOptions>,
    name: &str,
) -> Option<Source> {
    ssh.get(name)
        .cloned()
        .map(Source::Ssh)
        .or_else(|| local.get(name).cloned().map(Source::Local))
}

pub(crate) fn load_profiles(path: &Path) -> Result<Profiles> {
    let bytes = match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Profiles::default());
        }
        result => result.with_context(|| format!("reading {}", path.display()))?,
    };
    parse_profiles(&bytes, path)
}

pub(crate) fn parse_profiles(bytes: &[u8], path: &Path) -> Result<Profiles> {
    #[derive(Deserialize)]
    struct Document {
        profiles: BTreeMap<String, serde_json::Value>,
        #[serde(default)]
        profile_order: Option<serde_json::Value>,
        #[serde(default)]
        startup_profile: Option<String>,
        #[serde(default)]
        ssh_host_groups: BTreeMap<String, Vec<String>>,
        #[serde(default)]
        ssh_recipient_sets: BTreeMap<String, SshRecipientSets>,
        #[serde(default)]
        ssh_host_group_defaults: BTreeMap<String, SshRosterDefaults>,
    }
    let document: Document =
        serde_json::from_slice(bytes).with_context(|| format!("parsing {}", path.display()))?;
    let explicit_order: Option<Vec<String>> = document
        .profile_order
        .map(serde_json::from_value)
        .transpose()
        .context("profile_order must be an array of profile names")?;
    let mut profiles = Profiles {
        has_explicit_order: explicit_order.is_some(),
        startup_profile: document.startup_profile,
        ssh_host_groups: document.ssh_host_groups,
        ssh_recipient_sets: document.ssh_recipient_sets,
        ssh_host_group_defaults: document.ssh_host_group_defaults,
        ..Profiles::default()
    };
    for (name, value) in document.profiles {
        if value.get("backend").and_then(|v| v.as_str()) == Some("local") {
            let mut local: LocalOptions =
                serde_json::from_value(value).with_context(|| format!("local profile {name}"))?;
            local.resolve_paths(path)?;
            profiles.local.insert(name, local);
        } else {
            let ssh =
                serde_json::from_value(value).with_context(|| format!("SSH profile {name}"))?;
            profiles.local.remove(&name);
            profiles.profiles.insert(name, ssh);
        }
    }
    for (name, ssh) in &mut profiles.profiles {
        ssh.prepare_profile_connection(path)
            .with_context(|| format!("profile {name}"))?;
    }
    let mut seen = std::collections::HashSet::new();
    let known = explicit_order
        .iter()
        .flatten()
        .filter(|name| profiles.profiles.contains_key(*name) || profiles.local.contains_key(*name));
    profiles.profile_order = known
        .chain(profiles.profiles.keys())
        .chain(profiles.local.keys())
        .filter(|name| seen.insert(name.as_str()))
        .cloned()
        .collect();
    Ok(profiles)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Demo,
    Replay(PathBuf),
    /// A raw capture opened in the recording viewer, with desktop effects suppressed.
    ViewRecording(PathBuf),
    TimedReplay(PathBuf),
    PausedReplay(PathBuf),
    Local(LocalOptions),
    Ssh(SshOptions),
}

impl Source {
    pub fn start_paused(self) -> Result<Self> {
        match self {
            Self::TimedReplay(path) => Ok(Self::PausedReplay(path)),
            _ => bail!("Starting paused requires a timed recording with its .vt.jsonl file"),
        }
    }

    /// Choose the file's playback mode before opening a tab. An incomplete
    /// timed capture must report its error instead of silently skipping its index.
    pub fn open_recording(path: PathBuf) -> Result<Self> {
        let path = std::path::absolute(path)?;
        crate::recorder::check_raw_file(&path)?;
        if path.with_extension("vt.jsonl").try_exists()? {
            crate::recorder::TimedReplay::read_initial_size(&path)?;
            Ok(Self::TimedReplay(path))
        } else {
            crate::recorder::raw_grid(&path)?;
            Ok(Self::ViewRecording(path))
        }
    }

    pub fn color_scheme(&self) -> ColorScheme {
        match self {
            Self::Local(shell) => shell.color_scheme,
            Self::Ssh(ssh) => ssh.color_scheme,
            Self::Demo
            | Self::Replay(_)
            | Self::ViewRecording(_)
            | Self::TimedReplay(_)
            | Self::PausedReplay(_) => ColorScheme::default(),
        }
    }

    pub fn tab_color(&self) -> TabColor {
        match self {
            Self::Local(shell) => shell.tab_color,
            Self::Ssh(ssh) => ssh.tab_color,
            _ => TabColor::None,
        }
    }

    pub fn scrollback_lines(&self) -> ScrollbackLines {
        match self {
            Self::Local(shell) => shell.scrollback_lines,
            Self::Ssh(ssh) => ssh.scrollback_lines,
            Self::Demo
            | Self::Replay(_)
            | Self::ViewRecording(_)
            | Self::TimedReplay(_)
            | Self::PausedReplay(_) => ScrollbackLines::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub source: Source,
    pub workspace: Option<String>,
    pub switch_workspace: bool,
    pub benchmark: bool,
    pub smoke_seconds: Option<u64>,
    pub profiles_path: PathBuf,
    pub initial_profile: Option<String>,
    pub extra_profiles: Vec<String>,
    pub cols: Option<usize>,
    pub rows: Option<usize>,
}

impl Options {
    pub fn parse() -> Result<Option<Self>> {
        Self::parse_from(std::env::args().skip(1))
    }

    pub fn parse_from(args: impl IntoIterator<Item = String>) -> Result<Option<Self>> {
        let mut args = args.into_iter();
        let mut source = Source::Demo;
        let mut profile = None;
        let mut workspace_name = None;
        let mut switch_workspace = false;
        let mut explicit_source = false;
        let mut profiles_path = default_profiles_path();
        let mut list_profiles = false;
        let mut paused = false;
        let mut port = None;
        let mut authentication = None;
        let mut host_key = None;
        let mut host_key_alias = None;
        let mut benchmark = false;
        let mut smoke_seconds = None;
        let mut extra_profiles = Vec::new();
        let mut cols = None;
        let mut rows = None;
        while let Some(arg) = args.next() {
            explicit_source |= matches!(
                arg.as_str(),
                "--demo" | "--replay" | "--replay-timed" | "--profile" | "ssh" | "--tab"
            );
            match arg.as_str() {
                "--help" | "-h" => {
                    println!(
                        "w11term — Rust / Reactor SSH terminal prototype\n\n  w11term [--demo]\n  w11term --profile root|play-6789|play-6792 [--cols 170 --rows 48]\n  w11term --profile root --tab play-6789 --tab play-6792\n  w11term --list-profiles [--profiles profiles.json]\n  w11term --workspace NAME [--profiles profiles.json]\n  w11term --switch-workspace NAME [--profiles profiles.json]\n  w11term --replay capture.vt\n  w11term --replay-timed capture.vt [--paused]\n  w11term ssh user@host[:port] [--port 22] [--identity KEY | --auth none|agent|password|keyboard-interactive]\n      [--host-key SHA256:...] [--host-key-alias NAME]\n  w11term --benchmark\n  w11term --smoke-seconds 5\n\nAutomation: w11term-cli --help (named pipe, JSON replies, no focus changes).\nProfiles load from profiles.json in the working directory, or --profiles PATH.\nRelative identity paths resolve beside that file.\nSSH verifies OpenSSH known_hosts, or an explicitly supplied fingerprint.\nSSH destinations also accept ssh://user@host[:port] and bracketed IPv6.\nAuthentication defaults to the Windows OpenSSH agent; --auth none sends no key.\nCtrl+Shift+V: paste; Shift+PageUp/PageDown: scrollback; F5: reload demo."
                    );
                    return Ok(None);
                }
                "--demo" => {
                    source = Source::Demo;
                    profile = None;
                }
                "--replay" => {
                    source = Source::Replay(value(&mut args, &arg)?.into());
                    profile = None;
                }
                "--replay-timed" => {
                    source = Source::TimedReplay(value(&mut args, &arg)?.into());
                    profile = None;
                }
                "--profile" => profile = Some(value(&mut args, &arg)?),
                "--workspace" | "--switch-workspace" => {
                    ensure!(workspace_name.is_none(), "Choose one workspace");
                    workspace_name = Some(value(&mut args, &arg)?);
                    switch_workspace = arg == "--switch-workspace";
                }
                "--tab" => extra_profiles.push(value(&mut args, &arg)?),
                "--cols" => cols = Some(value(&mut args, &arg)?.parse::<usize>()?),
                "--rows" => rows = Some(value(&mut args, &arg)?.parse::<usize>()?),
                "--profiles" => profiles_path = value(&mut args, &arg)?.into(),
                "--list-profiles" => list_profiles = true,
                "--paused" => paused = true,
                "--benchmark" => benchmark = true,
                "--smoke-seconds" => smoke_seconds = Some(value(&mut args, &arg)?.parse()?),
                "ssh" => {
                    let target = value(&mut args, &arg)?;
                    profile = None;
                    source = Source::Ssh(SshOptions::from_destination(&target)?);
                }
                "--port" => port = Some(value(&mut args, &arg)?.parse::<u16>()?),
                "--identity" => {
                    ensure!(
                        authentication.is_none(),
                        "choose only one of --identity and --auth"
                    );
                    authentication = Some(Authentication::Identity {
                        identity: value(&mut args, &arg)?.into(),
                    });
                }
                "--auth" => {
                    ensure!(
                        authentication.is_none(),
                        "choose only one of --identity and --auth"
                    );
                    authentication = Some(match value(&mut args, &arg)?.as_str() {
                        "none" => Authentication::None,
                        "agent" => Authentication::Agent,
                        "password" => Authentication::Password,
                        "keyboard-interactive" => Authentication::KeyboardInteractive,
                        other => bail!(
                            "unknown authentication method {other}; use none, agent, password, keyboard-interactive, or --identity KEY"
                        ),
                    });
                }
                "--host-key" => host_key = Some(value(&mut args, &arg)?),
                "--host-key-alias" => {
                    let alias = value(&mut args, &arg)?;
                    validate_host_key_alias(&alias)?;
                    host_key_alias = Some(alias.to_ascii_lowercase());
                }
                _ => bail!("unknown argument: {arg}; use --help"),
            }
        }
        ensure!(
            workspace_name.is_none() || !(explicit_source || list_profiles || benchmark),
            "--workspace cannot be combined with a terminal source, --tab, --list-profiles or --benchmark"
        );
        ensure!(
            !paused
                || (!list_profiles
                    && workspace_name.is_none()
                    && matches!(&source, Source::TimedReplay(_))
                    && profile.is_none()),
            "--paused requires --replay-timed"
        );
        if paused {
            source = source.start_paused()?;
        }
        if list_profiles {
            let catalog = load_profiles(&profiles_path)?;
            for name in catalog.cli_order() {
                if let Some(shell) = catalog.local.get(name) {
                    println!("{name:16} {} (local)", shell.executable.display());
                } else if let Some(ssh) = catalog.profiles.get(name) {
                    println!(
                        "{name:16} {}@{}:{} ({})",
                        ssh.user,
                        ssh.host,
                        ssh.port,
                        match ssh.authentication {
                            Authentication::None => "none",
                            Authentication::Agent => "agent",
                            Authentication::Identity { .. } => "identity",
                            Authentication::Password => "password",
                            Authentication::KeyboardInteractive => "keyboard-interactive",
                        }
                    );
                }
            }
            return Ok(None);
        }
        let ssh_override = port.is_some()
            || authentication.is_some()
            || host_key.is_some()
            || host_key_alias.is_some();
        let mut initial_profile = profile.clone().filter(|_| !ssh_override);
        if !extra_profiles.is_empty() {
            let profiles = load_profiles(&profiles_path)?;
            for name in &extra_profiles {
                ensure!(profiles.source(name).is_some(), "unknown profile {name}");
            }
        }
        if let Some(name) = profile {
            source = load_profiles(&profiles_path)?
                .source(&name)
                .with_context(|| format!("unknown profile {name}; use --list-profiles"))?;
        } else if !explicit_source
            && !ssh_override
            && workspace_name.is_none()
            && !benchmark
            && smoke_seconds.is_none()
            && let Ok(profiles) = load_profiles(&profiles_path)
            && let Some(name) = &profiles.startup_profile
            && let Some(startup) = profiles.source(name)
        {
            source = startup;
            initial_profile = Some(name.clone());
        }
        if let Source::Ssh(ssh) = &mut source {
            if let Some(port) = port {
                ssh.port = port;
            }
            if let Some(authentication) = authentication {
                ssh.authentication = authentication;
            }
            if let Some(host_key) = host_key {
                ssh.host_key = Some(host_key);
            }
            if let Some(alias) = host_key_alias {
                ssh.host_key_alias = Some(alias);
            }
            ensure!(ssh.port != 0, "SSH port must be nonzero");
        } else {
            ensure!(
                !ssh_override,
                "SSH options require ssh user@host or --profile NAME"
            );
        }
        if let Source::TimedReplay(path) | Source::PausedReplay(path) = &source {
            let size = crate::recorder::TimedReplay::read_initial_size(path)?;
            // The initial window may be smaller than the recorded terminal grid.
            cols.get_or_insert(size.cols.min(1000));
            rows.get_or_insert(size.rows.min(500));
        } else if let Source::Replay(path) = &source {
            let metadata = path.with_extension("vt.json");
            if metadata.is_file() {
                let metadata: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&metadata)?)
                        .context("reading VT recording geometry")?;
                ensure!(
                    metadata["version"] == 1,
                    "unsupported recording metadata version"
                );
                if cols.is_none() {
                    cols = Some(usize::try_from(
                        metadata["cols"]
                            .as_u64()
                            .context("invalid recording columns")?,
                    )?);
                }
                if rows.is_none() {
                    rows = Some(usize::try_from(
                        metadata["rows"]
                            .as_u64()
                            .context("invalid recording rows")?,
                    )?);
                }
            }
        }
        ensure!(
            cols.is_none_or(|n| (1..=1000).contains(&n)),
            "--cols must be 1..1000"
        );
        ensure!(
            rows.is_none_or(|n| (1..=500).contains(&n)),
            "--rows must be 1..500"
        );
        Ok(Some(Self {
            source,
            workspace: workspace_name,
            switch_workspace,
            benchmark,
            smoke_seconds,
            profiles_path,
            initial_profile,
            extra_profiles,
            cols,
            rows,
        }))
    }
}

fn value(args: &mut impl Iterator<Item = String>, option: &str) -> Result<String> {
    args.next()
        .ok_or_else(|| anyhow::anyhow!("{option} requires a value"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_key_alias_is_one_literal_identity_on_the_destination_or_each_hop() {
        for valid in [
            "router",
            "MiXeD.example.",
            "127.0.0.1",
            "fe80::1%12",
            "[2001:db8::1]:2222",
            "[host]:22",
            "[::1]",
            "route+name=3",
            "cluster@blue!node#3|old",
            "[legacy]:blue",
            "%h-%p",
            "none",
        ] {
            assert!(validate_host_key_alias(valid).is_ok(), "{valid:?}");
        }
        for bad in [
            "",
            "prefix,suffix",
            "@revoked",
            "#comment",
            "|1|hash",
            "!inverse",
            "wild*card",
            "wild?",
            "a b",
            "a\nb",
            "a\0b",
        ] {
            assert!(validate_host_key_alias(bad).is_err(), "{bad:?}");
        }
        let ssh = serde_json::json!({"host":"127.0.0.1","user":"pilot","auth":"none"});
        let parse = |value: serde_json::Value| {
            parse_profiles(
                &serde_json::to_vec(&serde_json::json!({"profiles":{"test":value}})).unwrap(),
                Path::new("profiles.json"),
            )
        };
        let old = parse(ssh.clone()).unwrap();
        assert!(old.profiles["test"].host_key_alias.is_none());
        assert!(
            serde_json::to_value(&old.profiles["test"])
                .unwrap()
                .get("host_key_alias")
                .is_none()
        );
        let mut dest = ssh.clone();
        dest["host_key_alias"] = "Cockpit".into();
        let mut hop = ssh;
        hop["host_key_alias"] = "[GATE]:2202".into();
        dest["proxy_jump"] = serde_json::json!([hop]);
        let document = parse(dest.clone()).unwrap();
        let options = &document.profiles["test"];
        assert_eq!(options.host_key_alias.as_deref(), Some("cockpit"));
        assert_eq!(
            SshOptions::from(options.proxy_jump[0].clone())
                .host_key_alias
                .as_deref(),
            Some("[gate]:2202")
        );
        dest["proxy_jump"][0]["host_key_alias"] = "*.example.com".into();
        assert!(parse(dest.clone()).is_err());
        dest["proxy_jump"] = serde_json::json!([]);
        dest["host_key_alias"] = "!host".into();
        assert!(parse(dest).is_err());
    }

    #[test]
    fn host_key_alias_cli_preserves_endpoint_and_makes_profile_override_ad_hoc() {
        let parse = |args: &[&str]| Options::parse_from(args.iter().map(|s| s.to_string()));
        let launch = parse(&[
            "ssh",
            "pilot@localhost",
            "--port",
            "2200",
            "--host-key-alias",
            "DEV",
        ])
        .unwrap()
        .unwrap();
        let Source::Ssh(ssh) = launch.source else {
            panic!("expected SSH")
        };
        assert_eq!(
            (ssh.host.as_str(), ssh.port, ssh.host_key_alias.as_deref()),
            ("localhost", 2200, Some("dev"))
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profiles.json");
        std::fs::write(&path, r#"{"profiles":{"pilot":{"host":"localhost","user":"pilot","auth":"none","host_key_alias":"old"}}}"#).unwrap();
        let launch = parse(&[
            "--profiles",
            path.to_str().unwrap(),
            "--profile",
            "pilot",
            "--host-key-alias",
            "[work]:2233",
        ])
        .unwrap()
        .unwrap();
        assert!(launch.initial_profile.is_none());
        let Source::Ssh(ssh) = launch.source else {
            panic!("expected SSH")
        };
        assert_eq!(ssh.host_key_alias.as_deref(), Some("[work]:2233"));
        assert!(parse(&["ssh", "pilot@localhost", "--host-key-alias", "*"]).is_err());
        assert!(parse(&["--demo", "--host-key-alias", "work"]).is_err());
    }

    #[test]
    fn normalized_menu_and_cli_orders_preserve_their_defaults_without_an_explicit_order() {
        let mut doc = serde_json::json!({"profiles":{
            "Zulu":{"host":"z.test","user":"z","auth":"none"},
            "Alpha":{"host":"a.test","user":"a","auth":"none"},
            "Lima":{"backend":"local","executable":"cmd.exe"}
        }});
        let read = |doc: &serde_json::Value| {
            parse_profiles(
                &serde_json::to_vec(doc).unwrap(),
                Path::new("profiles.json"),
            )
        };
        let default = read(&doc).unwrap();
        assert_eq!(
            default.profile_order,
            default
                .profiles
                .keys()
                .chain(default.local.keys())
                .cloned()
                .collect::<Vec<_>>()
        );
        assert_eq!(
            default.cli_order(),
            default
                .local
                .keys()
                .chain(default.profiles.keys())
                .collect::<Vec<_>>()
        );
        doc["profile_order"] = serde_json::json!(["Lima", "missing", "Zulu", "Lima"]);
        let custom = read(&doc).unwrap();
        assert_eq!(&custom.profile_order[..3], &["Lima", "Zulu", "Alpha"]);
        assert_eq!(
            custom.cli_order(),
            custom.profile_order.iter().collect::<Vec<_>>()
        );
        assert_eq!(custom.profile_order.len(), default.profile_order.len());
        for bad in [serde_json::json!("Lima"), serde_json::json!(["Zulu", 42])] {
            doc["profile_order"] = bad;
            assert!(
                read(&doc)
                    .unwrap_err()
                    .to_string()
                    .contains("profile_order must be an array of profile names")
            );
        }
    }

    #[test]
    fn startup_profile_applies_only_to_implicit_launch_and_falls_back_when_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let mut doc = serde_json::json!({
            "profiles":{
                "Remote":{"host":"ssh.test","user":"pilot","auth":"none"},
                "Local":{"backend":"local","executable":"cmd.exe"}
            },
            "startup_profile":"Remote"
        });
        let parse = |args: &[&str]| {
            Options::parse_from(
                ["--profiles", path.to_str().unwrap()]
                    .into_iter()
                    .chain(args.iter().copied())
                    .map(str::to_owned),
            )
            .unwrap()
            .unwrap()
        };
        std::fs::write(&path, doc.to_string()).unwrap();
        let implicit = parse(&["--cols", "100", "--rows", "30"]);
        assert!(matches!(implicit.source, Source::Ssh(ssh) if ssh.host == "ssh.test"));
        assert_eq!(implicit.initial_profile.as_deref(), Some("Remote"));
        assert_eq!((implicit.cols, implicit.rows), (Some(100), Some(30)));
        assert!(matches!(
            parse(&["--profile", "Local"]).source,
            Source::Local(_)
        ));
        for args in [
            &["--demo"][..],
            &["--tab", "Local"],
            &["--workspace", "desk"],
            &["--smoke-seconds", "1"],
            &["--benchmark"],
        ] {
            assert_eq!(parse(args).source, Source::Demo);
        }
        assert_eq!(
            parse(&["--replay", "sample.vt"]).source,
            Source::Replay("sample.vt".into())
        );

        doc["startup_profile"] = "Local".into();
        std::fs::write(&path, doc.to_string()).unwrap();
        assert!(matches!(parse(&[]).source, Source::Local(_)));
        doc["startup_profile"] = "unavailable".into();
        std::fs::write(&path, doc.to_string()).unwrap();
        assert_eq!(parse(&[]).source, Source::Demo);
        std::fs::write(&path, b"{").unwrap();
        assert_eq!(parse(&[]).source, Source::Demo);
    }

    #[test]
    fn a_workspace_is_the_only_launch_source_and_overridden_profiles_are_ad_hoc() {
        let parse = |args: &[&str]| Options::parse_from(args.iter().map(|s| s.to_string()));
        for source in [
            vec!["--demo"],
            vec!["--profile", "root"],
            vec!["--tab", "root"],
            vec!["ssh", "alice@localhost"],
        ] {
            let mut args = vec!["--workspace", "desk"];
            args.extend(source);
            assert!(
                parse(&args)
                    .unwrap_err()
                    .to_string()
                    .contains("--workspace cannot")
            );
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profiles.json");
        std::fs::write(&path, include_str!("../profiles.json")).unwrap();
        let mut args = vec!["--profiles", path.to_str().unwrap(), "--profile", "root"];
        assert_eq!(
            parse(&args).unwrap().unwrap().initial_profile.as_deref(),
            Some("root")
        );
        args.extend(["--port", "2222"]);
        let launch = parse(&args).unwrap().unwrap();
        assert!(launch.initial_profile.is_none());
        assert!(matches!(launch.source, Source::Ssh(ssh) if ssh.port == 2222));
    }

    #[test]
    fn remote_extensions_follow_game_profiles_with_explicit_overrides() {
        let mut options = PlayerOptions::default();
        assert!(!options.sync_assets_enabled() && !options.remote_sounds_enabled());
        options.game_mode = true;
        assert!(options.sync_assets_enabled() && options.remote_sounds_enabled());
        options.sync_assets = Some(false);
        options.remote_sounds = Some(false);
        assert!(!options.sync_assets_enabled() && !options.remote_sounds_enabled());
        options.game_mode = false;
        options.sync_assets = Some(true);
        options.remote_sounds = Some(true);
        assert!(options.sync_assets_enabled() && options.remote_sounds_enabled());
    }
    #[test]
    fn profile_paths_are_relative_to_config_and_games_never_offer_keys() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("profiles.json");
        std::fs::write(&config, include_str!("../profiles.json")).unwrap();
        let profiles = load_profiles(&config).unwrap().profiles;
        assert_eq!(
            profiles["root"].authentication,
            Authentication::Identity {
                identity: directory.path().join("hostinger")
            }
        );
        for (name, port) in [("play-6789", 6789), ("play-6792", 6792)] {
            assert_eq!(profiles[name].authentication, Authentication::None);
            assert_eq!(profiles[name].port, port);
            assert_eq!(profiles[name].user, "play");
            assert!(
                !profiles[name].player.alternate_scroll_enabled(),
                "bundled game profiles must not turn the camera with wheel-generated arrows"
            );
        }
    }

    #[test]
    fn ssh_terminal_types_round_trip_and_reject_invalid_names() {
        let legacy = serde_json::json!({"host":"server","user":"pilot","auth":"none"});
        let profile: SshOptions = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(profile.terminal_type.as_str(), TerminalType::DEFAULT);
        assert!(
            serde_json::to_value(profile)
                .unwrap()
                .get("terminal_type")
                .is_none()
        );
        for name in ["xterm-sixel", "screen-256color", "My_TERM.1+test"] {
            let mut value = legacy.clone();
            value["terminal_type"] = name.into();
            let profile: SshOptions = serde_json::from_value(value).unwrap();
            assert_eq!(profile.terminal_type.as_str(), name);
            assert_eq!(
                serde_json::to_value(profile).unwrap()["terminal_type"],
                name
            );
        }
        for invalid in [
            "",
            "-x",
            "a/b",
            "x term",
            "x\0y",
            "x\n",
            "térm",
            &"a".repeat(65),
        ] {
            let mut value = legacy.clone();
            value["terminal_type"] = invalid.into();
            assert!(
                serde_json::from_value::<SshOptions>(value).is_err(),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn explicit_auth_modes_and_invalid_combinations() {
        let parse = |args: &[&str]| Options::parse_from(args.iter().map(|s| s.to_string()));
        let options = parse(&[
            "ssh",
            "ssh://play@conclav.io:6792",
            "--auth",
            "none",
            "--port",
            "6789",
        ])
        .unwrap()
        .unwrap();
        let Source::Ssh(ssh) = options.source else {
            panic!("expected SSH")
        };
        assert_eq!(ssh.authentication, Authentication::None);
        assert_eq!(ssh.port, 6789);
        assert!(parse(&["ssh", "a@b", "--auth", "none", "--identity", "key"]).is_err());
        assert!(parse(&["--demo", "--auth", "none"]).is_err());
        assert!(parse(&["ssh", "a@[]"]).is_err());
    }

    #[test]
    fn timed_launch_uses_recorded_geometry_and_keeps_the_viewport_override() {
        let parse = |args: &[&str]| Options::parse_from(args.iter().map(|s| s.to_string()));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bug.vt");
        let file = path.to_str().unwrap();
        assert!(parse(&["--replay-timed", file]).is_err());
        let size = wezterm_term::TerminalSize {
            cols: 91,
            rows: 37,
            ..Default::default()
        };
        crate::recorder::Recorder::start_timed(&path, size)
            .unwrap()
            .finish()
            .unwrap();
        let launch = parse(&["--replay-timed", file]).unwrap().unwrap();
        assert_eq!(launch.source, Source::TimedReplay(path.clone()));
        for args in [
            vec!["--replay-timed", file, "--paused"],
            vec!["--paused", "--replay-timed", file],
        ] {
            let paused = parse(&args).unwrap().unwrap();
            assert_eq!(paused.source, Source::PausedReplay(path.clone()));
            assert_eq!((paused.cols, paused.rows), (Some(91), Some(37)));
        }
        for args in [
            vec!["--paused"],
            vec!["--demo", "--paused"],
            vec!["--paused", "--replay", file],
        ] {
            assert!(parse(&args).is_err());
        }
        assert_eq!((launch.cols, launch.rows), (Some(91), Some(37)));
        let launch = parse(&["--cols", "73", "--replay-timed", file])
            .unwrap()
            .unwrap();
        assert_eq!((launch.cols, launch.rows), (Some(73), Some(37)));
        assert!(parse(&["--replay-timed", file, "--workspace", "ops"]).is_err());
        let raw = parse(&["--replay", file]).unwrap().unwrap();
        assert_eq!(
            (raw.source, raw.cols, raw.rows),
            (Source::Replay(path), Some(91), Some(37))
        );
    }

    #[test]
    fn launch_geometry_is_bounded_and_independent_of_source() {
        let parse = |args: &[&str]| Options::parse_from(args.iter().map(|s| s.to_string()));
        let options = parse(&["--cols", "170", "--rows", "48", "--demo"])
            .unwrap()
            .unwrap();
        assert_eq!((options.cols, options.rows), (Some(170), Some(48)));
        assert!(parse(&["--cols", "0"]).is_err());
        assert!(parse(&["--rows", "501"]).is_err());
    }
    #[test]
    fn alternate_scroll_defaults_follow_game_mode_and_explicit_profile_policy_wins() {
        let normal: PlayerOptions = serde_json::from_str("{}").unwrap();
        assert!(normal.alternate_scroll_enabled());
        let game: PlayerOptions = serde_json::from_str(r#"{"game_mode":true}"#).unwrap();
        assert!(!game.alternate_scroll_enabled());
        for (game, setting) in [(true, true), (true, false), (false, true), (false, false)] {
            let options: PlayerOptions = serde_json::from_value(
                serde_json::json!({"game_mode":game,"alternate_scroll":setting}),
            )
            .unwrap();
            assert_eq!(options.alternate_scroll_enabled(), setting);
            let restored: PlayerOptions =
                serde_json::from_value(serde_json::to_value(&options).unwrap()).unwrap();
            assert_eq!(restored, options);
        }
    }
}
