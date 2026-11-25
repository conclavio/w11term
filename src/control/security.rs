//! Authenticate both ends of the local control pipe, including integrity level.
use anyhow::{Context, Result, ensure};
use std::os::windows::io::AsRawHandle;
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, ServerOptions};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            },
            *,
        },
        System::{
            Pipes::{GetNamedPipeServerProcessId, ImpersonateNamedPipeClient},
            Threading::{
                GetCurrentProcess, GetCurrentThread, OpenProcess, OpenProcessToken,
                OpenThreadToken, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
    },
    core::{PCWSTR, PWSTR},
};

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
struct LocalMemory(*mut core::ffi::c_void);
impl Drop for LocalMemory {
    fn drop(&mut self) {
        unsafe {
            LocalFree(Some(HLOCAL(self.0)));
        }
    }
}
struct CoTaskMem(*mut core::ffi::c_void);
impl Drop for CoTaskMem {
    fn drop(&mut self) {
        unsafe {
            windows::Win32::System::Com::CoTaskMemFree(Some(self.0));
        }
    }
}

#[derive(Debug, Clone)]
struct Identity {
    sid: String,
    integrity: u32,
}
pub(crate) const HIGH_INTEGRITY_RID: u32 = 0x3000;

struct PinnedElevatedDir {
    path: std::path::PathBuf,
    // Hold open handles without FILE_SHARE_DELETE so Windows refuses to rename
    // or delete the elevated directory or any ancestor for the process lifetime.
    _handles: Vec<std::fs::File>,
}

static ELEVATED_DIR: std::sync::OnceLock<PinnedElevatedDir> = std::sync::OnceLock::new();

pub(crate) fn is_elevated() -> Result<bool> {
    Ok(Identity::current()?.integrity >= HIGH_INTEGRITY_RID)
}

/// Return a verified, kernel-pinned directory under `FOLDERID_ProgramData\w11term\elevated\<sid>`
/// isolated to High-integrity processes. The base directory is resolved through the
/// Windows known-folder API rather than the inherited `ProgramData` environment variable,
/// and every component from the root down is verified for non-reparse identity and kept
/// open without `FILE_SHARE_DELETE` so no ancestor or component can be renamed or swapped
/// with a junction while the process runs.
pub(crate) fn elevated_data_dir() -> Result<std::path::PathBuf> {
    let identity = Identity::current()?;
    ensure!(
        identity.integrity >= HIGH_INTEGRITY_RID,
        "elevated directory requires High integrity"
    );
    if let Some(pinned) = ELEVATED_DIR.get() {
        return Ok(pinned.path.clone());
    }

    let program_data = program_data_dir()?;
    let (root_handle, mut parent_final) = verify_non_reparse_root(&program_data)?;

    let admin_sddl = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)S:(ML;OICI;NWNR;;;HI)".to_string();
    let user_sddl = format!(
        "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{})S:(ML;OICI;NWNR;;;S-1-16-{})",
        identity.sid, identity.integrity
    );

    let mut current = program_data;
    let mut handles = Vec::with_capacity(4);
    handles.push(root_handle);
    for (component, sddl, allow_user_owner) in [
        ("w11term", admin_sddl.as_str(), false),
        ("elevated", admin_sddl.as_str(), false),
        (identity.sid.as_str(), user_sddl.as_str(), true),
    ] {
        current = current.join(component);
        let (handle, final_path) = ensure_pinned_elevated_component(
            &current,
            &parent_final,
            component,
            sddl,
            &identity.sid,
            allow_user_owner,
        )?;
        parent_final = final_path;
        handles.push(handle);
    }

    let pinned = ELEVATED_DIR.get_or_init(|| PinnedElevatedDir {
        path: current,
        _handles: handles,
    });
    Ok(pinned.path.clone())
}

fn program_data_dir() -> Result<std::path::PathBuf> {
    use windows::Win32::UI::Shell::{
        FOLDERID_ProgramData, KNOWN_FOLDER_FLAG, SHGetKnownFolderPath,
    };
    let raw = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, KNOWN_FOLDER_FLAG(0), None) }
        .context("resolving FOLDERID_ProgramData")?;
    ensure!(!raw.0.is_null(), "FOLDERID_ProgramData returned null");
    let _mem = CoTaskMem(raw.0.cast());
    let path = unsafe { raw.to_string()? };
    ensure!(!path.is_empty(), "FOLDERID_ProgramData returned empty path");
    Ok(std::path::PathBuf::from(path))
}

