use crate::diagnostic_history::{Artifact, Kind as DiagnosticKind};
use anyhow::{Context, Result, bail, ensure};
use russh::keys::{HashAlg, PublicKey, ssh_key::Fingerprint};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU32,
    path::{Path, PathBuf},
    time::Duration,
};
mod group_prompt;
mod local;
mod profile_text_size;
mod ssh_destination;
mod tab_color;
pub use group_prompt::GroupPromptPattern;
pub use local::LocalOptions;
pub use profile_text_size::ProfileTextSize;
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
    AgentKey {
        agent_fingerprint: AgentFingerprint,
    },
    AgentCertificate {
        certificate: Box<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_fingerprint: Option<AgentFingerprint>,
    },
    None,
    Identity {
        identity: Box<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        certificate: Option<Box<PathBuf>>,
    },
    Password,
    KeyboardInteractive,
}

/// An explicitly selected plain SSH agent key, identified by its OpenSSH SHA256 fingerprint.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(try_from = "String")]
pub struct AgentFingerprint(Box<str>);

/// A local Windows SSH agent pipe or an explicit request for SSH_AUTH_SOCK.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(try_from = "String")]
pub struct AgentPipe(Box<str>);

impl AgentPipe {
    pub const ENVIRONMENT: &str = "SSH_AUTH_SOCK";
    const LOCAL_PREFIX: &str = r"\\.\pipe\";

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn optional(value: &str) -> Result<Option<Self>> {
        let value = value.trim();
        (!value.is_empty()).then(|| value.parse()).transpose()
    }
}

impl std::str::FromStr for AgentPipe {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let value = value.trim();
        if matches!(value, "SSH_AUTH_SOCK" | "$SSH_AUTH_SOCK") {
            return Ok(Self(Self::ENVIRONMENT.into()));
        }
        let prefix = Self::LOCAL_PREFIX;
        let Some(name) = value.get(prefix.len()..).filter(|_| {
            value
                .get(..prefix.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
        }) else {
            bail!(r"Enter a local Windows SSH agent pipe (\\.\pipe\name) or SSH_AUTH_SOCK");
        };
        ensure!(
            !name.is_empty()
                && !name.ends_with('.')
                && !name
                    .chars()
                    .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
                && value.encode_utf16().count() <= 256,
            "Invalid Windows SSH agent pipe name"
        );
        Ok(Self(format!("{prefix}{name}").into_boxed_str()))
    }
}

impl TryFrom<String> for AgentPipe {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        value.parse()
    }
}

impl AgentFingerprint {
    pub(crate) fn for_key(key: &PublicKey) -> Self {
        Self(
            key.fingerprint(HashAlg::Sha256)
                .to_string()
                .into_boxed_str(),
        )
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for AgentFingerprint {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let Ok(fingerprint @ Fingerprint::Sha256(_)) = value.trim().parse() else {
            bail!("Enter an OpenSSH SHA256 fingerprint for the SSH agent key");
        };
        Ok(Self(fingerprint.to_string().into_boxed_str()))
    }
}

impl TryFrom<String> for AgentFingerprint {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        value.parse()
    }
}

impl Authentication {
    pub fn uses_agent(&self) -> bool {
        matches!(
            self,
            Self::Agent | Self::AgentKey { .. } | Self::AgentCertificate { .. }
        )
    }

    pub fn identity(identity: impl Into<PathBuf>, certificate: Option<PathBuf>) -> Self {
        Self::Identity {
            identity: Box::new(identity.into()),
            certificate: certificate.map(Box::new),
        }
    }

    pub fn agent_certificate(
        certificate: impl Into<PathBuf>,
        agent_fingerprint: Option<AgentFingerprint>,
    ) -> Self {
        Self::AgentCertificate {
            certificate: Box::new(certificate.into()),
            agent_fingerprint,
        }
    }

    pub(crate) fn identity_path(&self) -> Option<&Path> {
        match self {
            Self::Identity { identity, .. } => Some(identity.as_path()),
            _ => None,
        }
    }

    pub(crate) fn certificate_path(&self) -> Option<&Path> {
        match self {
            Self::Identity { certificate, .. } => certificate.as_deref().map(PathBuf::as_path),
            Self::AgentCertificate { certificate, .. } => Some(certificate.as_path()),
            _ => None,
        }
    }

