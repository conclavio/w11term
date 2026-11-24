use anyhow::{Context, Result};
use serde::Serialize;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

pub(crate) fn update<T: Serialize>(
    path: &Path,
    label: &str,
    edit: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let directory = path
        .parent()
        .with_context(|| format!("Saved {label} path has no parent"))?;
    fs::create_dir_all(directory)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))?;
    lock.try_lock()
        .with_context(|| format!("Another window is saving {label}. Try again."))?;
    let value = edit()?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer_pretty(&mut file, &value)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path)
        .with_context(|| format!("Saving {label}"))?;
    Ok(value)
}
