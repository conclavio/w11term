//! Windows file operations for a local config preview. Open the drive once per
//! lookup, then let NTFS/Windows resolve the entire relative path with reparsing
//! disabled. Directory globs enumerate that same checked handle.
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::{File, OpenOptions},
    io,
    mem::{offset_of, size_of},
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle},
    },
    path::Path,
};
use windows::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_NAMES_INFORMATION, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
            FILE_SYNCHRONOUS_IO_NONALERT, FileNamesInformation, NtCreateFile, NtQueryDirectoryFile,
        },
    },
    Win32::{
        Foundation::{
            HANDLE, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, RtlNtStatusToDosError,
            STATUS_NO_MORE_FILES, STATUS_NO_SUCH_FILE, STATUS_SUCCESS, UNICODE_STRING,
        },
        Storage::FileSystem::*,
        System::IO::IO_STATUS_BLOCK,
    },
    core::{PCWSTR, PWSTR},
};

pub(super) fn open(path: &Path, directory: bool) -> Result<Option<File>> {
    let text = path.to_str().context("non-Unicode config path")?;
    // Include patterns must already have been expanded to a drive-letter path.
    ensure!(
        matches!(text.as_bytes(),[letter,b':',b'\\',..] if letter.is_ascii_alphabetic()),
        "expected an absolute local config path"
    );
    let drive = &text[..3];
    let wide: Vec<u16> = drive.encode_utf16().chain(Some(0)).collect();
    ensure!(
        matches!(unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) }, 2 | 3),
        "config requires a local fixed or removable drive"
    );
    let tail = &text[3..];
    let mut access = FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
    if tail.is_empty() && directory {
        access |= FILE_LIST_DIRECTORY;
    }
    let root = OpenOptions::new()
        .access_mode(access.0)
        .custom_flags((FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS).0)
        .open(drive)?;
    let raw = HANDLE(root.as_raw_handle());
    // Network redirectors cannot identify a local volume GUID. In particular,
    // do not trust the original drive-letter mapping after opening its handle.
    let mut volume = [0u16; 64];
    let length = unsafe { GetFinalPathNameByHandleW(raw, &mut volume, VOLUME_NAME_GUID) } as usize;
    let volume = volume
        .get(..length)
        .and_then(|s| String::from_utf16(s).ok());
    let guid = volume
        .as_deref()
        .and_then(|v| v.strip_prefix(r"\\?\Volume{"))
        .and_then(|v| v.strip_suffix(r"}\"));
    ensure!(guid.is_some_and(|g|g.len()==36 && g.bytes().all(|b| b.is_ascii_hexdigit() || b==b'-')),
        "config requires a local volume root");
    let file = if tail.is_empty() {
        root
    } else {
        let mut units = std::ffi::OsStr::new(tail).encode_wide().collect::<Vec<_>>();
        let bytes = u16::try_from(units.len() * 2).context("config path too long")?;
        let name = UNICODE_STRING {
            Length: bytes,
            MaximumLength: bytes,
            Buffer: PWSTR(units.as_mut_ptr()),
        };
        let attrs = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: raw,
            ObjectName: &name,
            Attributes: OBJ_DONT_REPARSE | OBJ_CASE_INSENSITIVE,
            ..Default::default()
        };
        let mut file = HANDLE::default();
        let mut ios = IO_STATUS_BLOCK::default();
        let status = unsafe {
            NtCreateFile(
                &mut file,
                FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                &attrs,
                &mut ios,
                None,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_OPEN,
                FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
                None,
                0,
            )
        };
        if status != STATUS_SUCCESS {
            let e = io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32);
            if e.kind() == io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(e.into());
        }
        unsafe { File::from_raw_handle(file.0) }
    };
    let meta = file.metadata()?;
    ensure!(
        unsafe { GetFileType(HANDLE(file.as_raw_handle())) } == FILE_TYPE_DISK
            && meta.file_attributes() & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DEVICE).0
                == 0,
        "expected an ordinary local file or directory, not a reparse point"
    );
    ensure!(
        if directory {
            meta.is_dir()
        } else {
            meta.is_file()
        },
        "unexpected config path type"
    );
    Ok(Some(file))
}

pub(super) fn file_id(file: &File) -> Result<(u32, u64)> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }?;
    Ok((
        info.dwVolumeSerialNumber,
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    ))
}

pub(super) fn names(dir: &File, mut visit: impl FnMut(&[u16]) -> Result<()>) -> Result<()> {
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
    let mut first = true;
    loop {
        let mut ios = IO_STATUS_BLOCK::default();
        let status = unsafe {
            NtQueryDirectoryFile(
                HANDLE(dir.as_raw_handle()),
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
            return Ok(());
        }
        if status != STATUS_SUCCESS {
            return Err(io::Error::from_raw_os_error(
                unsafe { RtlNtStatusToDosError(status) } as i32
            )
            .into());
        }
        first = false;
        let bytes = record.length as usize;
        if record.next != 0
            || !bytes.is_multiple_of(2)
            || bytes > size_of_val(&record.name)
            || ios.Information < offset_of!(Record, name) + bytes
        {
            bail!("invalid local directory record");
        }
        let name = &record.name[..bytes / 2];
        if !matches!(name, [46] | [46, 46]) {
            visit(name)?;
        }
    }
}
