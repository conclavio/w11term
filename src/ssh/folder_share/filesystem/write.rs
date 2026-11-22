//! Mutations on an explicitly approved local NTFS directory. Every file-data
//! operation takes a transactional writer before checking its one primary link.
//! NTFS locks the object's contents and namespace against competing writers
//! through commit; failure rolls the private change back. No pathname-based
//! fallback is used if the volume does not provide TxF.
//! See Microsoft's [transactional locking rules](https://learn.microsoft.com/windows/win32/fileio/txf-basic-concepts#transactional-locking).
use super::*;
use std::os::windows::{fs::FileExt, io::FromRawHandle};
use windows::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_DIRECTORY_FILE, FILE_RENAME_INFORMATION, FileRenameInformation,
    NtSetInformationFile,
};

fn file_id(file: &File) -> io::Result<(u64, u64)> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle(file), &mut info) }.map_err(win_error)?;
    Ok((
        info.dwVolumeSerialNumber as u64,
        ((info.nFileIndexHigh as u64) << 32) | u64::from(info.nFileIndexLow),
    ))
}

fn volume_path(file: &File) -> io::Result<String> {
    let mut buffer = vec![0; 32768];
    let count =
        unsafe { GetFinalPathNameByHandleW(handle(file), &mut buffer, VOLUME_NAME_GUID) } as usize;
    if count == 0 {
        return Err(io::Error::last_os_error());
    }
    String::from_utf16(buffer.get(..count).ok_or_else(invalid)?).map_err(|_| excluded())
}

pub(super) struct PinnedDirectory {
    chain: Vec<File>,
    path: String,
}
impl PinnedDirectory {
    fn select(root: &Root) -> io::Result<Arc<Self>> {
        if let Some(pinned) = &root.write_location {
            return Ok(pinned.clone());
        }
        // The consent root could have moved since selection. Reacquire every
        // ancestor without delete sharing and recheck the selected file ID.
        let selected = volume_path(&root.directory)?;
        let (volume, tail) = selected.split_once("}\\").ok_or_else(excluded)?;
        if !volume.starts_with(r"\\?\Volume{") {
            return Err(excluded());
        }
        let absolute = format!("{volume}}}\\");
        let volume_root = OpenOptions::new()
            .access_mode((FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE).0)
            .share_mode(SHARE_ALL.0)
            .custom_flags(OPEN_FLAGS.0)
            .open(&absolute)?;
        let mut chain = vec![volume_root];
        for name in tail.split('\\') {
            if !valid_component(name) {
                return Err(excluded());
            }
            let next = relative(chain.last().unwrap(), name, FILE_TRAVERSE, SHARE_IN_PLACE)?;
            describe(&next, name)?.directory()?;
            chain.push(next);
        }
        if file_id(chain.last().unwrap())? != file_id(&root.directory)? {
            return Err(excluded());
        }
        Ok(Arc::new(Self {
            chain,
            path: selected,
        }))
    }
    fn selected(&self) -> &File {
        self.chain.last().unwrap()
    }
}

