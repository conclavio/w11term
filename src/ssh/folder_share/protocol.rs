//! SFTP v3 request bodies on an already negotiated passive SSHFS channel. The transport
//! feeds the decoder SSH stdout after consuming the frame length; encoding a reply with
//! `Bytes::try_from(packet)` includes that length. SSH stderr is diagnostic text.
use super::filesystem::{Entry, Root};
use russh_sftp::protocol::*;
use std::{collections::HashMap, fs, io, os::windows::fs::FileExt, vec::IntoIter};

const MAX_HANDLES: usize = 32;
const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
const MAX_READ: usize = 32 * 1024;
// At most 255 UTF-16 units per Windows component. Even with the v3 longname
// repeating the UTF-8 name, 16 entries fit in a 32 KiB NAME packet.
const PAGE_ENTRIES: usize = 16;

type Reply = Result<Packet, StatusCode>;

/// Check the original v3 body (type, request id, fields; no outer frame length)
/// before russh-sftp replaces invalid UTF-8 with U+FFFD. Only the five operations
/// that can look up a local pathname need this check. Binary WRITE data is opaque.
pub(in crate::ssh) fn valid_read_path_encoding(body: &[u8]) -> bool {
    let Some(&kind) = body.first() else {
        return false;
    };
    // SSH_FXP_OPEN, LSTAT, OPENDIR, REALPATH and STAT start with a path string.
    if !matches!(kind, 3 | 7 | 11 | 16 | 17) {
        return true;
    }
    let Some(length) = body.get(5..9) else {
        return false;
    };
    let length = u32::from_be_bytes(length.try_into().unwrap()) as usize;
    body[9..]
        .get(..length)
        .is_some_and(|path| str::from_utf8(path).is_ok())
}

enum OpenHandle {
    File(fs::File),
    Directory {
        entries: IntoIter<Entry>,
        allocated: usize,
    },
}

pub(in crate::ssh) struct Export {
    root: Root,
    handles: HashMap<String, OpenHandle>,
    next_handle: u64,
}

impl Export {
    pub(in crate::ssh) fn new(root: Root) -> Self {
        Self {
            root,
            handles: HashMap::new(),
            next_handle: 1,
        }
    }

    pub(in crate::ssh) fn reply(&mut self, packet: Packet) -> Packet {
        let id = packet.get_request_id();
        if self.root.permits_request() {
            let reply = self.request(id, packet);
            if self.root.permits_request() {
                return reply.unwrap_or_else(|code| Packet::error(id, code));
            }
        }
        Packet::error(id, StatusCode::PermissionDenied)
    }

