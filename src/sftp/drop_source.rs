use super::{
    Operation, TransferCancel,
    upload_directory::{Source, open_source},
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::File,
    os::windows::{fs::MetadataExt, io::AsRawHandle},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use windows::Win32::{
    Foundation::{ERROR_SHARING_VIOLATION, HANDLE},
    Storage::FileSystem::*,
};

const SHARE_ALL: FILE_SHARE_MODE =
    FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0);

#[derive(Clone, Debug)]
pub struct DropSource {
    path: PathBuf,
    directory: bool,
    reference: Reference,
}

#[derive(Clone, Debug)]
enum Reference {
    Identified(Identity),
    // On filesystems without usable IDs, clone only the token. Exactly one job
    // can take ownership of the original, read-only file or directory handle.
    Pinned(Arc<Mutex<Option<Source>>>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    volume: u64,
    file_id: [u8; 16],
    created: u64,
}

impl Identity {
    fn read(file: &File) -> Result<Option<Self>> {
        let mut id = FILE_ID_INFO::default();
        let result = unsafe {
            GetFileInformationByHandleEx(
                HANDLE(file.as_raw_handle()),
                FileIdInfo,
                (&raw mut id).cast(),
                size_of::<FILE_ID_INFO>() as u32,
            )
        };
        if result.is_err() || id.VolumeSerialNumber == 0 || id.FileId.Identifier == [0; 16] {
            return Ok(None);
        }
        Ok(Some(Self {
            volume: id.VolumeSerialNumber,
            file_id: id.FileId.Identifier,
            created: file.metadata()?.creation_time(),
        }))
    }
}

fn describe(source: &Source) -> Result<(bool, &File)> {
    match source {
        Source::Directory(dir) => Ok((true, dir)),
        Source::File(file, _) => Ok((false, file)),
        Source::Skipped => bail!(
            "a dropped item must be an ordinary file or folder, not a reparse point or device"
        ),
    }
}

/// Inspect the complete drop before starting uploads. Does filesystem I/O; call
/// on a background worker. Most volumes need no open handles while items queue.
pub fn classify_drop(paths: Vec<PathBuf>, cancel: &TransferCancel) -> Result<Vec<DropSource>> {
    classify_with(paths, cancel, Identity::read)
}

fn classify_with(
    paths: Vec<PathBuf>,
    cancel: &TransferCancel,
    identity: impl Fn(&File) -> Result<Option<Identity>>,
) -> Result<Vec<DropSource>> {
    let check_cancel = || {
        ensure!(!cancel.is_cancelled(), "drop preparation cancelled");
        Ok::<_, anyhow::Error>(())
    };
    paths
        .into_iter()
        .map(|path| {
            check_cancel()?;
            let inspect = || {
                // Prefer a handle we can use directly if the volume has no IDs.
                // Only retry a failed open. A successful handle is the chosen object.
                let (source, sharing_conflict) = match open_source(&path, FILE_READ_DATA, FILE_SHARE_READ) {
                    Ok(source) => (source, None),
                    Err(error) if error.downcast_ref::<std::io::Error>().and_then(std::io::Error::raw_os_error) == Some(ERROR_SHARING_VIOLATION.0 as i32) => {
                        check_cancel()?;
                        (open_source(&path, FILE_READ_ATTRIBUTES, SHARE_ALL)?, Some(error))
                    }
                    Err(error) => return Err(error),
                };
                check_cancel()?;
                let (directory, file) = describe(&source)?;
                let reference = if let Some(id) = identity(file)? {
                    Reference::Identified(id)
                } else {
                    if let Some(error) = sharing_conflict {
                        return Err(error).context("this filesystem needs a pinned source; close the other writer and drop the item again");
                    }
                    Reference::Pinned(Arc::new(Mutex::new(Some(source))))
                };
                check_cancel()?;
                Ok::<_, anyhow::Error>((directory, reference))
            };
            let (directory, reference) =
                inspect().with_context(|| format!("checking dropped item {}", path.display()))?;
            Ok(DropSource {
                path,
                directory,
                reference,
            })
        })
        .collect()
}

impl DropSource {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn is_directory(&self) -> bool {
        self.directory
    }
    pub fn into_operation(self, remote: String, overwrite: bool) -> Operation {
        Operation::PutDrop {
            source: self,
            remote,
            overwrite,
        }
    }

