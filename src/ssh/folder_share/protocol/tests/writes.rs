use super::*;

fn writable(folder: &tempfile::TempDir) -> Export {
    Export::new(Root::open(folder.path()).unwrap().with_write_access(true))
}

/// Exercise the same v3 validation and decoder the SSHFS worker uses.
fn request(export: &mut Export, packet: impl Into<Packet>) -> Packet {
    let mut body = Bytes::try_from(packet.into()).unwrap().slice(4..);
    assert!(valid_request_encoding(&body));
    let decoded = Packet::try_from(&mut body).unwrap();
    assert!(body.is_empty());
    export.reply(decoded)
}

fn write(export: &mut Export, handle: &str, offset: u64, data: &[u8]) -> Packet {
    request(
        export,
        Write {
            id: ID,
            handle: handle.into(),
            offset,
            data: data.to_vec(),
        },
    )
}

fn set_size(handle: &str, len: u64) -> FSetStat {
    FSetStat {
        id: ID,
        handle: handle.into(),
        attrs: FileAttributes {
            size: Some(len),
            ..FileAttributes::empty()
        },
    }
}

#[test]
fn writes_and_size_changes_publish_before_reply_and_survive_lost_close() {
    let (folder, _readonly) = fixture();
    let path = folder.path().join("input.bin");
    fs::write(&path, b"prior content").unwrap();
    let mut export = writable(&folder);
    let h = handle(request(
        &mut export,
        open(
            "input.bin",
            OpenFlags::READ | OpenFlags::WRITE | OpenFlags::TRUNCATE,
        ),
    ));
    assert_eq!(fs::metadata(&path).unwrap().len(), 0);
    status(write(&mut export, &h, 3, &[0xff, 0x80]), StatusCode::Ok);
    assert_eq!(fs::read(&path).unwrap(), [0, 0, 0, 0xff, 0x80]);
    let Packet::Data(data) = read(&mut export, &h, 2, 10) else {
        panic!("expected DATA")
    };
    assert_eq!(data.data, [0, 0xff, 0x80]);
    status(request(&mut export, set_size(&h, 4)), StatusCode::Ok);
    assert_eq!(fs::read(&path).unwrap(), [0, 0, 0, 0xff]);
    let Packet::Attrs(attrs) = request(
        &mut export,
        Fstat {
            id: ID,
            handle: h.clone(),
        },
    ) else {
        panic!("expected file attributes")
    };
    assert_eq!(
        (attrs.attrs.size, attrs.attrs.permissions),
        (Some(4), Some(0o100644))
    );

    // A rejected offset cannot leave work that DROP/CLOSE might apply later.
    assert!(
        matches!(write(&mut export, &h, u64::MAX, b"x"), Packet::Status(s) if s.status_code != StatusCode::Ok)
    );
    drop(export);
    assert_eq!(fs::read(path).unwrap(), [0, 0, 0, 0xff]);
}

#[test]
fn open_modes_exclusive_creation_and_append_honor_handle_access() {
    let (folder, mut readonly) = fixture();
    let mut export = writable(&folder);
    let path = folder.path().join("log");
    let exclusive = OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE;
    let h = handle(request(&mut export, open("/log", exclusive)));
    assert!(path.is_file());
    status(
        request(&mut export, open("/log", exclusive)),
        StatusCode::Failure,
    );
    status(write(&mut export, &h, 0, b"one"), StatusCode::Ok);
    status(read(&mut export, &h, 0, 0), StatusCode::PermissionDenied);
    status(
        request(&mut export, Close { id: ID, handle: h }),
        StatusCode::Ok,
    );
    let read_only = handle(request(&mut export, open("/log", OpenFlags::READ)));
    status(
        write(&mut export, &read_only, 0, b"wrong"),
        StatusCode::PermissionDenied,
    );
    status(
        request(&mut export, set_size(&read_only, 0)),
        StatusCode::PermissionDenied,
    );
    status(
        request(
            &mut export,
            Close {
                id: ID,
                handle: read_only,
            },
        ),
        StatusCode::Ok,
    );
    let append = handle(request(
        &mut export,
        open("/log", OpenFlags::WRITE | OpenFlags::APPEND),
    ));
    // Both handles were opened at length three. Each append must use the live
    // EOF under the writer lock, and FSTAT on either handle sees both writes.
    let second = handle(request(
        &mut export,
        open("/log", OpenFlags::WRITE | OpenFlags::APPEND),
    ));
    status(write(&mut export, &append, 0, b"two"), StatusCode::Ok);
    status(write(&mut export, &second, u64::MAX, b"!"), StatusCode::Ok);
    let Packet::Attrs(attrs) = request(
        &mut export,
        Fstat {
            id: ID,
            handle: append,
        },
    ) else {
        panic!("expected current file attributes")
    };
    assert_eq!(attrs.attrs.size, Some(7));
    status(
        request(
            &mut readonly,
            open("/log", OpenFlags::WRITE | OpenFlags::TRUNCATE),
        ),
        StatusCode::PermissionDenied,
    );
    assert_eq!(fs::read(&path).unwrap(), b"onetwo!");
    for flags in [
        OpenFlags::READ | OpenFlags::APPEND,
        OpenFlags::WRITE | OpenFlags::EXCLUDE,
        OpenFlags::empty(),
    ] {
        status(
            request(&mut export, open("/invalid", flags)),
            StatusCode::BadMessage,
        );
    }
    assert!(!folder.path().join("invalid").exists());
}

