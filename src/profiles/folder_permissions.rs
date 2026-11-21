//! A profile's consent epoch identifies the approval presented in a folder
//! picker. All verified peers of that profile share the same revocation record.
use super::*;
use crate::folder_grants::Authority;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{ErrorKind, Read},
    path::Path,
    time::{Duration, Instant},
};

fn id<'a>(raw: &'a serde_json::Value, key: &str) -> Result<Option<&'a str>> {
    raw.get(key)
        .map(|value| {
            value
                .as_str()
                .filter(|id| valid_id(id))
                .context("Invalid folder-sharing profile identifier")
        })
        .transpose()
}
fn valid_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
fn hash(value: impl serde::Serialize) -> Result<String> {
    let bytes = Sha256::digest(serde_json::to_vec(&value)?);
    Ok(format!("{:x}", bytes::Bytes::copy_from_slice(&bytes[..16])))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FolderEpoch {
    catalog: PathBuf,
    profile: String,
    consent: String,
    revision: Option<String>,
}

impl FolderEpoch {
    pub(crate) fn catalog(&self) -> &Path {
        &self.catalog
    }
    pub(crate) fn gate(&self, data_dir: &Path, token: &str) -> Result<FolderGrantGate> {
        Ok(FolderGrantGate::new(
            marker_path(data_dir, &self.catalog, &self.profile, token)?,
            revision_path(data_dir, &self.catalog, &self.profile)?,
            self.revision.clone(),
        ))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FolderGrantGate {
    marker: PathBuf,
    revision_path: PathBuf,
    expected_revision: Option<String>,
    selection: Option<PathBuf>,
}

/// The OS releases both shared locks when this disk operation completes or
/// its process exits. The profile editor and single-folder Forget drain the
/// corresponding exclusive lock before confirming revocation.
pub(crate) struct FolderMutationPermit {
    _profile: fs::File,
    _selection: Option<fs::File>,
}
impl FolderGrantGate {
    pub(crate) fn new(
        marker: PathBuf,
        revision_path: PathBuf,
        expected_revision: Option<String>,
    ) -> Self {
        Self {
            marker,
            revision_path,
            expected_revision,
            selection: None,
        }
    }
    pub(crate) fn for_selection(&self, marker: PathBuf) -> Self {
        Self {
            selection: Some(marker),
            ..self.clone()
        }
    }

    pub(crate) fn selection_retired(&self) -> Result<bool> {
        match &self.selection {
            Some(marker) => retired(marker),
            None => Ok(false),
        }
    }

    pub(crate) fn forget_selection(&self) -> Result<()> {
        let marker = self
            .selection
            .as_deref()
            .context("Missing saved folder identifier")?;
        record_revocation(marker)?;
        // A sibling can keep writing throughout. Only requests that passed
        // this exact selection's check before the marker appeared can remain.
        let writes = mutation_lock(marker)?;
        writes.lock()?;
        Ok(())
    }
    pub(crate) fn revoked(&self) -> Result<bool> {
        Ok(retired(&self.marker)?
            || self.selection_retired()?
            || revision(&self.revision_path)? != self.expected_revision)
    }
    /// Keep a single admitted filesystem mutation inside the lifetime of this
    /// profile's consent. The editor waits for these handles before reporting
    /// successful revocation, including when it runs in another process.
    pub(crate) fn lock_mutation(&self) -> Result<FolderMutationPermit> {
        let profile = mutation_lock(&self.revision_path)?;
        profile.lock_shared()?;
        let selection = self.selection.as_deref().map(mutation_lock).transpose()?;
        if let Some(lock) = &selection {
            lock.lock_shared()?;
        }
        ensure!(!self.revoked()?, "Folder sharing permission was revoked");
        Ok(FolderMutationPermit {
            _profile: profile,
            _selection: selection,
        })
    }
}

fn record_revocation(marker: &Path) -> Result<()> {
    fs::create_dir_all(marker.parent().context("Missing permission directory")?)
        .context("Creating folder revocation directory")?;
    fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(marker)
        .and_then(|file| file.sync_all())
        .context("Recording folder permission revocation")
}

fn mutation_lock(revision: &Path) -> Result<fs::File> {
    let parent = revision
        .parent()
        .context("Missing folder permission directory")?;
    fs::create_dir_all(parent)?;
    fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(revision.with_extension("io-lock"))
        .context("Opening the folder mutation lock")
}

#[cfg(test)]
#[test]
fn successful_revocation_waits_for_writes_already_admitted() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let catalog = temp.path().join("profiles.json");
    let token = "0123456789abcdef0123456789abcdef";
    fs::write(
        &catalog,
        serde_json::to_vec(&serde_json::json!({
            "profiles": {"linux": {"host":"example.test","user":"user","auth":"none","folder_share_id":token}}
        }))?,
    )?;
    let store = ProfileStore::new(catalog)?;
    let (_, snapshot) = store.inspect_catalog("linux")?;
    let marker = marker_path(&data, &snapshot.catalog_path, "linux", token)?;
    let revision = revision_path(&data, &snapshot.catalog_path, "linux")?;
    let gate = FolderGrantGate::new(marker, revision, None);
    let admitted = gate.lock_mutation()?;
    let (tx, rx) = std::sync::mpsc::channel();
    let revoke = std::thread::spawn(move || {
        let _ = tx.send(store.revoke_folder_permissions(&snapshot, &data));
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !gate.revoked()? {
        ensure!(
            Instant::now() < deadline,
            "revocation did not invalidate the grant"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(matches!(
        rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    drop(admitted);
    rx.recv_timeout(Duration::from_secs(5))??;
    revoke.join().unwrap();
    assert!(gate.lock_mutation().is_err());
    Ok(())
}

// The marker addresses a single grant namespace across peer fingerprints.
// The revision also invalidates open pickers when a catalog backup has *no*
// portable token to use as a lookup key for a previous revocation.
fn marker_path(data_dir: &Path, catalog: &Path, name: &str, token: &str) -> Result<PathBuf> {
    Ok(data_dir
        .join("folder-shares")
        .join("revoked")
        .join(format!("{}.revoked", hash((catalog, name, token))?)))
}
fn revision_path(data_dir: &Path, catalog: &Path, profile: &str) -> Result<PathBuf> {
    Ok(data_dir
        .join("folder-shares")
        .join("revoked")
        .join(format!("{}.epoch", hash((catalog, profile))?)))
}
fn retired(marker: &Path) -> Result<bool> {
    match fs::symlink_metadata(marker) {
        Ok(meta) => {
            ensure!(
                meta.file_type().is_file(),
                "Invalid folder permission revocation record"
            );
            Ok(true)
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).context("Checking folder permission revocation"),
    }
}
fn revision(path: &Path) -> Result<Option<String>> {
    if !retired(path)? {
        return Ok(None);
    }
    let file = fs::File::open(path).context("Checking folder permission revision")?;
    let mut revision = String::new();
    file.take(33).read_to_string(&mut revision)?;
    ensure!(valid_id(&revision), "Invalid folder permission revision");
    Ok(Some(revision))
}

fn save_revision(path: &Path, revision: &str) -> Result<()> {
    let parent = path
        .parent()
        .context("Missing folder permission directory")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(revision.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(path)
        .context("Saving folder permission revision")?;
    Ok(())
}

fn binding(
    catalog: PathBuf,
    profile: &str,
    raw: &serde_json::Value,
    data_dir: &Path,
) -> Result<(FolderEpoch, Option<String>)> {
    let local_revision = revision(&revision_path(data_dir, &catalog, profile)?)?;
    let mut token = id(raw, SHARE_TOKEN)?.map(str::to_owned);
    let consent = match token.as_deref() {
        Some(t)
            if {
                let marker = marker_path(data_dir, &catalog, profile, t)?;
                retired(&marker)? || revision(&marker.with_extension("issued"))? != local_revision
            } =>
        {
            let key = hash(("retired", t))?;
            token = None;
            key
        }
        _ => match id(raw, SHARE_EPOCH)?.or(token.as_deref()) {
            Some(s) => s.to_owned(),
            // Includes unknown fields so changing a never-shared profile while
            // a picker is open invalidates that pending selection.
            None => hash(("pristine", raw))?,
        },
    };
    Ok((
        FolderEpoch {
            catalog,
            profile: profile.into(),
            consent,
            revision: local_revision,
        },
        token,
    ))
}

impl ProfileStore {
    /// A previously issued identifier can also be retired after restoring a
    /// backup. This predicate uses only the editor's displayed snapshot.
    pub fn has_folder_permissions(&self, base: &ProfileSnapshot) -> Result<bool> {
        Ok(id(&base.raw, SHARE_TOKEN)?.is_some())
    }

    pub fn revoke_folder_permissions(
        &self,
        base: &ProfileSnapshot,
        data_dir: &Path,
    ) -> Result<ProfileSnapshot> {
        let mut updated = base.clone();
        self.modify(|profiles, catalog| {
            ensure!(
                fs::canonicalize(&self.path)? == base.catalog_path
                    && profiles.get(&base.name) == Some(&base.raw)
                    && catalog.profiles.get(&base.name) == Some(&base.options),
                "Profile changed on disk; reopen it before revoking folder permissions"
            );
            let token = id(&base.raw, SHARE_TOKEN)?.context("Profile has no folder permissions")?;
            let marker = marker_path(data_dir, &base.catalog_path, &base.name, token)?;
            // Existence alone revokes the grant even if the following profile
            // save fails. Repeating the operation from that snapshot is safe.
            record_revocation(&marker)?;
            let new = format!("{:032x}", rand::random::<u128>());
            let revision = revision_path(data_dir, &base.catalog_path, &base.name)?;
            save_revision(&revision, &new)?;
            // The marker and revision refuse new requests. Drain requests that
            // checked consent before either record was written.
            let writes = mutation_lock(&revision)?;
            writes.lock()?;
            let entry = profiles
                .get_mut(&base.name)
                .unwrap()
                .as_object_mut()
                .context("Invalid SSH profile")?;
            entry.remove(SHARE_TOKEN);
            entry.insert(SHARE_EPOCH.into(), new.into());
            updated.raw = serde_json::Value::Object(entry.clone());
            Ok(None)
        })?;
        Ok(updated)
    }

    pub(crate) fn folder_binding(
        &self,
        auth: &Authority,
        data_dir: &Path,
    ) -> Result<(FolderEpoch, Option<String>)> {
        let (_, snapshot) = self.inspect_catalog(&auth.profile)?;
        auth.check_profile(&snapshot.options)?;
        binding(
            snapshot.catalog_path,
            &snapshot.name,
            &snapshot.raw,
            data_dir,
        )
    }

    pub(crate) fn folder_lock(&self) -> Result<fs::File> {
        // Independent simultaneous first grants may share one newly issued token.
        // Wait briefly for the other profile writer, without blocking indefinitely.
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match self.lock() {
                Err(e)
                    if matches!(
                        e.downcast_ref::<fs::TryLockError>(),
                        Some(fs::TryLockError::WouldBlock)
                    ) && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10))
                }
                result => return result,
            }
        }
    }

    pub(crate) fn issue_folder_epoch(
        &self,
        auth: &Authority,
        data_dir: &Path,
        expected: &FolderEpoch,
    ) -> Result<String> {
        let _lock = self.folder_lock()?;
        let (current, token) = self.folder_binding(auth, data_dir)?;
        ensure!(
            &current == expected,
            "Folder permissions changed; reopen Shared folders"
        );
        if let Some(token) = token {
            return Ok(token);
        }
        let token = format!("{:032x}", rand::random::<u128>());
        self.modify_locked(|profiles, catalog| {
            auth.check_profile(
                catalog
                    .profiles
                    .get(&auth.profile)
                    .context("SSH profile was removed")?,
            )?;
            let profile = profiles
                .get_mut(&auth.profile)
                .context("SSH profile was removed")?;
            let (current, existing) = binding(
                fs::canonicalize(&self.path)?,
                &auth.profile,
                profile,
                data_dir,
            )?;
            ensure!(
                &current == expected && existing.is_none(),
                "Folder permissions changed; reopen Shared folders"
            );
            let entry = profile.as_object_mut().context("Invalid SSH profile")?;
            // Legacy tokens predate receipts. After a profile-wide revocation,
            // only tokens explicitly issued under this Windows user's current
            // revision can reuse grants, even if an older catalog is restored.
            if let Some(revision) = &current.revision {
                let receipt = marker_path(data_dir, &current.catalog, &auth.profile, &token)?
                    .with_extension("issued");
                save_revision(&receipt, revision)?;
            }
            entry.insert(SHARE_TOKEN.into(), token.clone().into());
            entry.insert(SHARE_EPOCH.into(), current.consent.into());
            Ok(None)
        })?;
        Ok(token)
    }
}