fn volume_path(file: &std::fs::File) -> Result<Vec<u16>> {
    use windows::Win32::Storage::FileSystem::{GetFinalPathNameByHandleW, VOLUME_NAME_GUID};
    let mut buf = vec![0u16; 32_768];
    let len = unsafe {
        GetFinalPathNameByHandleW(HANDLE(file.as_raw_handle()), &mut buf, VOLUME_NAME_GUID)
    } as usize;
    ensure!(len > 0, "GetFinalPathNameByHandleW failed");
    ensure!(len < buf.len(), "Windows path exceeds the supported length");
    buf.truncate(len);
    Ok(buf)
}

fn verify_non_reparse_root(path: &std::path::Path) -> Result<(std::fs::File, Vec<u16>)> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, GetFileInformationByHandle,
    };

    // Open with FILE_TRAVERSE (so NT share-access checks engage) and without
    // FILE_SHARE_DELETE or reparse traversal so this handle pins the root in place.
    let dir = std::fs::OpenOptions::new()
        .access_mode((FILE_TRAVERSE | FILE_READ_ATTRIBUTES).0)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
        .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0)
        .open(path)
        .with_context(|| format!("opening elevated base directory {}", path.display()))?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe {
        GetFileInformationByHandle(HANDLE(dir.as_raw_handle()), &mut info)?;
    }
    ensure!(
        (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0) != 0
            && (info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0) == 0,
        "elevated base directory {} must be a real directory, not a reparse point",
        path.display()
    );
    let final_path = volume_path(&dir)?;
    Ok((dir, final_path))
}

fn ensure_pinned_elevated_component(
    path: &std::path::Path,
    parent_final: &[u16],
    component: &str,
    sddl: &str,
    user_sid: &str,
    allow_user_owner: bool,
) -> Result<(std::fs::File, Vec<u16>)> {
    use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt};
    use windows::Win32::{
        Foundation::{ERROR_ALREADY_EXISTS, ERROR_SUCCESS},
        Globalization::{CSTR_EQUAL, CompareStringOrdinal},
        Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE,
            GetFileInformationByHandle, READ_CONTROL,
        },
    };

    let sddl_wide: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl_wide.as_ptr()),
            1,
            &mut descriptor,
            None,
        )?;
    }
    let _descriptor = LocalMemory(descriptor.0);
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    if let Err(error) = unsafe { CreateDirectoryW(PCWSTR(wide.as_ptr()), Some(&attributes)) } {
        ensure!(
            error.code() == ERROR_ALREADY_EXISTS.to_hresult(),
            "creating elevated directory {}: {error}",
            path.display()
        );
    }

    // Open with FILE_TRAVERSE (engaging NT share-access checks) and without
    // FILE_SHARE_DELETE or reparse traversal so this handle locks the directory in place.
    let dir = std::fs::OpenOptions::new()
        .access_mode((FILE_TRAVERSE | FILE_READ_ATTRIBUTES | READ_CONTROL).0)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
        .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0)
        .open(path)
        .with_context(|| format!("opening elevated directory {}", path.display()))?;
    let handle = HANDLE(dir.as_raw_handle());
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe {
        GetFileInformationByHandle(handle, &mut info)?;
    }
    ensure!(
        (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0) != 0
            && (info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0) == 0,
        "elevated directory {} must be a real directory, not a reparse point",
        path.display()
    );

    let actual_final = volume_path(&dir)?;
    let mut expected_final = parent_final.to_vec();
    if expected_final.last() != Some(&u16::from(b'\\')) {
        expected_final.push(u16::from(b'\\'));
    }
    expected_final.extend(component.encode_utf16());
    ensure!(
        unsafe { CompareStringOrdinal(&actual_final, &expected_final, true) } == CSTR_EQUAL,
        "elevated directory {} resolved outside its verified parent",
        path.display()
    );

    let mut owner_sid = PSID::default();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION,
            Some(&mut owner_sid),
            None,
            None,
            None,
            Some(&mut sd),
        )
    };
    ensure!(
        status == ERROR_SUCCESS,
        "reading security descriptor of {}: {status:?}",
        path.display()
    );
    let _sd = LocalMemory(sd.0);

    ensure!(
        !owner_sid.0.is_null() && unsafe { IsValidSid(owner_sid).as_bool() },
        "elevated directory {} has invalid owner SID",
        path.display()
    );
    let owner_text = unsafe {
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(owner_sid, &mut text)?;
        let _text = LocalMemory(text.0.cast());
        text.to_string()?
    };
    let trusted_owner = owner_text == "S-1-5-18"
        || owner_text == "S-1-5-32-544"
        || (allow_user_owner && owner_text == user_sid);
    ensure!(
        trusted_owner,
        "elevated directory {} has unexpected owner {owner_text}",
        path.display()
    );

    let label_sddl = label_sddl_string(sd)?;
    let label_rid = parse_high_mandatory_label(&label_sddl)?;
    ensure!(
        label_rid >= HIGH_INTEGRITY_RID,
        "elevated directory {} lacks a High-integrity No-Write-Up mandatory label ({label_sddl})",
        path.display()
    );
    Ok((dir, actual_final))
}

