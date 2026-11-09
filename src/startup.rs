//! Startup failures must be visible even when Explorer launches the GUI subsystem.
use std::{io::Write, path::Path, time::SystemTime};
use windows::{
    Win32::{
        System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE},
        UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
    },
    core::{PCWSTR, w},
};

fn message(error: &anyhow::Error, executable: Option<&Path>) -> String {
    let mut text = format!("Unable to start w11term:\n\n{error:#}");
    if error.chain().any(|cause| {
        cause
            .to_string()
            .contains("self-contained Windows App Runtime files are missing")
    }) {
        text.push_str("\n\nThis build needs the Windows App Runtime DLLs and resource files beside the executable. Copying w11term.exe alone is not sufficient. Run target\\release\\w11term.exe from the build, or move the complete runtime folder with the executable. Changing profiles.json will not fix missing runtime files.");
    }
    if let Some(executable) = executable {
        text.push_str(&format!("\n\nExecutable: {}", executable.display()));
    }
    text
}

fn write_log(directory: &Path, text: &str) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(directory)?;
    let path = directory.join("startup.log");
    // Keep repeated bad launches from growing the diagnostic file indefinitely.
    if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() > 256 * 1024) {
        std::fs::write(&path, [])?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    writeln!(file, "[{timestamp}] {text}\n")?;
    Ok(path)
}

pub fn report(error: &anyhow::Error) {
    let executable = std::env::current_exe().ok();
    let text = message(error, executable.as_deref());
    let mut text: String = text.chars().take(32 * 1024).collect();
    if let Ok(directory) = w11term::keys::data_dir() {
        match write_log(&directory, &text) {
            Ok(path) => text.push_str(&format!("\nStartup log: {}", path.display())),
            Err(error) => text.push_str(&format!("\nCould not write startup log: {error}")),
        }
    }
    // Preserve redirected stderr (Cargo, scripts, tests). For an unredirected GUI
    // launch, try the parent's console before resorting to a native error dialog.
    let has_stderr =
        || unsafe { GetStdHandle(STD_ERROR_HANDLE).is_ok_and(|handle| !handle.is_invalid()) };
    let mut delivered = has_stderr() && writeln!(std::io::stderr(), "{text}").is_ok();
    if !delivered {
        let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
        delivered = has_stderr() && writeln!(std::io::stderr(), "{text}").is_ok();
    }
    if !delivered {
        let wide: Vec<_> = text
            .chars()
            .take(8192)
            .collect::<String>()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        unsafe {
            MessageBoxW(
                None,
                PCWSTR(wide.as_ptr()),
                w!("w11term could not start"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_failure_explains_complete_folder_without_misdiagnosing_profile_errors() {
        let runtime = message(
            &anyhow::anyhow!("self-contained Windows App Runtime files are missing (0x8007007E)"),
            Some(Path::new("C:/app/w11term.exe")),
        );
        assert!(runtime.contains("Copying w11term.exe alone is not sufficient"));
        assert!(runtime.contains("C:/app/w11term.exe"));
        let profile = message(
            &anyhow::anyhow!("parsing profiles.json: missing field auth"),
            None,
        );
        assert!(profile.contains("missing field auth"));
        assert!(!profile.contains("Copying w11term.exe"));
    }
    #[test]
    fn error_log_appends_and_bounds_old_diagnostics() {
        let directory = tempfile::tempdir().unwrap();
        let path = write_log(directory.path(), "first").unwrap();
        write_log(directory.path(), "second").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("first") && text.contains("second"));
        std::fs::write(&path, vec![b'x'; 256 * 1024 + 1]).unwrap();
        write_log(directory.path(), "new failure").unwrap();
        assert!(std::fs::metadata(path).unwrap().len() < 1024);
    }
}
