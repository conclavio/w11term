use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

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
    pub sync_assets: bool,
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
            sync_assets: true,
            device_key: None,
        }
    }
}

impl PlayerOptions {
    pub fn alternate_scroll_enabled(&self) -> bool {
        self.alternate_scroll.unwrap_or(!self.game_mode)
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct SshOptions {
    pub host: String,
    pub user: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(flatten)]
    pub authentication: Authentication,
    pub host_key: Option<String>,
    #[serde(default)]
    pub player: PlayerOptions,
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

#[derive(Deserialize)]
pub(crate) struct Profiles {
    pub(crate) profiles: BTreeMap<String, SshOptions>,
}

pub(crate) fn load_profiles(path: &Path) -> Result<Profiles> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut profiles: Profiles =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
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
        if let Authentication::Identity { identity } = &mut ssh.authentication {
            ensure!(
                !identity.as_os_str().is_empty(),
                "profile {name}: identity path is empty"
            );
            if identity.is_relative() {
                *identity = path.parent().unwrap_or(Path::new(".")).join(&*identity);
            }
        }
    }
    Ok(profiles)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Demo,
    Replay(PathBuf),
    Ssh(SshOptions),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub source: Source,
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
            match arg.as_str() {
                "--help" | "-h" => {
                    println!(
                        "w11term — Rust / Reactor SSH terminal prototype\n\n  w11term [--demo]\n  w11term --profile root|play-6789|play-6792 [--cols 170 --rows 48]\n  w11term --profile root --tab play-6789 --tab play-6792\n  w11term --list-profiles [--profiles profiles.json]\n  w11term --replay capture.vt\n  w11term ssh user@host [--port 22] [--identity KEY | --auth none|agent|password|keyboard-interactive]\n      [--host-key SHA256:...]\n  w11term --benchmark\n  w11term --smoke-seconds 5\n\nAutomation: w11term-cli --help (named pipe, JSON replies, no focus changes).\nProfiles load from profiles.json in the working directory, or --profiles PATH.\nRelative identity paths resolve beside that file.\nSSH verifies OpenSSH known_hosts, or an explicitly supplied fingerprint.\nAuthentication defaults to the Windows OpenSSH agent; --auth none sends no key.\nCtrl+Shift+V: paste; Shift+PageUp/PageDown: scrollback; F5: reload demo."
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
                        authentication: Authentication::Agent,
                        host_key: None,
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
        if list_profiles {
            for (name, ssh) in load_profiles(&profiles_path)?.profiles {
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
            return Ok(None);
        }
        let initial_profile = profile.clone();
        if !extra_profiles.is_empty() {
            let profiles = load_profiles(&profiles_path)?;
            for name in &extra_profiles {
                ensure!(
                    profiles.profiles.contains_key(name),
                    "unknown profile {name}"
                );
            }
        }
        if let Some(name) = profile {
            source = Source::Ssh(
                load_profiles(&profiles_path)?
                    .profiles
                    .remove(&name)
                    .ok_or_else(|| {
                        anyhow::anyhow!("unknown profile {name}; use --list-profiles")
                    })?,
            );
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
