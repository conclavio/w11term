//! Windows-user permissions for read-only exports from a named SSH pane.
mod store;
use crate::{
    options::SshOptions,
    profiles::FolderEpoch,
    ssh::{
        ConnectionShare, FolderMount, FolderMountStatus, FolderRoot, MountedFolder, ShareSlot,
        valid_mount_name,
    },
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use store::{Grant, Store};
use tokio::sync::mpsc;

const MAX_FOLDERS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderScope {
    catalog: PathBuf,
    profile: String,
    store_dir: PathBuf,
}
impl FolderScope {
    pub fn new(catalog: PathBuf, profile: String, store_dir: PathBuf) -> Self {
        Self {
            catalog,
            profile,
            store_dir,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Authority {
    pub profile: String,
    pub host: String,
    pub user: String,
    pub port: u16,
    pub fingerprint: String,
    pub pane: u64,
    generation: u64,
    epoch: FolderEpoch,
}
impl Authority {
    pub fn connection_generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn check_profile(&self, options: &SshOptions) -> Result<()> {
        ensure!(
            self.host.eq_ignore_ascii_case(&options.host)
                && self.user == options.user
                && self.port == options.port
                && options
                    .host_key
                    .as_ref()
                    .is_none_or(|key| key == &self.fingerprint),
            "The profile no longer matches this verified SSH session"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderRow {
    pub name: String,
    pub path: PathBuf,
    pub status: FolderMountStatus,
    pub mounted: Option<MountedFolder>,
}

struct Row {
    path: PathBuf,
    grant: Option<Grant>,
    mount: Option<FolderMount>,
    status: FolderMountStatus,
}
impl Row {
    fn status(&self) -> FolderMountStatus {
        self.mount
            .as_ref()
            .map(FolderMount::status)
            .unwrap_or_else(|| self.status.clone())
    }
}

#[derive(Default)]
struct State {
    auth: Option<Authority>,
    rows: BTreeMap<String, Row>,
    error: Option<String>,
    closed: bool,
}
struct Core {
    notify: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    slot: ShareSlot,
    scope: FolderScope,
    pane: u64,
    state: Mutex<State>,
}

enum Action {
    Refresh,
    Add(Authority, String, PathBuf),
    Mount(Authority, Grant),
    Forget(Authority, String, Option<String>),
}

pub struct PaneShares {
    core: Arc<Core>,
    send: mpsc::Sender<Action>,
    worker: tokio::task::JoinHandle<()>,
}

impl PaneShares {
    pub(crate) fn new(pane: u64, slot: ShareSlot, scope: FolderScope) -> Self {
        let mut published = slot.subscribe();
        let core = Arc::new(Core {
            slot,
            scope,
            pane,
            state: Default::default(),
            notify: Default::default(),
        });
        let (send, mut receive) = mpsc::channel(32);
        let service = core.clone();
        let worker = crate::ssh::NETWORK.spawn(async move {
            let mut next = Action::Refresh;
            loop {
                let core = service.clone();
                let mut operation = tokio::task::spawn_blocking(move || core.process(next));
                let mut refresh = false;
                // Revocation never waits for disk I/O from another grant on this pane.
                loop {
                    tokio::select! {
                        result = &mut operation => { if result.is_err() { return; } break }
                        changed = published.changed() => {
                            if changed.is_err() { return; }
                            service.expire();
                            refresh = true;
                        }
                    }
                }
                if refresh { next = Action::Refresh; continue; }
                // Idle profiles need only a publication or user action. Active
                // permissions also follow edits/removals made by another process.
                let watch_disk = {
                    let state = service.state.lock().unwrap();
                    !state.rows.is_empty() || state.error.is_some()
                };
                next = tokio::select! {
                    changed = published.changed() => { if changed.is_err() { break; } service.expire(); Action::Refresh }
                    action = receive.recv() => { let Some(action) = action else { break }; action }
                    _ = tokio::time::sleep(Duration::from_secs(1)), if watch_disk => Action::Refresh,
                };
            }
        });
        Self { core, send, worker }
    }

    pub fn scope(&self) -> &FolderScope {
        &self.core.scope
    }
    pub fn refresh_profile(&self) {
        // Window activation can discover edits from an external profile editor.
        let _ = self.send.try_send(Action::Refresh);
    }
    pub fn on_authority_change(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self.core.notify.lock().unwrap() = Some(wake);
    }
    pub fn authority(&self) -> Option<Authority> {
        let state = self.core.state.lock().unwrap();
        let auth = state.auth.as_ref()?;
        self.core.live(auth).ok().map(|_| auth.clone())
    }
    pub fn rows(&self, auth: &Authority) -> Result<Vec<FolderRow>> {
        let state = self.core.check(auth)?;
        if let Some(error) = &state.error {
            bail!("{error}");
        }
        Ok(state
            .rows
            .iter()
            .map(|(name, row)| FolderRow {
                name: name.clone(),
                path: row.path.clone(),
                status: row.status(),
                mounted: row.mount.as_ref().and_then(FolderMount::mounted_folder),
            })
            .collect())
    }

    /// Recheck saved consent on a blocking worker, then resolve the exact mount
    /// the user clicked. A remount may reuse both the Linux and Windows paths.
    pub fn resolve_mounted(
        &self,
        auth: &Authority,
        name: &str,
        generation: &str,
    ) -> impl FnOnce() -> Result<MountedFolder> + Send + 'static + use<> {
        let core = self.core.clone();
        let auth = auth.clone();
        let name = name.to_owned();
        let generation = generation.to_owned();
        move || {
            let grants = core.current_store(&auth)?.read()?;
            let state = core.check(&auth)?;
            let row = state
                .rows
                .get(&name)
                .context("This folder is no longer shared")?;
            ensure!(
                row.grant
                    .as_ref()
                    .is_some_and(|grant| grants.contains(grant)),
                "Folder permission changed"
            );
            row.mount
                .as_ref()
                .and_then(FolderMount::mounted_folder)
                .filter(|mount| mount.generation == generation)
                .context("This folder mount has ended or changed")
        }
    }
    pub fn add(&self, auth: &Authority, name: &str, path: PathBuf) -> Result<()> {
        ensure!(
            valid_mount_name(name),
            "Use a Linux mount name of 1–64 ASCII letters, digits, dots, underscores or hyphens, starting with a letter or digit"
        );
        let mut state = self.core.check(auth)?;
        if let Some(error) = &state.error {
            bail!("{error}");
        }
        ensure!(
            state.rows.len() < MAX_FOLDERS,
            "A profile can share up to {MAX_FOLDERS} folders"
        );
        ensure!(
            !state.rows.contains_key(name),
            "This Linux mount name is already in use"
        );
        self.queue(Action::Add(auth.clone(), name.into(), path.clone()))?;
        state.rows.insert(
            name.into(),
            Row {
                path,
                grant: None,
                mount: None,
                status: FolderMountStatus::Starting,
            },
        );
        Ok(())
    }
    pub fn mount(&self, auth: &Authority, name: &str) -> Result<()> {
        let mut state = self.core.check(auth)?;
        let row = state
            .rows
            .get_mut(name)
            .context("Shared folder was removed")?;
        let grant = row
            .grant
            .as_ref()
            .context("This permission was not saved; forget the row and add the folder again")?;
        ensure!(
            matches!(
                row.status(),
                FolderMountStatus::Stopped | FolderMountStatus::Failed(_)
            ),
            "Folder is already active"
        );
        self.queue(Action::Mount(auth.clone(), grant.clone()))?;
        row.mount.take();
        row.status = FolderMountStatus::Starting;
        Ok(())
    }
    pub fn unmount(&self, auth: &Authority, name: &str) -> Result<()> {
        let mut state = self.core.check(auth)?;
        let row = state
            .rows
            .get_mut(name)
            .context("Shared folder was removed")?;
        if let Some(mount) = &row.mount {
            mount.unmount();
        }
        row.status = FolderMountStatus::Stopped;
        Ok(())
    }
    pub fn forget(&self, auth: &Authority, name: &str) -> Result<()> {
        let mut state = self.core.check(auth)?;
        let row = state
            .rows
            .get_mut(name)
            .context("Shared folder was removed")?;
        self.queue(Action::Forget(
            auth.clone(),
            name.into(),
            row.grant.as_ref().map(|g| g.id.clone()),
        ))?;
        if let Some(mount) = row.mount.take() {
            mount.unmount();
        }
        row.status = FolderMountStatus::Unmounting;
        Ok(())
    }
    fn queue(&self, action: Action) -> Result<()> {
        self.send
            .try_send(action)
            .map_err(|_| anyhow::anyhow!("The shared-folder worker is busy or closed"))
    }
}
impl Drop for PaneShares {
    fn drop(&mut self) {
        let mut state = self.core.state.lock().unwrap();
        state.closed = true;
        state.rows.clear();
        self.worker.abort();
    }
}

impl Core {
    fn expire(&self) {
        let mut state = self.state.lock().unwrap();
        if state
            .auth
            .as_ref()
            .is_some_and(|auth| self.live(auth).is_err())
        {
            state.auth = None;
            state.rows.clear();
            state.error = None;
            drop(state);
            if let Some(wake) = self.notify.lock().unwrap().clone() {
                wake();
            }
        }
    }
    fn live(&self, auth: &Authority) -> Result<ConnectionShare> {
        self.slot
            .generation()
            .filter(|(epoch, _)| self.pane == auth.pane && *epoch == auth.generation)
            .map(|(_, link)| link)
            .context("This SSH connection has ended")
    }
    fn check(&self, auth: &Authority) -> Result<std::sync::MutexGuard<'_, State>> {
        self.live(auth)?;
        let state = self.state.lock().unwrap();
        ensure!(
            !state.closed && state.auth.as_ref() == Some(auth),
            "This shared-folder view has expired"
        );
        Ok(state)
    }
    fn process(&self, action: Action) {
        match action {
            Action::Refresh => {
                let before = self.state.lock().unwrap().auth.clone();
                self.refresh();
                if before != self.state.lock().unwrap().auth
                    && let Some(wake) = self.notify.lock().unwrap().clone()
                {
                    wake();
                }
            }
            Action::Add(auth, name, path) => {
                if let Err(e) = self.add(&auth, &name, path) {
                    self.failed(&auth, &name, e);
                }
            }
            Action::Mount(auth, grant) => {
                if let Err(e) = self.start(&auth, &grant, None) {
                    self.failed(&auth, &grant.name, e);
                }
            }
            Action::Forget(auth, name, id) => {
                let result = self.remove(&auth, &name, id.as_deref());
                if let Err(e) = result {
                    self.failed(&auth, &name, e);
                }
            }
        }
    }
    fn failed(&self, auth: &Authority, name: &str, error: anyhow::Error) {
        if let Ok(mut state) = self.check(auth)
            && let Some(row) = state.rows.get_mut(name)
        {
            row.mount.take();
            row.status = FolderMountStatus::Failed(format!("{error:#}"));
        }
    }
    fn refresh(&self) {
        let expected = self.slot.generation().and_then(|(generation, link)| {
            let (host, user, port, key) = link.verified_endpoint();
            let mut auth = Authority {
                profile: self.scope.profile.clone(),
                host: host.to_ascii_lowercase(),
                user: user.into(),
                port,
                fingerprint: key.into(),
                pane: self.pane,
                generation,
                epoch: FolderEpoch::default(),
            };
            let (epoch, store) = Store::inspect(&self.scope, &auth).ok()?;
            auth.epoch = epoch;
            Some((auth, store))
        });
        {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return;
            }
            if state.auth.as_ref() != expected.as_ref().map(|(auth, _)| auth) {
                state.rows.clear();
                state.auth = None;
                state.error = None;
            }
        }
        let Some((auth, store)) = expected else {
            return;
        };
        let saved = store.as_ref().map(Store::read).transpose();
        let additions = {
            let mut state = self.state.lock().unwrap();
            if state.closed || self.live(&auth).is_err() {
                return;
            }
            state.auth = Some(auth.clone());
            let grants = match saved {
                Ok(grants) => grants.unwrap_or_default(),
                Err(e) => {
                    state.rows.clear();
                    state.error = Some(format!("{e:#}"));
                    return;
                }
            };
            state.error = None;
            state
                .rows
                .retain(|_, row| row.grant.as_ref().is_none_or(|g| grants.contains(g)));
            let mut additions = Vec::new();
            for grant in grants {
                if !state.rows.contains_key(&grant.name) {
                    state.rows.insert(
                        grant.name.clone(),
                        Row {
                            path: grant.path.clone(),
                            grant: Some(grant.clone()),
                            mount: None,
                            status: FolderMountStatus::Starting,
                        },
                    );
                    additions.push(grant);
                }
            }
            additions
        };
        for grant in additions {
            if let Err(e) = self.start(&auth, &grant, None) {
                self.failed(&auth, &grant.name, e);
            }
        }
    }
    fn current_store(&self, auth: &Authority) -> Result<Store> {
        drop(self.check(auth)?);
        let (epoch, store) = Store::inspect(&self.scope, auth)?;
        ensure!(epoch == auth.epoch, "This shared-folder view has expired");
        store.context("Profile has no folder permissions")
    }
    fn add(&self, auth: &Authority, name: &str, path: PathBuf) -> Result<()> {
        drop(self.check(auth)?);
        let root = FolderRoot::open(&path).context("Opening the selected local NTFS directory")?;
        let grant = Grant {
            id: format!("{:032x}", rand::random::<u128>()),
            name: name.into(),
            identity: root.identity()?,
            path,
        };
        let store = Store::issue(&self.scope, auth, &auth.epoch)?;
        drop(self.check(auth)?);
        store.add(grant.clone())?;
        {
            let mut state = self.check(auth)?;
            let row = state
                .rows
                .get_mut(name)
                .context("Folder selection was canceled")?;
            row.grant = Some(grant.clone());
        }
        self.start(auth, &grant, Some(root))
    }
    fn start(&self, auth: &Authority, grant: &Grant, selected: Option<FolderRoot>) -> Result<()> {
        let root = match selected {
            Some(root) => root,
            None => FolderRoot::open(&grant.path).context("Opening saved local folder")?,
        };
        ensure!(
            root.identity()? == grant.identity,
            "The selected Windows directory was replaced. Forget this permission and choose the folder again"
        );
        let store = self.current_store(auth)?;
        ensure!(store.read()?.contains(grant), "Folder permission changed");
        let root = root.with_profile_permission(store.revocation_gate());
        let mut state = self.check(auth)?;
        let row = state
            .rows
            .get_mut(&grant.name)
            .context("Shared folder was removed")?;
        if row.status != FolderMountStatus::Starting || row.grant.as_ref() != Some(grant) {
            return Ok(());
        }
        row.mount = Some(FolderMount::start(self.live(auth)?, root, &grant.name)?);
        Ok(())
    }
    fn remove(&self, auth: &Authority, name: &str, id: Option<&str>) -> Result<()> {
        let grant = self
            .check(auth)?
            .rows
            .get(name)
            .and_then(|r| r.grant.clone());
        if let Some(grant) = grant {
            ensure!(
                id.is_none_or(|id| id == grant.id),
                "Shared folder was replaced"
            );
            self.current_store(auth)?.remove(&grant)?;
        }
        self.check(auth)?.rows.remove(name);
        Ok(())
    }
}
