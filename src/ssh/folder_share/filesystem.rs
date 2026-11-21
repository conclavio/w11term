//! A capability for one selected directory on a local NTFS volume.
//! After drive selection, walks open one name relative to a checked directory
//! handle without reparsing. TxF requires a volume path; the writer pins its
//! ancestors and checks the resulting primary link and parent before commit.
//! A renamed root continues to refer to the originally selected directory.
//! Read-only hard-link counts are checked on admission. Already-open files identify the
//! same object even if a local owner later changes its contents or link count.
use std::{
    fs::{File, OpenOptions},
    io::{self, ErrorKind},
    mem::{offset_of, size_of},
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use windows::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_LINK_ENTRY_INFORMATION, FILE_LINKS_INFORMATION, FILE_NAMES_INFORMATION, FILE_OPEN,
            FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT, FileHardLinkInformation,
            FileNamesInformation, NtCreateFile, NtQueryDirectoryFile, NtQueryInformationFile,
        },
    },
    Win32::{
        Foundation::{
            HANDLE, NTSTATUS, OBJ_DONT_REPARSE, RtlNtStatusToDosError,
            STATUS_IO_REPARSE_TAG_NOT_HANDLED, STATUS_NO_MORE_FILES, STATUS_NO_SUCH_FILE,
            STATUS_REPARSE_POINT_ENCOUNTERED, STATUS_STOPPED_ON_SYMLINK, STATUS_SUCCESS,
            UNICODE_STRING,
        },
        Storage::FileSystem::*,
        System::IO::IO_STATUS_BLOCK,
    },
    core::{PCWSTR, PWSTR},
};
mod write;
pub(super) use write::PendingWrite;

const MAX_ENTRIES: usize = 4096;
const MAX_COMPONENTS: usize = 64;
const MAX_PATH_BYTES: usize = 4096;
const OPEN_FLAGS: FILE_FLAGS_AND_ATTRIBUTES =
    FILE_FLAGS_AND_ATTRIBUTES(FILE_FLAG_OPEN_REPARSE_POINT.0 | FILE_FLAG_BACKUP_SEMANTICS.0);
const SHARE_ALL: FILE_SHARE_MODE =
    FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0);
const SHARE_IN_PLACE: FILE_SHARE_MODE = FILE_SHARE_MODE(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0);

pub struct Root {
    directory: File,
    writable: bool,
    write_location: Option<Arc<write::PinnedDirectory>>,
    cursor: std::sync::Mutex<()>,
    pub(super) revoked: Arc<AtomicBool>,
    profile_permission: Option<crate::profiles::FolderGrantGate>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) name: String,
    pub(super) is_dir: bool,
    pub(super) len: u64,
}

fn invalid() -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, "invalid folder-share path")
}

fn excluded() -> io::Error {
    io::Error::new(
        ErrorKind::PermissionDenied,
        "entry excluded from folder share",
    )
}

fn win_error(error: windows::core::Error) -> io::Error {
    // The Win32 bindings wrap GetLastError in HRESULT_FROM_WIN32.
    io::Error::from_raw_os_error(error.code().0 & 0xffff)
}

fn nt_result(status: NTSTATUS) -> io::Result<()> {
    match status {
        STATUS_SUCCESS => Ok(()),
        STATUS_REPARSE_POINT_ENCOUNTERED
        | STATUS_STOPPED_ON_SYMLINK
        | STATUS_IO_REPARSE_TAG_NOT_HANDLED => Err(excluded()),
        _ => Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        )),
    }
}

fn handle(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}

