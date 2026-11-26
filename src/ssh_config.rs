//! Import a selected OpenSSH client configuration into editable native profiles.
use crate::options::{
    Authentication, JumpHost, MAX_JUMPS, SshKeepalive, SshOptions, validate_host_key_alias,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug)]
pub struct ImportReport {
    pub aliases: Vec<ImportedAlias>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ImportedAlias {
    pub name: String,
    pub options: Option<SshOptions>,
    pub warnings: Vec<String>,
}

mod forwards;
mod includes;

pub fn import_file(path: &Path) -> Result<ImportReport> {
    let home = std::env::home_dir().context("cannot determine the Windows home directory")?;
    let user = std::env::var("USERNAME").context("cannot determine the Windows user")?;
    includes::load(path, &home, &user)
}

#[derive(Clone)]
struct Location {
    path: Arc<Path>,
    line: usize,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.path.display(), self.line)
    }
}

enum Scope {
    Global,
    Hosts(Vec<String>),
    Conditional(Location, Vec<String>),
}
struct Section {
    scope: Scope,
    directives: Vec<Directive>,
}

impl Section {
    fn global() -> Self {
        Self {
            scope: Scope::Global,
            directives: Vec::new(),
        }
    }
}

struct Directive {
    line: Location,
    key: String,
    args: Vec<String>,
}

#[derive(Default)]
struct Parsed {
    sections: Vec<Section>,
    aliases: BTreeSet<String>,
    warnings: BTreeSet<String>,
}
impl Parsed {
    fn section(&mut self, section: Section) {
        if let Scope::Conditional(line, _) = &section.scope
            && section
                .directives
                .iter()
                .any(|d| !forwards::is_directive(&d.key))
        {
            self.warnings.insert(format!("{line}: Match conditions for SSH connection settings are unsupported and were not evaluated."));
        }
        if !section.directives.is_empty() {
            self.sections.push(section);
        }
    }
    fn report(mut self, home: &Path, user: &str) -> ImportReport {
        if self.aliases.is_empty() {
            self.warnings
                .insert("No explicit Host aliases were found.".into());
        }
        let config = Config {
            sections: self.sections,
            home,
            user,
        };
        let aliases = self
            .aliases
            .into_iter()
            .map(|name| {
                let mut warnings = BTreeSet::new();
                let options = match config.resolve(&name, &mut warnings) {
                    Ok(options) => Some(options),
                    Err(error) => {
                        warnings.insert(format!("{error:#}"));
                        None
                    }
                };
                ImportedAlias {
                    name,
                    options,
                    warnings: warnings.into_iter().collect(),
                }
            })
            .collect();
        ImportReport {
            aliases,
            warnings: self.warnings.into_iter().collect(),
        }
    }
}

fn words(line: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut word = String::new();
    let mut quoted = None;
    let mut started = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if quoted.is_none() && !started && ch == '#' {
            break;
        }
        if matches!(ch, '\'' | '"') && (quoted.is_none() || quoted == Some(ch)) {
            quoted = if quoted.is_some() { None } else { Some(ch) };
            started = true;
        } else if ch == '\\'
            && chars
                .peek()
                .is_some_and(|&c| matches!(c, '"' | '\'' | '\\') || (quoted.is_none() && c == ' '))
        {
            word.push(chars.next().unwrap());
            started = true;
        } else if ch.is_whitespace() && quoted.is_none() {
            if started {
                result.push(std::mem::take(&mut word));
                started = false;
            }
        } else {
            word.push(ch);
            started = true;
        }
    }
    ensure!(quoted.is_none(), "unterminated quoted argument");
    if started {
        result.push(word);
    }
    Ok(result)
}

fn matches_patterns<'a>(patterns: impl IntoIterator<Item = &'a str>, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let mut positive = false;
    for pattern in patterns {
        let negative = pattern.starts_with('!');
        let pattern = pattern
            .strip_prefix('!')
            .unwrap_or(pattern)
            .to_ascii_lowercase();
        if glob(pattern.as_bytes(), host.as_bytes()) {
            if negative {
                return false;
            }
            positive = true;
        }
    }
    positive
}

