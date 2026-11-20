//! Import a selected OpenSSH client configuration into editable native profiles.
use crate::options::{Authentication, JumpHost, MAX_JUMPS, SshOptions};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
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

pub fn import_file(path: &Path) -> Result<ImportReport> {
    const LIMIT: u64 = 1024 * 1024;
    let mut text = String::new();
    std::fs::File::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .take(LIMIT + 1)
        .read_to_string(&mut text)
        .context("reading UTF-8 SSH config")?;
    ensure!(text.len() as u64 <= LIMIT, "SSH config exceeds 1 MiB");
    let home = std::env::home_dir().context("cannot determine the Windows home directory")?;
    let user = std::env::var("USERNAME").context("cannot determine the Windows user")?;
    parse(&text, &home, &user)
}

enum Scope {
    Global,
    Hosts(Vec<String>),
    Conditional(usize),
}
struct Section {
    scope: Scope,
    directives: Vec<Directive>,
}
struct Directive {
    line: usize,
    key: String,
    args: Vec<String>,
}

fn parse(text: &str, home: &Path, user: &str) -> Result<ImportReport> {
    let mut sections = vec![Section {
        scope: Scope::Global,
        directives: Vec::new(),
    }];
    let mut aliases = BTreeSet::new();
    let mut warnings = BTreeSet::new();
    for (i, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let line_no = i + 1;
        ensure!(line_no <= 16_384, "SSH config exceeds 16384 lines");
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let split = line
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(line.len());
        let key = line[..split].to_ascii_lowercase();
        let value = line[split..].trim_start();
        let args = words(value.strip_prefix('=').unwrap_or(value))
            .with_context(|| format!("line {line_no}"))?;
        ensure!(
            !key.is_empty()
                && key.len() <= 128
                && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !args.is_empty(),
            "line {line_no}: invalid directive or missing argument"
        );
        match key.as_str() {
            "host" => {
                for alias in &args {
                    if !alias.starts_with('!') && !alias.contains(['*', '?']) {
                        valid_host(alias)
                            .with_context(|| format!("line {line_no}: invalid Host alias"))?;
                        aliases.insert(alias.clone());
                    }
                }
                sections.push(Section {
                    scope: Scope::Hosts(args),
                    directives: vec![],
                });
            }
            "match" => {
                warnings.insert(format!(
                    "Line {line_no}: Match conditions are unsupported and were not evaluated."
                ));
                sections.push(Section {
                    scope: Scope::Conditional(line_no),
                    directives: vec![],
                });
            }
            _ => {
                if key == "include" {
                    warnings.insert(format!(
                        "Line {line_no}: Include is unsupported. Included files were not opened."
                    ));
                }
                sections.last_mut().unwrap().directives.push(Directive {
                    line: line_no,
                    key,
                    args,
                });
            }
        }
    }
    ensure!(
        aliases.len() <= 1024,
        "SSH config exceeds 1024 explicit host aliases"
    );
    if aliases.is_empty() {
        warnings.insert("No explicit Host aliases were found.".into());
    }
    let config = Config {
        sections,
        home,
        user,
    };
    let aliases = aliases
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
    Ok(ImportReport {
        aliases,
        warnings: warnings.into_iter().collect(),
    })
}

