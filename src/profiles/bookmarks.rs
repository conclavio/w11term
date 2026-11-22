use super::ProfileStore;
use crate::{options::SshOptions, ssh::ConnectionShare};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub(super) const FIELD: &str = "sftp_bookmarks";

/// Identity of the destination that authenticated this SSH connection.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BookmarkPeer {
    host: String,
    user: String,
    port: u16,
    fingerprint: String,
}

impl BookmarkPeer {
    pub fn from_connection(link: &ConnectionShare) -> Self {
        let (host, user, port, fingerprint) = link.verified_endpoint();
        Self {
            host: host.to_ascii_lowercase(),
            user: user.into(),
            port,
            fingerprint: fingerprint.into(),
        }
    }

    fn check_profile(&self, options: &SshOptions) -> Result<()> {
        ensure!(
            self.host.eq_ignore_ascii_case(&options.host)
                && self.user == options.user
                && self.port == options.port
                && options
                    .host_key
                    .as_ref()
                    .is_none_or(|pin| pin == &self.fingerprint),
            "The SSH profile no longer matches this connection's host, user, port or host key"
        );
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bookmarks {
    peer: BookmarkPeer,
    directories: BTreeSet<String>,
}

impl Bookmarks {
    fn read(profile: &Value) -> Result<Vec<Self>> {
        let Some(value) = profile.get(FIELD) else {
            return Ok(Vec::new());
        };
        let groups = Vec::<Self>::deserialize(value).context("Reading SFTP bookmarks")?;
        let mut peers = BTreeSet::new();
        for group in &groups {
            ensure!(
                peers.insert(&group.peer),
                "Duplicate SSH endpoint in the saved SFTP bookmarks"
            );
            for directory in &group.directories {
                ensure!(
                    bookmark_directory(directory) == Some(directory.as_str()),
                    "Invalid directory in the saved SFTP bookmarks"
                );
            }
        }
        Ok(groups)
    }
}

/// Validate a remote absolute directory without interpreting Windows paths.
/// Accept a trailing slash from REALPATH; preserve the spelling of each component.
pub fn bookmark_directory(path: &str) -> Option<&str> {
    if path == "/" {
        return Some(path);
    }
    let path = path.strip_suffix('/').unwrap_or(path);
    let relative = path.strip_prefix('/')?;
    (!path.contains('\\')
        && !path.chars().any(char::is_control)
        && relative
            .split('/')
            .all(|part| !matches!(part, "" | "." | "..")))
    .then_some(path)
}

impl ProfileStore {
    pub fn list_bookmarks(&self, profile: &str, peer: &BookmarkPeer) -> Result<Vec<String>> {
        let (_, snapshot) = self.inspect(profile)?;
        peer.check_profile(&snapshot.options)?;
        Ok(Bookmarks::read(&snapshot.raw)?
            .into_iter()
            .find(|g| &g.peer == peer)
            .map(|g| g.directories.into_iter().collect())
            .unwrap_or_default())
    }

    pub fn add_bookmark(
        &self,
        profile: &str,
        peer: &BookmarkPeer,
        directory: &str,
    ) -> Result<Vec<String>> {
        self.change_bookmark(profile, peer, directory, true)
    }

    pub fn remove_bookmark(
        &self,
        profile: &str,
        peer: &BookmarkPeer,
        directory: &str,
    ) -> Result<Vec<String>> {
        self.change_bookmark(profile, peer, directory, false)
    }

    fn change_bookmark(
        &self,
        profile: &str,
        peer: &BookmarkPeer,
        directory: &str,
        add: bool,
    ) -> Result<Vec<String>> {
        let directory = bookmark_directory(directory)
            .context("Choose an absolute, unambiguous SFTP directory")?;
        let mut current = Vec::new();
        self.modify(|profiles, catalog| {
            let options = catalog
                .profiles
                .get(profile)
                .context("The saved SSH profile is no longer available")?;
            peer.check_profile(options)?;
            let entry = profiles
                .get_mut(profile)
                .context("The SSH profile has no saved entry")?;
            let mut groups = Bookmarks::read(entry)?;
            let index = groups.iter().position(|g| &g.peer == peer);
            if let Some(index) = index {
                let directories = &mut groups[index].directories;
                if add {
                    directories.insert(directory.to_owned());
                } else {
                    directories.remove(directory);
                }
                current.extend(directories.iter().cloned());
                if directories.is_empty() {
                    groups.remove(index);
                }
            } else if add {
                current.push(directory.to_owned());
                groups.push(Bookmarks {
                    peer: peer.clone(),
                    directories: BTreeSet::from([directory.to_owned()]),
                });
            }
            let fields = entry
                .as_object_mut()
                .context("The SSH profile must be an object")?;
            if groups.is_empty() {
                fields.remove(FIELD);
            } else {
                fields.insert(FIELD.into(), serde_json::to_value(groups)?);
            }
            Ok(None)
        })?;
        Ok(current)
    }
}

#[cfg(test)]
mod tests;
