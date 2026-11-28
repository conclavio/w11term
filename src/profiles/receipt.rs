use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{fs::File, io::Read, path::Path};

const MAX_BYTES: usize = 1024 * 1024;

#[derive(Deserialize)]
struct Envelope<T> {
    version: u32,
    ok: bool,
    result: Option<T>,
}

pub(super) fn read<T: DeserializeOwned>(path: &Path, operation: &str) -> Result<T> {
    ensure!(
        path.metadata()
            .with_context(|| format!("Opening rollout receipt {}", path.display()))?
            .is_file(),
        "Choose a regular rollout receipt file"
    );
    let mut bytes = Vec::new();
    File::open(path)
        .with_context(|| format!("Opening rollout receipt {}", path.display()))?
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_BYTES, "Rollout receipt exceeds 1 MiB");
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
    let reply: Envelope<T> = serde_json::from_slice(bytes)
        .with_context(|| format!("Reading the {operation} rollout receipt"))?;
    ensure!(
        reply.version == crate::control::VERSION && reply.ok,
        "Choose a successful {operation} rollout reply"
    );
    reply
        .result
        .with_context(|| format!("Choose the reply from an applied {operation} rollout"))
}

pub(super) fn check_catalog(recorded: &Path, current: &Path) -> Result<()> {
    ensure!(
        recorded == current
            || matches!(
                (recorded.canonicalize(), current.canonicalize()),
                (Ok(recorded), Ok(current)) if recorded == current
            ),
        "This rollout receipt is for a different profile catalog"
    );
    Ok(())
}

pub(super) fn write(path: &Path, report: &impl Serialize) -> Result<()> {
    let reply = crate::control::Reply::from_result(Ok(serde_json::to_value(report)?));
    let mut json = serde_json::to_string(&reply)?;
    json.push('\n');
    ensure!(json.len() <= MAX_BYTES, "Rollout receipt exceeds 1 MiB");
    crate::transcript::save(path, &json, false)
}