struct Location {
    root: Arc<PinnedDirectory>,
    directories: Vec<File>,
    name: String,
    absolute: Vec<u16>,
}
impl Location {
    fn new(root: &Root, path: &str) -> io::Result<Self> {
        root.check_write()?;
        let names = virtual_path(path)?;
        let (leaf, subdirs) = names.split_last().ok_or_else(invalid)?;
        let root = PinnedDirectory::select(root)?;
        let mut absolute = root.path.clone();
        let mut directories = Vec::new();
        for name in subdirs {
            let parent = directories.last().unwrap_or_else(|| root.selected());
            let next = relative(parent, name, FILE_TRAVERSE, SHARE_IN_PLACE)?;
            describe(&next, name)?.directory()?;
            if primary_link(&next)?.1 != *name {
                return Err(excluded());
            }
            directories.push(next);
            absolute.push('\\');
            absolute.push_str(name);
        }
        absolute.push('\\');
        absolute.push_str(leaf);
        Ok(Self {
            root,
            directories,
            name: (*leaf).to_owned(),
            absolute: absolute.encode_utf16().chain(Some(0)).collect(),
        })
    }
    fn parent(&self) -> &File {
        self.directories
            .last()
            .unwrap_or_else(|| self.root.selected())
    }
    fn entry(
        &self,
        rights: FILE_ACCESS_RIGHTS,
        share: FILE_SHARE_MODE,
    ) -> io::Result<(File, Entry)> {
        let file = relative(self.parent(), &self.name, rights, share)?;
        let entry = describe(&file, &self.name)?;
        self.verify_leaf(&file)?;
        Ok((file, entry))
    }
    fn verify_leaf(&self, file: &File) -> io::Result<u64> {
        let id = file_id(file)?;
        let parent = file_id(self.parent())?;
        let (actual_parent, primary) = primary_link(file)?;
        // This is a Windows volume-GUID path. Verify the resulting link even
        // when a local directory changes reparse metadata during path lookup.
        if id.0 != parent.0 || actual_parent != parent.1 || primary != self.name {
            return Err(excluded());
        }
        Ok(id.1)
    }
}

struct Transaction(File);
impl Transaction {
    fn new() -> io::Result<Self> {
        let h = unsafe {
            CreateTransaction(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                PCWSTR::null(),
            )
        }
        .map_err(win_error)?;
        Ok(Self(unsafe { File::from_raw_handle(h.0) }))
    }
    fn open(&self, path: &Location, disposition: FILE_CREATION_DISPOSITION) -> io::Result<File> {
        // OPEN_ALWAYS never truncates. Reject links, streams, alternate names
        // and a different parent *before* writing, including after creation.
        let h = unsafe {
            CreateFileTransactedW(
                PCWSTR(path.absolute.as_ptr()),
                (FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE).0,
                SHARE_ALL,
                None,
                disposition,
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_POSIX_SEMANTICS,
                None,
                handle(&self.0),
                None,
                None,
            )
        }
        .map_err(win_error)?;
        Ok(unsafe { File::from_raw_handle(h.0) })
    }
    fn commit(self, root: &Root) -> io::Result<()> {
        root.check_write()?;
        unsafe { CommitTransaction(handle(&self.0)) }.map_err(win_error)
    }
}
// Closing the last uncommitted KTM handle automatically rolls it back.

pub(in crate::ssh::folder_share) struct PendingWrite {
    path: Location,
    // Keep the original file record alive so a recycled NTFS file ID cannot
    // authorize data access after a delete or replace.
    original: File,
    id: u64,
    grant: Arc<AtomicBool>,
}
impl PendingWrite {
    pub(in crate::ssh::folder_share) fn len(&self) -> io::Result<u64> {
        check_active(&self.grant)?;
        self.path.verify_leaf(&self.original)?;
        Ok(describe(&self.original, &self.path.name)?.len)
    }
    pub(in crate::ssh::folder_share) fn read_at(
        &self,
        data: &mut [u8],
        offset: u64,
    ) -> io::Result<usize> {
        self.len()?;
        self.original.seek_read(data, offset)
    }
    fn modify(
        &mut self,
        root: &Root,
        change: impl FnOnce(&File, u64) -> io::Result<()>,
    ) -> io::Result<()> {
        if !Arc::ptr_eq(&self.grant, &root.revoked) {
            return Err(excluded());
        }
        root.check_write()?;
        let tx = Transaction::new()?;
        let file = tx.open(&self.path, OPEN_EXISTING)?;
        let info = describe(&file, &self.path.name)?;
        if info.is_dir || self.path.verify_leaf(&file)? != self.id {
            return Err(excluded());
        }
        change(&file, info.len)?;
        drop(file);
        tx.commit(root)
    }
    pub(in crate::ssh::folder_share) fn write_at(
        &mut self,
        root: &Root,
        bytes: &[u8],
        offset: u64,
    ) -> io::Result<()> {
        self.modify(root, |file, _| put(file, bytes, offset))
    }
    pub(in crate::ssh::folder_share) fn append(
        &mut self,
        root: &Root,
        bytes: &[u8],
    ) -> io::Result<()> {
        self.modify(root, |file, len| put(file, bytes, len))
    }
    pub(in crate::ssh::folder_share) fn set_len(
        &mut self,
        root: &Root,
        len: u64,
    ) -> io::Result<()> {
        check_length(len)?;
        self.modify(root, |file, _| file.set_len(len))
    }
}
fn check_length(len: u64) -> io::Result<()> {
    if len > i64::MAX as u64 {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "file offset exceeds the signed NTFS range",
        ));
    }
    Ok(())
}
fn put(file: &File, mut bytes: &[u8], mut offset: u64) -> io::Result<()> {
    check_length(offset.checked_add(bytes.len() as u64).ok_or_else(invalid)?)?;
    while !bytes.is_empty() {
        let n = file.seek_write(bytes, offset)?;
        if n == 0 {
            return Err(ErrorKind::WriteZero.into());
        }
        offset += n as u64;
        bytes = &bytes[n..];
    }
    Ok(())
}

