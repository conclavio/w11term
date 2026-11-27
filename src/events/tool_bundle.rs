use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolBundleSyncStatus {
    Updated,
    Current,
    Held,
    Skipped,
    Unconfirmed,
    NotRun,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolPathSyncStatus {
    Updated,
    Current,
    Skipped,
}

#[derive(Debug, Serialize)]
pub struct ToolBundleSyncReport {
    pub(crate) generation: u64,
    #[serde(skip)]
    pub bundle: String,
    pub status: ToolBundleSyncStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<ToolPathSyncStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