    pub(crate) fn credential_paths_mut(&mut self) -> impl Iterator<Item = &mut PathBuf> {
        let (identity, certificate) = match self {
            Self::Identity {
                identity,
                certificate,
            } => (Some(identity.as_mut()), certificate.as_deref_mut()),
            Self::AgentCertificate { certificate, .. } => (None, Some(certificate.as_mut())),
            _ => (None, None),
        };
        identity.into_iter().chain(certificate)
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlayerOptions {
    pub reconnect: bool,
    #[serde(skip_serializing_if = "default_reconnect_window")]
    pub reconnect_window_seconds: NonZeroU32,
    pub game_mode: bool,
    /// None follows game mode: compatible cursor-key scrolling outside games.
    pub alternate_scroll: Option<bool>,
    pub rustmush: bool,
    pub mute: bool,
    pub bell: String,
    pub sync_assets: Option<bool>,
    pub remote_sounds: Option<bool>,
    pub device_key: Option<Box<PathBuf>>,
}
impl Default for PlayerOptions {
    fn default() -> Self {
        Self {
            reconnect: true,
            reconnect_window_seconds: DEFAULT_RECONNECT_WINDOW,
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

const DEFAULT_RECONNECT_WINDOW: NonZeroU32 = NonZeroU32::new(300).unwrap();

fn default_reconnect_window(seconds: &NonZeroU32) -> bool {
    *seconds == DEFAULT_RECONNECT_WINDOW
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

/// Transport keepalives for one authenticated SSH endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SshKeepalive {
    pub interval_seconds: u32,
    pub max_unanswered: NonZeroU32,
}

impl Default for SshKeepalive {
    fn default() -> Self {
        Self {
            interval_seconds: 5,
            max_unanswered: NonZeroU32::new(2).unwrap(),
        }
    }
}

impl SshKeepalive {
    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn interval(self) -> Option<Duration> {
        (self.interval_seconds != 0).then(|| Duration::from_secs(self.interval_seconds.into()))
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct SshOptions {
    pub host: String,
    pub user: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_timeout_seconds: Option<NonZeroU32>,
    #[serde(default, skip_serializing_if = "SshKeepalive::is_default")]
    pub keepalive: SshKeepalive,
    #[serde(default, skip_serializing_if = "TerminalType::is_default")]
    pub terminal_type: TerminalType,
    #[serde(default, skip_serializing_if = "ScrollbackLines::is_default")]
    pub scrollback_lines: ScrollbackLines,
    #[serde(default, skip_serializing_if = "ColorScheme::is_default")]
    pub color_scheme: ColorScheme,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<ProfileTextSize>,
    #[serde(default, skip_serializing_if = "TabColor::is_default")]
    pub tab_color: TabColor,
    #[serde(flatten)]
    pub authentication: Authentication,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_pipe: Option<AgentPipe>,
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
    pub dotfiles: Option<Box<DotfileSync>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_bundle: Option<Box<ToolSync>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_prompt: Option<GroupPromptPattern>,
    #[serde(default)]
    pub durable: bool,
    #[serde(default)]
    pub player: PlayerOptions,
}

pub const MAX_JUMPS: usize = 8;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DotfileSync {
    pub bundle: PathBuf,
    pub shell: crate::sftp::DotfileShell,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zdotdir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fish_config_dir: Option<String>,
    pub host: String,
    pub user: String,
    pub port: u16,
    pub fingerprint: String,
    pub home: String,
}

impl DotfileSync {
    pub(crate) fn approves(&self, endpoint: (&str, &str, u16, &str)) -> bool {
        (
            self.host.as_str(),
            self.user.as_str(),
            self.port,
            self.fingerprint.as_str(),
        ) == endpoint
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSync {
    pub bundle: PathBuf,
    pub name: String,
    pub bin_directory: String,
    pub host: String,
    pub user: String,
    pub port: u16,
    pub fingerprint: String,
    pub home: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_to_path: Option<ToolPath>,
}

impl ToolSync {
    pub(crate) fn approves(&self, endpoint: (&str, &str, u16, &str)) -> bool {
        (
            self.host.as_str(),
            self.user.as_str(),
            self.port,
            self.fingerprint.as_str(),
        ) == endpoint
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolPath {
    pub shell: crate::sftp::DotfileShell,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zdotdir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fish_config_dir: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct JumpHost {
    pub host: String,
    pub user: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_timeout_seconds: Option<NonZeroU32>,
    #[serde(default, skip_serializing_if = "SshKeepalive::is_default")]
    pub keepalive: SshKeepalive,
    #[serde(flatten)]
    pub authentication: Authentication,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_pipe: Option<AgentPipe>,
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
            connect_timeout_seconds: hop.connect_timeout_seconds,
            keepalive: hop.keepalive,
            terminal_type: TerminalType::default(),
            scrollback_lines: ScrollbackLines::default(),
            color_scheme: ColorScheme::default(),
            font_size: None,
            tab_color: Default::default(),
            authentication: hop.authentication,
            agent_pipe: hop.agent_pipe,
            host_key: hop.host_key,
            host_key_alias: hop.host_key_alias,
            proxy_jump: Vec::new(),
            local_forwards: Vec::new(),
            dotfiles: None,
            tool_bundle: None,
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

/// Validate an SSH hostname or literal IP address so it cannot inject
/// `known_hosts` wildcards, multi-host lists, markers, options, or line breaks.
pub fn validate_ssh_host(host: &str) -> Result<()> {
    ensure!(
        !host.is_empty()
            && host.len() <= 255
            && !host.starts_with(['-', '!', '@', '#', '|'])
            && !host
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || "*,?/\\@#[]".contains(c)),
        "invalid SSH host {host:?}"
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
    pub fn set_authentication(&mut self, authentication: Authentication) {
        if !authentication.uses_agent() {
            self.agent_pipe = None;
        }
        self.authentication = authentication;
    }

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
        if let Some(sync) = &mut self.dotfiles
            && sync.bundle.is_relative()
            && !sync.bundle.as_os_str().is_empty()
        {
            sync.bundle = base.join(&sync.bundle);
        }
        if let Some(sync) = &mut self.tool_bundle
            && sync.bundle.is_relative()
            && !sync.bundle.as_os_str().is_empty()
        {
            sync.bundle = base.join(&sync.bundle);
        }
        if let Some(key) = self.player.device_key.as_deref_mut()
            && key.is_relative()
        {
            *key = base.join(&*key);
        }
        for path in self
            .authentications_mut()
            .flat_map(Authentication::credential_paths_mut)
        {
            if path.is_relative() {
                *path = base.join(&*path);
            }
        }
        Ok(())
    }

    pub(crate) fn validate_route(&self) -> Result<()> {
        validate_ssh_host(&self.host)?;
        for (auth, agent_pipe) in std::iter::once((&self.authentication, &self.agent_pipe)).chain(
            self.proxy_jump
                .iter()
                .map(|hop| (&hop.authentication, &hop.agent_pipe)),
        ) {
            ensure!(
                agent_pipe.is_none() || auth.uses_agent(),
                "An SSH agent pipe requires SSH agent authentication"
            );
            ensure!(
                auth.identity_path()
                    .is_none_or(|path| !path.as_os_str().is_empty()),
                "identity path is empty"
            );
            ensure!(
                auth.certificate_path()
                    .is_none_or(|path| !path.as_os_str().is_empty()),
                "SSH user certificate path is empty"
            );
        }
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
            validate_ssh_host(&hop.host).with_context(|| format!("jump host {}", hop.host))?;
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

pub fn default_profiles_path() -> Result<PathBuf> {
    Ok(crate::keys::data_dir()?.join("profiles.json"))
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
#[serde(rename_all = "kebab-case")]
pub enum SshGroupParallelLimit {
    #[default]
    Unlimited,
    One,
    Two,
    Four,
    Eight,
    Sixteen,
}

impl SshGroupParallelLimit {
    pub fn concurrency(self) -> Option<usize> {
        match self {
            Self::Unlimited => None,
            Self::One => Some(1),
            Self::Two => Some(2),
            Self::Four => Some(4),
            Self::Eight => Some(8),
            Self::Sixteen => Some(16),
        }
    }
}

pub type SshGroupLoginLimit = SshGroupParallelLimit;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub enum SshGroupCommandInterval {
    #[default]
    #[serde(rename = "none")]
    None,
    #[serde(rename = "500ms")]
    HalfSecond,
    #[serde(rename = "1s")]
    OneSecond,
    #[serde(rename = "2s")]
    TwoSeconds,
    #[serde(rename = "5s")]
    FiveSeconds,
    #[serde(rename = "10s")]
    TenSeconds,
}

impl SshGroupCommandInterval {
    pub fn duration(self) -> Duration {
        match self {
            Self::None => Duration::ZERO,
            Self::HalfSecond => Duration::from_millis(500),
            Self::OneSecond => Duration::from_secs(1),
            Self::TwoSeconds => Duration::from_secs(2),
            Self::FiveSeconds => Duration::from_secs(5),
            Self::TenSeconds => Duration::from_secs(10),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub enum SshGroupQueueExpiry {
    #[default]
    #[serde(rename = "never")]
    Never,
    #[serde(rename = "5s")]
    FiveSeconds,
    #[serde(rename = "15s")]
    FifteenSeconds,
    #[serde(rename = "30s")]
    ThirtySeconds,
    #[serde(rename = "1m")]
    OneMinute,
    #[serde(rename = "5m")]
    FiveMinutes,
}

impl SshGroupQueueExpiry {
    pub fn duration(self) -> Option<Duration> {
        let seconds = match self {
            Self::Never => return None,
            Self::FiveSeconds => 5,
            Self::FifteenSeconds => 15,
            Self::ThirtySeconds => 30,
            Self::OneMinute => 60,
            Self::FiveMinutes => 300,
        };
        Some(Duration::from_secs(seconds))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct SshRosterDefaults {
    pub order: SshRosterOrder,
    pub show_last_output: bool,
    pub show_terminal_line: bool,
    pub require_all_ready: bool,
    pub login_limit: SshGroupParallelLimit,
    pub command_limit: SshGroupParallelLimit,
    pub command_interval: SshGroupCommandInterval,
    pub command_queue_expiry: SshGroupQueueExpiry,
    pub stop_command_queue_on_failure: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Profiles {
    pub profiles: BTreeMap<String, SshOptions>,
    pub local: BTreeMap<String, LocalOptions>,
    /// Each available name once, including discovered local shells.
    pub profile_order: Vec<String>,
    pub pinned_profiles: BTreeSet<String>,
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
            pinned_profiles: BTreeSet::new(),
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

    pub fn ssh_roster_defaults(&self, name: &str) -> SshRosterDefaults {
        self.ssh_host_group_defaults
            .get(name)
            .copied()
            .unwrap_or_default()
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
        pinned_profiles: BTreeSet<String>,
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
        pinned_profiles: document.pinned_profiles,
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
#[allow(
    clippy::large_enum_variant,
    reason = "Keep the commonly used SSH source inline and allocate startup approvals only when configured"
)]
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

    pub fn profile_font_size(&self) -> Option<ProfileTextSize> {
        match self {
            Self::Local(shell) => shell.font_size,
            Self::Ssh(ssh) => ssh.font_size,
            _ => None,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagnosticLaunch {
    Open(Artifact),
    Compare { before: Artifact, after: Artifact },
}

impl DiagnosticLaunch {
    pub(crate) fn from_paths(before: Artifact, comparison_paths: Vec<PathBuf>) -> Result<Self> {
        ensure!(
            before.valid(),
            "Supply absolute paths for a saved diagnostic"
        );
        if comparison_paths.is_empty() {
            return Ok(Self::Open(before));
        }
        let kind = before.kind;
        let after = Artifact {
            kind,
            paths: comparison_paths,
        };
        ensure!(
            after.valid(),
            "Supply up to {} absolute comparison paths for this diagnostic",
            kind.max_paths()
        );
        Ok(Self::Compare { before, after })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub source: Source,
    pub diagnostic: Option<DiagnosticLaunch>,
    pub workspace: Option<String>,
    pub switch_workspace: bool,
    pub expected_workspace_key: Option<String>,
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
        let mut diagnostic = None;
        let mut comparison_paths = Vec::new();
        let mut profile = None;
        let mut workspace_name = None;
        let mut switch_workspace = false;
        let mut expected_workspace_key = None;
        let mut explicit_source = false;
        let mut profiles_path: Option<PathBuf> = None;
        let mut list_profiles = false;
        let mut paused = false;
        let mut port = None;
        let mut authentication = None;
        let mut certificate = None;
        let mut agent_pipe = None;
        let mut host_key = None;
        let mut host_key_alias = None;
        let mut benchmark = false;
        let mut smoke_seconds = None;
        let mut extra_profiles = Vec::new();
        let mut cols = None;
        let mut rows = None;
        while let Some(arg) = args.next() {
            let diagnostic_kind = match arg.as_str() {
                "--open-cpu-profile" => Some(DiagnosticKind::CpuProfile),
                "--open-triage-report" => Some(DiagnosticKind::TriageReport),
                "--open-packet-capture" => Some(DiagnosticKind::PacketCapture),
                "--open-syscall-trace" => Some(DiagnosticKind::SyscallTrace),
                _ => None,
            };
            if let Some(kind) = diagnostic_kind {
                let artifact = diagnostic.get_or_insert_with(|| Artifact {
                    kind,
                    paths: Vec::new(),
                });
                ensure!(
                    artifact.kind == kind,
                    "Choose one kind of saved diagnostic per launch"
                );
                ensure!(
                    artifact.paths.len() < kind.max_paths(),
                    "Too many {arg} paths (maximum {})",
                    kind.max_paths()
                );
                artifact.paths.push(diagnostic_path(&mut args, &arg)?);
                continue;
            }
            explicit_source |= matches!(
                arg.as_str(),
                "--demo" | "--replay" | "--replay-timed" | "--profile" | "ssh" | "--tab"
            );
            match arg.as_str() {
                "--help" | "-h" => {
                    println!(
                        "w11term — Rust / Reactor SSH terminal prototype\n\n  w11term [--demo]\n  w11term --profile root|play-6789|play-6792 [--cols 170 --rows 48]\n  w11term --profile root --tab play-6789 --tab play-6792\n  w11term --list-profiles [--profiles profiles.json]\n  w11term --workspace NAME [--profiles profiles.json]\n  w11term --switch-workspace NAME [--profiles profiles.json]\n  w11term --replay capture.vt\n  w11term --replay-timed capture.vt [--paused]\n  w11term --open-cpu-profile PATH [--compare-with PATH]\n  w11term --open-triage-report PATH [--compare-with PATH]\n  w11term --open-packet-capture PATH [--compare-with PATH]\n  w11term --open-syscall-trace PATH [--open-syscall-trace PATH ...] [--compare-with PATH ...]\n  w11term ssh user@host[:port] [--port 22] [--identity KEY | --agent-key SHA256:... | --auth none|agent|password|keyboard-interactive] [--certificate FILE]\n      [--host-key SHA256:...] [--host-key-alias NAME] [--agent-pipe PIPE]\n  w11term --benchmark\n  w11term --smoke-seconds 5\n\nAutomation: w11term-cli --help (named pipe, JSON replies, no focus changes).\nProfiles load from %LOCALAPPDATA%\\w11term\\profiles.json, or --profiles PATH.\nRelative identity and certificate paths in profiles resolve beside that file.\nSSH verifies OpenSSH known_hosts, or an explicitly supplied fingerprint.\nSSH destinations also accept ssh://user@host[:port] and bracketed IPv6.\nAuthentication defaults to the Windows OpenSSH agent; --auth none sends no key.\nCtrl+Shift+V: paste; Shift+PageUp/PageDown: scrollback; F5: reload demo."
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
                "--compare-with" => {
                    ensure!(
                        comparison_paths.len() < DiagnosticKind::SyscallTrace.max_paths(),
                        "Too many --compare-with paths"
                    );
                    comparison_paths.push(diagnostic_path(&mut args, &arg)?);
                }
                "--profile" => profile = Some(value(&mut args, &arg)?),
                "--workspace" | "--switch-workspace" => {
                    ensure!(workspace_name.is_none(), "Choose one workspace");
                    workspace_name = Some(value(&mut args, &arg)?);
                    switch_workspace = arg == "--switch-workspace";
                }
                "--expected-workspace-key" => {
                    expected_workspace_key = Some(value(&mut args, &arg)?);
                }
                "--tab" => extra_profiles.push(value(&mut args, &arg)?),
                "--cols" => cols = Some(value(&mut args, &arg)?.parse::<usize>()?),
                "--rows" => rows = Some(value(&mut args, &arg)?.parse::<usize>()?),
                "--profiles" => profiles_path = Some(value(&mut args, &arg)?.into()),
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
                        "choose only one of --identity, --agent-key, and --auth"
                    );
                    authentication = Some(Authentication::identity(value(&mut args, &arg)?, None));
                }
                "--agent-key" => {
                    ensure!(
                        authentication.is_none(),
                        "choose only one of --identity, --agent-key, and --auth"
                    );
                    authentication = Some(Authentication::AgentKey {
                        agent_fingerprint: value(&mut args, &arg)?.parse()?,
                    });
                }
                "--certificate" => {
                    ensure!(certificate.is_none(), "specify --certificate only once");
                    certificate = Some(PathBuf::from(value(&mut args, &arg)?));
                }
                "--agent-pipe" => {
                    ensure!(agent_pipe.is_none(), "specify --agent-pipe only once");
                    agent_pipe = Some(value(&mut args, &arg)?.parse::<AgentPipe>()?);
                }
                "--auth" => {
                    ensure!(
                        authentication.is_none(),
                        "choose only one of --identity, --agent-key, and --auth"
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
            diagnostic.is_some() || comparison_paths.is_empty(),
            "--compare-with requires a saved baseline from an --open-* option"
        );
        let diagnostic = diagnostic
            .map(|before| DiagnosticLaunch::from_paths(before, comparison_paths))
            .transpose()?;
        ensure!(
            diagnostic.is_none()
                || !(explicit_source || workspace_name.is_some() || list_profiles || benchmark),
            "A saved diagnostic launch cannot be combined with a terminal source, workspace, --tab, --list-profiles or --benchmark"
        );
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
        let profiles_path = match profiles_path {
            Some(path) => path,
            None => default_profiles_path()?,
        };
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
                            Authentication::AgentKey { .. } => "agent-key",
                            Authentication::AgentCertificate { .. } => "agent-certificate",
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
            || certificate.is_some()
            || agent_pipe.is_some()
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
            && diagnostic.is_none()
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
                ssh.set_authentication(authentication);
            }
            if let Some(path) = certificate {
                match &mut ssh.authentication {
                    Authentication::Identity { certificate, .. } => {
                        *certificate = Some(Box::new(path))
                    }
                    Authentication::AgentCertificate { certificate, .. } => **certificate = path,
                    Authentication::Agent => {
                        ssh.authentication = Authentication::agent_certificate(path, None);
                    }
                    Authentication::AgentKey { agent_fingerprint } => {
                        ssh.authentication = Authentication::agent_certificate(
                            path,
                            Some(agent_fingerprint.clone()),
                        );
                    }
                    _ => bail!("--certificate requires private-key or SSH agent authentication"),
                }
            }
            if let Some(host_key) = host_key {
                ssh.host_key = Some(host_key);
            }
            if let Some(agent_pipe) = agent_pipe {
                ssh.agent_pipe = Some(agent_pipe);
            }
            if let Some(alias) = host_key_alias {
                ssh.host_key_alias = Some(alias);
            }
            ensure!(ssh.port != 0, "SSH port must be nonzero");
            ssh.validate_route()?;
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
        ensure!(
            expected_workspace_key.is_none() || switch_workspace,
            "--expected-workspace-key requires --switch-workspace"
        );
        Ok(Some(Self {
            source,
            diagnostic,
            workspace: workspace_name,
            switch_workspace,
            expected_workspace_key,
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

fn diagnostic_path(args: &mut impl Iterator<Item = String>, option: &str) -> Result<PathBuf> {
    let path = value(args, option)?;
    std::path::absolute(Path::new(&path))
        .with_context(|| format!("Resolving the {option} file path"))
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
    fn ssh_agent_pipes_are_local_and_travel_with_each_endpoint() {
        const GATEWAY: &str = r"\\.\pipe\agent-jump";
        const DESTINATION: &str = r"\\.\pipe\agent-destination";
        for invalid in [
            r"\\server\pipe\relay",
            r"\\?\pipe\alternate",
            r"\\.\pipe\nested\socket",
            r"\\.\pipe\..",
            r"\\.\pipe\",
            "/tmp/agent",
            "$OTHER_AGENT",
        ] {
            assert!(invalid.parse::<AgentPipe>().is_err(), "{invalid}");
        }
        assert_eq!(
            "$SSH_AUTH_SOCK".parse::<AgentPipe>().unwrap().as_str(),
            "SSH_AUTH_SOCK"
        );
        let mut profile = serde_json::json!({
            "host": "destination", "user": "pilot", "auth": "agent", "agent_pipe": DESTINATION,
            "proxy_jump": [{ "host": "jump", "user": "pilot", "auth": "agent", "agent_pipe": GATEWAY }]
        });
        let options: SshOptions = serde_json::from_value(profile.clone()).unwrap();
        assert!(options.validate_route().is_ok());
        assert_eq!(options.agent_pipe.unwrap().as_str(), DESTINATION);
        let hop = SshOptions::from(options.proxy_jump[0].clone());
        assert_eq!(hop.agent_pipe.unwrap().as_str(), GATEWAY);
        profile["proxy_jump"][0]["auth"] = "password".into();
        assert!(
            serde_json::from_value::<SshOptions>(profile)
                .unwrap()
                .validate_route()
                .is_err()
        );

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profiles.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"profiles":{"pilot":{
                "host":"destination","user":"pilot","auth":"agent","agent_pipe":DESTINATION
            }}}))
            .unwrap(),
        )
        .unwrap();
        let parse = |args: &[&str]| {
            Options::parse_from(
                args.iter()
                    .map(|arg| arg.to_string())
                    .chain(["--profiles".into(), path.to_string_lossy().into_owned()]),
            )
        };
        let Source::Ssh(ssh) = parse(&["--profile", "pilot", "--agent-pipe", GATEWAY])
            .unwrap()
            .unwrap()
            .source
        else {
            panic!("expected SSH");
        };
        assert_eq!(ssh.agent_pipe.unwrap().as_str(), GATEWAY);
        let Source::Ssh(ssh) = parse(&["--profile", "pilot", "--auth", "none"])
            .unwrap()
            .unwrap()
            .source
        else {
            panic!("expected SSH");
        };
        assert!(ssh.agent_pipe.is_none());
        assert!(
            parse(&[
                "ssh",
                "pilot@localhost",
                "--auth",
                "password",
                "--agent-pipe",
                GATEWAY
            ])
            .is_err()
        );
    }

    #[test]
    fn explicit_agent_key_requires_and_round_trips_an_openssh_fingerprint() {
        const KEY: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let expected = Authentication::AgentKey {
            agent_fingerprint: KEY.parse().unwrap(),
        };
        let mut profile = serde_json::json!({
            "host": "destination", "user": "pilot", "auth": "agent-key", "agent_fingerprint": KEY,
            "proxy_jump":[{"host": "jump", "user": "pilot", "auth": "agent-key", "agent_fingerprint": KEY}]
        });
        let decoded: SshOptions = serde_json::from_value(profile.clone()).unwrap();
        assert_eq!(decoded.authentication, expected);
        assert_eq!(decoded.proxy_jump[0].authentication, expected);
        let encoded = serde_json::to_value(&decoded).unwrap();
        assert_eq!(encoded["agent_fingerprint"], KEY);
        assert_eq!(encoded["proxy_jump"][0]["agent_fingerprint"], KEY);

        profile.as_object_mut().unwrap().remove("agent_fingerprint");
        assert!(serde_json::from_value::<SshOptions>(profile.clone()).is_err());
        profile["agent_fingerprint"] = KEY.into();
        profile["proxy_jump"][0]["agent_fingerprint"] = "SHA256:short".into();
        assert!(serde_json::from_value::<SshOptions>(profile).is_err());
        assert!(
            serde_json::from_value::<Authentication>(serde_json::json!({"auth":"agent"})).is_ok()
        );

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profiles.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"profiles":{"pilot": {
                "host":"localhost","user":"pilot","auth":"password"
            }}}))
            .unwrap(),
        )
        .unwrap();
        let parse = |args: &[&str]| {
            Options::parse_from(
                args.iter()
                    .map(|arg| arg.to_string())
                    .chain(["--profiles".into(), path.to_string_lossy().into_owned()]),
            )
        };
        for base in [["ssh", "pilot@localhost"], ["--profile", "pilot"]] {
            let Source::Ssh(ssh) = parse(&[base[0], base[1], "--agent-key", KEY])
                .unwrap()
                .unwrap()
                .source
            else {
                panic!("expected SSH");
            };
            assert_eq!(ssh.authentication, expected);
        }
        for extra in [["--auth", "agent"], ["--identity", "id"]] {
            assert!(
                parse(&[
                    "ssh",
                    "pilot@localhost",
                    "--agent-key",
                    KEY,
                    extra[0],
                    extra[1]
                ])
                .is_err()
            );
        }
        let Source::Ssh(ssh) = parse(&[
            "ssh",
            "pilot@localhost",
            "--agent-key",
            KEY,
            "--certificate",
            "id-cert.pub",
        ])
        .unwrap()
        .unwrap()
        .source
        else {
            panic!("expected SSH");
        };
        assert_eq!(
            ssh.authentication,
            Authentication::agent_certificate("id-cert.pub", Some(KEY.parse().unwrap()))
        );
        assert!(parse(&["--demo", "--agent-key", KEY]).is_err());
    }

    #[test]
    fn explicitly_configured_user_certificates_follow_identity_path_rules() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profiles.json");
        std::fs::write(&path, br#"{"profiles":{"pilot":{"host":"localhost","user":"pilot",
            "auth":"identity","identity":"id","certificate":"id-cert.pub",
            "proxy_jump":[{"host":"gate","user":"pilot","auth":"identity","identity":"hop","certificate":"hop-cert.pub"}]}}}"#).unwrap();
        let profile = load_profiles(&path)
            .unwrap()
            .profiles
            .remove("pilot")
            .unwrap();
        assert_eq!(
            profile.authentication,
            Authentication::identity(
                directory.path().join("id"),
                Some(directory.path().join("id-cert.pub"))
            )
        );
        assert_eq!(
            profile.proxy_jump[0].authentication,
            Authentication::identity(
                directory.path().join("hop"),
                Some(directory.path().join("hop-cert.pub"))
            )
        );
        let parse = |args: &[&str]| {
            Options::parse_from(
                args.iter()
                    .map(|arg| arg.to_string())
                    .chain(["--profiles".into(), path.to_string_lossy().into_owned()]),
            )
        };
        let options = parse(&[
            "ssh",
            "pilot@localhost",
            "--identity",
            "id",
            "--certificate",
            "cert.pub",
        ])
        .unwrap()
        .unwrap();
        let Source::Ssh(ssh) = options.source else {
            panic!("expected SSH")
        };
        assert_eq!(
            ssh.authentication,
            Authentication::identity("id", Some("cert.pub".into()))
        );
        let replaced = parse(&["--profile", "pilot", "--identity", "unrelated"])
            .unwrap()
            .unwrap();
        let Source::Ssh(ssh) = replaced.source else {
            panic!("expected SSH")
        };
        assert_eq!(
            ssh.authentication,
            Authentication::identity("unrelated", None)
        );
        let Source::Ssh(ssh) = parse(&["ssh", "pilot@localhost", "--certificate", "cert.pub"])
            .unwrap()
            .unwrap()
            .source
        else {
            panic!("expected SSH");
        };
        assert_eq!(
            ssh.authentication,
            Authentication::agent_certificate("cert.pub", None)
        );
        assert!(
            parse(&[
                "ssh",
                "pilot@localhost",
                "--auth",
                "password",
                "--certificate",
                "cert.pub"
            ])
            .is_err()
        );
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
        for (option, kind) in [
            ("--open-cpu-profile", DiagnosticKind::CpuProfile),
            ("--open-triage-report", DiagnosticKind::TriageReport),
            ("--open-packet-capture", DiagnosticKind::PacketCapture),
            ("--open-syscall-trace", DiagnosticKind::SyscallTrace),
        ] {
            let launch = parse(&[option, "saved diagnostic.42"]);
            assert_eq!(launch.initial_profile, None);
            assert_eq!(launch.source, Source::Demo);
            let Some(DiagnosticLaunch::Open(artifact)) = launch.diagnostic else {
                panic!("Expected to open {kind:?}");
            };
            assert_eq!(artifact.kind, kind);
            assert_eq!(
                artifact.paths,
                [std::path::absolute("saved diagnostic.42").unwrap()]
            );
        }

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
    fn diagnostic_launch_preserves_split_order_and_rejects_conflicting_sources() {
        let flag = "--open-syscall-trace";
        let parse = |args: &[&str]| {
            Options::parse_from(
                ["--profiles", "profiles.json"]
                    .into_iter()
                    .chain(args.iter().copied())
                    .map(str::to_owned),
            )
        };
        let launch = parse(&[flag, "run.98", flag, "run.32"]).unwrap().unwrap();
        let Some(DiagnosticLaunch::Open(artifact)) = launch.diagnostic else {
            panic!("Expected a saved trace")
        };
        assert_eq!(
            artifact.paths,
            ["run.98", "run.32"].map(|path| std::path::absolute(path).unwrap())
        );
        for args in [
            &[flag][..],
            &[flag, ""],
            &[flag, "run.32", "--open-cpu-profile", "report.folded"],
            &[
                "--open-triage-report",
                "before.json",
                "--open-triage-report",
                "after.json",
            ],
            &["--profile", "Remote", flag, "run.32"],
            &[flag, "run.32", "--tab", "Remote"],
            &["--switch-workspace", "admin", flag, "run.32"],
            &[flag, "run.32", "--demo"],
            &[flag, "run.32", "--benchmark"],
            &["--replay", "session.vt", flag, "run.32"],
            &[flag, "run.32", "--list-profiles"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
        let oversized = [flag, "run.32"].repeat(DiagnosticKind::SyscallTrace.max_paths() + 1);
        assert!(parse(&oversized).is_err());
    }

    #[test]
    fn diagnostic_comparisons_preserve_each_side_and_require_compatible_inputs() {
        let open = "--open-syscall-trace";
        let compare = "--compare-with";
        let parse = |args: &[&str]| {
            Options::parse_from(
                ["--profiles", "profiles.json"]
                    .into_iter()
                    .chain(args.iter().copied())
                    .map(str::to_owned),
            )
        };
        let launch = parse(&[
            compare, "fail.79", open, "work.42", open, "work.41", compare, "fail.78",
        ])
        .unwrap()
        .unwrap();
        let Some(DiagnosticLaunch::Compare { before, after }) = launch.diagnostic else {
            panic!("Expected a saved comparison");
        };
        assert_eq!(before.kind, DiagnosticKind::SyscallTrace);
        assert_eq!(after.kind, before.kind);
        assert_eq!(
            before.paths,
            ["work.42", "work.41"].map(|p| std::path::absolute(p).unwrap())
        );
        assert_eq!(
            after.paths,
            ["fail.79", "fail.78"].map(|p| std::path::absolute(p).unwrap())
        );
        let packet = parse(&[
            "--open-packet-capture",
            "before.pcap",
            compare,
            "after.pcapng",
        ])
        .unwrap()
        .unwrap();
        let Some(DiagnosticLaunch::Compare { before, after }) = packet.diagnostic else {
            panic!("Expected a packet capture comparison");
        };
        assert_eq!(before.kind, DiagnosticKind::PacketCapture);
        assert_eq!(after.kind, before.kind);
        assert_eq!(before.paths, [std::path::absolute("before.pcap").unwrap()]);
        assert_eq!(after.paths, [std::path::absolute("after.pcapng").unwrap()]);
        for args in [
            &[compare, "after.folded"][..],
            &["--open-cpu-profile", "before.folded", compare],
            &[
                "--open-packet-capture",
                "before.pcap",
                compare,
                "after.pcap",
                compare,
                "extra.pcapng",
            ],
            &[
                "--open-triage-report",
                "before.json",
                compare,
                "after.json",
                compare,
                "extra.json",
            ],
            &[open, "work.42", compare, "fail.78", "--profile", "Remote"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
        let mut oversized = vec![open, "work.42"];
        oversized.extend([compare, "fail.78"].repeat(DiagnosticKind::SyscallTrace.max_paths() + 1));
        assert!(parse(&oversized).is_err());
    }

    #[test]
    fn a_workspace_is_the_only_launch_source_and_overridden_profiles_are_ad_hoc() {
        let parse = |args: &[&str]| Options::parse_from(args.iter().map(|s| s.to_string()));
        assert!(parse(&["--workspace", "desk", "--expected-workspace-key", "key"]).is_err());
        let guarded = parse(&[
            "--switch-workspace",
            "desk",
            "--expected-workspace-key",
            "key",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(guarded.expected_workspace_key.as_deref(), Some("key"));
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
            Authentication::identity(directory.path().join("hostinger"), None)
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

    #[test]
    fn default_profiles_path_uses_data_dir_not_cwd() {
        let path = default_profiles_path().unwrap();
        assert_eq!(path, crate::keys::data_dir().unwrap().join("profiles.json"));
        assert!(path.is_absolute());
    }

    #[test]
    fn ssh_host_validation_rejects_known_hosts_injection_and_accepts_literals() {
        for valid in [
            "example.com",
            "dev-01.internal.local",
            "127.0.0.1",
            "::1",
            "fe80::1%eth0",
        ] {
            assert!(validate_ssh_host(valid).is_ok(), "{valid:?}");
        }
        let too_long = "a".repeat(256);
        for bad in [
            "",
            "*",
            "*,github.com",
            "host?1",
            "!excluded",
            "@cert-authority *",
            "#comment",
            "|1|hash",
            "-oProxyCommand=calc",
            "host\n@cert-authority *",
            "host\r\n",
            "host name",
            "[::1]",
            "a/b",
            r"a\b",
            "user@host",
            too_long.as_str(),
        ] {
            assert!(validate_ssh_host(bad).is_err(), "{bad:?}");
        }

        let mut options = SshOptions {
            host: "*,github.com".into(),
            user: "alice".into(),
            port: 22,
            authentication: Authentication::Agent,
            agent_pipe: None,
            host_key: None,
            host_key_alias: None,
            terminal_type: TerminalType::default(),
            color_scheme: Default::default(),
            connect_timeout_seconds: None,
            keepalive: Default::default(),
            font_size: None,
            tab_color: Default::default(),
            scrollback_lines: ScrollbackLines::default(),
            player: PlayerOptions::default(),
            proxy_jump: Vec::new(),
            local_forwards: Vec::new(),
            durable: false,
            dotfiles: None,
            tool_bundle: None,
            group_prompt: None,
        };
        assert!(options.validate_route().is_err());

        options.host = "target.example".into();
        options.proxy_jump.push(JumpHost {
            host: "*".into(),
            user: "jump".into(),
            port: 22,
            connect_timeout_seconds: None,
            keepalive: Default::default(),
            authentication: Authentication::Agent,
            agent_pipe: None,
            host_key: None,
            host_key_alias: None,
        });
        assert!(options.validate_route().is_err());
    }
}
