use super::*;
use serde_json::json;
use tempfile::TempDir;

fn store(dir: &TempDir) -> ProfileStore {
    let path = dir.path().join("profiles.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({
            "theme":"blue",
            "profiles":{
                "work":{"user":"dev","host":"Box.Example","auth":"none","custom":{"leave":"alone"}},
                "other":{"user":"admin","host":"Box.Example","auth":"none"}
            }
        }))
        .unwrap(),
    )
    .unwrap();
    ProfileStore::new(path).unwrap()
}

fn peer() -> BookmarkPeer {
    BookmarkPeer {
        host: "box.example".into(),
        user: "dev".into(),
        port: 22,
        fingerprint: "SHA256:first".into(),
    }
}

#[test]
fn bookmarks_round_trip_and_share_neither_fingerprints_profiles_nor_catalogs() {
    let first = tempfile::tempdir().unwrap();
    let s = store(&first);
    let key = peer();
    let saved = "/srv/crew's 日本 $tree";
    assert_eq!(
        s.add_bookmark("work", &key, &format!("{saved}/")).unwrap(),
        [saved]
    );
    assert_eq!(s.add_bookmark("work", &key, "/").unwrap(), ["/", saved]);
    assert_eq!(s.add_bookmark("work", &key, saved).unwrap(), ["/", saved]);
    let second = ProfileStore::new(s.path().into()).unwrap();
    assert_eq!(second.list_bookmarks("work", &key).unwrap(), ["/", saved]);
    let mut other_key = key.clone();
    other_key.fingerprint = "SHA256:second".into();
    assert!(s.list_bookmarks("work", &other_key).unwrap().is_empty());
    s.add_bookmark("work", &other_key, "/after-key-change")
        .unwrap();
    assert!(s.list_bookmarks("other", &key).is_err());
    assert!(
        s.list_bookmarks(
            "other",
            &BookmarkPeer {
                user: "admin".into(),
                ..key.clone()
            }
        )
        .unwrap()
        .is_empty()
    );
    assert!(
        store(&tempfile::tempdir().unwrap())
            .list_bookmarks("work", &key)
            .unwrap()
            .is_empty()
    );

    let (_, snapshot) = s.inspect("work").unwrap();
    s.edit("work", "renamed", &snapshot.options, &snapshot.options)
        .unwrap();
    assert!(second.remove_bookmark("work", &key, saved).is_err());
    assert_eq!(s.list_bookmarks("renamed", &key).unwrap(), ["/", saved]);
    let (_, renamed) = s.inspect("renamed").unwrap();
    s.duplicate(&renamed, "copy", &renamed.options).unwrap();
    assert!(s.list_bookmarks("copy", &key).unwrap().is_empty());
    assert_eq!(s.remove_bookmark("renamed", &key, saved).unwrap(), ["/"]);
    assert!(s.remove_bookmark("renamed", &key, "/").unwrap().is_empty());
    assert_eq!(
        s.list_bookmarks("renamed", &other_key).unwrap(),
        ["/after-key-change"]
    );
    assert!(
        s.remove_bookmark("renamed", &other_key, "/after-key-change")
            .unwrap()
            .is_empty()
    );
    let doc: Value = serde_json::from_slice(&std::fs::read(s.path()).unwrap()).unwrap();
    for name in ["renamed", "copy"] {
        assert!(doc["profiles"][name].get(FIELD).is_none());
        assert_eq!(doc["profiles"][name]["custom"], json!({"leave":"alone"}));
    }
    assert_eq!(doc["theme"], "blue");
}

#[test]
fn bookmark_writes_validate_the_live_peer_and_fail_without_replacing_invalid_data() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(&dir);
    let key = peer();
    let (_, before) = s.inspect("work").unwrap();
    let pinned = SshOptions {
        host_key: Some(key.fingerprint.clone()),
        ..before.options.clone()
    };
    s.edit("work", "work", &pinned, &before.options).unwrap();
    let original = std::fs::read(s.path()).unwrap();
    for invalid in [
        BookmarkPeer {
            host: "other.example".into(),
            ..key.clone()
        },
        BookmarkPeer {
            user: "other".into(),
            ..key.clone()
        },
        BookmarkPeer {
            port: 2222,
            ..key.clone()
        },
        BookmarkPeer {
            fingerprint: "SHA256:other".into(),
            ..key.clone()
        },
    ] {
        assert!(s.add_bookmark("work", &invalid, "/work").is_err());
    }
    for invalid in [
        "", ".", "relative", "/../top", "/a/.", "//", "//a", "/a//b", "/a\\b", "/a\nb", "/a\0b",
    ] {
        assert!(
            s.add_bookmark("work", &key, invalid).is_err(),
            "{invalid:?}"
        );
    }
    assert_eq!(std::fs::read(s.path()).unwrap(), original);
    let mut doc: Value = serde_json::from_slice(&original).unwrap();
    doc["profiles"]["work"][FIELD] = json!({"future_schema": 2});
    let malformed = serde_json::to_vec(&doc).unwrap();
    std::fs::write(s.path(), &malformed).unwrap();
    assert!(s.list_bookmarks("work", &key).is_err());
    assert!(s.remove_bookmark("work", &key, "/work").is_err());
    assert_eq!(std::fs::read(s.path()).unwrap(), malformed);
}

#[test]
fn concurrent_bookmark_edits_merge_with_profile_changes_after_contention() {
    let dir = tempfile::tempdir().unwrap();
    let a = store(&dir);
    let b = ProfileStore::new(a.path().into()).unwrap();
    let key = peer();
    let (_, profile) = a.inspect("work").unwrap();
    a.add_bookmark("work", &key, "/a").unwrap();
    assert_eq!(b.list_bookmarks("work", &key).unwrap(), ["/a"]);
    let lock = a.lock().unwrap();
    assert!(b.add_bookmark("work", &key, "/b").is_err());
    drop(lock);
    b.add_bookmark("work", &key, "/b").unwrap();
    a.edit("work", "work", &profile.options, &profile.options)
        .unwrap();
    assert_eq!(b.remove_bookmark("work", &key, "/a").unwrap(), ["/b"]);
    assert_eq!(a.list_bookmarks("work", &key).unwrap(), ["/b"]);
}
