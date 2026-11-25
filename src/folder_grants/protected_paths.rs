use super::{FolderRoot, FolderScope};
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::{File, OpenOptions},
    io,
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::Path,
};
use windows::Win32::{
    Foundation::HANDLE,
    Globalization::{CSTR_EQUAL, CompareStringOrdinal},
    Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES, GetFinalPathNameByHandleW,
        VOLUME_NAME_GUID,
    },
};

// Compare the objects Windows actually opened. A short name, junction on the
// configuration side, or second drive letter for a volume must give the same result.
fn volume_path(file: &File) -> Result<Vec<u16>> {
    let mut buf = vec![0u16; 32_768];
    let len = unsafe {
        GetFinalPathNameByHandleW(HANDLE(file.as_raw_handle()), &mut buf, VOLUME_NAME_GUID)
    } as usize;
    if len == 0 {
        return Err(io::Error::last_os_error().into());
    }
    ensure!(len < buf.len(), "Windows path exceeds the supported length");
    buf.truncate(len);
    Ok(buf)
}

fn protected_path(path: &Path) -> Result<Vec<u16>> {
    let file = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(path)?;
    volume_path(&file)
}

fn contains(parent: &[u16], child: &[u16]) -> Result<bool> {
    let Some(prefix) = child.get(..parent.len()) else {
        return Ok(false);
    };
    let slash = u16::from(b'\\');
    if parent.len() != child.len() && parent.last() != Some(&slash) && child[parent.len()] != slash
    {
        return Ok(false);
    }
    let comparison = unsafe { CompareStringOrdinal(parent, prefix, true) };
    if comparison.0 == 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(comparison == CSTR_EQUAL)
}

fn overlaps(a: &[u16], b: &[u16]) -> Result<bool> {
    Ok(contains(a, b)? || contains(b, a)?)
}

fn protected_optional(path: &Path) -> Result<Option<Vec<u16>>> {
    match protected_path(path) {
        Ok(resolved) => Ok(Some(resolved)),
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|io_err| io_err.kind() == io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn check(root: &FolderRoot, scope: &FolderScope) -> Result<()> {
    let selected =
        volume_path(root.directory_handle()).context("Checking the selected writable folder")?;
    // A first grant may precede creation of the permissions store. Establish
    // its real directory before comparing locations or writing any grant.
    std::fs::create_dir_all(&scope.store_dir).context("Checking the w11term data directory")?;
    let data = protected_path(&scope.store_dir).context("Checking the w11term data directory")?;
    let catalog = protected_path(&scope.catalog).context("Checking the w11term profile catalog")?;
    let catalog_parent = scope
        .catalog
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| {
            protected_path(parent).context("Checking the w11term profile catalog directory")
        })
        .transpose()?;
    if overlaps(&selected, &data)?
        || overlaps(&selected, &catalog)?
        || catalog_parent
            .as_deref()
            .is_some_and(|parent| overlaps(&selected, parent).unwrap_or(true))
    {
        bail!(
            "Write access would expose w11term's data directory or profile catalog. Choose a different folder or share it read-only."
        );
    }
    // Protect the user's OpenSSH directory (~/.ssh) so a writable share cannot
    // tamper with known_hosts, private keys, or OpenSSH config. If ~/.ssh does
    // not exist yet, still block sharing the home directory (or its ancestors)
    // writable so a remote host cannot pre-create ~/.ssh/known_hosts.
    if let Some(home) = std::env::home_dir() {
        let ssh_dir = home.join(".ssh");
        if let Some(ssh_resolved) =
            protected_optional(&ssh_dir).context("Checking the OpenSSH directory")?
        {
            if overlaps(&selected, &ssh_resolved)? {
                bail!(
                    "Write access would expose your OpenSSH directory (.ssh). Choose a different folder or share it read-only."
                );
            }
        } else if let Some(home_resolved) =
            protected_optional(&home).context("Checking the user home directory")?
            && contains(&selected, &home_resolved)?
        {
            bail!(
                "Write access would expose your user profile and OpenSSH directory (.ssh). Choose a different folder or share it read-only."
            );
        }
    }
    // Protect the directory containing the running executable against DLL/binary
    // planting when --profiles resides outside the install directory.
    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
        && let Some(exe_resolved) =
            protected_optional(exe_dir).context("Checking the w11term executable directory")?
        && overlaps(&selected, &exe_resolved)?
    {
        bail!(
            "Write access would expose w11term's executable directory. Choose a different folder or share it read-only."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writable_selection_excludes_app_state_and_its_ancestors_but_allows_siblings() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let data = temp.path().join("State").join("Data");
        let catalogs = temp.path().join("Config");
        let catalog = catalogs.join("profiles.json");
        let sibling = temp.path().join("State").join("Database");
        std::fs::create_dir_all(&catalogs)?;
        std::fs::write(&catalog, b"{}")?;
        std::fs::create_dir_all(&sibling)?;
        let scope = FolderScope::new(catalog.clone(), "pilot".into(), data.clone());
        check(&FolderRoot::open(&sibling)?, &scope)?;
        let keys = data.join("keys");
        std::fs::create_dir(&keys)?;
        for blocked in [temp.path(), data.parent().unwrap(), &data, &keys, &catalogs] {
            let root = FolderRoot::open(blocked)?;
            assert!(
                check(&root, &scope)
                    .unwrap_err()
                    .to_string()
                    .contains("Write access would expose"),
                "{blocked:?}"
            );
        }
        // The immutable Windows path comparison is component-aware and ordinal
        // case-insensitive, including non-ASCII names.
        let wide = |path: &str| path.encode_utf16().collect::<Vec<_>>();
        assert!(contains(
            &wide(r"Volume\Données"),
            &wide(r"volume\DONNÉES\Keys")
        )?);
        assert!(!contains(&wide(r"Volume\Data"), &wide(r"volume\database"))?);
        Ok(())
    }

    #[test]
    fn writable_selection_excludes_catalog_parent_even_through_hardlink_and_blocks_exe_dir()
    -> Result<()> {
        let temp = tempfile::tempdir()?;
        let data = temp.path().join("State");
        let real_dir = temp.path().join("Dotfiles");
        let work_dir = temp.path().join("Work");
        let work_sub = work_dir.join("Sub");
        std::fs::create_dir_all(&real_dir)?;
        std::fs::create_dir_all(&work_sub)?;
        let real_catalog = real_dir.join("profiles.json");
        let linked_catalog = work_dir.join("profiles.json");
        std::fs::write(&real_catalog, b"{}")?;
        std::fs::hard_link(&real_catalog, &linked_catalog)?;

        let scope = FolderScope::new(linked_catalog, "pilot".into(), data);
        for blocked in [&work_dir, &work_sub] {
            let root = FolderRoot::open(blocked)?;
            assert!(
                check(&root, &scope)
                    .unwrap_err()
                    .to_string()
                    .contains("Write access would expose"),
                "{blocked:?}"
            );
        }

        if let Ok(exe) = std::env::current_exe()
            && let Some(exe_dir) = exe.parent()
        {
            let root = FolderRoot::open(exe_dir)?;
            assert!(
                check(&root, &scope)
                    .unwrap_err()
                    .to_string()
                    .contains("Write access would expose"),
                "executable directory must be protected"
            );
        }
        Ok(())
    }
}
