use super::super::address::{Endpoint, display};
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeMap, path::Path};
use w11term::options::{Authentication, Profiles, SshOptions};

const MAX_BYTES: usize = 32 * 1024;
const MAX_TARGETS: usize = 64;

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
    ensure!(
        input.len() <= MAX_BYTES,
        "The host list is limited to 32 KiB."
    );
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
    for (index, input) in input.replace("\r\n", "\n").split(['\n', '\r']).enumerate() {
        let input = input.trim();
        if input.is_empty() || input.starts_with('#') {
            continue;
        }
        let line = index + 1;
        let remaining = MAX_TARGETS - rows.len();
        ensure!(
            remaining > 0,
            "A host list can expand to at most {MAX_TARGETS} entries."
        );
        if catalog.profiles.contains_key(input) || catalog.local.contains_key(input) {
            push(&mut rows, &mut seen, line, saved(catalog, input));
            continue;
        }
        let expansion = match expansion(input) {
            Ok(expansion) => expansion,
            Err(error) => {
                push(&mut rows, &mut seen, line, Err(error));
                continue;
            }
        };
        if let Some(range) = expansion {
            ensure!(
                range.last - range.first < remaining as u32,
                "A host list can expand to at most {MAX_TARGETS} entries."
            );
            for number in range.first..=range.last {
                let expanded = format!(
                    "{}{number:0width$}{}",
                    range.prefix,
                    range.suffix,
                    width = range.width
                );
                let target = resolve(
                    &expanded,
                    default_user,
                    catalog,
                    catalog_path,
                    &profiles,
                    &authentication,
                );
                push(&mut rows, &mut seen, line, target);
            }
        } else {
            let target = resolve(
                input,
                default_user,
                catalog,
                catalog_path,
                &profiles,
                &authentication,
            );
            push(&mut rows, &mut seen, line, target);
        }
    }
    ensure!(
        !rows.is_empty(),
        "Enter at least one SSH profile or address."
    );
    Ok(rows)
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

struct Range<'a> {
    prefix: &'a str,
    suffix: &'a str,
    first: u32,
    last: u32,
    width: usize,
}

fn expansion(input: &str) -> Result<Option<Range<'_>>> {
    let Some((prefix, rest)) = input.split_once('<') else {
        ensure!(
            !input.contains('>'),
            "Use a numeric host range such as web<01-03>"
        );
        return Ok(None);
    };
    let invalid = "Use one ascending numeric range in the hostname, such as ops@web<01-03>";
    let (bounds, suffix) = rest.split_once('>').context(invalid)?;
    let (first, last) = bounds.split_once('-').context(invalid)?;
    let host_prefix = prefix.rsplit_once('@').map_or_else(
        || prefix.split_once("://").map_or(prefix, |(_, host)| host),
        |(_, host)| host,
    );
    ensure!(
        !prefix.contains('>')
            && !suffix.contains(['<', '>', '@'])
            && !host_prefix.contains([':', '[', ']', '/'])
            && first.len() <= 10
            && last.len() <= 10
            && first.bytes().all(|digit| digit.is_ascii_digit())
            && last.bytes().all(|digit| digit.is_ascii_digit()),
        "{invalid}"
    );
    let padded = |number: &str| number.len() > 1 && number.starts_with('0');
    let width = if padded(first) || padded(last) {
        first.len().max(last.len())
    } else {
        0
    };
    let first = first.parse().context(invalid)?;
    let last = last.parse().context(invalid)?;
    ensure!(first <= last, "{invalid}");
    Ok(Some(Range {
        prefix,
        suffix,
        first,
        last,
        width,
    }))
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
            "test@inactive\nDurable session\nunknown\nu@host<3-1>\nu@host:<1-2>\nu:secret@host",
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
                    .contains("ascending numeric range")
            );
        }
        assert!(!rows[5].target.as_ref().unwrap_err().contains("secret"));
        assert!(preview("u@host<0-4294967295>", "", &catalog).is_err());
        assert!(preview("u@host<1-65>", "", &catalog).is_err());
        assert!(preview("u@host<1-64>\nu@other", "", &catalog).is_err());
        assert_eq!(preview("u@host<1-64>", "", &catalog).unwrap().len(), 64);
        assert!(preview(&"x".repeat(MAX_BYTES + 1), "", &catalog).is_err());
    }
}
