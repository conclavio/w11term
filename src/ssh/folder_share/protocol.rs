//! SFTP v3 request bodies on an already negotiated passive SSHFS channel. The transport
//! feeds the decoder SSH stdout after consuming the frame length; encoding a reply with
//! `Bytes::try_from(packet)` includes that length. SSH stderr is diagnostic text.
use super::filesystem::{Entry, PendingWrite, Root};
use russh_sftp::protocol::*;
use std::{collections::HashMap, fs, io, os::windows::fs::FileExt, vec::IntoIter};

const MAX_HANDLES: usize = 32;
const MAX_WRITE_HANDLES: usize = 4;
const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
const MAX_READ: usize = 32 * 1024;
// At most 255 UTF-16 units per Windows component. Even with the v3 longname
// repeating the UTF-8 name, 16 entries fit in a 32 KiB NAME packet.
const PAGE_ENTRIES: usize = 16;

type Reply = Result<Packet, StatusCode>;

/// Validate v3 paths before the library converts invalid UTF-8 to U+FFFD. For
/// mutations, also verify the full request shape before decoding: accepting a
/// truncated ATTRS or ignoring an unknown flag could change the operation.
pub(in crate::ssh) fn valid_request_encoding(body: &[u8]) -> bool {
    let Some(mut fields) = body.get(5..) else {
        return false;
    };
    let kind = body[0];
    let Some(first) = take_string(&mut fields) else {
        return false;
    };
    if matches!(kind, 3 | 7 | 9 | 11 | 13..=20) && str::from_utf8(first).is_err() {
        return false;
    }
    match kind {
        3 => {
            take_u32(&mut fields).is_some_and(|bits| OpenFlags::from_bits(bits).is_some())
                && valid_attrs(fields)
        }
        4 | 7 | 8 | 11..=13 | 15..=17 | 19 => fields.is_empty(),
        5 => fields.len() == 12,
        6 => fields
            .get(8..)
            .is_some_and(|mut tail| take_string(&mut tail).is_some() && tail.is_empty()),
        9 | 10 | 14 => valid_attrs(fields),
        18 | 20 => {
            take_string(&mut fields).is_some_and(|path| str::from_utf8(path).is_ok())
                && fields.is_empty()
        }
        _ => false,
    }
}

fn take_u32(fields: &mut &[u8]) -> Option<u32> {
    let (value, rest) = fields.split_first_chunk::<4>()?;
    *fields = rest;
    Some(u32::from_be_bytes(*value))
}

fn take_string<'a>(fields: &mut &'a [u8]) -> Option<&'a [u8]> {
    let len = take_u32(fields)? as usize;
    let (value, rest) = fields.split_at_checked(len)?;
    *fields = rest;
    Some(value)
}

fn valid_attrs(mut fields: &[u8]) -> bool {
    let Some(mask) = take_u32(&mut fields) else {
        return false;
    };
    // SIZE, UIDGID, PERMISSIONS and ACMODTIME have fixed v3 widths. No
    // attribute extensions are negotiated by this server.
    let width: usize = [(1, 8), (2, 8), (4, 4), (8, 8)]
        .into_iter()
        .filter_map(|(bit, len)| (mask & bit != 0).then_some(len))
        .sum();
    mask & !0xf == 0 && width == fields.len()
}

enum OpenHandle {
    File(fs::File),
    Edit {
        file: PendingWrite,
        flags: OpenFlags,
    },
    Directory {
        entries: IntoIter<Entry>,
        allocated: usize,
    },
}

impl OpenHandle {
    fn len(&self) -> Result<u64, StatusCode> {
        match self {
            Self::File(file) => Ok(file.metadata().map_err(io_status)?.len()),
            Self::Edit { file, .. } => file.len().map_err(io_status),
            Self::Directory { .. } => Ok(0),
        }
    }

    fn read(&self, id: u32, offset: u64, len: u32) -> Reply {
        match self {
            Self::Directory { .. } => return Err(StatusCode::Failure),
            Self::Edit { flags, .. } if !flags.contains(OpenFlags::READ) => {
                return Err(StatusCode::PermissionDenied);
            }
            _ => {}
        }
        let mut data = vec![0; MAX_READ.min(len as usize)];
        if !data.is_empty() {
            if offset > i64::MAX as u64 {
                return Err(StatusCode::Eof);
            }
            let result = match self {
                Self::File(file) => file.seek_read(&mut data, offset),
                Self::Edit { file, .. } => file.read_at(&mut data, offset),
                Self::Directory { .. } => unreachable!(),
            };
            let n = match result {
                Ok(0) => return Err(StatusCode::Eof),
                Ok(n) => n,
                // Windows rejects a range overflowing the signed file offset.
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof || offset >= self.len()? => {
                    return Err(StatusCode::Eof);
                }
                Err(e) => return Err(io_status(e)),
            };
            data.truncate(n);
        }
        Ok(Data { id, data }.into())
    }
}

pub(in crate::ssh) struct Export {
    root: Root,
    handles: HashMap<String, OpenHandle>,
    next_handle: u64,
}