fn valid_component(name: &str) -> bool {
    if name.is_empty()
        || name.ends_with(['.', ' '])
        || name.encode_utf16().count() > 255
        || name
            .chars()
            .any(|c| c.is_control() || r#"\/:"<>|?*"#.contains(c))
    {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) && !stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|suffix| {
            matches!(
                suffix,
                "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}

fn parts<'a>(path: impl Iterator<Item = &'a str>) -> io::Result<Vec<&'a str>> {
    let mut result = Vec::new();
    for name in path {
        if result.len() == MAX_COMPONENTS || !valid_component(name) {
            return Err(invalid());
        }
        result.push(name);
    }
    Ok(result)
}

fn virtual_path(path: &str) -> io::Result<Vec<&str>> {
    if path.len() > MAX_PATH_BYTES {
        return Err(invalid());
    }
    if matches!(path, "/" | ".") {
        return Ok(Vec::new());
    }
    parts(path.strip_prefix('/').unwrap_or(path).split('/'))
}

fn drive_root(letter: u8) -> io::Result<File> {
    let drive = format!("{}:\\", char::from(letter));
    let wide: Vec<u16> = drive.encode_utf16().chain(Some(0)).collect();
    // DRIVE_REMOVABLE and DRIVE_FIXED. Remote drives have a distinct drive type.
    if !matches!(unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) }, 2 | 3) {
        return Err(excluded());
    }
    let directory = OpenOptions::new()
        .access_mode((FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE).0)
        .share_mode(SHARE_ALL.0)
        .custom_flags(OPEN_FLAGS.0)
        .open(drive)?;
    let mut fs_name = [0u16; 32];
    unsafe {
        GetVolumeInformationByHandleW(
            handle(&directory),
            None,
            None,
            None,
            None,
            Some(&mut fs_name),
        )
    }
    .map_err(win_error)?;
    if !fs_name[..5].iter().copied().eq("NTFS\0".encode_utf16()) {
        return Err(excluded());
    }
    // Reject SUBST paths to subdirectories and network/device aliases. The drive
    // handle itself must designate the root of a volume with a real volume GUID.
    let mut final_path = [0u16; 64];
    let n =
        unsafe { GetFinalPathNameByHandleW(handle(&directory), &mut final_path, VOLUME_NAME_GUID) }
            as usize;
    if n == 0 {
        return Err(io::Error::last_os_error());
    }
    let name = final_path
        .get(..n)
        .and_then(|s| String::from_utf16(s).ok())
        .ok_or_else(excluded)?;
    let guid = name
        .strip_prefix(r"\\?\Volume{")
        .and_then(|s| s.strip_suffix(r"}\"));
    if !guid.is_some_and(|g| g.len() == 36 && g.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
    {
        return Err(excluded());
    }
    describe(&directory, "/")?.directory()?;
    Ok(directory)
}

fn relative(
    parent: &File,
    name: &str,
    rights: FILE_ACCESS_RIGHTS,
    share: FILE_SHARE_MODE,
) -> io::Result<File> {
    open_relative(
        parent,
        name,
        rights,
        share,
        FILE_OPEN,
        FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
    )
}

fn open_relative(
    parent: &File,
    name: &str,
    rights: FILE_ACCESS_RIGHTS,
    share: FILE_SHARE_MODE,
    disposition: windows::Wdk::Storage::FileSystem::NTCREATEFILE_CREATE_DISPOSITION,
    options: windows::Wdk::Storage::FileSystem::NTCREATEFILE_CREATE_OPTIONS,
) -> io::Result<File> {
    // All callers validate the component, including entries returned by NTFS.
    let mut wide: Vec<u16> = name.encode_utf16().collect();
    let length = (wide.len() * 2) as u16;
    let unicode = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: PWSTR(wide.as_mut_ptr()),
    };
    let object = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: handle(parent),
        ObjectName: &unicode,
        Attributes: OBJ_DONT_REPARSE,
        ..Default::default()
    };
    let mut opened = HANDLE::default();
    let mut status = IO_STATUS_BLOCK::default();
    // The parent, name, IOSB and out handle remain live through NtCreateFile.
    nt_result(unsafe {
        NtCreateFile(
            &mut opened,
            rights | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            &object,
            &mut status,
            None,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            share,
            disposition,
            options,
            None,
            0,
        )
    })?;
    Ok(unsafe { File::from_raw_handle(opened.0) })
}

fn describe(file: &File, name: &str) -> io::Result<Entry> {
    match unsafe { GetFileType(handle(file)) } {
        FILE_TYPE_DISK => (),
        FILE_TYPE_UNKNOWN => return Err(io::Error::other("could not identify Windows file type")),
        _ => return Err(excluded()),
    }
    let mut data = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle(file), &mut data) }.map_err(win_error)?;
    let is_dir = data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
    if data.dwFileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT.0 | FILE_ATTRIBUTE_DEVICE.0) != 0
        || (!is_dir && data.nNumberOfLinks != 1)
    {
        return Err(excluded());
    }
    if name != "/" && name.encode_utf16().count() <= 12 {
        primary_link(file)?;
    }
    let len = if is_dir {
        0
    } else {
        (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow)
    };
    Ok(Entry {
        name: name.to_owned(),
        is_dir,
        len,
    })
}