impl Root {
    pub fn with_write_access(mut self, enabled: bool) -> Self {
        self.writable = enabled;
        if !enabled {
            self.write_location = None;
        }
        self
    }
    /// Keep the consented Windows location stable across the protected-path
    /// check and the entire writable export, including periods with no files open.
    pub(crate) fn pin_write_location(&mut self) -> io::Result<()> {
        if self.writable && self.write_location.is_none() {
            self.write_location = Some(PinnedDirectory::select(self)?);
        }
        Ok(())
    }
    pub fn is_writable(&self) -> bool {
        self.writable
    }
    pub(in crate::ssh) fn check_write_support(&self) -> io::Result<()> {
        if !self.writable {
            return Ok(());
        }
        let mut flags = 0;
        unsafe {
            GetVolumeInformationByHandleW(
                handle(&self.directory),
                None,
                None,
                None,
                Some(&mut flags),
                None,
            )
        }
        .map_err(win_error)?;
        if flags & windows::Win32::System::SystemServices::FILE_SUPPORTS_TRANSACTIONS == 0 {
            return Err(io::Error::new(
                ErrorKind::Unsupported,
                "Writable folder sharing requires NTFS transaction support on the selected volume",
            ));
        }
        Ok(())
    }
    fn check_write(&self) -> io::Result<()> {
        if !self.writable || !self.permits_request() {
            return Err(excluded());
        }
        Ok(())
    }
    pub(in crate::ssh::folder_share) fn lock_mutation(
        &self,
    ) -> io::Result<Option<crate::profiles::FolderMutationPermit>> {
        self.check_write()?;
        self.profile_permission
            .as_ref()
            .map(|gate| gate.lock_mutation().map_err(io::Error::other))
            .transpose()
    }
    pub(in crate::ssh::folder_share) fn open_write(
        &self,
        path: &str,
        create: bool,
        exclusive: bool,
        truncate: bool,
    ) -> io::Result<PendingWrite> {
        let path = Location::new(self, path)?;
        let tx = Transaction::new()?;
        let mode = if exclusive {
            if !create {
                return Err(invalid());
            }
            CREATE_NEW
        } else if create {
            OPEN_ALWAYS
        } else {
            OPEN_EXISTING
        };
        let file = tx.open(&path, mode)?;
        let info = describe(&file, &path.name)?;
        if info.is_dir {
            return Err(ErrorKind::IsADirectory.into());
        }
        let id = path.verify_leaf(&file)?;
        if truncate {
            file.set_len(0)?;
        }
        drop(file);
        tx.commit(self)?;
        let (original, _) = path.entry(FILE_READ_DATA, SHARE_ALL)?;
        if file_id(&original)?.1 != id {
            return Err(excluded());
        }
        Ok(PendingWrite {
            path,
            original,
            id,
            grant: self.revoked.clone(),
        })
    }

