use super::*;
use crate::profiles::{FolderEpoch, FolderGrantGate, ProfileStore};
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
    #[serde(default = "mount_by_default", skip_serializing_if = "is_enabled")]
    pub auto_mount: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub writable: bool,
}

fn mount_by_default() -> bool {
    true
}
fn is_enabled(value: &bool) -> bool {
    *value
}

impl Grant {
    /// A mount-policy change preserves the original consent and live mount.
    pub fn same_selection(&self, other: &Self) -> bool {
        self.id == other.id
            && self.name == other.name
            && self.path == other.path
            && self.identity == other.identity
            && self.writable == other.writable
    }
}

pub(super) struct Store {
    path: PathBuf,
    gate: FolderGrantGate,
    scope: FolderScope,
    auth: Authority,
    epoch: FolderEpoch,
    token: String,
}

impl Store {
    fn catalog(scope: &FolderScope, auth: &Authority) -> Result<ProfileStore> {
        ensure!(
            scope.profile == auth.profile,
            "The profile for this permission changed"
        );
        ProfileStore::new(scope.catalog.clone())
    }
    pub fn inspect(scope: &FolderScope, auth: &Authority) -> Result<(FolderEpoch, Option<Self>)> {
        let catalog = Self::catalog(scope, auth)?;
        let (epoch, token) = catalog.folder_binding(auth, &scope.store_dir)?;
        let store = token
            .map(|token| Self::bound(scope, auth, &epoch, token))
            .transpose()?;
        Ok((epoch, store))
    }
    pub fn issue(scope: &FolderScope, auth: &Authority, expected: &FolderEpoch) -> Result<Self> {
        let catalog = Self::catalog(scope, auth)?;
        let token = catalog.issue_folder_epoch(auth, &scope.store_dir, expected)?;
        Self::bound(scope, auth, expected, token)
    }
    fn bound(
        scope: &FolderScope,
        auth: &Authority,
        epoch: &FolderEpoch,
        token: String,
    ) -> Result<Self> {
        let binding = serde_json::to_vec(&(
            epoch.catalog(),
            &auth.profile,
            &auth.host,
            &auth.user,
            auth.port,
            &auth.fingerprint,
            &token,
        ))?;
        let hash = bytes::Bytes::copy_from_slice(&Sha256::digest(binding));
        Ok(Self {
            path: scope
                .store_dir
                .join("folder-shares")
                .join(format!("{hash:x}.json")),
            gate: epoch.gate(&scope.store_dir, &token)?,
            scope: scope.clone(),
            auth: auth.clone(),
            epoch: epoch.clone(),
            token,
        })
    }

    pub fn revocation_gate(&self, grant: &Grant) -> Result<FolderGrantGate> {
        // The filename already binds the catalog, profile, SSH endpoint, host
        // key and token. The random selection ID distinguishes a replacement
        // even if it has the same local path and Linux mount name.
        ensure!(
            grant.id.len() == 32 && grant.id.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid saved folder identifier"
        );
        let marker = self.path.with_extension(format!("{}.forgotten", grant.id));
        Ok(self.gate.for_selection(marker))
    }

    #[cfg(test)]
    fn for_profile(scope: &FolderScope, auth: &Authority, issue: bool) -> Result<Option<Self>> {
        let (epoch, store) = Self::inspect(scope, auth)?;
        if issue && store.is_none() {
            return Self::issue(scope, auth, &epoch).map(Some);
        }
        Ok(store)
    }