    fn request(&mut self, id: u32, packet: Packet) -> Reply {
        match packet {
            Packet::RealPath(p) => {
                let entry = self.root.metadata(&p.path).map_err(io_status)?;
                // Root validates the unmodified path first. Only "." and "/" alias
                // the root; accepted relative paths consist of ordinary components.
                let absolute = match p.path.as_str() {
                    "." => "/".into(),
                    path if path.starts_with('/') => p.path,
                    path => format!("/{path}"),
                };
                Ok(Name {
                    id,
                    files: vec![File::new(absolute, attributes(entry.is_dir, entry.len))],
                }
                .into())
            }
            Packet::Stat(Stat { path, .. }) | Packet::Lstat(Lstat { path, .. }) => {
                let entry = self.root.metadata(&path).map_err(io_status)?;
                Ok(Attrs {
                    id,
                    attrs: attributes(entry.is_dir, entry.len),
                }
                .into())
            }
            Packet::Fstat(p) => {
                let attrs = match self.handles.get(&p.handle).ok_or(StatusCode::Failure)? {
                    OpenHandle::File(file) => {
                        attributes(false, file.metadata().map_err(io_status)?.len())
                    }
                    OpenHandle::Directory { .. } => attributes(true, 0),
                };
                Ok(Attrs { id, attrs }.into())
            }
            Packet::Open(p) => {
                if p.pflags.bits() != OpenFlags::READ.bits() {
                    return Err(StatusCode::PermissionDenied);
                }
                self.open(id, &p.filename, false)
            }
            Packet::OpenDir(p) => self.open(id, &p.path, true),
            Packet::ReadDir(p) => {
                let Some(OpenHandle::Directory { entries, allocated }) =
                    self.handles.get_mut(&p.handle)
                else {
                    return Err(StatusCode::Failure);
                };
                let files: Vec<_> = entries
                    .by_ref()
                    .take(PAGE_ENTRIES)
                    .map(|e| File::new(e.name, attributes(e.is_dir, e.len)))
                    .collect();
                if entries.len() == 0 {
                    *entries = Vec::new().into_iter();
                    *allocated = 0;
                }
                if files.is_empty() {
                    return Err(StatusCode::Eof);
                }
                Ok(Name { id, files }.into())
            }
            Packet::Read(p) => {
                let Some(OpenHandle::File(file)) = self.handles.get(&p.handle) else {
                    return Err(StatusCode::Failure);
                };
                if p.len == 0 {
                    return Ok(Data {
                        id,
                        data: Vec::new(),
                    }
                    .into());
                }
                // Windows file offsets are signed; a larger v3 offset is beyond any EOF.
                if p.offset > i64::MAX as u64 {
                    return Err(StatusCode::Eof);
                }
                let mut data = vec![0; MAX_READ.min(p.len as usize)];
                let n = match file.seek_read(&mut data, p.offset) {
                    Ok(n) => n,
                    // ReadFile may reject a range that overflows the signed end
                    // offset. Determine EOF from the open object in that case.
                    Err(e)
                        if e.kind() == io::ErrorKind::UnexpectedEof
                            || p.offset >= file.metadata().map_err(io_status)?.len() =>
                    {
                        0
                    }
                    Err(e) => return Err(io_status(e)),
                };
                if n == 0 {
                    return Err(StatusCode::Eof);
                }
                data.truncate(n);
                Ok(Data { id, data }.into())
            }
            Packet::Close(p) => {
                self.handles.remove(&p.handle).ok_or(StatusCode::Failure)?;
                Ok(Packet::status(id, StatusCode::Ok, "", ""))
            }
            Packet::Write(_)
            | Packet::SetStat(_)
            | Packet::FSetStat(_)
            | Packet::Remove(_)
            | Packet::Rename(_)
            | Packet::MkDir(_)
            | Packet::RmDir(_)
            | Packet::Symlink(_) => Err(StatusCode::PermissionDenied),
            Packet::ReadLink(_) | Packet::Extended(_) => Err(StatusCode::OpUnsupported),
            _ => Err(StatusCode::BadMessage),
        }
    }

    fn open(&mut self, id: u32, path: &str, directory: bool) -> Reply {
        if self.handles.len() >= MAX_HANDLES {
            return Err(StatusCode::Failure);
        }
        let resource = if directory {
            self.directory_snapshot(self.root.list(path).map_err(io_status)?)?
        } else {
            OpenHandle::File(self.root.file(path).map_err(io_status)?)
        };
        let handle = self.next_handle.to_string();
        self.next_handle = self.next_handle.checked_add(1).ok_or(StatusCode::Failure)?;
        self.handles.insert(handle.clone(), resource);
        Ok(Handle { id, handle }.into())
    }

    fn directory_snapshot(&self, entries: Vec<Entry>) -> Result<OpenHandle, StatusCode> {
        let allocated = entries.capacity() * size_of::<Entry>()
            + entries.iter().map(|e| e.name.capacity()).sum::<usize>();
        let retained: usize = self
            .handles
            .values()
            .map(|h| match h {
                OpenHandle::Directory { allocated, .. } => *allocated,
                OpenHandle::File(_) => 0,
            })
            .sum();
        if allocated + retained > MAX_SNAPSHOT_BYTES {
            return Err(StatusCode::Failure);
        }
        Ok(OpenHandle::Directory {
            entries: entries.into_iter(),
            allocated,
        })
    }
}

fn attributes(directory: bool, len: u64) -> FileAttributes {
    FileAttributes {
        size: Some(if directory { 0 } else { len }),
        permissions: Some(if directory { 0o040555 } else { 0o100444 }),
        ..FileAttributes::empty()
    }
}

