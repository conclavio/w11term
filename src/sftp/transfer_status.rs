use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// A drawer transfer stays identified across batch items and window moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TransferId {
    pub pane: u64,
    pub generation: u64,
    pub serial: u64,
}

impl fmt::Display for TransferId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.pane, self.generation, self.serial)
    }
}

impl FromStr for TransferId {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let mut parts = value.split(':');
        let mut next = || {
            let part = parts.next().context("Missing transfer ID component")?;
            ensure!(
                !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()),
                "Transfer ID components must be unsigned decimal identifiers"
            );
            let number = part.parse::<u64>()?;
            ensure!(number > 0, "Transfer ID components must be positive");
            Ok(number)
        };
        let id = Self {
            pane: next()?,
            generation: next()?,
            serial: next()?,
        };
        ensure!(
            parts.next().is_none(),
            "Expected three transfer ID components"
        );
        Ok(id)
    }
}

impl TryFrom<String> for TransferId {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        value.parse()
    }
}

impl From<TransferId> for String {
    fn from(value: TransferId) -> Self {
        value.to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransferOperation {
    PrepareUpload,
    UploadFile,
    UploadFolder,
    DownloadFile,
    DownloadFolder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransferPhase {
    Preparing,
    Running,
    Cancelling,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransferOutcome {
    Completed,
    Cancelled,
    Failed,
    Interrupted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct TransferBatchProgress {
    /// One-based index of the current item.
    pub item: usize,
    pub total: usize,
}