    pub(in crate::ssh::folder_share) fn mkdir(&self, path: &str) -> io::Result<()> {
        let path = Location::new(self, path)?;
        self.check_write()?;
        open_relative(
            path.parent(),
            &path.name,
            FILE_TRAVERSE,
            SHARE_IN_PLACE,
            FILE_CREATE,
            FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
        )?;
        Ok(())
    }
    pub(in crate::ssh::folder_share) fn remove(&self, path: &str) -> io::Result<()> {
        self.unlink(path, false)
    }
    pub(in crate::ssh::folder_share) fn rmdir(&self, path: &str) -> io::Result<()> {
        self.unlink(path, true)
    }
    fn unlink(&self, path: &str, directory: bool) -> io::Result<()> {
        let path = Location::new(self, path)?;
        let (file, entry) = path.entry(DELETE, SHARE_IN_PLACE)?;
        if entry.is_dir != directory {
            return Err(if directory {
                ErrorKind::NotADirectory
            } else {
                ErrorKind::IsADirectory
            }
            .into());
        }
        self.check_write()?;
        let info = FILE_DISPOSITION_INFO { DeleteFile: true };
        unsafe {
            SetFileInformationByHandle(
                handle(&file),
                FileDispositionInfo,
                (&raw const info).cast(),
                size_of_val(&info) as u32,
            )
        }
        .map_err(win_error)
    }
    pub(in crate::ssh::folder_share) fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        let source = Location::new(self, from)?;
        let destination = Location::new(self, to)?;
        let (file, _) = source.entry(DELETE, SHARE_IN_PLACE)?;
        let wide: Vec<_> = destination.name.encode_utf16().collect();
        let bytes = offset_of!(FILE_RENAME_INFORMATION, FileName) + wide.len() * 2;
        // A u64 allocation meets the native structure alignment on Windows.
        let mut buffer = vec![0u64; bytes.max(size_of::<FILE_RENAME_INFORMATION>()).div_ceil(8)];
        let raw = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
        unsafe {
            (*raw).RootDirectory = handle(destination.parent());
            (*raw).FileNameLength = (wide.len() * 2) as u32;
            std::ptr::copy_nonoverlapping(
                wide.as_ptr(),
                (&raw mut (*raw).FileName).cast::<u16>(),
                wide.len(),
            );
        }
        self.check_write()?;
        let mut ios = IO_STATUS_BLOCK::default();
        // Zero flags means fail if an entry already exists.
        nt_result(unsafe {
            NtSetInformationFile(
                handle(&file),
                &mut ios,
                raw.cast(),
                (buffer.len() * 8) as u32,
                FileRenameInformation,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetKernelObjectSecurity, PSECURITY_DESCRIPTOR,
    };

    fn dacl(path: &Path) -> io::Result<Vec<u8>> {
        let file = File::open(path)?;
        let mut needed = 0;
        let _ = unsafe {
            GetKernelObjectSecurity(
                handle(&file),
                DACL_SECURITY_INFORMATION.0,
                None,
                0,
                &mut needed,
            )
        };
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut descriptor = vec![0u8; needed as usize];
        unsafe {
            GetKernelObjectSecurity(
                handle(&file),
                DACL_SECURITY_INFORMATION.0,
                Some(PSECURITY_DESCRIPTOR(descriptor.as_mut_ptr().cast())),
                needed,
                &mut needed,
            )
        }
        .map_err(win_error)?;
        Ok(descriptor)
    }

    #[test]
    fn writes_are_committed_before_ack_and_preserve_other_streams() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let root = Root::open(dir.path())?.with_write_access(true);
        root.check_write_support()?;
        root.mkdir("assets")?;
        let path = dir.path().join("assets").join("tile.bin");
        let mut edit = root.open_write("assets/tile.bin", true, true, false)?;
        assert_eq!(fs::read(&path)?, b"");
        assert!(
            root.open_write("assets/tile.bin", true, true, false)
                .is_err()
        );
        edit.write_at(&root, b"hello", 0)?;
        assert_eq!(fs::read(&path)?, b"hello");
        edit.write_at(&root, b"X", 2)?;
        edit.append(&root, b"!")?;
        assert_eq!(fs::read(&path)?, b"heXlo!");
        edit.set_len(&root, 8)?;
        assert_eq!(fs::read(&path)?, b"heXlo!\0\0");
        edit.set_len(&root, 3)?;
        assert_eq!(fs::read(&path)?, b"heX");
        drop(edit);
        let stream = dir.path().join("assets").join("tile.bin:local-metadata");
        fs::write(&stream, b"local metadata")?;
        let security = dacl(&path)?;
        let original_id = file_id(&File::open(&path)?)?;
        let mut edit = root.open_write("/assets/tile.bin", false, false, true)?;
        assert!(fs::read(&path)?.is_empty());
        edit.write_at(&root, b"new", 0)?;
        assert_eq!(fs::read(stream)?, b"local metadata");
        assert_eq!(dacl(&path)?, security);
        assert_eq!(file_id(&File::open(&path)?)?, original_id);
        assert!(root.rmdir("assets").is_err());
        drop(edit);
        root.rename("assets/tile.bin", "tile.bin")?;
        fs::write(dir.path().join("other"), b"other")?;
        assert!(root.rename("tile.bin", "other").is_err());
        assert!(root.remove("assets").is_err());
        assert!(root.rmdir("tile.bin").is_err());
        root.rmdir("assets")?;
        root.remove("tile.bin")?;
        assert_eq!(fs::read(dir.path().join("other"))?, b"other");
        Ok(())
    }

    #[test]
    fn data_writes_require_a_single_live_original_link_and_the_original_grant() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let dir = temp.path().join("selected");
        fs::create_dir(&dir)?;
        let path = dir.join("live");
        fs::write(&path, b"old data")?;
        let root = Root::open(&dir)?.with_write_access(true);
        let mut edit = root.open_write("live", false, false, false)?;
        let outside = temp.path().join("external");
        fs::hard_link(&path, &outside)?;
        assert!(edit.write_at(&root, b"bad", 0).is_err());
        assert!(edit.set_len(&root, 0).is_err());
        assert!(root.open_write("live", false, false, true).is_err());
        assert!(root.remove("live").is_err());
        assert_eq!(fs::read(&outside)?, b"old data");
        fs::remove_file(&outside)?;
        fs::rename(&path, &outside)?;
        fs::write(&path, b"new identity")?;
        assert!(edit.write_at(&root, b"bad", 0).is_err());
        assert!(edit.read_at(&mut [0; 8], 0).is_err());
        assert!(edit.len().is_err());
        assert_eq!(fs::read(&outside)?, b"old data");
        assert_eq!(fs::read(&path)?, b"new identity");
        let mut fresh = root.open_write("live", false, false, false)?;
        let unrelated = Root::open(&dir)?.with_write_access(true);
        assert!(fresh.write_at(&unrelated, b"bad", 0).is_err());
        root.revoked.store(true, Ordering::Release);
        assert!(fresh.write_at(&root, b"bad", 0).is_err());
        assert_eq!(fs::read(&path)?, b"new identity");
        Ok(())
    }