// A DOS alias can have an arbitrary 8.3 spelling. Query the primary basename
// from the opened object itself so such an alias cannot expose a name that
// directory enumeration deliberately omits (e.g. a trailing period).
fn primary_link(file: &File) -> io::Result<(u64, String)> {
    #[repr(C, align(8))]
    struct SingleLink {
        needed: u32,
        count: u32,
        next: u32,
        padding: u32,
        parent_id: i64,
        chars: u32,
        name: [u16; 255],
    }
    const {
        assert!(
            offset_of!(SingleLink, name)
                == offset_of!(FILE_LINKS_INFORMATION, Entry)
                    + offset_of!(FILE_LINK_ENTRY_INFORMATION, FileName)
        );
    }
    let mut link = SingleLink {
        needed: 0,
        count: 0,
        next: 0,
        padding: 0,
        parent_id: 0,
        chars: 0,
        name: [0; 255],
    };
    let mut ios = IO_STATUS_BLOCK::default();
    let status = unsafe {
        NtQueryInformationFile(
            handle(file),
            &mut ios,
            (&raw mut link).cast(),
            size_of::<SingleLink>() as u32,
            FileHardLinkInformation,
        )
    };
    nt_result(status)?;
    let length = link.chars as usize; // This information class counts UTF-16 code units.
    if link.count != 1 || link.next != 0 || length > link.name.len() {
        return Err(excluded());
    }
    if ios.Information < offset_of!(SingleLink, name) + length * 2 {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "invalid NTFS link record",
        ));
    }
    let primary = String::from_utf16(&link.name[..length]).map_err(|_| excluded())?;
    if !valid_component(&primary) {
        return Err(excluded());
    }
    Ok((link.parent_id as u64, primary))
}

impl Entry {
    fn directory(&self) -> io::Result<()> {
        if self.is_dir {
            Ok(())
        } else {
            Err(ErrorKind::NotADirectory.into())
        }
    }
}

struct Opened {
    file: File,
    entry: Entry,
    // Hold intermediate directories without FILE_SHARE_DELETE through the whole
    // operation. A concurrent ordinary rename cannot relocate a subtree mid-walk.
    _parents: Vec<File>,
}

fn walk(
    root: &File,
    names: &[&str],
    final_rights: FILE_ACCESS_RIGHTS,
    final_share: FILE_SHARE_MODE,
    revoked: &AtomicBool,
) -> io::Result<Opened> {
    let mut parents = Vec::new();
    let (last, ancestors) = names.split_last().ok_or_else(invalid)?;
    for name in ancestors {
        check_active(revoked)?;
        let child = relative(
            parents.last().unwrap_or(root),
            name,
            FILE_TRAVERSE,
            SHARE_IN_PLACE,
        )?;
        describe(&child, name)?.directory()?;
        parents.push(child);
    }
    check_active(revoked)?;
    let file = relative(
        parents.last().unwrap_or(root),
        last,
        final_rights,
        final_share,
    )?;
    let entry = describe(&file, last)?;
    Ok(Opened {
        file,
        entry,
        _parents: parents,
    })
}

fn check_active(revoked: &AtomicBool) -> io::Result<()> {
    if revoked.load(Ordering::Acquire) {
        return Err(io::Error::new(
            ErrorKind::Interrupted,
            "folder share revoked",
        ));
    }
    Ok(())
}

impl Root {
    pub(crate) fn directory_handle(&self) -> &File {
        &self.directory
    }