fn label_sddl_string(sd: PSECURITY_DESCRIPTOR) -> Result<String> {
    use windows::Win32::Security::Authorization::ConvertSecurityDescriptorToStringSecurityDescriptorW;
    let mut text = PWSTR::null();
    unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            1,
            LABEL_SECURITY_INFORMATION,
            &mut text,
            None,
        )?;
    }
    ensure!(!text.0.is_null(), "missing label SDDL string");
    let _text = LocalMemory(text.0.cast());
    Ok(unsafe { text.to_string()? })
}

fn has_no_write_up(rights: &str) -> bool {
    if let Some(hex) = rights
        .strip_prefix("0x")
        .or_else(|| rights.strip_prefix("0X"))
    {
        return u32::from_str_radix(hex, 16).is_ok_and(|mask| (mask & 1) != 0);
    }
    let (pairs, rem) = rights.as_bytes().as_chunks::<2>();
    rem.is_empty() && pairs.contains(b"NW")
}

/// Parse the mandatory integrity RID from an SDDL `LABEL_SECURITY_INFORMATION`
/// string (e.g. `S:(ML;OICI;NWNR;;;HI)` or `S:(ML;;NW;;;S-1-16-12288)`),
/// requiring No-Write-Up (`NW`) enforcement.
fn parse_high_mandatory_label(sddl: &str) -> Result<u32> {
    let Some(rest) = sddl.split_once("S:").map(|(_, tail)| tail) else {
        return Ok(0);
    };
    let mut highest = 0u32;
    let mut slice = rest;
    while let Some(start) = slice.find('(') {
        let after = &slice[start + 1..];
        let Some(end) = after.find(')') else {
            break;
        };
        let ace = &after[..end];
        slice = &after[end + 1..];
        let parts: Vec<&str> = ace.split(';').collect();
        if parts.len() == 6 && parts[0] == "ML" && has_no_write_up(parts[2]) {
            let rid = match parts[5] {
                "LW" => 0x1000,
                "ME" => 0x2000,
                "MP" => 0x2100,
                "HI" => HIGH_INTEGRITY_RID,
                "SI" => 0x4000,
                sid => sid
                    .strip_prefix("S-1-16-")
                    .and_then(|num| num.parse::<u32>().ok())
                    .unwrap_or(0),
            };
            highest = highest.max(rid);
        }
    }
    Ok(highest)
}

pub(super) fn current_scope() -> Result<(String, u32, u32)> {
    let user = Identity::current()?;
    let mut session = 0;
    unsafe {
        windows::Win32::System::RemoteDesktop::ProcessIdToSessionId(
            std::process::id(),
            &mut session,
        )?;
    }
    Ok((user.sid, user.integrity, session))
}