    #[test]
    fn large_positional_write_and_append_preserve_prior_commits_on_failure() -> io::Result<()> {
        const OFFSET: u64 = 17 * 1024 * 1024;
        let dir = tempfile::tempdir()?;
        let root = Root::open(dir.path())?.with_write_access(true);
        let mut edit = root.open_write("large", true, true, false)?;
        edit.set_len(&root, OFFSET)?;
        edit.write_at(&root, b"kept", OFFSET)?;
        edit.append(&root, b"!")?;
        assert!(edit.write_at(&root, b"no", u64::MAX).is_err());
        assert!(edit.set_len(&root, (i64::MAX as u64) + 1).is_err());
        assert_eq!(edit.len()?, OFFSET + 5);
        let f = File::open(dir.path().join("large"))?;
        let mut tail = [0; 5];
        assert_eq!(f.seek_read(&mut tail, OFFSET)?, 5);
        assert_eq!(&tail, b"kept!");
        Ok(())
    }

    #[test]
    fn transactional_writer_excludes_new_hardlinks_and_rolls_back_before_publication()
    -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let dir = temp.path().join("selected");
        fs::create_dir(&dir)?;
        let path = dir.join("existing");
        fs::write(&path, b"original")?;
        let outside = temp.path().join("outside");
        let root = Root::open(&dir)?.with_write_access(true);
        let mut approved = root.open_write("existing", false, false, false)?;
        approved.write_at(&root, b"previous", 0)?;
        let location = Location::new(&root, "existing")?;
        let tx = Transaction::new()?;
        let file = tx.open(&location, OPEN_EXISTING)?;
        location.verify_leaf(&file)?;
        assert!(fs::hard_link(&path, &outside).is_err());
        put(&file, b"private", 0)?;
        assert_eq!(fs::read(&path)?, b"previous");
        drop(file);
        assert!(fs::hard_link(&path, &outside).is_err());
        root.revoked.store(true, Ordering::Release);
        assert!(tx.commit(&root).is_err());
        assert_eq!(fs::read(path)?, b"previous");
        assert!(!outside.exists());
        Ok(())
    }

    #[test]
    fn read_only_and_invalid_namespace_requests_leave_the_tree_untouched() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let ro = Root::open(temp.path())?;
        assert!(!ro.is_writable());
        ro.check_write_support()?;
        assert!(ro.open_write("x", true, true, false).is_err());
        assert!(ro.mkdir("d").is_err());
        let root = ro.with_write_access(true);
        for invalid in [
            ".",
            "/",
            "../outside",
            "alias:stream",
            "NUL.txt",
            "a/../new",
            "\\\\server\\share",
        ] {
            assert!(root.open_write(invalid, true, true, false).is_err());
            assert!(root.mkdir(invalid).is_err());
        }
        assert!(fs::read_dir(temp.path())?.next().is_none());
        Ok(())
    }

    #[test]
    fn writable_paths_pin_ancestors_and_refuse_junctions_for_each_mutation() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let selected = temp.path().join("selected");
        let outside = temp.path().join("outside");
        fs::create_dir(&selected)?;
        fs::create_dir(&outside)?;
        fs::write(outside.join("secret"), b"outside")?;
        fs::write(selected.join("local"), b"local")?;
        assert!(
            std::process::Command::new("cmd.exe")
                .current_dir(temp.path())
                .args(["/d", "/c", "mklink", "/j", r"selected\alias", "outside"])
                .output()?
                .status
                .success()
        );
        let root = Root::open(&selected)?.with_write_access(true);
        assert!(root.open_write("alias/secret", false, false, true).is_err());
        assert!(root.open_write("alias/new", true, false, false).is_err());
        assert!(root.mkdir("alias/new-dir").is_err());
        assert!(root.rename("local", "alias/renamed").is_err());
        assert!(root.remove("alias/secret").is_err());
        assert!(root.rmdir("alias").is_err());
        assert_eq!(fs::read(outside.join("secret"))?, b"outside");
        assert_eq!(fs::read_dir(&outside)?.count(), 1);
        root.mkdir("sub")?;
        let edit = root.open_write("sub/new", true, true, false)?;
        assert!(fs::rename(selected.join("sub"), outside.join("moved")).is_err());
        assert!(fs::rename(&selected, temp.path().join("renamed")).is_err());
        drop(edit);
        fs::rename(&selected, temp.path().join("renamed"))?;
        root.open_write("after", true, true, false)?;
        assert!(temp.path().join("renamed/after").is_file());
        Ok(())
    }

    #[test]
    fn idle_writable_mount_pins_the_selected_location_until_teardown() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let parent = temp.path().join("parent");
        let selected = parent.join("selected");
        fs::create_dir_all(&selected)?;
        let mut root = Root::open(&selected)?.with_write_access(true);
        root.pin_write_location()?;
        root.pin_write_location()?;
        assert!(fs::rename(&selected, parent.join("other")).is_err());
        assert!(fs::rename(&parent, temp.path().join("elsewhere")).is_err());
        let mut read_only = root.with_write_access(false);
        read_only.pin_write_location()?;
        fs::rename(&selected, parent.join("other"))?;
        assert!(read_only.list("/")?.is_empty());
        drop(read_only);
        fs::rename(&parent, temp.path().join("elsewhere"))?;
        Ok(())
    }
}