    pub fn read(&self) -> Result<Vec<Grant>> {
        if self.gate.revoked()? {
            return Ok(Vec::new());
        }
        let file = match std::fs::File::open(&self.path) {
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
        let mut ids = std::collections::BTreeSet::new();
        ensure!(
            grants.len() <= MAX_FOLDERS
                && grants.iter().all(|g| valid_mount_name(&g.name)
                    && names.insert(&g.name)
                    && ids.insert(g.id.to_ascii_lowercase())
                    && !g.identity.is_empty()
                    && g.path.is_absolute()),
            "Invalid saved folder permissions"
        );
        // A backup of the grant list cannot reactivate a forgotten selection.
        let mut active = Vec::new();
        for grant in grants {
            if !self.revocation_gate(&grant)?.selection_retired()? {
                active.push(grant);
            }
        }
        Ok(active)
    }

    fn edit(&self, change: impl FnOnce(&mut Vec<Grant>) -> Result<()>) -> Result<()> {
        // Serialize grant writes with offline revocation and token issuance.
        // Retain the existing peer-specific lock for older profile writers.
        let catalog = Self::catalog(&self.scope, &self.auth)?;
        let _catalog_lock = catalog.folder_lock()?;
        let (epoch, token) = catalog.folder_binding(&self.auth, &self.scope.store_dir)?;
        ensure!(
            epoch == self.epoch && token.as_ref() == Some(&self.token),
            "Folder permissions changed; reopen Shared folders"
        );
        let parent = self.path.parent().context("Missing permission directory")?;
        std::fs::create_dir_all(parent)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.path.with_extension("lock"))?;
        lock.try_lock()
            .context("Another window is changing shared folders. Try again")?;
        let mut grants = self.read()?;
        change(&mut grants)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut file, &grants)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(&self.path)
            .context("Saving folder permission")?;
        Ok(())
    }

    pub fn add(&self, grant: Grant) -> Result<()> {
        self.edit(|grants| {
            ensure!(
                grants.len() < MAX_FOLDERS,
                "A profile can share up to {MAX_FOLDERS} folders"
            );
            ensure!(
                !grants
                    .iter()
                    .any(|g| g.name == grant.name || g.id.eq_ignore_ascii_case(&grant.id)),
                "This Linux mount name or folder permission is already saved"
            );
            ensure!(
                !self.revocation_gate(&grant)?.selection_retired()?,
                "This folder permission was forgotten"
            );
            grants.push(grant);
            Ok(())
        })
    }

    pub fn remove(&self, grant: &Grant) -> Result<()> {
        self.edit(|grants| {
            ensure!(
                !grants.iter().any(|g| (g.name == grant.name
                    || g.id.eq_ignore_ascii_case(&grant.id))
                    && !g.same_selection(grant)),
                "This shared folder has changed. Reopen Shared folders"
            );
            self.revocation_gate(grant)?.forget_selection()?;
            grants.retain(|g| !g.same_selection(grant));
            Ok(())
        })
    }

    pub fn set_auto_mount(&self, grant: &Grant, enabled: bool) -> Result<()> {
        self.edit(|grants| {
            let saved = grants
                .iter_mut()
                .find(|saved| saved.same_selection(grant))
                .context("This shared folder has changed. Reopen Shared folders")?;
            saved.auto_mount = enabled;
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
        fixture_binding(catalog, dir.join("data"))
    }

    fn fixture_binding(catalog: PathBuf, data: PathBuf) -> (FolderScope, Authority) {
        (
            FolderScope::new(catalog, "test".into(), data),
            Authority {
                profile: "test".into(),
                host: "example.test".into(),
                user: "pilot".into(),
                port: 22,
                fingerprint: "SHA256:test".into(),
                pane: 5,
                generation: 3,
                epoch: Default::default(),
            },
        )
    }
    fn grant(path: PathBuf, id: u128) -> Grant {
        Grant {
            name: "sprites".into(),
            id: format!("{id:032x}"),
            identity: "ntfs-v1:volume:directory".into(),
            auto_mount: true,
            writable: false,
            path,
        }
    }

    #[test]
    fn forgetting_one_folder_drains_its_other_process_and_survives_backup_restore() -> Result<()> {
        use std::{
            fs,
            process::Command,
            thread,
            time::{Duration, Instant},
        };
        const CHILD: &str = "W11TERM_TEST_FORGET_ACTIVE_FOLDER";
        const TIMEOUT: Duration = Duration::from_secs(15);
        fn wait_for(mut ready: impl FnMut() -> Result<bool>) -> Result<()> {
            let deadline = Instant::now() + TIMEOUT;
            while !ready()? {
                ensure!(
                    Instant::now() < deadline,
                    "Timed out waiting for the other folder permission process"
                );
                thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        }

        if let Some(dir) = std::env::var_os(CHILD) {
            let dir = PathBuf::from(dir);
            let (scope, auth) = fixture_binding(dir.join("profiles.json"), dir.join("data"));
            let store = Store::for_profile(&scope, &auth, false)?.unwrap();
            let grants = store.read()?;
            let target = store.revocation_gate(&grants[0])?;
            let sibling = store.revocation_gate(&grants[1])?;
            let admitted = target.lock_mutation()?;
            let sibling_admitted = sibling.lock_mutation()?;
            fs::write(dir.join("admitted"), [])?;
            wait_for(|| target.revoked())?;
            assert!(!sibling.revoked()?);
            fs::write(dir.join("saw-retirement"), [])?;
            wait_for(|| Ok(dir.join("release-target").try_exists()?))?;
            drop(admitted);
            wait_for(|| Ok(dir.join("forget-returned").try_exists()?))?;
            assert!(target.lock_mutation().is_err());
            drop(sibling_admitted);
            sibling.lock_mutation()?;
            return Ok(());
        }

        let temp = tempfile::tempdir()?;
        let dir = temp.path();
        let (scope, auth) = fixture(dir);
        let store = Store::for_profile(&scope, &auth, true)?.unwrap();
        let target = Grant {
            writable: true,
            ..grant(dir.join("one"), 1)
        };
        let sibling = Grant {
            name: "audio".into(),
            writable: true,
            ..grant(dir.join("two"), 2)
        };
        store.add(target.clone())?;
        store.add(sibling.clone())?;
        let backup = fs::read(&store.path)?;
        let test_name = std::thread::current()
            .name()
            .context("Test name required for subprocess")?
            .to_owned();
        let mut child = Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .env(CHILD, dir)
            .spawn()?;
        wait_for(|| Ok(dir.join("admitted").try_exists()?))?;
        let reopener = Store::for_profile(&scope, &auth, false)?.unwrap();
        let captured = target.clone();
        let (sent, done) = std::sync::mpsc::channel();
        let remover = thread::spawn(move || {
            let _ = sent.send(reopener.remove(&captured));
        });
        wait_for(|| Ok(dir.join("saw-retirement").try_exists()?))?;
        assert!(matches!(
            done.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        fs::write(dir.join("release-target"), [])?;
        done.recv_timeout(TIMEOUT)??;
        fs::write(dir.join("forget-returned"), [])?;
        remover.join().unwrap();
        assert!(child.wait()?.success());

        fs::write(&store.path, backup)?;
        assert_eq!(store.read()?.as_slice(), std::slice::from_ref(&sibling));
        assert!(store.add(target.clone()).is_err());
        let replacement = Grant {
            id: format!("{:032x}", 3),
            ..target.clone()
        };
        store.add(replacement.clone())?;
        assert!(store.remove(&target).is_err());
        assert_eq!(store.read()?, [sibling, replacement]);
        Ok(())
    }

    #[test]
    fn legacy_grants_mount_by_default_and_the_policy_survives_another_writer() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (scope, auth) = fixture(dir.path());
        let store = Store::for_profile(&scope, &auth, true)?.unwrap();
        let first = grant(dir.path().join("first"), 1);
        std::fs::create_dir_all(store.path.parent().unwrap())?;
        std::fs::write(
            &store.path,
            serde_json::to_vec(&[serde_json::json!({
                "id": first.id, "name": first.name, "path": first.path, "identity": first.identity
            })])?,
        )?;
        assert_eq!(store.read()?.as_slice(), std::slice::from_ref(&first));
        store.set_auto_mount(&first, false)?;
        let other_writer = Store::inspect(&scope, &auth)?.1.unwrap();
        let mut second = grant(dir.path().join("second"), 2);
        second.name = "music".into();
        second.writable = true;
        other_writer.add(second.clone())?;
        let grants = store.read()?;
        assert!(!grants[0].auto_mount && grants[0].same_selection(&first));
        assert_eq!(grants[1], second);
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&store.path)?)?;
        assert_eq!(json[0]["auto_mount"], false);
        assert!(json[0].get("writable").is_none());
        assert_eq!(json[1]["writable"], true);
        assert!(json[1].get("auto_mount").is_none());
        store.set_auto_mount(&grants[0], true)?;
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&store.path)?)?;
        assert!(json[0].get("auto_mount").is_none());
        assert_eq!(other_writer.read()?, [first, second.clone()]);
        // The captured selection still refers to the same consent after an
        // independent policy edit; Forget must still remove that selection.
        other_writer.remove(&grants[0])?;
        assert_eq!(store.read()?, [second]);
        Ok(())
    }

    #[test]
    fn stale_mount_policy_edits_cannot_change_a_replacement_or_retired_grant() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (scope, auth) = fixture(dir.path());
        let store = Store::for_profile(&scope, &auth, true)?.unwrap();
        let first = grant(dir.path().join("first"), 1);
        store.add(first.clone())?;
        for stale in [
            Grant {
                name: "different".into(),
                ..first.clone()
            },
            Grant {
                path: dir.path().join("different"),
                ..first.clone()
            },
            Grant {
                identity: "ntfs-v1:replacement".into(),
                ..first.clone()
            },
            Grant {
                writable: true,
                ..first.clone()
            },
        ] {
            assert!(store.set_auto_mount(&stale, false).is_err());
            assert!(store.remove(&stale).is_err());
        }
        store.remove(&first)?;
        let replacement = Grant {
            id: format!("{:032x}", 2),
            ..first.clone()
        };
        store.add(replacement.clone())?;
        assert!(store.set_auto_mount(&first, false).is_err());
        assert_eq!(store.read()?.as_slice(), std::slice::from_ref(&replacement));
        let catalog = ProfileStore::new(scope.catalog.clone())?;
        let (_, profile) = catalog.inspect_catalog(&scope.profile)?;
        catalog.revoke_folder_permissions(&profile, &scope.store_dir)?;
        assert!(store.set_auto_mount(&replacement, false).is_err());
        Ok(())
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
        std::fs::write(&store.path, b"{")?;
        assert!(store.read().is_err());
        assert!(store.add(old).is_err());
        assert_eq!(std::fs::read(&store.path)?, b"{");
        Ok(())
    }

    #[test]
    fn offline_revocation_invalidates_pickers_mounts_and_historical_catalogs() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (scope, auth) = fixture(dir.path());
        let profiles = ProfileStore::new(scope.catalog.clone())?;
        let pristine = std::fs::read(&scope.catalog)?;
        let (before_first, _) = Store::inspect(&scope, &auth)?;
        // A pre-feature config has an active random token but no consent epoch.
        let mut legacy: serde_json::Value = serde_json::from_slice(&pristine)?;
        legacy["profiles"]["test"]["folder_share_id"] = format!("{:032x}", 42).into();
        let backup_a = serde_json::to_vec(&legacy)?;
        std::fs::write(&scope.catalog, &backup_a)?;
        let (epoch_a, a) = Store::inspect(&scope, &auth)?;
        let a = a.unwrap();
        a.add(grant(dir.path().join("one"), 1))?;
        let (_, original) = profiles.inspect_catalog("test")?;
        let mut changed = original.options.clone();
        changed.user = "second-account".into();
        profiles.edit("test", "test", &changed, &original.options)?;
        let second = Authority {
            user: changed.user,
            ..auth.clone()
        };
        let epoch_b = Store::inspect(&scope, &second)?.0;
        let b = Store::issue(&scope, &second, &epoch_b)?;
        b.add(grant(dir.path().join("two"), 2))?;
        assert!(
            profiles
                .revoke_folder_permissions(&original, &scope.store_dir)
                .is_err()
        );
        assert!(!a.gate.revoked()? && !b.gate.revoked()?);
        let backup_b = std::fs::read(&scope.catalog)?;
        let (_, before_revoke) = profiles.inspect_catalog("test")?;
        let snapshot = profiles.revoke_folder_permissions(&before_revoke, &scope.store_dir)?;
        assert!(!profiles.has_folder_permissions(&snapshot)?);
        assert!(a.gate.revoked()? && b.gate.revoked()?);
        assert!(a.read()?.is_empty() && b.read()?.is_empty());
        assert!(b.add(grant(dir.path().join("delayed"), 3)).is_err());
        for (backup, peer, stale) in [
            (&backup_b, &second, &epoch_b),
            (&backup_a, &auth, &epoch_a),
            (&pristine, &auth, &before_first),
        ] {
            std::fs::write(&scope.catalog, backup)?;
            assert!(Store::inspect(&scope, peer)?.1.is_none());
            assert!(Store::issue(&scope, peer, stale).is_err());
        }
        let fresh = Store::inspect(&scope, &auth)?.0;
        let renewed = Store::issue(&scope, &auth, &fresh)?;
        assert!(!renewed.gate.revoked()? && renewed.read()?.is_empty());
        renewed.add(grant(dir.path().join("chosen-again"), 4))?;
        let (issued, saved) = Store::inspect(&scope, &auth)?;
        assert_eq!(issued, fresh);
        assert_eq!(saved.unwrap().read()?[0].id, format!("{:032x}", 4));
        Ok(())
    }

    #[test]
    fn concurrent_first_consent_reuses_one_grant_namespace() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (scope, auth) = fixture(dir.path());
        let (expected, no_store) = Store::inspect(&scope, &auth)?;
        assert!(no_store.is_none());
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|s| -> Result<()> {
            let handles = ["game", "admin"].map(|name| {
                s.spawn({
                    let (barrier, scope, auth, expected, dir) =
                        (&barrier, &scope, &auth, &expected, dir.path());
                    move || -> Result<()> {
                        barrier.wait();
                        let store = Store::issue(scope, auth, expected)?;
                        let mut g = grant(dir.join(name), name.len() as u128);
                        g.name = name.into();
                        store.add(g)
                    }
                })
            });
            for handle in handles {
                handle.join().unwrap()?;
            }
            Ok(())
        })?;
        let (epoch, store) = Store::inspect(&scope, &auth)?;
        assert_eq!(epoch, expected);
        assert_eq!(store.unwrap().read()?.len(), 2);
        Ok(())
    }

    #[test]
    fn snapshot_and_retirement_are_isolated_by_catalog_and_profile() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (scope, auth) = fixture(dir.path());
        let first = Store::for_profile(&scope, &auth, true)?.unwrap();
        let original = ProfileStore::new(scope.catalog.clone())?;
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&scope.catalog)?)?;
        document["profiles"]["sibling"] = document["profiles"]["test"].clone();
        std::fs::write(&scope.catalog, serde_json::to_vec(&document)?)?;
        let copy = FolderScope {
            catalog: dir.path().join("copy.json"),
            ..scope.clone()
        };
        std::fs::copy(&scope.catalog, &copy.catalog)?;
        let sibling = Store::for_profile(
            &FolderScope {
                profile: "sibling".into(),
                ..scope.clone()
            },
            &Authority {
                profile: "sibling".into(),
                ..auth.clone()
            },
            true,
        )?
        .unwrap();
        let other = Store::for_profile(&copy, &auth, true)?.unwrap();
        for store in [&first, &sibling, &other] {
            store.add(grant(dir.path().join("ok"), 5))?;
        }
        let (_, base) = original.inspect_catalog("test")?;
        assert!(
            ProfileStore::new(copy.catalog)?
                .revoke_folder_permissions(&base, &scope.store_dir)
                .is_err()
        );
        document["profiles"]["test"]["extra"] = true.into();
        std::fs::write(&scope.catalog, serde_json::to_vec(&document)?)?;
        assert!(
            original
                .revoke_folder_permissions(&base, &scope.store_dir)
                .is_err()
        );
        let (_, current) = original.inspect_catalog("test")?;
        original.revoke_folder_permissions(&current, &scope.store_dir)?;
        assert!(first.gate.revoked()? && first.read()?.is_empty());
        for unaffected in [&sibling, &other] {
            assert!(!unaffected.gate.revoked()? && unaffected.read()?.len() == 1);
        }
        Ok(())
    }

    #[test]
    fn replacing_an_unshared_profile_invalidates_its_pending_picker() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let (scope, auth) = fixture(dir.path());
        let catalog = ProfileStore::new(scope.catalog.clone())?;
        let (_, legacy) = catalog.inspect_catalog("test")?;
        catalog.delete(&legacy)?;
        catalog.add("test", &legacy.options)?;
        let (pending, _) = Store::inspect(&scope, &auth)?;
        let (_, first) = catalog.inspect_catalog("test")?;
        catalog.delete(&first)?;
        catalog.add("test", &first.options)?;
        assert!(Store::issue(&scope, &auth, &pending).is_err());
        let (current, store) = Store::inspect(&scope, &auth)?;
        assert!(store.is_none());
        assert_ne!(pending, current);
        Ok(())
    }
}