impl Identity {
    fn current() -> Result<Self> {
        let mut token = HANDLE::default();
        unsafe {
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        }
        Self::token(&Handle(token))
    }
    fn process(pid: u32) -> Result<Self> {
        let process =
            Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)? });
        let mut token = HANDLE::default();
        unsafe {
            OpenProcessToken(process.0, TOKEN_QUERY, &mut token)?;
        }
        Self::token(&Handle(token))
    }
    fn token(token: &Handle) -> Result<Self> {
        fn information(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> Result<Vec<usize>> {
            let mut length = 0;
            unsafe {
                let _ = GetTokenInformation(token, class, None, 0, &mut length);
            }
            ensure!(length > 0, "unable to size token information");
            let mut storage =
                vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
            unsafe {
                GetTokenInformation(
                    token,
                    class,
                    Some(storage.as_mut_ptr().cast()),
                    length,
                    &mut length,
                )?;
            }
            Ok(storage)
        }
        let user = information(token.0, TokenUser)?;
        let label = information(token.0, TokenIntegrityLevel)?;
        // SAFETY: the OS populated aligned buffers for the requested token classes;
        // each embedded SID remains alive until conversion/extraction completes.
        unsafe {
            let user = &*user.as_ptr().cast::<TOKEN_USER>();
            let label = &*label.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
            let mut text = PWSTR::null();
            ConvertSidToStringSidW(user.User.Sid, &mut text)?;
            let _text = LocalMemory(text.0.cast());
            let sid = text.to_string()?;
            let count = *GetSidSubAuthorityCount(label.Label.Sid);
            ensure!(count > 0, "invalid integrity SID");
            let integrity = *GetSidSubAuthority(label.Label.Sid, u32::from(count - 1));
            Ok(Self { sid, integrity })
        }
    }
    fn authorize_client(&self, client: &Self) -> Result<()> {
        ensure!(
            client.sid == self.sid || client.sid == "S-1-5-18",
            "control client belongs to another user"
        );
        ensure!(
            client.integrity >= self.integrity,
            "control client has lower integrity than the server"
        );
        Ok(())
    }
}

// No await or dispatch may occur while impersonating. Failure to revert must
// terminate the process, rather than let a worker continue in the wrong context.
struct Impersonation;
impl Drop for Impersonation {
    fn drop(&mut self) {
        if unsafe { RevertToSelf() }.is_err() {
            std::process::abort();
        }
    }
}

pub(super) struct PipeSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
    identity: Identity,
}
// The descriptor and identity are immutable and live through each CreateNamedPipe.
unsafe impl Send for PipeSecurity {}
unsafe impl Sync for PipeSecurity {}
impl PipeSecurity {
    pub(super) fn new() -> Result<Self> {
        let identity = Identity::current()?;
        let sddl: Vec<u16> = format!(
            "D:P(A;;GA;;;SY)(A;;GA;;;{})S:(ML;;NWNR;;;S-1-16-{})",
            identity.sid, identity.integrity
        )
        .encode_utf16()
        .chain(Some(0))
        .collect();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                1,
                &mut descriptor,
                None,
            )?;
        }
        Ok(Self {
            descriptor,
            identity,
        })
    }
    pub(super) fn create(&self, name: &str, first: bool) -> Result<NamedPipeServer> {
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.descriptor.0,
            bInheritHandle: false.into(),
        };
        // SAFETY: both descriptor and attributes outlive the synchronous create.
        Ok(unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(
                    name,
                    (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
                )?
        })
    }
    pub(super) fn authorize(&self, pipe: &NamedPipeServer) -> Result<()> {
        // Read at least one request byte before this call so Windows can identify
        // the client. Scope is synchronous and the guard always reverts it.
        unsafe {
            ImpersonateNamedPipeClient(HANDLE(pipe.as_raw_handle()))?;
        }
        let _revert = Impersonation;
        let mut token = HANDLE::default();
        unsafe {
            OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &mut token)?;
        }
        self.identity
            .authorize_client(&Identity::token(&Handle(token))?)
    }
}
impl Drop for PipeSecurity {
    fn drop(&mut self) {
        unsafe {
            LocalFree(Some(HLOCAL(self.descriptor.0)));
        }
    }
}

