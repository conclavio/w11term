//! Directory enumeration and child opens anchored to a Windows directory handle.
use anyhow::{Result, bail};
use std::{
    fs::File,
    io,
    mem::{offset_of, size_of},
    os::windows::io::{AsRawHandle, FromRawHandle},
};
use windows::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_DIRECTORY_INFORMATION, FILE_NAMES_INFORMATION, FileDirectoryInformation,
            FileNamesInformation, NtCreateFile, NtQueryDirectoryFile,
        },
    },
    Win32::{
        Foundation::{
            HANDLE, NTSTATUS, OBJ_DONT_REPARSE, RtlNtStatusToDosError, STATUS_NO_MORE_FILES,
            STATUS_NO_SUCH_FILE, STATUS_SUCCESS, UNICODE_STRING,
        },
        Storage::FileSystem::*,
        System::IO::IO_STATUS_BLOCK,
    },
    core::PWSTR,
};

pub(crate) fn open_relative(
    parent: &File,
    name: &str,
    rights: FILE_ACCESS_RIGHTS,
    share: FILE_SHARE_MODE,
    disposition: windows::Wdk::Storage::FileSystem::NTCREATEFILE_CREATE_DISPOSITION,
    options: windows::Wdk::Storage::FileSystem::NTCREATEFILE_CREATE_OPTIONS,
) -> Result<File, NTSTATUS> {
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
        RootDirectory: HANDLE(parent.as_raw_handle()),
        ObjectName: &unicode,
        Attributes: OBJ_DONT_REPARSE,
        ..Default::default()
    };
    let mut opened = HANDLE::default();
    let mut status = IO_STATUS_BLOCK::default();
    // The parent, name, IOSB and out handle remain live through NtCreateFile.
    let result = unsafe {
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
    };
    if result != STATUS_SUCCESS {
        return Err(result);
    }
    Ok(unsafe { File::from_raw_handle(opened.0) })
}

pub(crate) fn names(dir: &File, mut visit: impl FnMut(&[u16]) -> Result<()>) -> Result<()> {
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

pub(crate) struct DirectoryEntry<'a> {
    pub name: &'a [u16],
    pub attributes: FILE_FLAGS_AND_ATTRIBUTES,
    pub size: Option<u64>,
    pub last_write_time: i64,
}

pub(crate) fn entries(
    dir: &File,
    mut visit: impl FnMut(DirectoryEntry<'_>) -> Result<()>,
) -> Result<()> {
    #[repr(C, align(8))]
    #[derive(Default)]
    struct RecordHeader {
        next: u32,
        index: u32,
        created: i64,
        accessed: i64,
        written: i64,
        changed: i64,
        end_of_file: i64,
        allocated: i64,
        attributes: u32,
        name_bytes: u32,
    }
    #[repr(C, align(8))]
    struct Record {
        header: RecordHeader,
        name: [u16; 256],
    }
    const {
        assert!(offset_of!(Record, name) == offset_of!(FILE_DIRECTORY_INFORMATION, FileName));
        assert!(
            offset_of!(RecordHeader, written)
                == offset_of!(FILE_DIRECTORY_INFORMATION, LastWriteTime)
        );
        assert!(
            offset_of!(RecordHeader, end_of_file)
                == offset_of!(FILE_DIRECTORY_INFORMATION, EndOfFile)
        );
        assert!(
            offset_of!(RecordHeader, attributes)
                == offset_of!(FILE_DIRECTORY_INFORMATION, FileAttributes)
        );
        assert!(
            offset_of!(RecordHeader, name_bytes)
                == offset_of!(FILE_DIRECTORY_INFORMATION, FileNameLength)
        );
    }
    let mut record = Record {
        header: RecordHeader::default(),
        name: [0; 256],
    };
    let mut first = true;
    loop {
        let mut ios = IO_STATUS_BLOCK::default();
        // Return one record at a time. The borrowed name is used before the buffer is reused.
        let status = unsafe {
            NtQueryDirectoryFile(
                HANDLE(dir.as_raw_handle()),
                None,
                None,
                None,
                &mut ios,
                (&raw mut record).cast(),
                size_of::<Record>() as u32,
                FileDirectoryInformation,
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
        let bytes = record.header.name_bytes as usize;
        if record.header.next != 0
            || bytes == 0
            || !bytes.is_multiple_of(2)
            || bytes > size_of_val(&record.name)
            || ios.Information < offset_of!(Record, name) + bytes
        {
            bail!("invalid local directory record");
        }
        let name = &record.name[..bytes / 2];
        if !matches!(name, [46] | [46, 46]) {
            visit(DirectoryEntry {
                name,
                attributes: FILE_FLAGS_AND_ATTRIBUTES(record.header.attributes),
                size: u64::try_from(record.header.end_of_file).ok(),
                last_write_time: record.header.written,
            })?;
        }
    }
}
