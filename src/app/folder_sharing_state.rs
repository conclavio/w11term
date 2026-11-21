use super::*;
use anyhow::{Context, Result};
use std::path::PathBuf;
pub(super) use w11term::folder_grants::{Authority, FolderRow};
use w11term::folder_grants::{FolderScope, PaneShares};

impl TerminalApp {
    /// Keep permissions attached to the live pane across moves and hidden tabs.
    /// Each worker follows its actual SSH session independently of UI redraws.
    pub(super) fn bind_folder_shares(&mut self) {
        let Ok(data_dir) = w11term::keys::data_dir() else {
            return;
        };
        let ids: Vec<_> = self.tabs.entries().iter().map(|tab| tab.id).collect();
        for id in ids {
            let Some(AppTab {
                page: Page::Terminal(live),
                ..
            }) = self.tabs.get_mut(id)
            else {
                continue;
            };
            let profile = live.profile_name.as_ref().filter(|name| {
                matches!((&live.source,self.catalog.profiles.get(*name)), (Source::Ssh(original),Some(saved))
                    if original.host.eq_ignore_ascii_case(&saved.host) && original.port == saved.port && original.user == saved.user)
            });
            let scope = profile.map(|name| {
                FolderScope::new(self.store.path().into(), name.clone(), data_dir.clone())
            });
            if live.folder_sharing.as_ref().map(PaneShares::scope) != scope.as_ref() {
                live.folder_sharing = scope.map(|scope| live.session.profile_folders(scope));
            }
            if let Some(manager) = &live.folder_sharing {
                manager.on_authority_change(self.folder_wake.clone());
            }
        }
    }
    fn folder_manager(&self, pane: u64) -> Option<&PaneShares> {
        let tab = self.tabs.get(self.tab_for_pane(pane)?)?;
        let Page::Terminal(live) = &tab.page else {
            return None;
        };
        live.folder_sharing.as_ref()
    }
    pub(super) fn folder_authority(&self, pane: u64) -> Option<Authority> {
        self.folder_manager(pane)?.authority()
    }
    fn folders_for(&self, auth: &Authority) -> Result<&PaneShares> {
        self.folder_manager(auth.pane)
            .context("The original SSH pane is closed or its profile changed")
    }
    pub(super) fn folder_rows(&self, auth: &Authority) -> Result<Vec<FolderRow>> {
        self.folders_for(auth)?.rows(auth)
    }
    pub(super) fn folder_add(&self, auth: &Authority, name: &str, path: PathBuf) -> Result<()> {
        self.folders_for(auth)?.add(auth, name, path)
    }
    pub(super) fn folder_mount(&self, auth: &Authority, name: &str) -> Result<()> {
        self.folders_for(auth)?.mount(auth, name)
    }
    pub(super) fn folder_unmount(&self, auth: &Authority, name: &str) -> Result<()> {
        self.folders_for(auth)?.unmount(auth, name)
    }
    pub(super) fn folder_forget(&self, auth: &Authority, name: &str) -> Result<()> {
        self.folders_for(auth)?.forget(auth, name)
    }
}
