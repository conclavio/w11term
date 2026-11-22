use crate::options::LocalOptions;
use anyhow::{Context, Result, ensure};
use std::{
    ffi::OsStr,
    fs::File,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
};
use wezterm_term::TerminalSize;
use windows::{
    Win32::{
        Foundation::HANDLE,
        System::{Console::*, JobObjects::*, Pipes::CreatePipe, Threading::*},
    },
    core::{PCWSTR, PWSTR},
};

pub(super) fn handle(value: &impl AsRawHandle) -> HANDLE {
    HANDLE(value.as_raw_handle())
}
fn pipe() -> Result<(File, File)> {
    let (mut rx, mut tx) = (HANDLE::default(), HANDLE::default());
    unsafe {
        CreatePipe(&mut rx, &mut tx, None, 0)?;
        Ok((File::from_raw_handle(rx.0), File::from_raw_handle(tx.0)))
    }
}
fn dimensions(size: TerminalSize) -> Result<COORD> {
    ensure!(
        size.cols > 0 && size.rows > 0,
        "a local terminal needs positive rows and columns"
    );
    Ok(COORD {
        X: size
            .cols
            .try_into()
            .context("too many local terminal columns")?,
        Y: size
            .rows
            .try_into()
            .context("too many local terminal rows")?,
    })
}
pub(super) struct Console(HPCON);
impl Console {
    pub fn resize(&self, size: TerminalSize) -> Result<()> {
        unsafe { ResizePseudoConsole(self.0, dimensions(size)?) }.context("resize pseudoconsole")
    }
}
impl Drop for Console {
    fn drop(&mut self) {
        unsafe { ClosePseudoConsole(self.0) }
    }
}

struct Attributes {
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    _storage: Vec<usize>,
}
impl Attributes {
    fn new() -> Result<Self> {
        let mut bytes = 0;
        unsafe {
            let _ = InitializeProcThreadAttributeList(None, 2, None, &mut bytes);
        }
        ensure!(bytes > 0, "could not size process startup attributes");
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        unsafe {
            InitializeProcThreadAttributeList(Some(list), 2, None, &mut bytes)?;
        }
        Ok(Self {
            list,
            _storage: storage,
        })
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.list);
        }
    }
}

// Quote a Windows argv element using CommandLineToArgvW's backslash rules.
fn argument(line: &mut Vec<u16>, arg: &OsStr) {
    let quote = u16::from(b'"');
    let slash = u16::from(b'\\');
    line.push(quote);
    let mut slashes = 0;
    for c in arg.encode_wide() {
        if c == slash {
            slashes += 1;
            continue;
        }
        line.extend(std::iter::repeat_n(
            slash,
            if c == quote { slashes * 2 + 1 } else { slashes },
        ));
        slashes = 0;
        line.push(c);
    }
    line.extend(std::iter::repeat_n(slash, slashes * 2));
    line.push(quote);
}

fn command_line(options: &LocalOptions, host_exe: Option<&Path>) -> Result<Vec<u16>> {
    let mut cmd = Vec::new();
    argument(&mut cmd, options.executable.as_os_str());
    let mut append = |arg: &OsStr| {
        cmd.push(u16::from(b' '));
        argument(&mut cmd, arg);
    };
    for arg in &options.args {
        append(OsStr::new(arg));
    }
    if options.powershell_integration
        && LocalOptions::supports_powershell_integration(&options.executable, &options.args)
    {
        let script = host_exe.map(|exe| exe.with_file_name("shell-integration.ps1"));
        if let Some(script) = script.filter(|path| path.is_absolute() && path.is_file()) {
            if !options
                .args
                .iter()
                .any(|a| a.eq_ignore_ascii_case("-NoExit"))
            {
                append(OsStr::new("-NoExit"));
            }
            // A literal file argument leaves signing and zone checks to PowerShell
            // and never interprets characters in the install path as commands.
            append(OsStr::new("-File"));
            append(script.as_os_str());
        } else {
            log::warn!(
                "PowerShell integration script missing beside w11term; starting the ordinary shell"
            );
        }
    }
    cmd.push(0);
    ensure!(
        cmd.len() <= 32767,
        "local command line exceeds the Windows limit"
    );
    Ok(cmd)
}