fn glob(pattern: &[u8], host: &[u8]) -> bool {
    let (mut p, mut h) = (0, 0);
    let mut star = None;
    while h < host.len() {
        if pattern.get(p) == Some(&b'*') {
            star = Some((p, h));
            p += 1;
        } else if pattern.get(p).is_some_and(|c| *c == b'?' || *c == host[h]) {
            p += 1;
            h += 1;
        } else if let Some((s, mark)) = star {
            h = mark + 1;
            star = Some((s, h));
            p = s + 1;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == b'*')
}

fn valid_host(host: &str) -> Result<()> {
    crate::options::validate_ssh_host(host)
}

#[derive(Default)]
struct EndpointSpec {
    alias: String,
    user: Option<String>,
    port: Option<u16>,
}

impl EndpointSpec {
    fn jump(text: &str) -> Result<Self> {
        let text = text.strip_prefix("ssh://").unwrap_or(text);
        let (user, address) = match text.rsplit_once('@') {
            Some((user, address)) => {
                ensure!(
                    !user.is_empty() && !user.contains(['@', ':']),
                    "invalid ProxyJump username"
                );
                (Some(user.to_string()), address)
            }
            None => (None, text),
        };
        let (alias, port) = if let Some(bracket) = address.strip_prefix('[') {
            let (ip, rest) = bracket
                .split_once(']')
                .context("unterminated IPv6 ProxyJump address")?;
            ip.parse::<std::net::Ipv6Addr>()
                .context("invalid IPv6 jump host")?;
            (
                ip,
                if rest.is_empty() {
                    None
                } else {
                    Some(rest.strip_prefix(':').context("invalid jump host port")?)
                },
            )
        } else if address.matches(':').count() == 1 {
            let (alias, port) = address.split_once(':').unwrap();
            (alias, Some(port))
        } else {
            (address, None)
        };
        valid_host(alias)?;
        Ok(Self {
            alias: alias.into(),
            user,
            port: port.map(parse_port).transpose()?,
        })
    }
}

fn parse_port(text: &str) -> Result<u16> {
    let port = text.parse().context("invalid SSH port")?;
    ensure!(port != 0, "SSH port must be nonzero");
    Ok(port)
}

fn sensitive(key: &str) -> bool {
    matches!(
        key,
        "include"
            | "userknownhostsfile"
            | "globalknownhostsfile"
            | "knownhostscommand"
            | "stricthostkeychecking"
            | "checkhostip"
            | "verifyhostkeydns"
            | "revokedhostkeys"
            | "hostkeyalgorithms"
            | "pubkeyacceptedalgorithms"
            | "identityagent"
            | "certificatefile"
            | "canonicalizehostname"
            | "preferredauthentications"
            | "hostbasedauthentication"
            | "hostkeyagent"
            | "kexalgorithms"
            | "ciphers"
            | "macs"
            | "proxyusefdpass"
            | "passwordauthentication"
            | "kbdinteractiveauthentication"
            | "pubkeyauthentication"
    )
}

struct Config<'a> {
    sections: Vec<Section>,
    home: &'a Path,
    user: &'a str,
}
impl Config<'_> {
    fn resolve(&self, alias: &str, warnings: &mut BTreeSet<String>) -> Result<SshOptions> {
        let mut route = Vec::new();
        self.append(
            EndpointSpec {
                alias: alias.into(),
                ..Default::default()
            },
            &mut route,
            &mut Vec::new(),
            true,
            warnings,
        )?;
        let mut options = SshOptions::from(route.pop().unwrap());
        options.proxy_jump = route;
        options.local_forwards = forwards::resolve(&self.sections, alias, &options.host, warnings);
        Ok(options)
    }

    fn append(
        &self,
        spec: EndpointSpec,
        route: &mut Vec<JumpHost>,
        stack: &mut Vec<String>,
        expand_jump: bool,
        warnings: &mut BTreeSet<String>,
    ) -> Result<()> {
        ensure!(
            stack.len() <= MAX_JUMPS && route.len() <= MAX_JUMPS,
            "route exceeds {MAX_JUMPS} jump hosts"
        );
        ensure!(
            !stack.iter().any(|a| a.eq_ignore_ascii_case(&spec.alias)),
            "recursive ProxyJump for {}",
            spec.alias
        );
        let (endpoint, via) = self
            .endpoint(&spec, expand_jump, warnings)
            .with_context(|| format!("Host {}", spec.alias))?;
        if let Some((via, line)) = via.filter(|(s, _)| !s.eq_ignore_ascii_case("none")) {
            stack.push(spec.alias);
            for (index, hop) in via.split(',').enumerate() {
                // An explicit preceding -J path on the command line wins over that intermediate alias's own ProxyJump.
                EndpointSpec::jump(hop)
                    .and_then(|spec| self.append(spec, route, stack, index == 0, warnings))
                    .with_context(|| format!("{line}: ProxyJump"))?;
            }
            stack.pop();
        }
        ensure!(
            route.len() <= MAX_JUMPS,
            "route exceeds {MAX_JUMPS} jump hosts"
        );
        route.push(endpoint);
        Ok(())
    }

    fn endpoint(
        &self,
        spec: &EndpointSpec,
        expand_jump: bool,
        warnings: &mut BTreeSet<String>,
    ) -> Result<(JumpHost, Option<(String, Location)>)> {
        let mut scalars = BTreeMap::new();
        let mut identities = Vec::new();
        let mut proxy = None;
        for section in &self.sections {
            match &section.scope {
                Scope::Hosts(patterns)
                    if !matches_patterns(patterns.iter().map(String::as_str), &spec.alias) =>
                {
                    continue;
                }
                Scope::Conditional(line, _) => {
                    if section.directives.iter().any(|d| {
                        sensitive(&d.key)
                            || matches!(
                                d.key.as_str(),
                                "hostname"
                                    | "hostkeyalias"
                                    | "user"
                                    | "port"
                                    | "identityfile"
                                    | "identitiesonly"
                                    | "proxyjump"
                                    | "proxycommand"
                            )
                    }) {
                        bail!(
                            "{line}: unsupported Match may change the connection, identity or trust settings"
                        );
                    }
                    continue;
                }
                _ => {}
            }
            for d in &section.directives {
                let key = d.key.as_str();
                match key {
                    "hostname"
                    | "hostkeyalias"
                    | "user"
                    | "port"
                    | "identitiesonly"
                    | "serveraliveinterval"
                    | "connecttimeout"
                    | "serveralivecountmax" => {
                        scalars.entry(key).or_insert(d);
                    }
                    "identityfile" => identities.push(d),
                    "addressfamily" => warn(warnings, || {
                        format!(
                            "Host {}, {}: the SSH connection does not import AddressFamily. Local forward presets are checked separately.",
                            spec.alias, d.line
                        )
                    }),
                    key if forwards::is_directive(key) => {}
                    "proxyjump" | "proxycommand" => {
                        if expand_jump && proxy.is_none() {
                            proxy = Some(d);
                        }
                    }
                    key if sensitive(key) => bail!(
                        "{}: unsupported {key} could change routing, authentication or trust",
                        d.line
                    ),
                    key => warn(warnings, || {
                        format!(
                            "Host {}, {}: unsupported {key} was ignored.",
                            spec.alias, d.line
                        )
                    }),
                }
            }
        }
        let alias = &spec.alias;
        let host = configured(scalars.get("hostname").copied(), alias, |name| {
            let host = expand(name, &[('h', alias)])?;
            valid_host(&host)?;
            Ok(host)
        })?;
        // OpenSSH treats HostKeyAlias as a literal. In particular, %h is not expanded.
        let host_key_alias = scalars
            .get("hostkeyalias")
            .map(|d| -> Result<_> {
                let value = argument(d)?;
                validate_host_key_alias(value)
                    .with_context(|| format!("{}: HostKeyAlias", d.line))?;
                Ok(value.to_ascii_lowercase())
            })
            .transpose()?;
        let user = configured(
            scalars.get("user").copied().filter(|_| spec.user.is_none()),
            spec.user.as_deref().unwrap_or(self.user),
            |user| {
                ensure!(
                    !user.is_empty() && !user.chars().any(|c| c.is_whitespace() || c.is_control()),
                    "invalid SSH username"
                );
                Ok(user)
            },
        )?;
        let port = match spec.port {
            Some(p) => p,
            None => configured(scalars.get("port").copied(), "22", parse_port)?,
        };
        let interval = scalars.get("serveraliveinterval").copied();
        let max_unanswered = scalars.get("serveralivecountmax").copied();
        let keepalive = if interval.is_none() && max_unanswered.is_none() {
            SshKeepalive::default()
        } else {
            // When an alias supplies a keepalive directive, use OpenSSH's
            // complementary default for any directive it leaves unspecified.
            SshKeepalive {
                interval_seconds: configured(interval, "0", parse_time_seconds)?,
                max_unanswered: configured(max_unanswered, "3", |value| {
                    value
                        .parse()
                        .context("ServerAliveCountMax must be a positive whole number")
                })?,
            }
        };
        let connect_timeout_seconds =
            configured(scalars.get("connecttimeout").copied(), "none", |value| {
                if value.eq_ignore_ascii_case("none") {
                    return Ok(None);
                }
                Ok(std::num::NonZeroU32::new(parse_time_seconds(value)?))
            })?;
        let port_text = port.to_string();
        let tokens = [
            ('h', host.as_str()),
            ('n', alias.as_str()),
            ('p', port_text.as_str()),
            ('r', user),
        ];
        let via = if let Some(d) = proxy {
            if d.key == "proxycommand" {
                ensure!(
                    d.args.len() == 1 && d.args[0].eq_ignore_ascii_case("none"),
                    "{}: ProxyCommand is unsupported; this route cannot be imported",
                    d.line
                );
                None
            } else {
                Some((
                    expand(argument(d)?, &tokens)
                        .with_context(|| format!("{}: ProxyJump", d.line))?,
                    d.line.clone(),
                ))
            }
        } else {
            None
        };
        let mut identity = None;
        for d in identities {
            let value = argument(d)?;
            if value.eq_ignore_ascii_case("none") {
                continue;
            }
            let mut tokens = tokens.to_vec();
            let home = self.home.to_str().context("non-Unicode home path")?;
            tokens.extend([
                ('d', home),
                ('u', self.user),
                ('k', host_key_alias.as_deref().unwrap_or(alias)),
            ]);
            let path =
                expand(value, &tokens).with_context(|| format!("{}: IdentityFile", d.line))?;
            let path =
                if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
                    self.home.join(rest)
                } else {
                    ensure!(
                        !path.starts_with('~'),
                        "{}: unsupported named-user home in IdentityFile",
                        d.line
                    );
                    PathBuf::from(path)
                };
            ensure!(
                path.is_absolute(),
                "{}: relative IdentityFile is ambiguous; use ~/ or an absolute path",
                d.line
            );
            ensure!(
                identity.as_ref().is_none_or(|first| first == &path),
                "{}: multiple IdentityFile entries are additive in OpenSSH; select one identity for this profile",
                d.line
            );
            identity = Some(path);
        }
        configured(scalars.get("identitiesonly").copied(), "no", |value| {
            ensure!(
                value.eq_ignore_ascii_case("yes") || value.eq_ignore_ascii_case("no"),
                "IdentitiesOnly requires yes or no"
            );
            ensure!(
                identity.is_some() || !value.eq_ignore_ascii_case("yes"),
                "IdentitiesOnly yes requires an explicit identity file"
            );
            Ok(())
        })?;
        let authentication = match identity {
            Some(identity) => Authentication::Identity { identity },
            None => Authentication::Agent,
        };
        Ok((
            JumpHost {
                host,
                user: user.into(),
                port,
                connect_timeout_seconds,
                keepalive,
                authentication,
                host_key: None,
                host_key_alias,
            },
            via,
        ))
    }
}

