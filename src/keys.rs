//! In-process device keys. Only public material is returned to the UI.
use anyhow::{Context, Result, ensure};
use russh::keys::{Algorithm, HashAlg, PrivateKey};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
};
use windows::{
    Win32::{
        Foundation::*,
        Security::{Authorization::*, Cryptography::*, *},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    },
    core::{PCWSTR, PWSTR, w},
};
use zeroize::{Zeroize, Zeroizing};

const MAGIC: &[u8] = b"W11TERM-DPAPI-KEY-V1\n";
#[derive(Clone, Debug)]
pub struct DeviceKey {
    pub path: PathBuf,
    pub public_line: String,
    pub fingerprint: String,
    pub device: String,
}
pub fn data_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("W11TERM_DATA_DIR") {
        return Ok(std::path::absolute(path)?);
    }
    Ok(
        PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA unavailable")?)
            .join("w11term"),
    )
}
pub fn profile_file(profile: &str) -> String {
    let stem: String = profile
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .take(48)
        .collect();
    let hash: String = Sha256::digest(profile.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!(
        "{}-{}",
        if stem.is_empty() { "profile" } else { &stem },
        &hash[..12]
    )
}
pub fn current_user_acl(path: &Path) -> Result<()> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let mut len = 0;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buffer = vec![0usize; (len as usize).div_ceil(std::mem::size_of::<usize>())];
        let result = GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            len,
            &mut len,
        );
        let _ = CloseHandle(token);
        result?;
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut sid = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid)?;
        let sid_text = sid.to_string();
        LocalFree(Some(HLOCAL(sid.0.cast())));
        let sddl: Vec<u16> = format!("D:P(A;;FA;;;SY)(A;;FA;;;{})", sid_text?)
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            1,
            &mut descriptor,
            None,
        )?;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let result = SetFileSecurityW(
            PCWSTR(wide.as_ptr()),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
        .ok();
        LocalFree(Some(HLOCAL(descriptor.0)));
        result.context("setting current-user private key ACL")
    }
}
use std::os::windows::ffi::OsStrExt;
fn crypt(data: &[u8], protect: bool) -> Result<Zeroizing<Vec<u8>>> {
    ensure!(data.len() <= u32::MAX as usize, "DPAPI input too large");
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        if protect {
            CryptProtectData(
                &input,
                w!("w11term device key"),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
        .context("Windows user-bound DPAPI")?;
        let bytes = std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize);
        let result = Zeroizing::new(bytes.to_vec());
        bytes.zeroize();
        LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(result)
    }
}
pub fn load(path: &Path) -> Result<PrivateKey> {
    let bytes = Zeroizing::new(
        std::fs::read(path).with_context(|| format!("reading identity {}", path.display()))?,
    );
    if let Some(encrypted) = bytes.strip_prefix(MAGIC) {
        let plain = crypt(encrypted, false)?;
        Ok(PrivateKey::from_openssh(plain.as_slice())?)
    } else {
        russh::keys::load_secret_key(path, None)
            .context("loading OpenSSH identity (use the agent for passphrase-protected keys)")
    }
}
pub fn generate(profile: &str) -> Result<DeviceKey> {
    generate_in(&data_dir()?.join("keys"), profile)
}
pub fn describe(path: &Path) -> Result<DeviceKey> {
    let key = load(path)?;
    Ok(DeviceKey {
        path: path.into(),
        public_line: key.public_key().to_openssh()?,
        fingerprint: key.public_key().fingerprint(HashAlg::Sha256).to_string(),
        device: key.comment().to_string(),
    })
}
pub fn generate_in(directory: &Path, profile: &str) -> Result<DeviceKey> {
    ensure!(
        !profile.trim().is_empty(),
        "name the profile before generating its device key"
    );
    std::fs::create_dir_all(directory)?;
    let path = directory.join(format!("{}.ed25519", profile_file(profile)));
    let device: String = std::env::var("COMPUTERNAME")
        .unwrap_or_else(|_| "w11term-device".into())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(64)
        .collect();
    let key = if path.exists() {
        load(&path)?
    } else {
        let mut key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
        key.set_comment(device.as_str());
        let plain = key.to_openssh(russh::keys::ssh_key::LineEnding::LF)?;
        let encrypted = crypt(plain.as_bytes(), true)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.write_all(MAGIC)?;
        file.write_all(&encrypted)?;
        file.as_file().sync_all()?;
        current_user_acl(file.path())?;
        file.persist_noclobber(&path)
            .context("saving device key without replacing existing identities")?;
        key
    };
    let public_line = key.public_key().to_openssh()?;
    ensure!(
        public_line.starts_with("ssh-ed25519 "),
        "device key must be Ed25519"
    );
    let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let public_path = path.with_extension("ed25519.pub");
    std::fs::write(public_path, format!("{public_line}\n"))?;
    Ok(DeviceKey {
        path,
        public_line,
        fingerprint,
        device,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn user_protected_ed25519_roundtrip_and_public_only_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let key = generate_in(dir.path(), "../../player+pilot").unwrap();
        assert_eq!(key.path.parent(), Some(dir.path()));
        let bytes = std::fs::read(&key.path).unwrap();
        assert!(bytes.starts_with(MAGIC));
        assert!(!bytes.windows(7).any(|w| w == b"PRIVATE"));
        let loaded = load(&key.path).unwrap();
        assert_eq!(
            loaded.public_key().fingerprint(HashAlg::Sha256).to_string(),
            key.fingerprint
        );
        assert_eq!(
            generate_in(dir.path(), "../../player+pilot")
                .unwrap()
                .fingerprint,
            key.fingerprint
        );
        assert!(key.public_line.starts_with("ssh-ed25519 "));
        assert!(!format!("{key:?}").contains("PRIVATE KEY"));
        let mut damaged = bytes;
        *damaged.last_mut().unwrap() ^= 0x80;
        std::fs::write(&key.path, damaged).unwrap();
        assert!(load(&key.path).is_err());
    }
}