#[test]
fn mkdir_rename_remove_rmdir_and_path_truncate_are_visible_on_ack() {
    let (folder, _readonly) = fixture();
    let mut export = writable(&folder);
    let dir = folder.path().join("保存");
    let attrs = FileAttributes {
        permissions: Some(0o40750),
        ..FileAttributes::empty()
    };
    status(
        request(
            &mut export,
            MkDir {
                id: ID,
                path: "/保存".into(),
                attrs,
            },
        ),
        StatusCode::Ok,
    );
    let path = dir.join("item.bin");
    fs::write(&path, [1, 2, 3, 4]).unwrap();
    let rename = || Rename {
        id: ID,
        oldpath: "/保存/item.bin".into(),
        newpath: "/other".into(),
    };
    let other = folder.path().join("other");
    fs::write(&other, b"keep").unwrap();
    assert!(
        matches!(request(&mut export, rename()), Packet::Status(s) if matches!(s.status_code, StatusCode::Failure | StatusCode::PermissionDenied))
    );
    assert_eq!(fs::read(&other).unwrap(), b"keep");
    assert_eq!(fs::read(&path).unwrap(), [1, 2, 3, 4]);
    let remove = |name: &str| Remove {
        id: ID,
        filename: name.into(),
    };
    status(request(&mut export, remove("/other")), StatusCode::Ok);
    status(request(&mut export, rename()), StatusCode::Ok);
    assert!(!path.exists());
    let change = |size, permissions| SetStat {
        id: ID,
        path: "/other".into(),
        attrs: FileAttributes {
            size,
            permissions,
            ..FileAttributes::empty()
        },
    };
    status(
        request(&mut export, change(Some(0), Some(0o600))),
        StatusCode::OpUnsupported,
    );
    assert_eq!(fs::read(&other).unwrap(), [1, 2, 3, 4]);
    status(request(&mut export, change(Some(2), None)), StatusCode::Ok);
    assert_eq!(fs::read(&other).unwrap(), [1, 2]);
    status(request(&mut export, remove("/other")), StatusCode::Ok);
    status(
        request(
            &mut export,
            RmDir {
                id: ID,
                path: "/保存".into(),
            },
        ),
        StatusCode::Ok,
    );
    assert!(!dir.exists());
    assert!(!other.exists());
}

#[test]
fn retiring_an_active_writable_grant_refuses_mutation_on_existing_handles() {
    let (folder, _readonly) = fixture();
    let perms = tempfile::tempdir().unwrap();
    let marker = perms.path().join("revoked");
    let gate =
        crate::profiles::FolderGrantGate::new(marker.clone(), perms.path().join("revision"), None);
    let mut export = Export::new(
        Root::open(folder.path())
            .unwrap()
            .with_write_access(true)
            .with_profile_permission(gate),
    );
    let handle = handle(request(
        &mut export,
        open("log", OpenFlags::WRITE | OpenFlags::CREATE),
    ));
    status(write(&mut export, &handle, 0, b"completed"), StatusCode::Ok);
    fs::write(&marker, []).unwrap();
    status(
        write(&mut export, &handle, 0, b"unapproved"),
        StatusCode::PermissionDenied,
    );
    fs::remove_file(marker).unwrap();
    status(
        request(&mut export, set_size(&handle, 0)),
        StatusCode::PermissionDenied,
    );
    drop(export);
    assert_eq!(fs::read(folder.path().join("log")).unwrap(), b"completed");
}

#[test]
fn full_writable_handle_budget_denies_creation_before_it_reaches_disk() {
    let (folder, _readonly) = fixture();
    let mut export = writable(&folder);
    let flags = OpenFlags::WRITE | OpenFlags::CREATE;
    let handles = (0..MAX_WRITE_HANDLES)
        .map(|i| handle(request(&mut export, open(&format!("f{i}"), flags))))
        .collect::<Vec<_>>();
    status(
        request(&mut export, open("new", flags)),
        StatusCode::Failure,
    );
    assert!(!folder.path().join("new").exists());
    status(
        request(
            &mut export,
            Close {
                id: ID,
                handle: handles[0].clone(),
            },
        ),
        StatusCode::Ok,
    );
    let h = handle(request(&mut export, open("new", flags)));
    assert!(!handles.contains(&h));
    assert!(folder.path().join("new").is_file());
}

#[test]
fn wire_checks_both_rename_paths_and_never_drop_unknown_flags_on_mutations() {
    let rename: Packet = Rename {
        id: ID,
        oldpath: "x".into(),
        newpath: "y".into(),
    }
    .into();
    let frame = Bytes::try_from(rename).unwrap();
    let body = &frame[4..];
    assert!(valid_request_encoding(body));
    for index in [9, 14] {
        let mut invalid = body.to_vec();
        invalid[index] = 0xff;
        assert!(!valid_request_encoding(&invalid));
    }
    let frame = Bytes::try_from(open("x", OpenFlags::WRITE | OpenFlags::CREATE)).unwrap();
    let body = &frame[4..];
    assert!(valid_request_encoding(body));
    for index in [10, 14] {
        let mut invalid = body.to_vec();
        invalid[index] = 0x80;
        assert!(!valid_request_encoding(&invalid));
    }
    let mut trailing = body.to_vec();
    trailing.push(0);
    assert!(!valid_request_encoding(&trailing));
    assert!(!valid_request_encoding(&body[..body.len() - 1]));
    let frame = Bytes::try_from(Packet::from(Write {
        id: ID,
        handle: "1".into(),
        offset: 0,
        data: vec![0xff, 0x80],
    }))
    .unwrap();
    assert!(valid_request_encoding(&frame[4..]));
    assert!(!valid_request_encoding(&frame[4..frame.len() - 1]));
}