fn parse_time_seconds(mut value: &str) -> Result<u32> {
    ensure!(!value.is_empty(), "Missing time value");
    let mut seconds = 0_u32;
    while !value.is_empty() {
        let digits = value.len() - value.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let (number, suffix) = value.split_at(digits);
        let number: u32 = number.parse().context("Invalid time value")?;
        let factor = match suffix.as_bytes().first().map(u8::to_ascii_lowercase) {
            None | Some(b's') => 1,
            Some(b'm') => 60,
            Some(b'h') => 3_600,
            Some(b'd') => 86_400,
            Some(b'w') => 604_800,
            _ => bail!("Invalid time unit"),
        };
        seconds = number
            .checked_mul(factor)
            .and_then(|part| seconds.checked_add(part))
            .context("Time value is too large")?;
        value = suffix.get(1..).unwrap_or_default();
    }
    Ok(seconds)
}

fn warn(warnings: &mut BTreeSet<String>, message: impl FnOnce() -> String) {
    if warnings.len() < 64 {
        warnings.insert(message());
    } else if warnings.len() == 64 {
        warnings.insert("Additional unsupported directives omitted from this preview.".into());
    }
}

fn configured<'a, T>(
    directive: Option<&'a Directive>,
    default: &'a str,
    parse: impl FnOnce(&'a str) -> Result<T>,
) -> Result<T> {
    match directive {
        Some(d) => parse(argument(d)?).with_context(|| format!("{}: {}", d.line, d.key)),
        None => parse(default),
    }
}