    /// Select an absolute drive-letter directory below the root of local NTFS.
    /// Whole drives, UNC/device paths, and reparse points anywhere in the
    /// selected path are refused.
    pub fn open(path: &Path) -> io::Result<Self> {
        let path = path.to_str().ok_or_else(invalid)?;
        let bytes = path.as_bytes();
        if path.len() > MAX_PATH_BYTES
            || !matches!(bytes, [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic())
        {
            return Err(invalid());
        }
        let tail = &path[3..];
        let tail = tail.strip_suffix(['\\', '/']).unwrap_or(tail);
        let names = parts(tail.split(['\\', '/']))?;
        let drive = drive_root(bytes[0])?;
        let revoked = Arc::new(AtomicBool::new(false));
        let selected = walk(
            &drive,
            &names,
            FILE_TRAVERSE | FILE_LIST_DIRECTORY,
            SHARE_ALL,
            &revoked,
        )?;
        selected.entry.directory()?;
        Ok(Self {
            directory: selected.file,
            writable: false,
            write_location: None,
            cursor: std::sync::Mutex::new(()),
            revoked,
            profile_permission: None,
        })
    }

    pub(crate) fn with_profile_permission(
        mut self,
        gate: crate::profiles::FolderGrantGate,
    ) -> Self {
        self.profile_permission = Some(gate);
        self
    }

    // Runs on the blocking SFTP worker. A profile editor in another process can
    // retire this token while Linux holds a file handle or a directory snapshot.
    pub(super) fn permits_request(&self) -> bool {
        if self.revoked.load(Ordering::Acquire) {
            return false;
        }
        if let Some(permission) = &self.profile_permission
            && !matches!(permission.revoked(), Ok(false))
        {
            self.revoked.store(true, Ordering::Release);
            return false;
        }
        true
    }

    /// Identify the selected directory itself across rename or reopen on the same volume.
    pub fn identity(&self) -> io::Result<String> {
        let mut id = FILE_ID_INFO::default();
        unsafe {
            GetFileInformationByHandleEx(
                handle(&self.directory),
                FileIdInfo,
                (&raw mut id).cast(),
                size_of::<FILE_ID_INFO>() as u32,
            )
        }
        .map_err(win_error)?;
        Ok(format!(
            "ntfs-v1:{:016x}:{:032x}",
            id.VolumeSerialNumber,
            u128::from_be_bytes(id.FileId.Identifier)
        ))
    }

    pub(super) fn metadata(&self, path: &str) -> io::Result<Entry> {
        check_active(&self.revoked)?;
        let names = virtual_path(path)?;
        let root = describe(&self.directory, "/")?;
        root.directory()?;
        if names.is_empty() {
            return Ok(root);
        }
        Ok(walk(
            &self.directory,
            &names,
            FILE_READ_ATTRIBUTES,
            SHARE_ALL,
            &self.revoked,
        )?
        .entry)
    }

    pub(super) fn file(&self, path: &str) -> io::Result<File> {
        check_active(&self.revoked)?;
        let names = virtual_path(path)?;
        describe(&self.directory, "/")?.directory()?;
        if names.is_empty() {
            return Err(ErrorKind::IsADirectory.into());
        }
        let opened = walk(
            &self.directory,
            &names,
            FILE_READ_DATA,
            SHARE_ALL,
            &self.revoked,
        )?;
        if opened.entry.is_dir {
            return Err(ErrorKind::IsADirectory.into());
        }
        Ok(opened.file)
    }

    /// Return only admissible names. A directory exceeding the scan cap or
    /// failing a filesystem query returns an error rather than a partial list.
    pub(super) fn list(&self, path: &str) -> io::Result<Vec<Entry>> {
        check_active(&self.revoked)?;
        let names = virtual_path(path)?;
        describe(&self.directory, "/")?.directory()?;
        if names.is_empty() {
            // Restarting the pinned handle cursor is independent of pathname changes.
            let _cursor = self
                .cursor
                .lock()
                .map_err(|_| io::Error::other("folder listing interrupted"))?;
            return entries(&self.directory, MAX_ENTRIES, &self.revoked);
        }
        let dir = walk(
            &self.directory,
            &names,
            FILE_LIST_DIRECTORY | FILE_TRAVERSE,
            SHARE_IN_PLACE,
            &self.revoked,
        )?;
        dir.entry.directory()?;
        entries(&dir.file, MAX_ENTRIES, &self.revoked)
    }
}

fn entries(directory: &File, limit: usize, revoked: &AtomicBool) -> io::Result<Vec<Entry>> {
    // FILE_NAMES_INFORMATION followed by the full NTFS component. NtQuery returns
    // one record at a time, so neither kernel nor peer offsets become pointers.
    #[repr(C, align(8))]
    struct Record {
        next: u32,
        index: u32,
        length: u32,
        name: [u16; 256],
    }
    const {
        assert!(offset_of!(Record, name) == offset_of!(FILE_NAMES_INFORMATION, FileName));
    }
    let mut record = Record {
        next: 0,
        index: 0,
        length: 0,
        name: [0; 256],
    };
    let mut result = Vec::new();
    let mut scanned = 0;
    let mut first = true;
    loop {
        check_active(revoked)?;
        let mut ios = IO_STATUS_BLOCK::default();
        let status = unsafe {
            NtQueryDirectoryFile(
                handle(directory),
                None,
                None,
                None,
                &mut ios,
                (&raw mut record).cast(),
                size_of::<Record>() as u32,
                FileNamesInformation,
                true,
                None,
                first,
            )
        };
        if status == STATUS_NO_MORE_FILES || (first && status == STATUS_NO_SUCH_FILE) {
            break;
        }
        nt_result(status)?;
        first = false;
        let bytes = record.length as usize;
        if record.next != 0
            || !bytes.is_multiple_of(2)
            || bytes > size_of_val(&record.name)
            || ios.Information < offset_of!(Record, name) + bytes
        {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "invalid NTFS directory record",
            ));
        }
        let units = &record.name[..bytes / 2];
        if matches!(units, [46] | [46, 46]) {
            continue;
        }
        scanned += 1;
        if scanned > limit {
            return Err(io::Error::other(
                "folder share directory exceeds entry limit",
            ));
        }
        let Ok(name) = String::from_utf16(units) else {
            continue;
        };
        if !valid_component(&name) {
            continue;
        }
        match relative(directory, &name, FILE_READ_ATTRIBUTES, SHARE_ALL)
            .and_then(|file| describe(&file, &name))
        {
            Ok(entry) => result.push(entry),
            Err(e) if e.kind() == ErrorKind::PermissionDenied && e.raw_os_error().is_none() => {
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    result.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        io::{Read, Write},
        os::windows::fs::symlink_file,
        process::Command,
    };

    fn read(root: &Root, path: &str) -> io::Result<Vec<u8>> {
        let mut result = Vec::new();
        root.file(path)?.read_to_end(&mut result)?;
        Ok(result)
    }

    #[test]
    fn remote_names_are_single_normal_ntfs_components() {
        for path in [
            "",
            "//",
            "//server/file",
            "C:/x",
            "../x",
            "a/../b",
            "a/./b",
            "a//b",
            "a/",
            r"a\b",
            "file:stream",
            "NUL",
            "con .log",
            "AUX.txt",
            "LPT³",
            "COM¹.txt",
            "CONOUT$",
            "a.",
            " a ",
            "bad\nname",
            "a*b",
            "a?b",
            r"\\.\pipe\foo",
            "\0",
        ] {
            assert_eq!(
                virtual_path(path).unwrap_err().kind(),
                ErrorKind::InvalidInput,
                "{path:?}"
            );
        }
        assert!(virtual_path(&"😀".repeat(128)).is_err());
        assert!(virtual_path(&["a"; MAX_COMPONENTS + 1].join("/")).is_err());
        assert_eq!(virtual_path("/データ/𐐀.txt").unwrap(), ["データ", "𐐀.txt"]);
        assert!(virtual_path("/").unwrap().is_empty());
        assert!(virtual_path(".").unwrap().is_empty());
        for selection in [
            "C:",
            r"C:\",
            r"C:\.\",
            r"C:\folder\..",
            r"\\server\share",
            r"\\?\C:\folder",
            r"\\.\PhysicalDrive0",
            "folder",
        ] {
            assert!(
                matches!(Root::open(Path::new(selection)), Err(e) if e.kind()==ErrorKind::InvalidInput),
                "{selection:?}"
            );
        }
    }

    #[test]
    fn local_ntfs_nested_unicode_and_bounded_directory_snapshots() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let nested = temp.path().join("文書");
        fs::create_dir(&nested)?;
        fs::write(nested.join("café 🔑.bin"), [0, 255, 129])?;
        fs::write(temp.path().join("other.txt"), "x")?;
        let root = Root::open(temp.path())?;
        assert_eq!(
            root.metadata("文書/café 🔑.bin")?,
            Entry {
                name: "café 🔑.bin".into(),
                is_dir: false,
                len: 3
            }
        );
        assert_eq!(read(&root, "/文書/café 🔑.bin")?, [0, 255, 129]);
        let mut read_only = root.file("/文書/café 🔑.bin")?;
        assert_eq!(
            read_only.write(b"x").unwrap_err().kind(),
            ErrorKind::PermissionDenied
        );
        assert_eq!(root.list("/文書")?, [root.metadata("文書/café 🔑.bin")?]);
        let listing = root.list("/")?;
        assert_eq!(
            listing,
            [root.metadata("other.txt")?, root.metadata("文書")?]
        );
        assert_eq!(root.list(".")?, listing);
        assert_eq!(
            entries(&root.directory, 1, &root.revoked)
                .unwrap_err()
                .kind(),
            ErrorKind::Other
        );
        assert_eq!(
            root.file("文書").unwrap_err().kind(),
            ErrorKind::IsADirectory
        );
        assert_eq!(root.file("absent").unwrap_err().kind(), ErrorKind::NotFound);
        Ok(())
    }