fn io_status(error: io::Error) -> StatusCode {
    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => StatusCode::NoSuchFile,
        io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidInput => {
            StatusCode::PermissionDenied
        }
        _ => StatusCode::Failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::collections::BTreeSet;

    const ID: u32 = 0x1100_22ff;

    fn fixture() -> (tempfile::TempDir, Export) {
        let folder = tempfile::tempdir().unwrap();
        let export = Export::new(Root::open(folder.path()).unwrap());
        (folder, export)
    }

    fn open(path: &str, flags: OpenFlags) -> Packet {
        Open {
            id: ID,
            filename: path.into(),
            pflags: flags,
            attrs: FileAttributes::empty(),
        }
        .into()
    }

    fn opendir(path: &str) -> Packet {
        OpenDir {
            id: ID,
            path: path.into(),
        }
        .into()
    }

    fn handle(packet: Packet) -> String {
        let Packet::Handle(h) = packet else {
            panic!("expected HANDLE, got {packet:?}")
        };
        assert_eq!(h.id, ID);
        h.handle
    }

    fn status(packet: Packet, expected: StatusCode) {
        let Packet::Status(s) = packet else {
            panic!("expected {expected}, got {packet:?}")
        };
        assert_eq!((s.id, s.status_code), (ID, expected));
    }

    fn read(export: &mut Export, handle: &str, offset: u64, len: u32) -> Packet {
        export.reply(
            Read {
                id: ID,
                handle: handle.into(),
                offset,
                len,
            }
            .into(),
        )
    }

    #[test]
    fn retiring_a_profile_denies_open_files_and_buffered_directory_entries() {
        let folder = tempfile::tempdir().unwrap();
        let permissions = tempfile::tempdir().unwrap();
        let marker = permissions.path().join("retired");
        let gate = crate::profiles::FolderGrantGate::new(
            marker.clone(),
            permissions.path().join("revision"),
            None,
        );
        let mut export = Export::new(
            Root::open(folder.path())
                .unwrap()
                .with_profile_permission(gate),
        );
        fs::write(folder.path().join("file"), b"private").unwrap();
        let file = handle(export.reply(open("file", OpenFlags::READ)));
        let directory = handle(export.reply(opendir("/")));
        assert!(matches!(read(&mut export, &file, 0, 1), Packet::Data(_)));
        fs::write(&marker, []).unwrap();
        status(read(&mut export, &file, 1, 1), StatusCode::PermissionDenied);
        status(
            export.reply(
                ReadDir {
                    id: ID,
                    handle: directory,
                }
                .into(),
            ),
            StatusCode::PermissionDenied,
        );
        status(
            export.reply(open("file", OpenFlags::READ)),
            StatusCode::PermissionDenied,
        );
        // Once closed, a running export cannot be revived by changing the marker.
        fs::remove_file(marker).unwrap();
        status(read(&mut export, &file, 0, 1), StatusCode::PermissionDenied);
    }

    #[test]
    fn reads_use_the_requested_offset_and_bound_each_reply() {
        let (folder, mut export) = fixture();
        let payload: Vec<_> = (0..3 * MAX_READ).map(|n| (n % 251) as u8).collect();
        fs::write(folder.path().join("input.bin"), &payload).unwrap();
        let h = handle(export.reply(open("/input.bin", OpenFlags::READ)));
        for (offset, size, expected) in [
            (7821, u32::MAX, &payload[7821..7821 + MAX_READ]),
            (255, 41, &payload[255..296]),
            (payload.len() - 4, 1024, &payload[payload.len() - 4..]),
            (0, 0, &payload[0..0]),
        ] {
            let Packet::Data(reply) = read(&mut export, &h, offset as u64, size) else {
                panic!("expected DATA")
            };
            assert_eq!(reply.id, ID);
            assert_eq!(reply.data, expected);
        }
        for offset in [payload.len() as u64, i64::MAX as u64, u64::MAX] {
            status(read(&mut export, &h, offset, 128), StatusCode::Eof);
        }
        let Packet::Attrs(reply) = export.reply(
            Fstat {
                id: ID,
                handle: h.clone(),
            }
            .into(),
        ) else {
            panic!("expected file attributes")
        };
        assert_eq!(
            (reply.id, reply.attrs.size, reply.attrs.permissions),
            (ID, Some(payload.len() as u64), Some(0o100444))
        );
        status(
            export.reply(
                ReadDir {
                    id: ID,
                    handle: h.clone(),
                }
                .into(),
            ),
            StatusCode::Failure,
        );
        status(
            export.reply(
                Close {
                    id: ID,
                    handle: h.clone(),
                }
                .into(),
            ),
            StatusCode::Ok,
        );
        status(read(&mut export, &h, 0, 128), StatusCode::Failure);
        status(
            export.reply(Close { id: ID, handle: h }.into()),
            StatusCode::Failure,
        );
    }

    #[test]
    fn readdir_returns_all_unicode_entries_once_in_bounded_wire_pages() {
        let (folder, mut export) = fixture();
        let path = folder.path().join("庭");
        fs::create_dir(&path).unwrap();
        let names: BTreeSet<_> = (0..PAGE_ENTRIES * 2 + 1)
            .map(|i| format!("{}{i:03}", "界".repeat(252)))
            .collect();
        for name in &names {
            fs::write(path.join(name), b"x").unwrap();
        }
        let h = handle(export.reply(opendir("/庭")));
        let Packet::Attrs(dir) = export.reply(
            Fstat {
                id: ID,
                handle: h.clone(),
            }
            .into(),
        ) else {
            panic!("expected directory attributes")
        };
        assert_eq!((dir.id, dir.attrs.permissions), (ID, Some(0o040555)));
        status(read(&mut export, &h, 0, 10), StatusCode::Failure);

        let mut seen = BTreeSet::new();
        for count in [PAGE_ENTRIES, PAGE_ENTRIES, 1] {
            let reply = export.reply(
                ReadDir {
                    id: ID,
                    handle: h.clone(),
                }
                .into(),
            );
            let encoded = Bytes::try_from(reply).unwrap();
            assert!(encoded.len() < 32 * 1024);
            assert_eq!(
                u32::from_be_bytes(encoded[..4].try_into().unwrap()) as usize,
                encoded.len() - 4
            );
            let Packet::Name(page) = Packet::try_from(&mut encoded.slice(4..)).unwrap() else {
                panic!("expected a v3 NAME frame")
            };
            assert_eq!((page.id, page.files.len()), (ID, count));
            for file in page.files {
                assert_eq!(file.attrs.permissions, Some(0o100444));
                assert!(seen.insert(file.filename));
            }
        }
        assert_eq!(seen, names);
        status(
            export.reply(ReadDir { id: ID, handle: h }.into()),
            StatusCode::Eof,
        );
    }

    #[test]
    fn realpath_returns_only_virtual_names_and_validates_before_anchoring() {
        let (folder, mut export) = fixture();
        fs::create_dir(folder.path().join("資料")).unwrap();
        for (path, expected) in [
            (".", "/"),
            ("/", "/"),
            ("資料", "/資料"),
            ("/資料", "/資料"),
        ] {
            let Packet::Name(reply) = export.reply(
                RealPath {
                    id: ID,
                    path: path.into(),
                }
                .into(),
            ) else {
                panic!("expected REALPATH name for {path}")
            };
            assert_eq!((reply.id, reply.files.len()), (ID, 1));
            assert_eq!(reply.files[0].filename, expected);
            for request in [
                Packet::from(Stat {
                    id: ID,
                    path: path.into(),
                }),
                Lstat {
                    id: ID,
                    path: path.into(),
                }
                .into(),
            ] {
                let Packet::Attrs(reply) = export.reply(request) else {
                    panic!("expected stat attributes")
                };
                assert_eq!((reply.id, reply.attrs.permissions), (ID, Some(0o040555)));
            }
        }
        for path in ["..", "/資料/..", "//資料", r"資料\file", "file:stream"] {
            status(
                export.reply(
                    RealPath {
                        id: ID,
                        path: path.into(),
                    }
                    .into(),
                ),
                StatusCode::PermissionDenied,
            );
        }
        status(
            export.reply(
                RealPath {
                    id: ID,
                    path: "/absent".into(),
                }
                .into(),
            ),
            StatusCode::NoSuchFile,
        );
    }

    #[test]
    fn write_operations_never_change_file_and_extensions_are_unadvertised() {
        let (folder, mut export) = fixture();
        let path = folder.path().join("existing");
        fs::write(&path, b"preserve this").unwrap();
        for flags in [
            OpenFlags::WRITE,
            OpenFlags::READ | OpenFlags::WRITE,
            OpenFlags::READ | OpenFlags::CREATE,
            OpenFlags::READ | OpenFlags::TRUNCATE,
            OpenFlags::APPEND,
        ] {
            status(
                export.reply(open("existing", flags)),
                StatusCode::PermissionDenied,
            );
        }
        let h = handle(export.reply(open("existing", OpenFlags::READ)));
        for request in [
            Packet::from(Write {
                id: ID,
                handle: h.clone(),
                offset: 0,
                data: vec![0],
            }),
            FSetStat {
                id: ID,
                handle: h,
                attrs: FileAttributes::empty(),
            }
            .into(),
            Remove {
                id: ID,
                filename: "existing".into(),
            }
            .into(),
            Rename {
                id: ID,
                oldpath: "existing".into(),
                newpath: "renamed".into(),
            }
            .into(),
        ] {
            status(export.reply(request), StatusCode::PermissionDenied);
        }
        status(
            export.reply(
                Extended {
                    id: ID,
                    request: "posix-rename@openssh.com".into(),
                    data: vec![],
                }
                .into(),
            ),
            StatusCode::OpUnsupported,
        );
        assert_eq!(fs::read(path).unwrap(), b"preserve this");
    }

    #[test]
    fn handle_limit_frees_a_slot_on_close_without_reusing_the_token() {
        let (_folder, mut export) = fixture();
        let handles: Vec<_> = (0..MAX_HANDLES)
            .map(|_| handle(export.reply(opendir("/"))))
            .collect();
        status(export.reply(opendir("/")), StatusCode::Failure);
        status(
            export.reply(
                Close {
                    id: ID,
                    handle: handles[0].clone(),
                }
                .into(),
            ),
            StatusCode::Ok,
        );
        let new = handle(export.reply(opendir("/")));
        assert!(!handles.contains(&new));
        status(
            export.reply(
                Fstat {
                    id: ID,
                    handle: handles[0].clone(),
                }
                .into(),
            ),
            StatusCode::Failure,
        );
    }

    #[test]
    fn unreadable_wire_names_cannot_alias_an_actual_replacement_character() {
        let name = "�";
        for packet in [
            open(name, OpenFlags::READ),
            Lstat {
                id: ID,
                path: name.into(),
            }
            .into(),
            opendir(name),
            RealPath {
                id: ID,
                path: name.into(),
            }
            .into(),
            Stat {
                id: ID,
                path: name.into(),
            }
            .into(),
        ] {
            let frame = Bytes::try_from(packet).unwrap();
            let mut body = frame[4..].to_vec();
            assert!(valid_read_path_encoding(&body));
            assert!(!valid_read_path_encoding(&body[..8]));
            assert!(!valid_read_path_encoding(&body[..11]));
            body[9] = 0xff;
            assert!(!valid_read_path_encoding(&body));
            body[5..9].copy_from_slice(&u32::MAX.to_be_bytes());
            assert!(!valid_read_path_encoding(&body));
        }
        let write = Write {
            id: ID,
            handle: "1".into(),
            offset: 0,
            data: vec![0xff],
        };
        let frame = Bytes::try_from(Packet::from(write)).unwrap();
        assert!(valid_read_path_encoding(&frame[4..]));
    }

    #[test]
    fn snapshot_memory_budget_is_shared_and_released_after_the_last_page() {
        let (_folder, mut export) = fixture();
        // Count spare Vec capacity as well as populated entries. These two
        // snapshots would exceed the session limit even with short filenames.
        let entries = || {
            let mut items = Vec::with_capacity(MAX_SNAPSHOT_BYTES / (2 * size_of::<Entry>()) + 1);
            items.push(Entry {
                name: "file".into(),
                is_dir: false,
                len: 1,
            });
            items
        };
        let snapshot = export.directory_snapshot(entries()).unwrap();
        export.handles.insert("pending".into(), snapshot);
        assert!(matches!(
            export.directory_snapshot(entries()),
            Err(StatusCode::Failure)
        ));
        let page = export.reply(
            ReadDir {
                id: ID,
                handle: "pending".into(),
            }
            .into(),
        );
        assert!(matches!(page, Packet::Name(_)));
        assert!(export.directory_snapshot(entries()).is_ok());
    }
}
