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
