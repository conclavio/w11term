use super::super::address::{Endpoint, display};
use super::ranges::Expansion;
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeMap, path::Path};
use w11term::options::{Authentication, Profiles, SshOptions};

pub(super) const MAX_BYTES: usize = 32 * 1024;
pub(super) const MAX_TARGETS: usize = 64;

#[derive(Debug, PartialEq)]
pub(super) struct Row {
    pub line: usize,
    pub target: Result<Target, String>,
}

#[derive(Debug, PartialEq)]
pub(super) struct Target {
    pub profile: Option<String>,
    pub ssh: SshOptions,
}

impl Target {
    pub fn label(&self) -> String {
        let address = address(&self.ssh);
        self.profile.as_ref().map_or_else(
            || format!("{address} · One-time connection"),
            |name| format!("{name} · {address}"),
        )
    }
}

pub(super) fn address(ssh: &SshOptions) -> String {
    display(&ssh.user, &ssh.host, ssh.port)
}

pub(super) fn build(
    input: &str,
    default_user: &str,
    catalog: &Profiles,
    catalog_path: &Path,
    authentication: Result<Authentication, String>,
) -> Result<Vec<Row>> {
    check_size(input.len())?;
    let mut profiles = BTreeMap::<(String, u16), Vec<&str>>::new();
    for (name, ssh) in &catalog.profiles {
        let endpoint = Endpoint::from(ssh);
        profiles
            .entry((endpoint.host, endpoint.port))
            .or_default()
            .push(name);
    }
    let mut rows = Vec::new();
    let mut seen = BTreeMap::<Endpoint, usize>::new();
    let named = |name| catalog.profiles.contains_key(name) || catalog.local.contains_key(name);
    for (index, input) in input.replace("\r\n", "\n").split(['\n', '\r']).enumerate() {
        let input = input.trim();
        if input.is_empty() || input.starts_with('#') {
            continue;
        }
        let input = if named(input) {
            input
        } else {
            without_comment(input)
        };
        let line = index + 1;
        let remaining = MAX_TARGETS - rows.len();
        ensure!(
            remaining > 0,
            "A host list can expand to at most {MAX_TARGETS} entries."
        );
        if named(input) {
            push(&mut rows, &mut seen, line, saved(catalog, input));
            continue;
        }
        let expansion = match Expansion::parse(input) {
            Ok(expansion) => expansion,
            Err(error) => {
                push(&mut rows, &mut seen, line, Err(error));
                continue;
            }
        };
        let mut append = |input: &str| {
            let target = resolve(
                input,
                default_user,
                catalog,
                catalog_path,
                &profiles,
                &authentication,
            );
            push(&mut rows, &mut seen, line, target);
        };
        if let Some(expansion) = expansion {
            for address in expansion.expand(remaining)? {
                append(&address);
            }
        } else {
            append(input);
        }
    }
    ensure!(
        !rows.is_empty(),
        "Enter at least one SSH profile or address."
    );
    Ok(rows)
}

fn without_comment(input: &str) -> &str {
    let Some((body, _)) = input.split_once('#') else {
        return input;
    };
    let trimmed = body.trim_end();
    // An attached hash in an SSH URI is a fragment. With whitespace it starts a comment.
    if body == trimmed && body.contains("://") {
        input
    } else {
        trimmed
    }
}

pub(super) fn check_size(bytes: usize) -> Result<()> {
    ensure!(bytes <= MAX_BYTES, "The host list is limited to 32 KiB.");
    Ok(())
}

fn push(
    rows: &mut Vec<Row>,
    seen: &mut BTreeMap<Endpoint, usize>,
    line: usize,
    target: Result<Target>,
) {
    let target = target
        .and_then(|target| {
            match seen.entry((&target.ssh).into()) {
                std::collections::btree_map::Entry::Occupied(first) => anyhow::bail!(
                    "Duplicate {} · first listed on line {}",
                    address(&target.ssh),
                    first.get()
                ),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(line);
                }
            }
            Ok(target)
        })
        .map_err(|error| format!("{error:#}"));
    rows.push(Row { line, target });
}