fn mutating(packet: &Packet) -> bool {
    match packet {
        Packet::Open(p) => p.pflags.bits() != OpenFlags::READ.bits(),
        Packet::Write(_)
        | Packet::SetStat(_)
        | Packet::FSetStat(_)
        | Packet::Remove(_)
        | Packet::Rename(_)
        | Packet::MkDir(_)
        | Packet::RmDir(_) => true,
        _ => false,
    }
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
        self.authorized_request(id, packet)
            .unwrap_or_else(|code| Packet::error(id, code))
    }

    fn authorized_request(&mut self, id: u32, packet: Packet) -> Reply {
        if !self.root.permits_request() {
            return Err(StatusCode::PermissionDenied);
        }
        // Hold the permission epoch's lock through publication and the final
        // gate check. Revoke can return only after any admitted write finishes.
        let _permit = if mutating(&packet) {
            if !self.root.is_writable() {
                return Err(StatusCode::PermissionDenied);
            }
            Some(self.root.lock_mutation().map_err(io_status)?)
        } else {
            None
        };
        let result = self.request(id, packet);
        if !self.root.permits_request() {
            return Err(StatusCode::PermissionDenied);
        }
        result
    }

    fn request(&mut self, id: u32, packet: Packet) -> Reply {
        let writable = self.root.is_writable();
        match packet {
            Packet::RealPath(p) => {
                let entry = self.root.metadata(&p.path).map_err(io_status)?;
                // Validate the original name before anchoring it at the virtual root.
                let absolute = match p.path.as_str() {
                    "." => "/".into(),
                    path if path.starts_with('/') => p.path,
                    path => format!("/{path}"),
                };
                Ok(Name {
                    id,
                    files: vec![File::new(
                        absolute,
                        attributes(entry.is_dir, entry.len, writable),
                    )],
                }
                .into())
            }
            Packet::Stat(Stat { path, .. }) | Packet::Lstat(Lstat { path, .. }) => {
                let entry = self.root.metadata(&path).map_err(io_status)?;
                Ok(Attrs {
                    id,
                    attrs: attributes(entry.is_dir, entry.len, writable),
                }
                .into())
            }
            Packet::Fstat(p) => {
                let handle = self.handles.get(&p.handle).ok_or(StatusCode::Failure)?;
                Ok(Attrs {
                    id,
                    attrs: attributes(
                        matches!(handle, OpenHandle::Directory { .. }),
                        handle.len()?,
                        writable,
                    ),
                }
                .into())
            }
            Packet::Open(p) => self.open(id, |this| this.open_file(p)),
            Packet::OpenDir(p) => self.open(id, |this| {
                this.directory_snapshot(this.root.list(&p.path).map_err(io_status)?)
            }),
            Packet::ReadDir(p) => {
                let Some(OpenHandle::Directory { entries, allocated }) =
                    self.handles.get_mut(&p.handle)
                else {
                    return Err(StatusCode::Failure);
                };
                let files: Vec<_> = entries
                    .by_ref()
                    .take(PAGE_ENTRIES)
                    .map(|e| File::new(e.name, attributes(e.is_dir, e.len, writable)))
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
            Packet::Read(p) => self
                .handles
                .get(&p.handle)
                .ok_or(StatusCode::Failure)?
                .read(id, p.offset, p.len),
            Packet::Close(p) => {
                // OPEN/WRITE/SETSTAT publish before their reply; CLOSE never
                // replays or commits an edit the peer did not request.
                self.handles.remove(&p.handle).ok_or(StatusCode::Failure)?;
                Ok(success(id))
            }
            Packet::Write(p) => {
                let (file, flags) = edit_handle(&mut self.handles, &p.handle)?;
                if flags.contains(OpenFlags::APPEND) {
                    file.append(&self.root, &p.data)
                } else {
                    file.write_at(&self.root, &p.data, p.offset)
                }
                .map_err(io_status)?;
                Ok(success(id))
            }
            Packet::FSetStat(p) => {
                let (file, _) = edit_handle(&mut self.handles, &p.handle)?;
                file.set_len(&self.root, size_change(&p.attrs)?)
                    .map_err(io_status)?;
                Ok(success(id))
            }
            Packet::SetStat(p) => {
                let len = size_change(&p.attrs)?;
                self.check_edit_capacity()?;
                let mut file = self
                    .root
                    .open_write(&p.path, false, false, false)
                    .map_err(io_status)?;
                file.set_len(&self.root, len).map_err(io_status)?;
                Ok(success(id))
            }
            Packet::MkDir(p) => {
                creation_attrs(&p.attrs)?;
                self.root.mkdir(&p.path).map_err(io_status)?;
                Ok(success(id))
            }
            Packet::Remove(p) => {
                self.root.remove(&p.filename).map_err(io_status)?;
                Ok(success(id))
            }
            Packet::RmDir(p) => {
                self.root.rmdir(&p.path).map_err(io_status)?;
                Ok(success(id))
            }
            Packet::Rename(p) => {
                self.root
                    .rename(&p.oldpath, &p.newpath)
                    .map_err(io_status)?;
                Ok(success(id))
            }
            Packet::Symlink(_) => Err(StatusCode::PermissionDenied),
            Packet::ReadLink(_) | Packet::Extended(_) => Err(StatusCode::OpUnsupported),
            _ => Err(StatusCode::BadMessage),
        }
    }

    fn open(
        &mut self,
        id: u32,
        resource: impl FnOnce(&Self) -> Result<OpenHandle, StatusCode>,
    ) -> Reply {
        if self.handles.len() >= MAX_HANDLES {
            return Err(StatusCode::Failure);
        }
        let next = self.next_handle.checked_add(1).ok_or(StatusCode::Failure)?;
        let resource = resource(self)?;
        let handle = self.next_handle.to_string();
        self.next_handle = next;
        self.handles.insert(handle.clone(), resource);
        Ok(Handle { id, handle }.into())
    }

    fn check_edit_capacity(&self) -> Result<(), StatusCode> {
        if self
            .handles
            .values()
            .filter(|h| matches!(h, OpenHandle::Edit { .. }))
            .count()
            >= MAX_WRITE_HANDLES
        {
            return Err(StatusCode::Failure);
        }
        Ok(())
    }

    fn open_file(&self, p: Open) -> Result<OpenHandle, StatusCode> {
        if p.pflags.bits() == OpenFlags::READ.bits() {
            return Ok(OpenHandle::File(
                self.root.file(&p.filename).map_err(io_status)?,
            ));
        }
        creation_attrs(&p.attrs)?;
        let f = p.pflags;
        let write = f.contains(OpenFlags::WRITE);
        let create = f.contains(OpenFlags::CREATE);
        let exclusive = f.contains(OpenFlags::EXCLUDE);
        let truncate = f.contains(OpenFlags::TRUNCATE);
        if OpenFlags::from_bits(f.bits()).is_none()
            || !(write || f.contains(OpenFlags::READ))
            || (truncate || f.contains(OpenFlags::APPEND)) && !write
            || exclusive && !create
        {
            return Err(StatusCode::BadMessage);
        }
        self.check_edit_capacity()?;
        let file = self
            .root
            .open_write(&p.filename, create, exclusive, truncate)
            .map_err(io_status)?;
        Ok(OpenHandle::Edit { file, flags: f })
    }

    fn directory_snapshot(&self, entries: Vec<Entry>) -> Result<OpenHandle, StatusCode> {
        let allocated = entries.capacity() * size_of::<Entry>()
            + entries.iter().map(|e| e.name.capacity()).sum::<usize>();
        let retained: usize = self
            .handles
            .values()
            .map(|h| match h {
                OpenHandle::Directory { allocated, .. } => *allocated,
                _ => 0,
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

fn edit_handle<'a>(
    handles: &'a mut HashMap<String, OpenHandle>,
    id: &str,
) -> Result<(&'a mut PendingWrite, OpenFlags), StatusCode> {
    match handles.get_mut(id).ok_or(StatusCode::Failure)? {
        OpenHandle::Edit { file, flags } if flags.contains(OpenFlags::WRITE) => Ok((file, *flags)),
        _ => Err(StatusCode::PermissionDenied),
    }
}

fn success(id: u32) -> Packet {
    Packet::status(id, StatusCode::Ok, "", "")
}

fn has_owner_or_time(attrs: &FileAttributes) -> bool {
    attrs.uid.is_some()
        || attrs.gid.is_some()
        || attrs.user.is_some()
        || attrs.group.is_some()
        || attrs.atime.is_some()
        || attrs.mtime.is_some()
}

// SSHFS supplies the requested POSIX permissions as a creation hint. New
// objects inherit the Windows directory's ACL; the export synthesizes modes.
fn creation_attrs(attrs: &FileAttributes) -> Result<(), StatusCode> {
    if attrs.size.is_some() || has_owner_or_time(attrs) {
        return Err(StatusCode::OpUnsupported);
    }
    Ok(())
}

fn size_change(attrs: &FileAttributes) -> Result<u64, StatusCode> {
    if attrs.permissions.is_some() || has_owner_or_time(attrs) {
        return Err(StatusCode::OpUnsupported);
    }
    attrs.size.ok_or(StatusCode::OpUnsupported)
}

fn attributes(directory: bool, len: u64, writable: bool) -> FileAttributes {
    let mode = if directory { 0o040555 } else { 0o100444 };
    FileAttributes {
        size: Some(if directory { 0 } else { len }),
        permissions: Some(mode | if writable { 0o200 } else { 0 }),
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

    #[track_caller]
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
    fn read_only_grant_refuses_mutations_and_extensions() {
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
            assert!(valid_request_encoding(&body));
            assert!(!valid_request_encoding(&body[..8]));
            assert!(!valid_request_encoding(&body[..11]));
            body[9] = 0xff;
            assert!(!valid_request_encoding(&body));
            body[5..9].copy_from_slice(&u32::MAX.to_be_bytes());
            assert!(!valid_request_encoding(&body));
        }
        let write = Write {
            id: ID,
            handle: "1".into(),
            offset: 0,
            data: vec![0xff],
        };
        let frame = Bytes::try_from(Packet::from(write)).unwrap();
        assert!(valid_request_encoding(&frame[4..]));
    }

    mod writes;

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
