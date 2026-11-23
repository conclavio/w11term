use super::LocalOptions;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
};
use windows::{
    Win32::{
        Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS},
        System::{
            Registry::{
                HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW,
                RegGetValueW, RegOpenKeyExW,
            },
            SystemInformation::GetSystemDirectoryW,
        },
    },
    core::{PCWSTR, PWSTR, w},
};

struct RegistryKey(HKEY);

impl Drop for RegistryKey {
    fn drop(&mut self) {
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

fn launcher() -> Option<PathBuf> {
    let capacity = unsafe { GetSystemDirectoryW(None) };
    if capacity == 0 {
        return None;
    }
    let mut directory = vec![0; capacity as usize];
    let written = unsafe { GetSystemDirectoryW(Some(&mut directory)) };
    (written > 0 && written < capacity)
        .then(|| PathBuf::from(OsString::from_wide(&directory[..written as usize])).join("wsl.exe"))
}

fn distribution_name(root: HKEY, subkey: PCWSTR) -> Option<String> {
    let field = w!("DistributionName");
    let mut bytes = 0;
    if unsafe {
        RegGetValueW(
            root,
            subkey,
            field,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut bytes),
        )
    } != ERROR_SUCCESS
    {
        return None;
    }
    // Leave room for RegGetValueW to terminate a string stored without its NUL.
    let mut data = vec![0u16; (bytes as usize).div_ceil(2) + 1];
    bytes = u32::try_from(data.len() * 2).ok()?;
    if unsafe {
        RegGetValueW(
            root,
            subkey,
            field,
            RRF_RT_REG_SZ,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    } != ERROR_SUCCESS
        || !bytes.is_multiple_of(2)
    {
        return None;
    }
    let value = data.get(..bytes as usize / 2)?;
    String::from_utf16(value.strip_suffix(&[0])?).ok()
}

fn profile(executable: &Path, name: String) -> Option<(String, LocalOptions)> {
    if name.trim().is_empty()
        || name.chars().any(char::is_control)
        || [
            "docker-desktop",
            "docker-desktop-data",
            "rancher-desktop",
            "rancher-desktop-data",
        ]
        .iter()
        .any(|helper| name.eq_ignore_ascii_case(helper))
    {
        return None;
    }
    let label = format!("WSL: {name}");
    let arguments = vec!["--distribution".into(), name, "--cd".into(), "~".into()];
    Some((
        label,
        LocalOptions::discovered(executable.to_owned(), arguments),
    ))
}

fn discover(executable: PathBuf, registration_key: PCWSTR) -> BTreeMap<String, LocalOptions> {
    let mut profiles = BTreeMap::new();
    if !executable.is_absolute() || !executable.is_file() {
        return profiles;
    }
    let mut root = HKEY::default();
    if unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            registration_key,
            None,
            KEY_READ,
            &mut root,
        )
    } != ERROR_SUCCESS
    {
        return profiles;
    }
    let root = RegistryKey(root);
    // A distribution registration uses a 38-character, braced GUID as its subkey.
    for index in 0..u32::MAX {
        let mut subkey = [0u16; 39];
        let mut length = subkey.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                root.0,
                index,
                Some(PWSTR(subkey.as_mut_ptr())),
                &mut length,
                None,
                None,
                None,
                None,
            )
        };
        if status == ERROR_MORE_DATA {
            continue;
        }
        if status != ERROR_SUCCESS {
            break;
        }
        if length == 38
            && subkey[0] == u16::from(b'{')
            && subkey[37] == u16::from(b'}')
            && let Some(name) = distribution_name(root.0, PCWSTR(subkey.as_ptr()))
            && let Some((label, options)) = profile(&executable, name)
        {
            profiles.insert(label, options);
        }
    }
    profiles
}

pub(super) fn installed() -> BTreeMap<String, LocalOptions> {
    #[cfg(feature = "ui-smoke")]
    if let Some(fixture) = std::env::var_os("W11TERM_TEST_WSL_FIXTURE").map(PathBuf::from) {
        let Some(id) = fixture.file_name().and_then(|name| name.to_str()) else {
            return BTreeMap::new();
        };
        let fixture_key =
            windows::core::HSTRING::from(format!("Software\\w11term-wsl-fixture-{id}"));
        return discover(fixture.join("wsl.exe"), PCWSTR(fixture_key.as_ptr()));
    }
    launcher().map_or_else(BTreeMap::new, |path| {
        discover(
            path,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Lxss"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_absolute_system_launcher_path() {
        let executable = launcher().expect("Windows system directory");
        assert!(executable.is_absolute());
        assert_eq!(executable.file_name().unwrap(), "wsl.exe");
    }

    #[test]
    fn builds_one_typed_local_invocation_and_skips_utility_instances() {
        let launcher = Path::new(r"C:\Windows\System32\wsl.exe");
        let (name, shell) = profile(launcher, "QA Linux 雪".into()).unwrap();
        assert_eq!(name, "WSL: QA Linux 雪");
        assert_eq!(shell.executable, launcher);
        assert_eq!(shell.args, ["--distribution", "QA Linux 雪", "--cd", "~"]);
        assert!(!shell.powershell_integration);
        for name in [
            "",
            "bad\0name",
            "docker-desktop",
            "Docker-Desktop-Data",
            "rancher-desktop",
        ] {
            assert!(profile(launcher, name.into()).is_none(), "{name:?}");
        }
        assert!(profile(launcher, "docker-desktop-dev".into()).is_some());
    }
}