pub(super) fn verify_server(pipe: &NamedPipeClient, expected_pid: u32) -> Result<()> {
    let mut actual_pid = 0;
    unsafe {
        GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &mut actual_pid)?;
    }
    ensure!(
        actual_pid == expected_pid,
        "control pipe server PID differs from discovery record"
    );
    let server = Identity::process(actual_pid).context("unable to verify control server token")?;
    let caller = Identity::current()?;
    ensure!(
        server.sid == caller.sid,
        "control pipe server belongs to another user"
    );
    ensure!(
        server.integrity >= caller.integrity,
        "control pipe server has lower integrity than the client"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::windows::named_pipe::ClientOptions,
    };
    use windows::Win32::{
        Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT},
        Storage::FileSystem::SECURITY_IDENTIFICATION,
        System::SystemServices::{
            SE_GROUP_INTEGRITY, SYSTEM_MANDATORY_LABEL_ACE_TYPE, SYSTEM_MANDATORY_LABEL_NO_READ_UP,
            SYSTEM_MANDATORY_LABEL_NO_WRITE_UP,
        },
    };
    #[test]
    fn policy_rejects_lower_integrity_and_other_users() {
        let id = |sid: &str, integrity| Identity {
            sid: sid.into(),
            integrity,
        };
        let server = id("owner", 12288);
        assert!(server.authorize_client(&id("owner", 8192)).is_err());
        assert!(server.authorize_client(&id("other", 12288)).is_err());
        assert!(server.authorize_client(&id("owner", 12288)).is_ok());
        assert!(server.authorize_client(&id("owner", 16384)).is_ok());
        assert!(server.authorize_client(&id("S-1-5-18", 16384)).is_ok());
    }
    #[tokio::test]
    async fn native_pipe_has_mandatory_policy_and_authenticates_both_ends() {
        let mut security = PipeSecurity::new().unwrap();
        let name = format!("\\\\.\\pipe\\w11term-security-test-{}", std::process::id());
        let mut server = security.create(&name, true).unwrap();
        unsafe {
            let mut sd = PSECURITY_DESCRIPTOR::default();
            let mut sacl = std::ptr::null_mut();
            GetSecurityInfo(
                HANDLE(server.as_raw_handle()),
                SE_KERNEL_OBJECT,
                LABEL_SECURITY_INFORMATION,
                None,
                None,
                None,
                Some(&mut sacl),
                Some(&mut sd),
            )
            .ok()
            .unwrap();
            let _sd = LocalMemory(sd.0);
            assert!(!sacl.is_null());
            let mut ace = std::ptr::null_mut();
            GetAce(sacl, 0, &mut ace).unwrap();
            let label = &*ace.cast::<SYSTEM_MANDATORY_LABEL_ACE>();
            assert_eq!(label.Header.AceType, SYSTEM_MANDATORY_LABEL_ACE_TYPE as u8);
            assert_eq!(
                label.Mask,
                SYSTEM_MANDATORY_LABEL_NO_WRITE_UP | SYSTEM_MANDATORY_LABEL_NO_READ_UP
            );
            let sid = PSID((&label.SidStart as *const u32).cast_mut().cast());
            assert_eq!(
                *GetSidSubAuthority(sid, u32::from(*GetSidSubAuthorityCount(sid) - 1)),
                security.identity.integrity
            );
        }
        let mut client = ClientOptions::new()
            .security_qos_flags(SECURITY_IDENTIFICATION.0)
            .open(&name)
            .unwrap();
        verify_server(&client, std::process::id()).unwrap();
        assert!(verify_server(&client, std::process::id().wrapping_add(1)).is_err());
        client.write_all(b"x").await.unwrap();
        server.connect().await.unwrap();
        let mut byte = [0];
        server.read_exact(&mut byte).await.unwrap();
        security.authorize(&server).unwrap();
        security.identity.integrity += 1;
        assert!(
            security.authorize(&server).is_err(),
            "authorization failure must also revert impersonation"
        );
        let mut token = HANDLE::default();
        assert!(
            unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &mut token) }.is_err(),
            "impersonation must be reverted"
        );
    }

    fn lower_integrity_open_is_denied(server_level: u32, client_level: u32) {
        use windows::Win32::{
            Security::Authorization::ConvertStringSidToSidW,
            Storage::FileSystem::{
                CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
                FILE_SHARE_NONE, OPEN_EXISTING,
            },
            System::Threading::SetThreadToken,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _enter = runtime.enter();
        let security = PipeSecurity::new().unwrap();
        assert!(
            security.identity.integrity >= server_level,
            "this native test requires a server token at integrity {server_level}"
        );
        let name = format!(
            "\\\\.\\pipe\\w11term-integrity-test-{}-{client_level}",
            std::process::id()
        );
        let _server = security.create(&name, true).unwrap();
        std::thread::spawn(move || unsafe {
            let mut original = HANDLE::default();
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_DUPLICATE,
                &mut original,
            )
            .unwrap();
            let original = Handle(original);
            let mut duplicate = HANDLE::default();
            DuplicateTokenEx(
                original.0,
                TOKEN_QUERY | TOKEN_ADJUST_DEFAULT | TOKEN_IMPERSONATE,
                None,
                SecurityImpersonation,
                TokenImpersonation,
                &mut duplicate,
            )
            .unwrap();
            let duplicate = Handle(duplicate);
            let text: Vec<u16> = format!("S-1-16-{client_level}")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let mut sid = PSID::default();
            ConvertStringSidToSidW(PCWSTR(text.as_ptr()), &mut sid).unwrap();
            let _sid = LocalMemory(sid.0);
            let label = TOKEN_MANDATORY_LABEL {
                Label: SID_AND_ATTRIBUTES {
                    Sid: sid,
                    Attributes: SE_GROUP_INTEGRITY as u32,
                },
            };
            SetTokenInformation(
                duplicate.0,
                TokenIntegrityLevel,
                (&label as *const TOKEN_MANDATORY_LABEL).cast(),
                std::mem::size_of_val(&label) as u32 + GetLengthSid(sid),
            )
            .unwrap();
            SetThreadToken(None, Some(duplicate.0)).unwrap();
            let _revert = Impersonation;
            let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            let result = CreateFileW(
                PCWSTR(name.as_ptr()),
                FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(SECURITY_IDENTIFICATION.0 | 0x0010_0000),
                None,
            );
            if let Ok(handle) = result {
                let _handle = Handle(handle);
                panic!("lower-integrity client opened protected pipe");
            }
            assert_eq!(
                result.unwrap_err().code().0 as u32,
                0x80070005,
                "must fail with access denied"
            );
        })
        .join()
        .unwrap();
    }
    #[test]
    fn mandatory_label_sddl_parser_requires_high_no_write_up() {
        assert_eq!(
            parse_high_mandatory_label("S:(ML;OICI;NWNR;;;HI)").unwrap(),
            HIGH_INTEGRITY_RID
        );
        assert_eq!(
            parse_high_mandatory_label("S:AI(ML;;NW;;;SI)").unwrap(),
            0x4000
        );
        assert_eq!(
            parse_high_mandatory_label("S:(ML;OICI;NWNR;;;S-1-16-12288)").unwrap(),
            HIGH_INTEGRITY_RID
        );
        assert_eq!(
            parse_high_mandatory_label("S:(ML;;0x1;;;HI)").unwrap(),
            HIGH_INTEGRITY_RID
        );
        assert_eq!(parse_high_mandatory_label("S:(ML;;NR;;;HI)").unwrap(), 0);
        assert_eq!(parse_high_mandatory_label("S:(ML;;0x2;;;HI)").unwrap(), 0);
        assert_eq!(
            parse_high_mandatory_label("S:(ML;;NW;;;ME)").unwrap(),
            0x2000
        );
        assert_eq!(parse_high_mandatory_label("").unwrap(), 0);
    }
    #[test]
    fn program_data_known_folder_and_root_pinning_work() {
        let root = program_data_dir().expect("FOLDERID_ProgramData must resolve");
        assert!(root.is_absolute() && root.is_dir());
        let (_pinned_root, final_path) =
            verify_non_reparse_root(&root).expect("ProgramData must pin as a real directory");
        assert!(!final_path.is_empty());

        // Retaining the verified root handle must prevent renaming that directory.
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("pinned-root");
        std::fs::create_dir(&target).unwrap();
        let (handle, _) = verify_non_reparse_root(&target).unwrap();
        assert!(std::fs::rename(&target, temp.path().join("moved")).is_err());
        drop(handle);
        std::fs::rename(&target, temp.path().join("moved")).unwrap();
    }
    #[test]
    fn native_medium_server_refuses_low_integrity_client() {
        lower_integrity_open_is_denied(8192, 4096);
    }
    #[test]
    #[ignore = "requires an elevated test process; run explicitly from an administrator terminal"]
    fn native_high_server_refuses_medium_integrity_client() {
        lower_integrity_open_is_denied(12288, 8192);
    }
}