fn argument(directive: &Directive) -> Result<&str> {
    ensure!(
        directive.args.len() == 1,
        "{}: {} requires one argument",
        directive.line,
        directive.key
    );
    Ok(&directive.args[0])
}

fn expand(value: &str, tokens: &[(char, &str)]) -> Result<String> {
    ensure!(
        !value.contains("${"),
        "environment expansion is unsupported"
    );
    let mut output = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            output.push(c);
            continue;
        }
        let token = chars.next().context("incomplete SSH config token")?;
        if token == '%' {
            output.push('%');
            continue;
        }
        let (_, replacement) = tokens
            .iter()
            .find(|(key, _)| *key == token)
            .with_context(|| format!("unsupported SSH config token %{token}"))?;
        output.push_str(replacement);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse_config(text: &str) -> ImportReport {
        includes::parse_test(text, Path::new(r"C:\Users\pilot"), "local-pilot").unwrap()
    }
    fn alias<'a>(report: &'a ImportReport, name: &str) -> &'a ImportedAlias {
        report
            .aliases
            .iter()
            .find(|alias| alias.name == name)
            .unwrap()
    }
    #[test]
    fn host_key_alias_is_literal_per_hop_and_available_as_identity_file_token() {
        let report = parse_config(
            r#"
            Host dest
                HostName 127.0.0.1
                Port 2200
                HostKeyAlias DEV-%h
                ProxyJump gate
                IdentityFile ~/keys/%k
            Host gate
                HostKeyAlias [BASTION]:22 # comment
            Host plain
                HostName real.example
                IdentityFile ~/keys/%k
            Host dest gate
                HostKeyAlias ignored
            Host bad
                ProxyJump %k
            Host punctuated
                HostKeyAlias CLUSTER+slot=2@blue!node#3|old
        "#,
        );
        let dest = alias(&report, "dest").options.as_ref().unwrap();
        assert_eq!(dest.host_key_alias.as_deref(), Some("dev-%h"));
        assert_eq!((dest.host.as_str(), dest.port), ("127.0.0.1", 2200));
        let identity = |key: &str| Authentication::Identity {
            identity: Path::new(r"C:\Users\pilot").join("keys").join(key),
        };
        assert_eq!(dest.authentication, identity("dev-%h"));
        assert_eq!(
            dest.proxy_jump[0].host_key_alias.as_deref(),
            Some("[bastion]:22")
        );
        let plain = alias(&report, "plain").options.as_ref().unwrap();
        assert!(plain.host_key_alias.is_none());
        assert_eq!(plain.authentication, identity("plain"));
        assert!(alias(&report, "bad").options.is_none());
        assert_eq!(
            alias(&report, "punctuated")
                .options
                .as_ref()
                .unwrap()
                .host_key_alias
                .as_deref(),
            Some("cluster+slot=2@blue!node#3|old")
        );
        for bad in ["a,b", "foo*bar", "two words", "\"line with whitespace\""] {
            let report = parse_config(&format!("Host bad\n HostKeyAlias {bad}"));
            assert!(alias(&report, "bad").options.is_none(), "{bad}");
        }
        let conditional =
            parse_config("Host guarded\nMatch exec never-run\n HostKeyAlias conditional");
        assert!(alias(&conditional, "guarded").options.is_none());
    }
    #[test]
    fn resolves_explicit_hosts_patterns_quotes_and_first_obtained_scalars() {
        let report = parse_config(
            r#"
            User default
            Host admin admin-dev
                User too-late
                HostName = %h.example
                IdentityFile "~/.ssh/with space #1" # outside comment
                Port=2209
            Host *
                Port 22
                ServerAliveInterval 15
            Host a* !admin
                ProxyJump gateway
            Host gateway
                HostName gateway.example
                User never-used
        "#,
        );
        assert_eq!(report.aliases.len(), 3);
        let admin = alias(&report, "admin");
        let options = admin.options.as_ref().unwrap();
        assert_eq!(
            (&*options.host, &*options.user, options.port),
            ("admin.example", "default", 2209)
        );
        assert_eq!(
            options.authentication,
            Authentication::Identity {
                identity: Path::new(r"C:\Users\pilot").join(".ssh/with space #1")
            }
        );
        assert!(options.proxy_jump.is_empty());
        assert_eq!(options.keepalive.interval_seconds, 15);
        assert_eq!(options.keepalive.max_unanswered.get(), 3);
        assert!(admin.warnings.is_empty());
        let dev = alias(&report, "admin-dev").options.as_ref().unwrap();
        assert_eq!(dev.proxy_jump[0].host, "gateway.example");
        assert_eq!(dev.proxy_jump[0].port, 22);
        assert_eq!(dev.proxy_jump[0].authentication, Authentication::Agent);
        assert_eq!(dev.proxy_jump[0].keepalive, options.keepalive);
    }

    #[test]
    fn keepalives_respect_each_hop_and_first_value_with_openssh_time_units() {
        let report = parse_config(
            "Host target\n ProxyJump gateway\n ServerAliveInterval 1m15s\n ServerAliveCountMax 6\n Host gateway\n ServerAliveInterval 0\n Host *\n ServerAliveInterval 25",
        );
        let target = alias(&report, "target").options.as_ref().unwrap();
        assert_eq!(target.keepalive.interval_seconds, 75);
        assert_eq!(target.keepalive.max_unanswered.get(), 6);
        assert_eq!(target.proxy_jump[0].keepalive.interval(), None);
        assert_eq!(target.proxy_jump[0].keepalive.max_unanswered.get(), 3);
        for directive in [
            "ServerAliveInterval -1",
            "ServerAliveInterval 5x",
            "ServerAliveInterval 999999w",
            "ServerAliveInterval 5\n ServerAliveCountMax 0",
        ] {
            let report = parse_config(&format!("Host bad\n {directive}"));
            assert!(alias(&report, "bad").options.is_none(), "{directive}");
        }
    }

    #[test]
    fn connect_timeout_imports_independent_destination_and_jump_values() {
        let report = parse_config(
            "Host target\n ProxyJump gateway\n ConnectTimeout 1m5s\n ConnectTimeout 4\n Host gateway\n ConnectTimeout 2s\n Host default\n ConnectTimeout none\n Host zero\n ConnectTimeout 0",
        );
        let timeout = |host| alias(&report, host).options.as_ref().unwrap();
        let target = timeout("target");
        assert_eq!(target.connect_timeout_seconds.unwrap().get(), 65);
        assert_eq!(
            target.proxy_jump[0].connect_timeout_seconds.unwrap().get(),
            2
        );
        assert!(timeout("default").connect_timeout_seconds.is_none());
        assert!(timeout("zero").connect_timeout_seconds.is_none());
        let bad = parse_config("Host bad\n ConnectTimeout 2x");
        assert!(alias(&bad, "bad").options.is_none());
    }
    #[test]
    fn resolves_nested_and_explicit_jump_routes_with_independent_flags() {
        let report = parse_config(
            r#"
            Host dest
                User destination
                ProxyJump alice@outer:2201,ssh://bob@[::1]:2202
            Host outer
                HostName outer.example
                User fromconfig
                Port 2999
                IdentityFile ~/keys/%r-%p
                ProxyJump first
            Host first
                HostName first.example
        "#,
        );
        let dest = alias(&report, "dest").options.as_ref().unwrap();
        let route: Vec<_> = dest
            .proxy_jump
            .iter()
            .map(|j| (j.host.as_str(), j.user.as_str(), j.port))
            .collect();
        assert_eq!(
            route,
            [
                ("first.example", "local-pilot", 22),
                ("outer.example", "alice", 2201),
                ("::1", "bob", 2202)
            ]
        );
        assert_eq!(
            dest.proxy_jump[1].authentication,
            Authentication::Identity {
                identity: Path::new(r"C:\Users\pilot").join("keys/alice-2201")
            }
        );
    }
    #[test]
    fn unknown_proxy_commands_conditional_trust_and_ambiguous_keys_disable_import() {
        let report = parse_config(
            r#"
            Host one
                ProxyCommand external-tool %h
                ProxyJump ignored
            Host two
                ProxyJump none
                ProxyCommand ignored
            Host keys
                IdentityFile ~/.ssh/one
                IdentityFile ~/.ssh/two
            Host trust
                UserKnownHostsFile ~/.ssh/custom
        "#,
        );
        assert!(alias(&report, "one").options.is_none());
        assert!(
            alias(&report, "one")
                .warnings
                .iter()
                .any(|w| w.contains("ProxyCommand"))
        );
        assert!(
            alias(&report, "two")
                .options
                .as_ref()
                .unwrap()
                .proxy_jump
                .is_empty()
        );
        assert!(alias(&report, "keys").options.is_none());
        assert!(alias(&report, "trust").options.is_none());
        let conditional =
            parse_config("Host one\n  Port 2222\nMatch exec ignored\n ProxyJump unknown\n");
        assert!(alias(&conditional, "one").options.is_none());
        assert!(conditional.warnings.iter().any(|w| w.contains("Match")));
    }
    #[test]
    fn rejects_route_cycles_excessive_depth_and_invalid_ports() {
        let cycle = parse_config("Host a\n ProxyJump b\nHost b\n ProxyJump a\nHost bad\n Port 0");
        assert!(cycle.aliases.iter().all(|a| a.options.is_none()));
        let targets = (1..=MAX_JUMPS + 1)
            .map(|i| format!("hop{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let too_many = parse_config(&format!("Host final\n ProxyJump {targets}"));
        assert!(alias(&too_many, "final").options.is_none());
        let too_few = parse_config("Host ip\n ProxyJump ::1:22,");
        assert!(alias(&too_few, "ip").options.is_none());
    }
}
