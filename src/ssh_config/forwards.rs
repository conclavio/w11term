use super::{Directive, Scope, Section, matches_patterns, warn};
use crate::options::LocalForwardPreset;
use std::{collections::BTreeSet, num::NonZeroU16};

#[cfg(test)]
mod tests;

pub(super) fn is_directive(key: &str) -> bool {
    matches!(
        key,
        "localforward"
            | "remoteforward"
            | "dynamicforward"
            | "gatewayports"
            | "clearallforwardings"
    )
}

#[derive(Clone, Copy, PartialEq)]
enum Applies {
    Yes,
    No,
    Unknown,
}

// Match originalhost and host are decidable without running a command, opening
// a network connection, or relying on the current Windows network or account.
fn match_scope(args: &[String], alias: &str, host: &str) -> Applies {
    let mut result = Applies::Yes;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let (criterion, inline) = match arg.split_once('=') {
            Some((key, value)) => (key, Some(value)),
            None => (arg.as_str(), None),
        };
        let name = criterion
            .strip_prefix('!')
            .unwrap_or(criterion)
            .to_ascii_lowercase();
        let negated = criterion.starts_with('!');
        match name.as_str() {
            "all" if inline.is_none() => {
                if negated {
                    return Applies::No;
                }
                continue;
            }
            "canonical" | "final" if inline.is_none() => {
                result = Applies::Unknown;
                continue;
            }
            _ => {}
        }
        let Some(value) = inline.or_else(|| args.next().map(String::as_str)) else {
            return Applies::Unknown;
        };
        let target = match name.as_str() {
            "host" => host,
            "originalhost" => alias,
            _ => {
                result = Applies::Unknown;
                continue;
            }
        };
        if matches_patterns(value.split(','), target) == negated {
            return Applies::No;
        }
    }
    result
}

// The first applicable value wins. An unresolved Match may precede it. Keep
// the first directive that could restrict a forward, including malformed values.
#[derive(Default)]
struct Restriction<'a> {
    final_value_seen: bool,
    possible: Option<&'a Directive>,
}

impl<'a> Restriction<'a> {
    fn read(&mut self, d: &'a Directive, applies: Applies, allowed: &[&str]) {
        if self.final_value_seen {
            return;
        }
        self.final_value_seen = applies == Applies::Yes;
        if !matches!(d.args.as_slice(), [value] if allowed.iter().any(|a| value.eq_ignore_ascii_case(a)))
        {
            self.possible.get_or_insert(d);
        }
    }
}

pub(super) fn resolve(
    sections: &[Section],
    alias: &str,
    resolved_host: &str,
    warnings: &mut BTreeSet<String>,
) -> Vec<LocalForwardPreset> {
    let mut gateway = Restriction::default();
    let mut clear = Restriction::default();
    let mut address_family = Restriction::default();
    let mut rules = Vec::new();
    let mut host = alias;
    for section in sections {
        let applies = match &section.scope {
            Scope::Global => Applies::Yes,
            Scope::Hosts(patterns) => {
                if !matches_patterns(patterns.iter().map(String::as_str), alias) {
                    continue;
                }
                Applies::Yes
            }
            Scope::Conditional(_, args) => match_scope(args, alias, host),
        };
        if applies == Applies::No {
            continue;
        }
        for d in &section.directives {
            match d.key.as_str() {
                // Match host uses the HostName obtained up to that point.
                // The endpoint resolver already validated and expanded it.
                "hostname" if applies == Applies::Yes => host = resolved_host,
                "gatewayports" => gateway.read(d, applies, &["no"]),
                "clearallforwardings" => clear.read(d, applies, &["no"]),
                "addressfamily" => address_family.read(d, applies, &["any", "inet"]),
                "localforward" | "remoteforward" | "dynamicforward" => rules.push((d, applies)),
                _ => {}
            }
        }
    }
    if let Some(d) = clear.possible {
        if !rules.is_empty() {
            warn(warnings, || {
                format!(
                    "Host {alias}, {}: ClearAllForwardings {} can clear this host’s forwarding rules. No presets were imported.",
                    d.line,
                    arguments(d)
                )
            });
        }
        return Vec::new();
    }

    let mut seen = BTreeSet::new();
    let mut presets = Vec::new();
    for (d, applies) in rules {
        let name = match d.key.as_str() {
            "remoteforward" => "RemoteForward",
            "dynamicforward" => "DynamicForward",
            _ => "LocalForward",
        };
        let parsed = match (applies, name) {
            (Applies::Unknown, _) => Err("the Match condition could not be determined"),
            (_, "RemoteForward") => Err("remote listeners are unsupported"),
            (_, "DynamicForward") => Err("SOCKS proxy listeners are unsupported"),
            _ => parse(&d.args, gateway.possible.is_some()),
        };
        match parsed {
            Ok(_) if let Some(family) = address_family.possible => warn(warnings, || {
                format!(
                    "Host {alias}, {}: LocalForward {} was skipped because AddressFamily {} at {} can exclude an IPv4 listener.",
                    d.line,
                    arguments(d),
                    arguments(family),
                    family.line
                )
            }),
            Ok(preset) if seen.insert(preset) => presets.push(preset),
            Ok(_) => {}
            Err(reason) => warn(warnings, || {
                format!(
                    "Host {alias}, {}: {name} {} was skipped: {reason}.",
                    d.line,
                    arguments(d)
                )
            }),
        }
    }
    presets
}

// Many aliases can share a rule. Keep preview text bounded even for long
// socket paths, while preserving the location and the original endpoint pair.
fn arguments(d: &Directive) -> String {
    let mut chars = d
        .args
        .iter()
        .flat_map(|s| std::iter::once(' ').chain(s.chars()))
        .skip(1);
    let mut text: String = chars.by_ref().take(192).collect();
    if chars.next().is_some() {
        text.push('…');
    }
    text
}

fn parse(args: &[String], gateway_may_enable: bool) -> Result<LocalForwardPreset, &'static str> {
    let [listener, destination] = args else {
        return Err("expected a local bind and one remote destination");
    };
    if listener.contains('/') || destination.contains('/') {
        return Err("Unix-domain socket paths are unsupported");
    }
    let port = |text: &str| {
        let invalid = "ports must be decimal numbers from 1 to 65535";
        if !text.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid);
        }
        text.parse::<NonZeroU16>().map_err(|_| invalid)
    };
    let loopback = |host: &str| {
        let host = host
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(host);
        host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost")
    };
    let local_port = match listener.rsplit_once(':') {
        Some((bind, value)) if loopback(bind) => port(value)?,
        Some(_) => return Err("the local bind must be IPv4 loopback"),
        None if gateway_may_enable => {
            return Err("GatewayPorts can expose an implicit bind beyond loopback");
        }
        None => port(listener)?,
    };
    let (host, value) = destination
        .rsplit_once(':')
        .ok_or("expected a remote host and TCP port")?;
    if !loopback(host) {
        return Err("the remote destination must be IPv4 loopback");
    }
    Ok(LocalForwardPreset {
        local_port,
        remote_port: port(value)?,
    })
}