    #[test]
    fn root_and_open_file_identity_survive_path_replacement() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let selected = temp.path().join("selected");
        let original = temp.path().join("original");
        fs::create_dir(&selected)?;
        fs::write(selected.join("file"), b"selected bytes")?;
        let root = Root::open(&selected)?;
        let identity = root.identity()?;
        fs::rename(&selected, &original)?;
        fs::create_dir(&selected)?;
        assert_eq!(Root::open(&original)?.identity()?, identity);
        assert_ne!(Root::open(&selected)?.identity()?, identity);
        fs::write(selected.join("redirection"), b"other directory")?;
        assert_eq!(root.list("/")?, [root.metadata("/file")?]);
        let mut admitted = root.file("file")?;
        fs::rename(original.join("file"), original.join("moved"))?;
        fs::write(original.join("file"), b"new file")?;
        assert_eq!(read(&root, "file")?, b"new file");
        let mut from_handle = Vec::new();
        admitted.read_to_end(&mut from_handle)?;
        assert_eq!(from_handle, b"selected bytes");
        assert_eq!(
            root.file("redirection").unwrap_err().kind(),
            ErrorKind::NotFound
        );
        Ok(())
    }

    #[test]
    fn simultaneous_listings_each_restart_the_pinned_root_cursor() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        for i in 0..12 {
            fs::write(temp.path().join(format!("fixture{i}")), [])?;
        }
        let root = Root::open(temp.path())?;
        let expected = root.list("/")?;
        assert_eq!(expected.len(), 12);
        std::thread::scope(|scope| {
            let other = scope.spawn(|| root.list("."));
            assert_eq!(root.list("/")?, expected);
            assert_eq!(other.join().unwrap()?, expected);
            Ok(())
        })
    }

    #[test]
    fn ntfs_short_alias_cannot_reveal_an_omitted_long_name() -> io::Result<()> {
        use std::{
            ffi::OsString,
            os::windows::ffi::{OsStrExt, OsStringExt},
            path::PathBuf,
        };
        fn short_name(path: &Path) -> io::Result<String> {
            let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let mut result = vec![0u16; 32768];
            let len =
                unsafe { GetShortPathNameW(PCWSTR(wide.as_ptr()), Some(&mut result)) } as usize;
            if len == 0 {
                return Err(io::Error::last_os_error());
            }
            let path = PathBuf::from(OsString::from_wide(&result[..len]));
            Ok(path.file_name().unwrap().to_str().unwrap().to_owned())
        }
        let temp = tempfile::tempdir()?;
        let root = Root::open(temp.path())?;
        // The extended prefix is local test setup for an actual NTFS name that
        // normal Win32 path processing would have trimmed.
        let directory = fs::canonicalize(temp.path())?;
        let hidden = directory.join("private long filename.");
        let hidden_dir = directory.join("private directory.");
        let ordinary = directory.join("ordinary long filename.txt");
        fs::write(&hidden, b"hidden")?;
        fs::create_dir(&hidden_dir)?;
        fs::write(hidden_dir.join("entry"), b"hidden")?;
        fs::write(&ordinary, b"public")?;
        fs::write(directory.join("README.md"), b"short primary name")?;
        assert_eq!(read(&root, "README.md")?, b"short primary name");
        assert_eq!(read(&root, &short_name(&ordinary)?)?, b"public");
        let alias = short_name(&hidden)?;
        if valid_component(&alias) {
            assert_eq!(
                root.file(&alias).unwrap_err().kind(),
                ErrorKind::PermissionDenied
            );
            assert_eq!(
                root.metadata(&alias).unwrap_err().kind(),
                ErrorKind::PermissionDenied
            );
            eprintln!("verified the volume's actual 8.3 alias for the omitted entry");
        } else {
            // 8.3 generation is disabled on some NTFS volumes. In that case the
            // OS returns the long name directly, which the virtual parser rejects.
            assert_eq!(
                root.file(&alias).unwrap_err().kind(),
                ErrorKind::InvalidInput
            );
        }
        let parent_alias = short_name(&hidden_dir)?;
        assert!(root.file(&format!("{parent_alias}/entry")).is_err());
        assert!(root.list(&parent_alias).is_err());
        assert_eq!(root.list("/")?.len(), 2);
        fs::remove_file(hidden)?;
        fs::remove_file(hidden_dir.join("entry"))?;
        fs::remove_dir(hidden_dir)?;
        Ok(())
    }

    #[test]
    fn ntfs_junctions_and_multiply_linked_files_are_excluded() -> io::Result<()> {
        let temp = tempfile::tempdir()?;
        let selected = temp.path().join("selected");
        let sibling = temp.path().join("sibling");
        fs::create_dir(&selected)?;
        fs::create_dir(&sibling)?;
        fs::create_dir(sibling.join("child"))?;
        fs::write(sibling.join("outside"), b"unshared")?;
        fs::hard_link(sibling.join("outside"), selected.join("alias"))?;
        // Junctions do not require symlink privilege or developer mode.
        let junction = Command::new("cmd.exe")
            .current_dir(temp.path())
            .args(["/d", "/c", "mklink", "/j", r"selected\junction", "sibling"])
            .output()?;
        assert!(
            junction.status.success(),
            "{}",
            String::from_utf8_lossy(&junction.stderr)
        );
        let root = Root::open(&selected)?;
        for path in ["alias", "junction", "junction/outside"] {
            assert_eq!(
                root.metadata(path).unwrap_err().kind(),
                ErrorKind::PermissionDenied,
                "{path}"
            );
            assert_eq!(
                root.file(path).unwrap_err().kind(),
                ErrorKind::PermissionDenied,
                "{path}"
            );
        }
        for path in [selected.join("junction"), selected.join("junction/child")] {
            assert!(matches!(Root::open(&path),Err(e) if e.kind()==ErrorKind::PermissionDenied));
        }
        match symlink_file(sibling.join("outside"), selected.join("symlink")) {
            Ok(()) => assert_eq!(
                root.file("symlink").unwrap_err().kind(),
                ErrorKind::PermissionDenied
            ),
            Err(e) if e.raw_os_error() == Some(1314) => {} // Mandatory junction assertions cover this protection on all hosts.
            Err(e) => return Err(e),
        }
        assert!(root.list(".")?.is_empty());
        // Removing the out-of-tree hard link leaves a genuinely single-linked local file.
        fs::remove_file(sibling.join("outside"))?;
        assert_eq!(read(&root, "alias")?, b"unshared");
        Ok(())
    }
}