pub(super) struct Launch {
    // If an aborted startup future drops Launch before the reader starts, the
    // output pipe and private job must close before ClosePseudoConsole.
    pub input: File,
    pub output: File,
    pub job: OwnedHandle,
    pub console: Console,
    pub process: OwnedHandle,
}
impl Launch {
    pub fn start(options: &LocalOptions, size: TerminalSize) -> Result<Self> {
        options.validate()?;
        ensure!(
            !options
                .executable
                .as_os_str()
                .encode_wide()
                .any(|c| c == u16::from(b'"')),
            "invalid quote in local executable path"
        );
        let mut cmd = command_line(options, std::env::current_exe().ok().as_deref())?;
        let cwd = options
            .cwd
            .as_ref()
            .map(|p| p.as_os_str().encode_wide().chain([0]).collect::<Vec<_>>());
        let child_cwd = cwd.as_ref().map_or(PCWSTR::null(), |p| PCWSTR(p.as_ptr()));
        let (input_read, input) = pipe()?;
        let (output_read, output_write) = pipe()?;
        let console = Console(
            unsafe {
                CreatePseudoConsole(
                    dimensions(size)?,
                    handle(&input_read),
                    handle(&output_write),
                    0,
                )
            }
            .context("create Windows pseudoconsole")?,
        );
        // Failed startup has no reader. Drop this endpoint before the console
        // so ClosePseudoConsole cannot wait for unconsumed output.
        let output = output_read;
        let job = unsafe { OwnedHandle::from_raw_handle(CreateJobObjectW(None, None)?.0) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let attributes = Attributes::new()?;
        let job_handle = handle(&job);
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        // Explicit null handles ask the console subsystem for this child's
        // ConPTY handles, even when w11term's own stdout/stderr are redirected.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.lpAttributeList = attributes.list;
        let mut child = PROCESS_INFORMATION::default();
        unsafe {
            SetInformationJobObject(
                job_handle,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                size_of_val(&limits) as u32,
            )?;
            UpdateProcThreadAttribute(
                attributes.list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                Some(console.0.0 as *const _),
                size_of::<HPCON>(),
                None,
                None,
            )?;
            UpdateProcThreadAttribute(
                attributes.list,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                Some(std::ptr::from_ref(&job_handle).cast()),
                size_of::<HANDLE>(),
                None,
                None,
            )?;
            // Quoted argv[0] allows normal Windows executable lookup for a bare name.
            CreateProcessW(
                None,
                Some(PWSTR(cmd.as_mut_ptr())),
                None,
                None,
                false,
                EXTENDED_STARTUPINFO_PRESENT,
                None,
                child_cwd,
                &si.StartupInfo,
                &mut child,
            )
            .with_context(|| format!("start local executable {}", options.executable.display()))?;
            drop(OwnedHandle::from_raw_handle(child.hThread.0));
            Ok(Self {
                input,
                output,
                job,
                console,
                process: OwnedHandle::from_raw_handle(child.hProcess.0),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_startup_preserves_argv_and_uses_only_the_installed_file() {
        let temp = tempfile::tempdir().unwrap();
        let install = temp.path().join("w11term's $dir [x] & tools");
        std::fs::create_dir(&install).unwrap();
        let exe = install.join("w11term.exe");
        let script = install.join("shell-integration.ps1");
        let mut shell = LocalOptions {
            executable: "pwsh.exe".into(),
            args: vec!["-nOlOgO".into()],
            cwd: None,
            scrollback_lines: Default::default(),
            color_scheme: Default::default(),
            powershell_integration: true,
        };
        let line = |shell: &LocalOptions, exe: Option<&Path>| {
            let utf16 = command_line(shell, exe).unwrap();
            String::from_utf16(utf16.strip_suffix(&[0]).unwrap()).unwrap()
        };
        let baseline = r#""pwsh.exe" "-nOlOgO""#;
        assert_eq!(line(&shell, None), baseline);
        assert_eq!(line(&shell, Some(&exe)), baseline);

        std::fs::write(&script, "# fixture").unwrap();
        let suffix = format!(r#""-File" "{}""#, script.display());
        assert_eq!(
            line(&shell, Some(&exe)),
            format!(r#"{baseline} "-NoExit" {suffix}"#)
        );
        shell.args.push("-nOExIt".into());
        assert_eq!(
            line(&shell, Some(&exe)),
            format!(r#"{baseline} "-nOExIt" {suffix}"#)
        );
        shell.powershell_integration = false;
        assert_eq!(line(&shell, Some(&exe)), format!(r#"{baseline} "-nOExIt""#));
        shell.powershell_integration = true;
        shell.args.extend(["-Command".into(), "exit 5".into()]);
        assert_eq!(
            line(&shell, Some(&exe)),
            format!(r#"{baseline} "-nOExIt" "-Command" "exit 5""#)
        );
    }
}
