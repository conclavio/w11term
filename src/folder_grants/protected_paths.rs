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

pub(super) fn check(root: &FolderRoot, scope: &FolderScope) -> Result<()> {
    let selected =
        volume_path(root.directory_handle()).context("Checking the selected writable folder")?;
    // A first grant may precede creation of the permissions store. Establish
    // its real directory before comparing locations or writing any grant.
    std::fs::create_dir_all(&scope.store_dir).context("Checking the w11term data directory")?;
    let data = protected_path(&scope.store_dir).context("Checking the w11term data directory")?;
    let catalog = protected_path(&scope.catalog).context("Checking the w11term profile catalog")?;
    if contains(&selected, &data)? || contains(&data, &selected)? || contains(&selected, &catalog)?
    {
        bail!(
            "Write access would expose w11term's data directory or profile catalog. Choose a different folder or share it read-only."
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
}