fn words(line: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut word = String::new();
    let mut quoted = None;
    let mut started = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if quoted.is_none() && ch == '#' {
            break;
        }
        if matches!(ch, '\'' | '"') && (quoted.is_none() || quoted == Some(ch)) {
            quoted = if quoted.is_some() { None } else { Some(ch) };
            started = true;
        } else if ch == '\\'
            && chars
                .peek()
                .is_some_and(|c| matches!(c, '"' | '\'' | '#') || c.is_whitespace())
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

fn matches_patterns(patterns: &[String], host: &str) -> bool {
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
    ensure!(
        !host.is_empty()
            && host.len() <= 255
            && !host.starts_with('-')
            && !host
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || "/\\@?#[]".contains(c)),
        "invalid SSH host {host:?}"
    );
    Ok(())
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
            | "hostkeyalias"
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
        if let Some(via) = via.filter(|s| !s.eq_ignore_ascii_case("none")) {
            stack.push(spec.alias);
            for (index, hop) in via.split(',').enumerate() {
                // An explicit preceding -J path on the command line wins over that intermediate alias's own ProxyJump.
                self.append(EndpointSpec::jump(hop)?, route, stack, index == 0, warnings)?;
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
    ) -> Result<(JumpHost, Option<String>)> {
        let mut scalars = BTreeMap::new();
        let mut identities = Vec::new();
        let mut proxy = None;
        for section in &self.sections {
            match &section.scope {
                Scope::Hosts(patterns) if !matches_patterns(patterns, &spec.alias) => continue,
                Scope::Conditional(line) => {
                    if section.directives.iter().any(|d| {
                        sensitive(&d.key)
                            || matches!(
                                d.key.as_str(),
                                "hostname"
                                    | "user"
                                    | "port"
                                    | "identityfile"
                                    | "identitiesonly"
                                    | "proxyjump"
                                    | "proxycommand"
                            )
                    }) {
                        bail!(
                            "Line {line}: unsupported Match may change the connection, identity or trust settings"
                        );
                    }
                    continue;
                }
                _ => {}
            }
            for d in &section.directives {
                let key = d.key.as_str();
                match key {
                    "hostname" | "user" | "port" | "identitiesonly" => {
                        scalars.entry(key).or_insert(d);
                    }
                    "identityfile" => identities.push(d),
                    "proxyjump" | "proxycommand" => {
                        if expand_jump && proxy.is_none() {
                            proxy = Some(d);
                        }
                    }
                    key if sensitive(key) => bail!(
                        "Line {}: unsupported {key} could change routing, authentication or trust",
                        d.line
                    ),
                    key if warnings.len() < 64 => {
                        warnings.insert(format!(
                            "Host {}, line {}: unsupported {key} was ignored.",
                            spec.alias, d.line
                        ));
                    }
                    _ if warnings.len() == 64 => {
                        warnings.insert(
                            "Additional unsupported directives omitted from this preview.".into(),
                        );
                    }
                    _ => {}
                }
            }
        }
        let scalar =
            |k: &str| -> Result<Option<&str>> { scalars.get(k).map(|d| argument(d)).transpose() };
        let alias = &spec.alias;
        let host = expand(scalar("hostname")?.unwrap_or(alias), &[('h', alias)])?;
        valid_host(&host)?;
        let user = spec
            .user
            .as_deref()
            .or(scalar("user")?)
            .unwrap_or(self.user);
        ensure!(
            !user.is_empty() && !user.chars().any(|c| c.is_whitespace() || c.is_control()),
            "invalid SSH username"
        );
        let port = match spec.port {
            Some(p) => p,
            None => scalar("port")?.map(parse_port).transpose()?.unwrap_or(22),
        };
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
                    "Line {}: ProxyCommand is unsupported; this route cannot be imported",
                    d.line
                );
                None
            } else {
                Some(expand(argument(d)?, &tokens)?)
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
            tokens.extend([('d', home), ('u', self.user)]);
            let path = expand(value, &tokens)?;
            let path =
                if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
                    self.home.join(rest)
                } else {
                    ensure!(
                        !path.starts_with('~'),
                        "unsupported named-user home in IdentityFile"
                    );
                    PathBuf::from(path)
                };
            ensure!(
                path.is_absolute(),
                "relative IdentityFile is ambiguous; use ~/ or an absolute path"
            );
            ensure!(
                identity.as_ref().is_none_or(|first| first == &path),
                "multiple IdentityFile entries are additive in OpenSSH; select one identity for this profile"
            );
            identity = Some(path);
        }
        let identities_only = scalar("identitiesonly")?.unwrap_or("no");
        ensure!(
            identities_only.eq_ignore_ascii_case("yes")
                || identities_only.eq_ignore_ascii_case("no"),
            "IdentitiesOnly requires yes or no"
        );
        let authentication = match identity {
            Some(identity) => Authentication::Identity { identity },
            None => {
                ensure!(
                    !identities_only.eq_ignore_ascii_case("yes"),
                    "IdentitiesOnly yes requires an explicit identity file"
                );
                Authentication::Agent
            }
        };
        Ok((
            JumpHost {
                host,
                user: user.into(),
                port,
                authentication,
                host_key: None,
            },
            via,
        ))
    }
}

fn argument(directive: &Directive) -> Result<&str> {
    ensure!(
        directive.args.len() == 1,
        "line {}: {} requires one argument",
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
        parse(text, Path::new(r"C:\Users\pilot"), "local-pilot").unwrap()
    }
    fn alias<'a>(report: &'a ImportReport, name: &str) -> &'a ImportedAlias {
        report
            .aliases
            .iter()
            .find(|alias| alias.name == name)
            .unwrap()
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
        assert!(
            admin
                .warnings
                .iter()
                .any(|s| s.contains("serveraliveinterval"))
        );
        let dev = alias(&report, "admin-dev").options.as_ref().unwrap();
        assert_eq!(dev.proxy_jump[0].host, "gateway.example");
        assert_eq!(dev.proxy_jump[0].port, 22);
        assert_eq!(dev.proxy_jump[0].authentication, Authentication::Agent);
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