    pub(super) fn open_confirmed(self, overwrite: bool) -> Result<(PathBuf, Source)> {
        ensure!(
            !(self.directory && overwrite),
            "dropped folders require a new remote destination"
        );
        let source = match self.reference {
            Reference::Pinned(pin) => pin
                .lock()
                .unwrap()
                .take()
                .context("this dropped item was already used")?,
            Reference::Identified(original) => {
                let open = || {
                    let (access, share) = if self.directory {
                        (FILE_LIST_DIRECTORY | FILE_TRAVERSE, FILE_SHARE_READ)
                    } else {
                        (FILE_READ_DATA, SHARE_ALL)
                    };
                    let source = open_source(&self.path, access, share)?;
                    let (directory, file) = describe(&source)?;
                    ensure!(
                        self.directory == directory
                            && Identity::read(file)?.as_ref() == Some(&original),
                        "local source identity differs from the drop"
                    );
                    Ok::<_, anyhow::Error>(source)
                };
                open().with_context(|| {
                    format!(
                        "dropped item changed or is no longer accessible: {}",
                        self.path.display()
                    )
                })?
            }
        };
        Ok((self.path, source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Read};

    #[test]
    fn reopening_an_identified_drop_checks_object_and_kind_before_upload() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let file = temp.path().join("dropped.txt");
        let dir = temp.path().join("dropped-folder");
        fs::write(&file, "original")?;
        fs::create_dir(&dir)?;
        let sources = classify_drop(vec![file.clone(), dir.clone()], &TransferCancel::default())?;
        let [file_token, dir_token] = sources.as_slice() else {
            unreachable!()
        };
        assert!(matches!(file_token.reference, Reference::Identified(_)));
        assert!(!file_token.is_directory());
        assert!(dir_token.is_directory());
        assert_eq!(file_token.path(), file);
        assert!(
            serde_json::to_value(dir_token.clone().into_operation("/remote".into(), false))
                .is_err()
        );
        assert!(
            serde_json::from_str::<Operation>(
                r#"{"op":"put-drop","remote":"/remote","overwrite":false}"#
            )
            .is_err()
        );
        assert!(
            dir_token
                .clone()
                .open_confirmed(true)
                .unwrap_err()
                .to_string()
                .contains("new remote destination")
        );
        let (_, checked) = file_token.clone().open_confirmed(true)?;
        let Source::File(mut content, _) = checked else {
            unreachable!()
        };
        let mut bytes = String::new();
        content.read_to_string(&mut bytes)?;
        assert_eq!(bytes, "original");
        drop(content);
        fs::rename(&file, temp.path().join("original.txt"))?;
        fs::rename(&dir, temp.path().join("original-folder"))?;
        fs::write(&file, "replaced")?;
        fs::create_dir(&dir)?;
        for source in sources.iter().cloned() {
            assert!(
                source
                    .open_confirmed(false)
                    .unwrap_err()
                    .to_string()
                    .contains("dropped item changed")
            );
        }
        fs::remove_file(&file)?;
        fs::create_dir(&file)?;
        assert!(
            file_token
                .clone()
                .open_confirmed(true)
                .unwrap_err()
                .to_string()
                .contains("dropped item changed")
        );
        Ok(())
    }

    #[test]
    fn identified_file_keeps_the_existing_live_writer_compatible() -> Result<()> {
        use std::io::Write;
        let temp = tempfile::tempdir()?;
        let log = temp.path().join("active.log");
        let mut writer = fs::File::create(&log)?;
        writer.write_all(b"started\n")?;
        let error =
            classify_with(vec![log.clone()], &TransferCancel::default(), |_| Ok(None)).unwrap_err();
        assert!(format!("{error:#}").contains("close the other writer"));
        let original = classify_drop(vec![log], &TransferCancel::default())?
            .pop()
            .unwrap();
        assert!(matches!(original.reference, Reference::Identified(_)));
        let (_, Source::File(mut file, _)) = original.open_confirmed(false)? else {
            unreachable!()
        };
        writer.write_all(b"running\n")?;
        writer.flush()?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        assert_eq!(text, "started\nrunning\n");
        Ok(())
    }

    #[test]
    fn filesystems_without_ids_pin_only_top_level_sources_until_consumed_or_dropped() -> Result<()>
    {
        let temp = tempfile::tempdir()?;
        let file = temp.path().join("file");
        let dir = temp.path().join("folder");
        fs::write(&file, "pinned")?;
        fs::create_dir(&dir)?;
        let child = dir.join("child");
        fs::write(&child, "contents")?;
        let mut sources = classify_with(
            vec![file.clone(), dir.clone()],
            &TransferCancel::default(),
            |_| Ok(None),
        )?;
        assert!(fs::rename(&file, temp.path().join("before")).is_err());
        fs::write(&child, "updated while the top-level directory was queued")
            .context("writing child file while its parent is queued")?;
        let folder_token = sources.pop().unwrap();
        assert!(fs::rename(&dir, temp.path().join("queued-folder")).is_err());
        let (_, Source::Directory(pinned)) = folder_token.open_confirmed(false)? else {
            unreachable!()
        };
        let mut names = Vec::new();
        crate::windows_dirs::names(&pinned, |name| {
            names.push(String::from_utf16(name)?);
            Ok(())
        })?;
        assert_eq!(names, ["child"]);
        let Source::File(mut entry, _) =
            super::super::upload_directory::open_child(&pinned, &names[0])?
        else {
            unreachable!()
        };
        let mut contents = String::new();
        entry.read_to_string(&mut contents)?;
        assert_eq!(contents, "updated while the top-level directory was queued");
        drop(entry);
        assert!(fs::rename(&dir, temp.path().join("active-folder")).is_err());
        drop(pinned);
        fs::rename(&dir, temp.path().join("released-folder"))
            .context("renaming released directory")?;
        let file_token = sources.pop().unwrap();
        let duplicate = file_token.clone();
        let (_, Source::File(mut content, _)) = file_token.open_confirmed(false)? else {
            unreachable!()
        };
        let mut bytes = String::new();
        content.read_to_string(&mut bytes)?;
        assert_eq!(bytes, "pinned");
        assert!(duplicate.open_confirmed(false).is_err());
        drop(content);
        fs::rename(file, temp.path().join("after")).context("renaming released file")?;
        Ok(())
    }

    #[test]
    fn cancelling_preflight_releases_earlier_pins_and_never_opens_later_items() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let paths = ["first", "second", "third"].map(|name| temp.path().join(name));
        fs::write(&paths[0], "one")?;
        fs::write(&paths[1], "two")?;
        let cancel = TransferCancel::default();
        let inspected = std::cell::Cell::new(0);
        let error = classify_with(paths.to_vec(), &cancel, |_| {
            inspected.set(inspected.get() + 1);
            if inspected.get() == 2 {
                cancel.cancel();
            }
            Ok(None)
        })
        .unwrap_err();
        assert_eq!(inspected.get(), 2);
        assert!(format!("{error:#}").contains("drop preparation cancelled"));
        for source in &paths[..2] {
            fs::rename(source, source.with_extension("released"))?;
        }
        Ok(())
    }

    #[test]
    fn a_top_level_junction_rejects_the_entire_drop_and_releases_fallback_pins() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let file = temp.path().join("ordinary");
        fs::write(&file, "ordinary")?;
        fs::create_dir(temp.path().join("actual"))?;
        let status = std::process::Command::new("cmd.exe")
            .current_dir(temp.path())
            .args(["/d", "/c", "mklink", "/J", "alias", "actual"])
            .output()?;
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
        let error = classify_with(
            vec![file.clone(), temp.path().join("alias")],
            &TransferCancel::default(),
            |_| Ok(None),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("reparse point"));
        fs::rename(file, temp.path().join("released"))?;
        Ok(())
    }
}
