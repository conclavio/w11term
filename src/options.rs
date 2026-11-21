use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
mod local;
pub use local::LocalOptions;

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
    #[serde(flatten)]
    pub authentication: Authentication,
    pub host_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proxy_jump: Vec<JumpHost>,
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
}

impl From<JumpHost> for SshOptions {
    fn from(hop: JumpHost) -> Self {
        Self {
            host: hop.host,
            user: hop.user,
            port: hop.port,
            terminal_type: TerminalType::default(),
            scrollback_lines: ScrollbackLines::default(),
            authentication: hop.authentication,
            host_key: hop.host_key,
            proxy_jump: Vec::new(),
            durable: false,
            player: PlayerOptions::default(),
        }
    }
}

impl SshOptions {
    pub(crate) fn validate_route(&self) -> Result<()> {
        ensure!(
            self.proxy_jump.len() <= MAX_JUMPS,
            "at most {MAX_JUMPS} SSH jump hosts are supported"
        );
        for hop in &self.proxy_jump {
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

#[derive(Clone, Debug, PartialEq)]
pub struct Profiles {
    pub profiles: BTreeMap<String, SshOptions>,
    pub local: BTreeMap<String, LocalOptions>,
    /// Each available name once, including discovered local shells.
    pub profile_order: Vec<String>,
    pub(crate) has_explicit_order: bool,
}

impl Default for Profiles {
    fn default() -> Self {
        let local = LocalOptions::installed();
        Self {
            profile_order: local.keys().cloned().collect(),
            profiles: BTreeMap::new(),
            local,
            has_explicit_order: false,
        }
    }
}

impl Profiles {
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
        ensure!(
            crate::assets::name(&ssh.player.bell),
            "profile {name}: bell must be a sound name, system, or off"
        );
        if let Some(device_key) = &mut ssh.player.device_key
            && device_key.is_relative()
        {
            *device_key = path.parent().unwrap_or(Path::new(".")).join(&*device_key);
        }
        ensure!(
            !ssh.host.is_empty() && !ssh.user.is_empty() && ssh.port != 0,
            "profile {name}: host, user, and port must be nonempty"
        );
        ssh.validate_route()
            .with_context(|| format!("profile {name}"))?;
        for auth in ssh.authentications_mut() {
            let Authentication::Identity { identity } = auth else {
                continue;
            };
            ensure!(
                !identity.as_os_str().is_empty(),
                "profile {name}: identity path is empty"
            );
            if identity.is_relative() {
                *identity = path.parent().unwrap_or(Path::new(".")).join(&*identity);
            }
        }
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
    Local(LocalOptions),
    Ssh(SshOptions),
}

impl Source {
    pub fn scrollback_lines(&self) -> ScrollbackLines {
        match self {
            Self::Local(shell) => shell.scrollback_lines,
            Self::Ssh(ssh) => ssh.scrollback_lines,
            Self::Demo | Self::Replay(_) => ScrollbackLines::default(),
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
        let mut port = None;
        let mut authentication = None;
        let mut host_key = None;
        let mut benchmark = false;
        let mut smoke_seconds = None;
        let mut extra_profiles = Vec::new();
        let mut cols = None;
        let mut rows = None;
        while let Some(arg) = args.next() {
            explicit_source |= matches!(
                arg.as_str(),
                "--demo" | "--replay" | "--profile" | "ssh" | "--tab"
            );
            match arg.as_str() {
                "--help" | "-h" => {
                    println!(
                        "w11term — Rust / Reactor SSH terminal prototype\n\n  w11term [--demo]\n  w11term --profile root|play-6789|play-6792 [--cols 170 --rows 48]\n  w11term --profile root --tab play-6789 --tab play-6792\n  w11term --list-profiles [--profiles profiles.json]\n  w11term --workspace NAME [--profiles profiles.json]\n  w11term --switch-workspace NAME [--profiles profiles.json]\n  w11term --replay capture.vt\n  w11term ssh user@host [--port 22] [--identity KEY | --auth none|agent|password|keyboard-interactive]\n      [--host-key SHA256:...]\n  w11term --benchmark\n  w11term --smoke-seconds 5\n\nAutomation: w11term-cli --help (named pipe, JSON replies, no focus changes).\nProfiles load from profiles.json in the working directory, or --profiles PATH.\nRelative identity paths resolve beside that file.\nSSH verifies OpenSSH known_hosts, or an explicitly supplied fingerprint.\nAuthentication defaults to the Windows OpenSSH agent; --auth none sends no key.\nCtrl+Shift+V: paste; Shift+PageUp/PageDown: scrollback; F5: reload demo."
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
                "--benchmark" => benchmark = true,
                "--smoke-seconds" => smoke_seconds = Some(value(&mut args, &arg)?.parse()?),
                "ssh" => {
                    let target = value(&mut args, &arg)?;
                    let (user, host) = target
                        .rsplit_once('@')
                        .ok_or_else(|| anyhow::anyhow!("specify an explicit user@host"))?;
                    let host = host.trim_matches(['[', ']']);
                    ensure!(
                        !user.is_empty() && !host.is_empty(),
                        "specify a nonempty user and host"
                    );
                    profile = None;
                    source = Source::Ssh(SshOptions {
                        host: host.into(),
                        user: user.into(),
                        port: 22,
                        terminal_type: TerminalType::default(),
                        scrollback_lines: ScrollbackLines::default(),
                        authentication: Authentication::Agent,
                        host_key: None,
                        proxy_jump: Vec::new(),
                        durable: false,
                        player: Default::default(),
                    });
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
                _ => bail!("unknown argument: {arg}; use --help"),
            }
        }
        ensure!(
            workspace_name.is_none() || !(explicit_source || list_profiles || benchmark),
            "--workspace cannot be combined with a terminal source, --tab, --list-profiles or --benchmark"
        );
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
        let initial_profile = profile
            .clone()
            .filter(|_| port.is_none() && authentication.is_none() && host_key.is_none());
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
            ensure!(ssh.port != 0, "SSH port must be nonzero");
        } else {
            ensure!(
                port.is_none() && authentication.is_none() && host_key.is_none(),
                "SSH options require ssh user@host or --profile NAME"
            );
        }
        if let Source::Replay(path) = &source {
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
        let options = parse(&["ssh", "play@conclav.io", "--auth", "none", "--port", "6789"])
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
