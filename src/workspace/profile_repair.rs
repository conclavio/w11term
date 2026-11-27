use super::*;

#[derive(Debug, PartialEq, Eq)]
pub struct MissingProfile {
    pub name: String,
    pub panes: usize,
    pub replacements: Vec<String>,
}

impl SavedSource {
    pub(crate) fn ssh_profile(&self) -> Option<&str> {
        match self {
            Self::SavedProfile(name) | Self::Discovery(name) | Self::HerdrDiscovery(name) => {
                Some(name)
            }
            Self::Startup { connection, .. } | Self::Durable { connection, .. } => {
                connection.ssh_profile()
            }
            _ => None,
        }
    }

    fn set_ssh_profile(&mut self, replacement: &str) {
        match self {
            Self::SavedProfile(name) | Self::Discovery(name) | Self::HerdrDiscovery(name) => {
                *name = replacement.to_owned();
            }
            Self::Startup { connection, .. } | Self::Durable { connection, .. } => {
                connection.set_ssh_profile(replacement);
            }
            _ => unreachable!("checked saved SSH profile reference"),
        }
    }

    fn replacement(&self, name: &str, profiles: &BTreeMap<String, SshOptions>) -> Result<Self> {
        let mut updated = self.clone();
        updated.set_ssh_profile(name);
        updated.resolve(profiles)?;
        Ok(updated)
    }
}

impl Workspace {
    pub fn missing_profiles(
        &self,
        profiles: &BTreeMap<String, SshOptions>,
    ) -> Result<Vec<MissingProfile>> {
        let mut missing = BTreeMap::new();
        for tab in &self.tabs {
            tab.layout.visit(0, &mut |pane| {
                let Some(name) = pane
                    .source
                    .ssh_profile()
                    .filter(|name| !profiles.contains_key(*name))
                else {
                    return Ok(());
                };
                let row = missing
                    .entry(name.to_owned())
                    .or_insert_with(|| MissingProfile {
                        name: name.to_owned(),
                        panes: 0,
                        replacements: profiles.keys().cloned().collect(),
                    });
                row.panes += 1;
                // A plain profile reference accepts any saved SSH profile. Wrapped
                // sources use the same checks as a normal workspace launch.
                if !matches!(pane.source, SavedSource::SavedProfile(_)) {
                    row.replacements.retain(|replacement| {
                        pane.source.replacement(replacement, profiles).is_ok()
                    });
                }
                Ok(())
            })?;
        }
        Ok(missing.into_values().collect())
    }

    fn repair_profiles(
        mut self,
        replacements: &BTreeMap<String, String>,
        profiles: &BTreeMap<String, SshOptions>,
    ) -> Result<Self> {
        ensure!(!replacements.is_empty(), "Choose a replacement SSH profile");
        let mut applied = BTreeSet::new();
        for tab in &mut self.tabs {
            tab.layout
                .repair_profiles(replacements, profiles, &mut applied)?;
        }
        ensure!(
            applied.len() == replacements.len(),
            "A missing SSH profile is no longer in the layout"
        );
        Ok(self)
    }
}

impl SavedLayout {
    fn repair_profiles(
        &mut self,
        replacements: &BTreeMap<String, String>,
        profiles: &BTreeMap<String, SshOptions>,
        applied: &mut BTreeSet<String>,
    ) -> Result<()> {
        match self {
            Self::Split { first, second, .. } => {
                first.repair_profiles(replacements, profiles, applied)?;
                second.repair_profiles(replacements, profiles, applied)
            }
            Self::Leaf(pane) => {
                if let Some(name) = pane.source.ssh_profile()
                    && let Some(replacement) = replacements.get(name)
                {
                    ensure!(
                        !profiles.contains_key(name),
                        "SSH profile {name} is available again. Review the workspace profiles again."
                    );
                    let updated = pane
                        .source
                        .replacement(replacement, profiles)
                        .with_context(|| {
                            format!("Replacing SSH profile {name} with {replacement}")
                        })?;
                    applied.insert(name.to_owned());
                    pane.source = updated;
                }
                Ok(())
            }
        }
    }
}

impl WorkspaceStore {
    /// Repair saved profile names without launching the layout or changing its identity.
    pub fn repair_profiles(
        &self,
        name: &str,
        expected: &WorkspaceEntry,
        replacements: &BTreeMap<String, String>,
        profiles: &BTreeMap<String, SshOptions>,
    ) -> Result<()> {
        self.edit(|catalog| {
            catalog.check_entry(name, expected)?;
            let updated = expected
                .layout
                .clone()
                .repair_profiles(replacements, profiles)?;
            catalog.workspaces.insert(name.to_owned(), updated);
            Ok(())
        })
    }
}
