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

/// Return a verified, kernel-pinned directory under `%ProgramData%\w11term\elevated\<sid>`
/// isolated to High-integrity processes. Every directory component is created with
/// a protected DACL and High-integrity `NWNR` mandatory label, verified for non-reparse
/// identity and ownership, and kept open without `FILE_SHARE_DELETE` so no ancestor
/// or component can be renamed or swapped with a junction while the process runs.
pub(crate) fn elevated_data_dir() -> Result<std::path::PathBuf> {
    let identity = Identity::current()?;
    ensure!(
        identity.integrity >= HIGH_INTEGRITY_RID,
        "elevated directory requires High integrity"
    );
    if let Some(pinned) = ELEVATED_DIR.get() {
        return Ok(pinned.path.clone());
    }

    let program_data = std::path::PathBuf::from(
        std::env::var_os("ProgramData").context("ProgramData is unavailable")?,
    );
    let mut parent_final = verify_non_reparse_root(&program_data)?;

    let admin_sddl = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)S:(ML;OICI;NWNR;;;HI)".to_string();
    let user_sddl = format!(
        "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{})S:(ML;OICI;NWNR;;;S-1-16-{})",
        identity.sid, identity.integrity
    );

    let mut current = program_data;
    let mut handles = Vec::with_capacity(3);
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

fn verify_non_reparse_root(path: &std::path::Path) -> Result<Vec<u16>> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        GetFileInformationByHandle,
    };

    let dir = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES.0)
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
    volume_path(&dir)
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
            FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
            READ_CONTROL,
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

    // Open without FILE_SHARE_DELETE and without following reparse points so
    // the verified handle locks this directory and its ancestors against rename.
    let dir = std::fs::OpenOptions::new()
        .access_mode((FILE_READ_ATTRIBUTES | READ_CONTROL).0)
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
    let mut sacl: *mut ACL = std::ptr::null_mut();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION,
            Some(&mut owner_sid),
            None,
            None,
            Some(&mut sacl),
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

    let label_rid = unsafe { mandatory_label_rid(sacl)? };
    ensure!(
        label_rid >= HIGH_INTEGRITY_RID,
        "elevated directory {} lacks a High-integrity mandatory label (found {label_rid})",
        path.display()
    );
    Ok((dir, actual_final))
}

unsafe fn mandatory_label_rid(sacl: *const ACL) -> Result<u32> {
    if sacl.is_null() {
        return Ok(0);
    }
    let mut size_info = ACL_SIZE_INFORMATION::default();
    unsafe {
        GetAclInformation(
            sacl,
            (&mut size_info as *mut ACL_SIZE_INFORMATION).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )?;
    }
    for index in 0..size_info.AceCount {
        let mut ace_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        unsafe {
            GetAce(sacl, index, &mut ace_ptr)?;
        }
        if ace_ptr.is_null() {
            continue;
        }
        let header = unsafe { std::ptr::read_unaligned(ace_ptr as *const ACE_HEADER) };
        if header.AceType
            == windows::Win32::System::SystemServices::SYSTEM_MANDATORY_LABEL_ACE_TYPE as u8
            && usize::from(header.AceSize) >= std::mem::size_of::<SYSTEM_MANDATORY_LABEL_ACE>()
        {
            let sid_offset = std::mem::offset_of!(SYSTEM_MANDATORY_LABEL_ACE, SidStart);
            let sid = PSID(unsafe { (ace_ptr as *mut u8).add(sid_offset).cast() });
            ensure!(
                !sid.0.is_null() && unsafe { IsValidSid(sid).as_bool() },
                "invalid mandatory label SID"
            );
            let count_ptr = unsafe { GetSidSubAuthorityCount(sid) };
            ensure!(
                !count_ptr.is_null(),
                "missing mandatory label subauthority count"
            );
            let count = unsafe { *count_ptr };
            ensure!(count > 0, "empty mandatory label SID");
            let rid_ptr = unsafe { GetSidSubAuthority(sid, u32::from(count - 1)) };
            ensure!(!rid_ptr.is_null(), "missing mandatory label RID");
            return Ok(unsafe { *rid_ptr });
        }
    }
    Ok(0)
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
    fn native_medium_server_refuses_low_integrity_client() {
        lower_integrity_open_is_denied(8192, 4096);
    }
    #[test]
    #[ignore = "requires an elevated test process; run explicitly from an administrator terminal"]
    fn native_high_server_refuses_medium_integrity_client() {
        lower_integrity_open_is_denied(12288, 8192);
    }
}
