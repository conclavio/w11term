use super::*;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{ErrorKind, Read, Write};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Grant {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub identity: String,
}

#[derive(Clone)]
pub(super) struct Store(PathBuf);

impl Store {
    pub fn for_profile(scope: &FolderScope, auth: &Authority, issue: bool) -> Result<Option<Self>> {
        ensure!(
            scope.profile == auth.profile,
            "The profile for this permission changed"
        );
        let catalog = crate::profiles::ProfileStore::new(scope.catalog.clone())?;
        let Some(token) = catalog.folder_token(auth, issue)? else {
            return Ok(None);
        };
        let catalog_path =
            std::fs::canonicalize(catalog.path()).context("Locating the saved profile catalog")?;
        let binding = serde_json::to_vec(&(
            catalog_path,
            &auth.profile,
            &auth.host,
            &auth.user,
            auth.port,
            &auth.fingerprint,
            token,
        ))?;
        let hash = bytes::Bytes::copy_from_slice(&Sha256::digest(binding));
        Ok(Some(Self(
            scope
                .store_dir
                .join("folder-shares")
                .join(format!("{hash:x}.json")),
        )))
    }

    pub fn read(&self) -> Result<Vec<Grant>> {
        let file = match std::fs::File::open(&self.0) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).context("Opening saved folder permissions"),
        };
        let mut buf = Vec::new();
        file.take(256 * 1024 + 1).read_to_end(&mut buf)?;
        ensure!(
            buf.len() <= 256 * 1024,
            "Folder permission file is too large"
        );
        let grants: Vec<Grant> =
            serde_json::from_slice(&buf).context("Reading saved folder permissions")?;
        let mut names = std::collections::BTreeSet::new();
        ensure!(
            grants.len() <= MAX_FOLDERS
                && grants.iter().all(|g| valid_mount_name(&g.name)
                    && names.insert(&g.name)
                    && g.id.len() == 32
                    && g.id.bytes().all(|b| b.is_ascii_hexdigit())
                    && !g.identity.is_empty()
                    && g.path.is_absolute()),
            "Invalid saved folder permissions"
        );
        Ok(grants)
    }

    fn edit(&self, change: impl FnOnce(&mut Vec<Grant>) -> Result<()>) -> Result<()> {
        let parent = self.0.parent().context("Missing permission directory")?;
        std::fs::create_dir_all(parent)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.0.with_extension("lock"))?;
        lock.try_lock()
            .context("Another window is changing shared folders. Try again")?;
        let mut grants = self.read()?;
        change(&mut grants)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut file, &grants)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(&self.0).context("Saving folder permission")?;
        Ok(())
    }

    pub fn add(&self, grant: Grant) -> Result<()> {
        self.edit(|grants| {
            ensure!(
                grants.len() < MAX_FOLDERS,
                "A profile can share up to {MAX_FOLDERS} folders"
            );
            ensure!(
                !grants.iter().any(|g| g.name == grant.name),
                "This Linux mount name is already saved"
            );
            grants.push(grant);
            Ok(())
        })
    }

    pub fn remove(&self, grant: &Grant) -> Result<()> {
        self.edit(|grants| {
            ensure!(
                !grants
                    .iter()
                    .any(|g| g.name == grant.name && g.id != grant.id),
                "This shared folder has changed. Reopen Shared folders"
            );
            grants.retain(|g| g.id != grant.id);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::ProfileStore;

    fn fixture(dir: &std::path::Path) -> (FolderScope, Authority) {
        let catalog = dir.join("profiles.json");
        std::fs::write(&catalog, br#"{"profiles":{"test":{"host":"example.test","user":"pilot","auth":"none","custom":3}}}"#).unwrap();
        (
            FolderScope::new(catalog, "test".into(), dir.join("data")),
            Authority {
                profile: "test".into(),
                host: "example.test".into(),
                user: "pilot".into(),
                port: 22,
                fingerprint: "SHA256:test".into(),
                pane: 5,
                generation: 3,
            },
        )
    }
    fn grant(path: PathBuf, id: u128) -> Grant {
        Grant {
            name: "sprites".into(),
            id: format!("{id:032x}"),
            identity: "ntfs-v1:volume:directory".into(),
            path,
        }
    }

    #[test]
    fn consent_is_bound_to_the_catalog_profile_account_and_verified_key() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (scope, auth) = fixture(dir.path());
        assert!(Store::for_profile(&scope, &auth, false)?.is_none());
        assert!(!scope.store_dir.exists());
        let original = Store::for_profile(&scope, &auth, true)?.unwrap();
        let allowed = grant(dir.path().join("sprites"), 1);
        original.add(allowed.clone())?;
        let restored = Authority {
            pane: 8,
            generation: 1,
            ..auth.clone()
        };
        assert_eq!(
            Store::for_profile(&scope, &restored, false)?
                .unwrap()
                .read()?,
            vec![allowed]
        );
        let changed_key = Authority {
            fingerprint: "SHA256:different".into(),
            ..auth.clone()
        };
        assert!(
            Store::for_profile(&scope, &changed_key, false)?
                .unwrap()
                .read()?
                .is_empty()
        );
        for renamed in [
            Authority {
                user: "other".into(),
                ..auth.clone()
            },
            Authority {
                port: 2222,
                ..auth.clone()
            },
            Authority {
                host: "other.test".into(),
                ..auth.clone()
            },
        ] {
            assert!(Store::for_profile(&scope, &renamed, true).is_err());
        }
        let portable = std::fs::read_to_string(&scope.catalog)?;
        assert!(!portable.contains("sprites"));
        assert!(portable.contains("folder_share_id"));
        let other = FolderScope {
            catalog: dir.path().join("copy.json"),
            ..scope.clone()
        };
        std::fs::write(&other.catalog, portable)?;
        assert!(
            Store::for_profile(&other, &auth, false)?
                .unwrap()
                .read()?
                .is_empty()
        );
        let profiles = ProfileStore::new(scope.catalog.clone())?;
        let (_, before) = profiles.inspect_catalog("test")?;
        profiles.duplicate(&before, "copy", &before.options)?;
        let copied = Authority {
            profile: "copy".into(),
            ..auth.clone()
        };
        assert!(
            Store::for_profile(
                &FolderScope {
                    profile: "copy".into(),
                    ..scope.clone()
                },
                &copied,
                false
            )?
            .is_none()
        );
        profiles.edit("test", "renamed", &before.options, &before.options)?;
        let renamed = Authority {
            profile: "renamed".into(),
            ..auth
        };
        assert!(
            Store::for_profile(
                &FolderScope {
                    profile: "renamed".into(),
                    ..scope
                },
                &renamed,
                false
            )?
            .is_none()
        );
        Ok(())
    }

    #[test]
    fn stale_forget_cannot_remove_a_replacement_and_malformed_permissions_fail_closed() -> Result<()>
    {
        let dir = tempfile::tempdir()?;
        let (scope, auth) = fixture(dir.path());
        let store = Store::for_profile(&scope, &auth, true)?.unwrap();
        let old = grant(dir.path().join("first"), 1);
        let new = grant(dir.path().join("second"), 2);
        store.add(old.clone())?;
        assert!(store.add(new.clone()).is_err());
        store.remove(&old)?;
        store.add(new.clone())?;
        assert!(store.remove(&old).is_err());
        assert_eq!(store.read()?, vec![new]);
        std::fs::write(&store.0, b"{")?;
        assert!(store.read().is_err());
        assert!(store.add(old).is_err());
        assert_eq!(std::fs::read(&store.0)?, b"{");
        Ok(())
    }
}