fn saved(catalog: &Profiles, name: &str) -> Result<Target> {
    let ssh = catalog
        .ssh_host_group_member(name)
        .with_context(|| format!("Profile {name} is unavailable for SSH groups"))?;
    Ok(Target {
        profile: Some(name.into()),
        ssh: ssh.clone(),
    })
}

fn resolve(
    input: &str,
    default_user: &str,
    catalog: &Profiles,
    catalog_path: &Path,
    profiles: &BTreeMap<(String, u16), Vec<&str>>,
    authentication: &Result<Authentication, String>,
) -> Result<Target> {
    let has_user = input.contains('@');
    let mut ssh = if has_user {
        SshOptions::from_destination(input)?
    } else {
        let address = if let Some((scheme, host)) = input.split_once("://") {
            format!("{scheme}://user@{host}")
        } else {
            format!("user@{input}")
        };
        let mut ssh = SshOptions::from_destination(&address)?;
        let user = default_user.trim();
        if !user.is_empty() {
            ssh.user = SshOptions::from_destination(&format!("{user}@localhost"))
                .context("Enter a valid default SSH user")?
                .user;
        } else {
            ssh.user.clear();
        }
        ssh
    };
    ensure!(
        !ssh.host.contains(['<', '>']),
        "Use a numeric host range such as web<01-03>"
    );
    let endpoint = Endpoint::from(&ssh);
    let names = profiles
        .get(&(endpoint.host, endpoint.port))
        .into_iter()
        .flatten()
        .copied()
        .filter(|name| ssh.user.is_empty() || catalog.profiles[*name].user == ssh.user)
        .collect::<Vec<_>>();
    if !names.is_empty() {
        let matches = names
            .iter()
            .copied()
            .filter(|name| catalog.ssh_host_group_member(name).is_some())
            .collect::<Vec<_>>();
        return match matches.as_slice() {
            [name] => saved(catalog, name),
            [] => anyhow::bail!("This address matches a profile unavailable for SSH groups"),
            _ => anyhow::bail!(
                "Multiple SSH profiles match · use an exact profile name ({})",
                matches.join(", ")
            ),
        };
    }
    ensure!(
        !ssh.user.is_empty(),
        "Enter user@host or supply a default SSH user for this host"
    );
    ssh.authentication = authentication
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))?
        .clone();
    ssh.prepare_profile_connection(catalog_path)?;
    Ok(Target { profile: None, ssh })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(input: &str, default_user: &str, catalog: &Profiles) -> Result<Vec<Row>> {
        build(
            input,
            default_user,
            catalog,
            Path::new("profiles.json"),
            Ok(Authentication::None),
        )
    }

    #[test]
    fn host_list_expands_padded_ranges_in_order_and_reuses_only_unambiguous_profiles() {
        let mut catalog = Profiles::default();
        let mut first = SshOptions::from_destination("ops@web01.example:2222").unwrap();
        first.authentication = Authentication::Password;
        first.host_key_alias = Some("pinned-web".into());
        catalog
            .profiles
            .insert("West Primary".into(), first.clone());
        catalog.profiles.insert(
            "West Alternate".into(),
            SshOptions::from_destination("root@web01.example:2222").unwrap(),
        );
        let rows = preview(
            "# web fleet\r\nweb01.example:2222\rWest Primary\nOPS@web<01-03>.example:2222",
            "",
            &catalog,
        )
        .unwrap();
        assert!(
            rows[0]
                .target
                .as_ref()
                .unwrap_err()
                .contains("Multiple SSH profiles")
        );
        assert_eq!(rows[1].line, 3);
        assert_eq!(rows[1].target.as_ref().unwrap().ssh, first);
        for (row, host) in rows[2..]
            .iter()
            .zip(["web01.example", "web02.example", "web03.example"])
        {
            let target = row.target.as_ref().unwrap();
            assert_eq!(row.line, 4);
            assert_eq!(target.profile, None);
            assert_eq!(target.ssh.host, host);
            assert_eq!(target.ssh.user, "OPS");
            assert_eq!(target.ssh.authentication, Authentication::None);
        }
        let explicit = preview("WEB01.EXAMPLE:2222", "ops", &catalog).unwrap();
        assert_eq!(
            explicit[0].target.as_ref().unwrap().profile.as_deref(),
            Some("West Primary")
        );
    }

    #[test]
    fn equivalent_ipv6_and_default_port_addresses_are_duplicates_after_profile_resolution() {
        let mut catalog = Profiles::default();
        catalog.profiles.insert(
            "Lab IPv6".into(),
            SshOptions::from_destination("u@[::1]").unwrap(),
        );
        let rows = preview(
            "ssh://u@[0:0:0:0:0:0:0:1]:22/\nLab IPv6\nserver<8-10>\nserver9:22",
            "u",
            &catalog,
        )
        .unwrap();
        assert_eq!(
            rows[0].target.as_ref().unwrap().profile.as_deref(),
            Some("Lab IPv6")
        );
        assert!(
            rows[1]
                .target
                .as_ref()
                .unwrap_err()
                .ends_with("first listed on line 1")
        );
        assert_eq!(rows[2].target.as_ref().unwrap().ssh.host, "server8");
        assert_eq!(rows[4].target.as_ref().unwrap().ssh.host, "server10");
        assert!(
            rows[5]
                .target
                .as_ref()
                .unwrap_err()
                .ends_with("first listed on line 3")
        );
    }

    #[test]
    fn host_list_reports_unusable_rows_and_rejects_excessive_expansion() {
        let mut catalog = Profiles::default();
        let mut saved = SshOptions::from_destination("test@inactive").unwrap();
        saved.durable = true;
        catalog.profiles.insert("Durable session".into(), saved);
        let rows = preview(
            "test@inactive\nDurable session\nunknown\nu@host<3--1>\nu@host:<1-2>\nu:secret@host",
            "",
            &catalog,
        )
        .unwrap();
        assert_eq!(rows.len(), 6);
        for row in &rows[..2] {
            assert!(
                row.target
                    .as_ref()
                    .unwrap_err()
                    .contains("unavailable for SSH groups")
            );
        }
        assert!(
            rows[2]
                .target
                .as_ref()
                .unwrap_err()
                .contains("default SSH user")
        );
        for row in &rows[3..5] {
            assert!(
                row.target
                    .as_ref()
                    .unwrap_err()
                    .contains("numeric ranges in the hostname")
            );
        }
        assert!(!rows[5].target.as_ref().unwrap_err().contains("secret"));
        assert!(preview("u@host<0-4294967295>", "", &catalog).is_err());
        assert!(preview("u@host<1-65>", "", &catalog).is_err());
        assert!(preview("u@host<1-64>\nu@other", "", &catalog).is_err());
        assert_eq!(preview("u@host<1-64>", "", &catalog).unwrap().len(), 64);
        assert_eq!(
            preview("u@rack<1-8>-web<8-1> # only hosts\n# comment", "", &catalog)
                .unwrap()
                .len(),
            64
        );
        assert!(preview("u@first\nu@rack<1-8>-web<8-1>", "", &catalog).is_err());
        assert!(preview(&"x".repeat(MAX_BYTES + 1), "", &catalog).is_err());
    }

    #[test]
    fn host_list_keeps_profile_names_and_line_numbers_with_inline_comments() {
        let mut catalog = Profiles::default();
        catalog.profiles.insert(
            "Build #1".into(),
            SshOptions::from_destination("ops@build").unwrap(),
        );
        let rows = preview(
            "# header\r\nBuild #1\r\nops@web<03-02,3,03>:2222#canaries\rssh://ops@other # comment\nssh://ops@host#fragment",
            "",
            &catalog,
        )
        .unwrap();
        assert_eq!(rows.len(), 7);
        assert_eq!(
            rows[0].target.as_ref().unwrap().profile.as_deref(),
            Some("Build #1")
        );
        let hosts: Vec<_> = rows[1..4]
            .iter()
            .map(|row| (row.line, row.target.as_ref().unwrap().ssh.host.as_str()))
            .collect();
        assert_eq!(hosts, [(3, "web03"), (3, "web02"), (3, "web3")]);
        assert!(
            rows[4]
                .target
                .as_ref()
                .unwrap_err()
                .contains("first listed on line 3")
        );
        assert_eq!(rows[5].target.as_ref().unwrap().ssh.host, "other");
        assert!(rows[6].target.as_ref().unwrap_err().contains("fragment"));
    }
}
