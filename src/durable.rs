//! A durable reference identifies one generation of a PTY on an authenticated host.
use crate::options::SshOptions;
use anyhow::{Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableReference {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub host_key: String,
    pub name: String,
    pub generation: String,
}

impl DurableReference {
    pub fn new(options: &SshOptions, host_key: &str) -> Self {
        Self {
            host: options.host.clone(),
            port: options.port,
            user: options.user.clone(),
            host_key: host_key.to_owned(),
            name: format!("w11term-{:032x}", rand::random::<u128>()),
            generation: format!("{:032x}", rand::random::<u128>()),
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            valid_identity(&self.name, &self.generation),
            "Invalid durable session name or generation"
        );
        ensure!(
            !self.user.is_empty() && !self.host.is_empty() && self.port != 0,
            "Incomplete durable SSH endpoint"
        );
        ensure!(
            self.host_key
                .strip_prefix("SHA256:")
                .and_then(|encoded| STANDARD_NO_PAD.decode(encoded).ok())
                .is_some_and(|bytes| bytes.len() == 32),
            "Invalid durable host-key fingerprint"
        );
        Ok(())
    }

    pub fn matches_endpoint(&self, options: &SshOptions) -> bool {
        self.host.eq_ignore_ascii_case(&options.host)
            && self.port == options.port
            && self.user == options.user
            && options
                .host_key
                .as_ref()
                .is_none_or(|pin| pin == &self.host_key)
    }
}

fn valid_identity(name: &str, generation: &str) -> bool {
    fn token(value: &str) -> bool {
        value.len() == 32
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
    name.strip_prefix("w11term-").is_some_and(token) && token(generation)
}

#[derive(Clone, Debug)]
pub struct DiscoveredSession {
    pub reference: DurableReference,
    pub clients: u32,
    pub directory: String,
    pub command: String,
}

/// zmx 1.x emits tab-separated key=value fields. Free-form display fields
/// never become SSH commands. The attach path independently checks the label.
pub(crate) fn parse_listing(
    output: &[u8],
    options: &SshOptions,
    peer: &str,
) -> Result<Vec<DiscoveredSession>> {
    use std::collections::BTreeMap;
    let mut entries = BTreeMap::new();
    for row in output.split(|b| *b == b'\n') {
        let row = String::from_utf8_lossy(row);
        let row = row.trim_start_matches(' ');
        let fields = row.strip_prefix("→ ").unwrap_or(row);
        let mut fields = fields.split('\t');
        let Some(name) = fields.next().and_then(|value| value.strip_prefix("name=")) else {
            continue;
        };
        let mut metadata = BTreeMap::new();
        let mut duplicate = false;
        for (key, value) in fields.filter_map(|field| field.split_once('=')) {
            if matches!(
                key,
                "name" | "w11term_generation" | "clients" | "cwd" | "cmd" | "err" | "ended"
            ) && (key == "name" || metadata.insert(key, value).is_some())
            {
                duplicate = true;
                break;
            }
        }
        let Some(&generation) = metadata.get("w11term_generation") else {
            continue;
        };
        if duplicate
            || metadata.contains_key("err")
            || metadata.contains_key("ended")
            || !valid_identity(name, generation)
        {
            continue;
        }
        let Some(clients) = metadata.get("clients").and_then(|v| v.parse::<u32>().ok()) else {
            continue;
        };
        let display = |key: &str| {
            metadata
                .get(key)
                .copied()
                .unwrap_or("")
                .chars()
                .filter(|c| {
                    !c.is_control()
                        && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                })
                .take(200)
                .collect()
        };
        let entry = DiscoveredSession {
            reference: DurableReference {
                host: options.host.clone(),
                port: options.port,
                user: options.user.clone(),
                host_key: peer.into(),
                name: name.into(),
                generation: generation.into(),
            },
            clients,
            directory: display("cwd"),
            command: display("cmd"),
        };
        ensure!(
            entries.insert(name.to_owned(), entry).is_none(),
            "Ambiguous zmx listing: duplicate session name"
        );
    }
    Ok(entries.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_filters_unmanaged_dead_malformed_and_ambiguous_sessions() {
        let options: SshOptions =
            serde_json::from_str(r#"{"host":"example","user":"dev","auth":"none"}"#).unwrap();
        let key = format!("SHA256:{}", STANDARD_NO_PAD.encode([9; 32]));
        let reference = DurableReference::new(&options, &key);
        let name = &reference.name;
        let generation = &reference.generation;
        let line = format!(
            "name={name}\tpid=7\tclients=3\tcwd=/src\tcmd=vim/file=$HOME\tw11term_generation={generation}"
        );
        let listed = format!(
            "name=unmanaged\tclients=0\n{line}\tended=100\n{line}\terr=Busy\n{line}\tw11term_generation={generation}\n{}\n{}\n→ {line}\n",
            line.replace(name, "../bad"),
            line.replace(generation, "$bad")
        );
        let entries = parse_listing(listed.as_bytes(), &options, &key).unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.reference, reference);
        assert_eq!(
            (entry.clients, &*entry.directory, &*entry.command),
            (3, "/src", "vim/file=$HOME")
        );
        let ambiguous = format!("{line}\n{}\n", line.replace(generation, &"a".repeat(32)));
        assert!(parse_listing(ambiguous.as_bytes(), &options, &key).is_err());
    }

    #[test]
    fn reference_is_unique_and_bound_to_the_pinned_account() {
        let mut options: SshOptions =
            serde_json::from_str(r#"{"host":"host.example","user":"pilot","auth":"none"}"#)
                .unwrap();
        let pin = format!("SHA256:{}", STANDARD_NO_PAD.encode([3u8; 32]));
        let first = DurableReference::new(&options, &pin);
        assert!(first.validate().is_ok() && first.matches_endpoint(&options));
        assert_ne!(first, DurableReference::new(&options, &pin));
        options.user = "other".into();
        assert!(!first.matches_endpoint(&options));
        options.user = first.user.clone();
        options.host_key = Some(format!("SHA256:{}", STANDARD_NO_PAD.encode([4u8; 32])));
        assert!(!first.matches_endpoint(&options));
        let mut invalid = first;
        invalid.name.push('/');
        assert!(invalid.validate().is_err());
    }
}
