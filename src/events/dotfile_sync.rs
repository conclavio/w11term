use crate::sftp::DotfileShell;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DotfileSyncStatus {
    Updated,
    Current,
    Held,
    Skipped,
    Unconfirmed,
    NotRun,
}

#[derive(Debug, Serialize)]
pub struct DotfileSyncReport {
    pub(crate) generation: u64,
    #[serde(skip)]
    pub shell: DotfileShell,
    pub status: DotfileSyncStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
