use super::*;
use std::{borrow::Cow, net::Ipv6Addr, num::NonZeroU16};

impl SshOptions {
    /// Resolve an explicit SSH address without consulting a profile or inferring a user.
    pub fn from_destination(input: &str) -> Result<Self> {
        let input = input.trim();
        let (uri, authority) = match input.split_once("://") {
            Some((scheme, rest)) => {
                ensure!(scheme.eq_ignore_ascii_case("ssh"), "Use an ssh:// address");
                (true, rest.strip_suffix('/').unwrap_or(rest))
            }
            None => (false, input),
        };
        ensure!(
            !authority
                .chars()
                .any(|ch| ch.is_whitespace() || ch.is_control())
                && !authority.contains(['/', '?', '#'])
                && (!uri || !authority.contains('\\')),
            "Enter an SSH address without spaces, a path, query, or fragment"
        );
        let (user, address) = authority
            .rsplit_once('@')
            .context("Enter an explicit user@host or ssh://user@host address")?;
        ensure!(
            !user.contains(':') && (!uri || !user.contains('@')),
            "Enter an SSH username. Supply passwords through the authentication prompt"
        );
        let user = decode(user, uri)?;
        ensure!(
            !user.is_empty() && !user.chars().any(|ch| ch.is_whitespace() || ch.is_control()),
            "Enter a nonempty SSH username without spaces or control characters"
        );
        let (host, port) = parse_host(address, uri)?;
        Ok(Self {
            host: host.into_owned(),
            user: user.into_owned(),
            port,
            terminal_type: Default::default(),
            scrollback_lines: Default::default(),
            color_scheme: Default::default(),
            font_size: None,
            tab_color: Default::default(),
            authentication: Authentication::Agent,
            host_key: None,
            host_key_alias: None,
            proxy_jump: Vec::new(),
            local_forwards: Vec::new(),
            durable: false,
            group_prompt: None,
            player: Default::default(),
        })
    }
}

fn decode(value: &str, uri: bool) -> Result<Cow<'_, str>> {
    if !uri {
        return Ok(value.into());
    }
    let bytes = value.as_bytes();
    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b'%' {
            ensure!(
                bytes
                    .get(index + 1..index + 3)
                    .is_some_and(|pair| pair.iter().all(u8::is_ascii_hexdigit)),
                "Invalid percent escape in the SSH address"
            );
        }
    }
    percent_encoding::percent_decode_str(value)
        .decode_utf8()
        .context("The SSH address must contain valid UTF-8")
}

fn parse_host(input: &str, uri: bool) -> Result<(Cow<'_, str>, u16)> {
    let (host, explicit_port, bracketed) = if let Some(input) = input.strip_prefix('[') {
        let (host, end) = input
            .split_once(']')
            .context("Close the IPv6 address with ]")?;
        let port = if end.is_empty() {
            None
        } else {
            Some(
                end.strip_prefix(':')
                    .context("Use :port after the IPv6 address")?,
            )
        };
        (host, port, true)
    } else if let Some((host, port)) = input.split_once(':')
        && !port.contains(':')
    {
        (host, Some(port), false)
    } else {
        (input, None, false)
    };
    let host = decode(host, uri)?;
    crate::options::validate_ssh_host(&host)
        .map_err(|_| anyhow::anyhow!("Enter a valid SSH host"))?;
    if bracketed || host.contains(':') {
        let address = match host.split_once('%') {
            Some((address, zone)) => {
                ensure!(
                    !zone.is_empty()
                        && zone
                            .bytes()
                            .all(|ch| ch.is_ascii_alphanumeric() || b"_.-".contains(&ch)),
                    "Enter a valid IPv6 scope"
                );
                address
            }
            None => &host,
        };
        ensure!(
            (bracketed || (!uri && explicit_port.is_none())) && address.parse::<Ipv6Addr>().is_ok(),
            "Use [IPv6] in an SSH URL or when specifying a port"
        );
    }
    let port = explicit_port.map_or(Ok(default_port()), |port| {
        ensure!(
            port.bytes().all(|byte| byte.is_ascii_digit()),
            "SSH port must be between 1 and 65535"
        );
        Ok(port
            .parse::<NonZeroU16>()
            .context("SSH port must be between 1 and 65535")?
            .get())
    })?;
    Ok((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_explicit_users_ports_ipv6_and_uri_escapes() {
        for (input, user, host, port) in [
            ("root@example.org", "root", "example.org", 22),
            ("play@example.org:6792", "play", "example.org", 6792),
            ("ssh://acct%2Bpilot@host:123/", "acct+pilot", "host", 123),
            (
                "SSH://acct%40realm@[fe80::1%2512]",
                "acct@realm",
                "fe80::1%12",
                22,
            ),
            ("alice@realm@[::1]:2222", "alice@realm", "::1", 2222),
            ("root@2001:db8::1", "root", "2001:db8::1", 22),
        ] {
            let options = SshOptions::from_destination(input).unwrap();
            assert_eq!(
                (options.user.as_str(), options.host.as_str(), options.port),
                (user, host, port)
            );
        }
    }

    #[test]
    fn rejects_ambiguous_authorities_and_invalid_ports() {
        for input in [
            "host",
            "ssh://host",
            "https://u@host",
            "ssh://u:pass@host",
            "ssh://u@host/command",
            "ssh://u@host?x",
            "ssh://u@host#x",
            "ssh://u%0A@host",
            "ssh://u%FF@host",
            "ssh://u%2@host",
            "ssh://u@[::1",
            "ssh://u@::1",
            "ssh://u@host%2Fother",
            "u@[host]:22",
            "u@bad host",
            "u@host:0",
            "u@host:",
            "u@host:+22",
            "u@host:65536",
            "u@[::1]22",
        ] {
            assert!(SshOptions::from_destination(input).is_err(), "{input}");
        }
    }
}
