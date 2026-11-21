//! Coordinate explicitly requested workspace switches before any sessions start.
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    fs::{File, OpenOptions},
    os::windows::{ffi::OsStrExt, process::CommandExt},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};
use w11term::{
    control,
    options::Options,
    workspace::{Workspace, WorkspaceLaunch, WorkspaceStore},
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM},
        UI::WindowsAndMessaging::*,
    },
    core::BOOL,
};

pub struct Opening {
    pub key: String,
    pub layout: WorkspaceLaunch,
    registration_lock: RefCell<Option<File>>,
}

impl PartialEq for Opening {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.layout == other.layout
    }
}

impl Opening {
    pub fn published(&self) {
        self.registration_lock.take();
    }
}

pub type Launch = (Options, Option<Rc<Opening>>);

pub fn prepare(options: Options) -> Result<Option<Launch>> {
    let Some(name) = &options.workspace else {
        return Ok(Some((options, None)));
    };
    let catalog = w11term::keys::data_dir()?.join("workspaces.json");
    let key = workspace_key(&catalog, &options.profiles_path, name)?;
    let lock = registration_lock(&key)?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(error) => {
                return Err(error).context("Another workspace launch is still registering");
            }
        }
    }
    if options.switch_workspace
        && let Some(pid) = owner(&key)?
    {
        drop(lock);
        activate(pid)?;
        return Ok(None);
    }
    let layout =
        WorkspaceLaunch::load(&WorkspaceStore::new(catalog), name, &options.profiles_path)?;
    Ok(Some((
        options,
        Some(Rc::new(Opening {
            key,
            layout,
            registration_lock: RefCell::new(Some(lock)),
        })),
    )))
}

fn owner(key: &str) -> Result<Option<u32>> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(control::workspace_owner(key))
}

pub fn switch(name: &str, profiles: &Path) -> Result<()> {
    let catalog = w11term::keys::data_dir()?.join("workspaces.json");
    if let Some(pid) = owner(&workspace_key(&catalog, profiles, name)?)? {
        return activate(pid);
    }
    // The child rechecks under the registration lock. Two simultaneous clicks
    // may launch two processes, but only one can start terminal sessions.
    std::process::Command::new(std::env::current_exe()?)
        .arg("--profiles")
        .arg(profiles)
        .arg("--switch-workspace")
        .arg(name)
        .creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0)
        .spawn()
        .context("Starting workspace window")?;
    Ok(())
}

fn registration_lock(key: &str) -> Result<File> {
    let directory = control::discovery_dir()?.join("workspace-locks");
    std::fs::create_dir_all(&directory)?;
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(key))?)
}

pub fn save(
    name: &str,
    profiles: &Path,
    layout: &Workspace,
    server: &control::Server,
) -> Result<()> {
    let catalog = w11term::keys::data_dir()?.join("workspaces.json");
    let key = workspace_key(&catalog, profiles, name)?;
    let lock = registration_lock(&key)?;
    lock.try_lock()
        .context("Another window is launching this workspace. Try saving again.")?;
    WorkspaceStore::new(catalog).save(name, layout)?;
    server
        .publish_workspace(Some(key))
        .context("Workspace saved, but its window could not be registered")
}

// An absent profile file is valid for demo/local layouts. Resolve its nearest
// existing ancestor so relative paths and directory junctions agree.
fn canonical_path(path: &Path) -> Result<PathBuf> {
    let path = std::path::absolute(path)?;
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .context("Config path has no existing ancestor")?;
            Ok(canonical_path(parent)?.join(path.file_name().context("Invalid config file name")?))
        }
        Err(error) => Err(error.into()),
    }
}

fn workspace_key(catalog: &Path, profiles: &Path, name: &str) -> Result<String> {
    let mut hash = Sha256::new();
    for path in [catalog, profiles] {
        for unit in canonical_path(path)?.as_os_str().encode_wide() {
            hash.update(unit.to_le_bytes());
        }
        hash.update([0, 0]);
    }
    let (sid, integrity, session) = control::desktop_scope()?;
    hash.update(sid.as_bytes());
    hash.update(integrity.to_le_bytes());
    hash.update(session.to_le_bytes());
    hash.update(name.as_bytes());
    Ok(URL_SAFE_NO_PAD.encode(hash.finalize()))
}

fn activate(pid: u32) -> Result<()> {
    struct Search {
        pid: u32,
        hwnd: HWND,
    }
    unsafe extern "system" fn visit(hwnd: HWND, param: LPARAM) -> BOOL {
        // EnumWindows invokes us synchronously on this thread.
        let search = unsafe { &mut *(param.0 as *mut Search) };
        unsafe {
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == search.pid
                && IsWindowVisible(hwnd).as_bool()
                && GetWindow(hwnd, GW_OWNER).is_err()
            {
                let mut title = [0u16; 16];
                let len = GetWindowTextW(hwnd, &mut title);
                if String::from_utf16_lossy(&title[..len as usize]).starts_with("w11term") {
                    search.hwnd = hwnd;
                    return false.into();
                }
            }
        }
        true.into()
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut search = Search {
        pid,
        hwnd: HWND::default(),
    };
    loop {
        unsafe {
            let _ = EnumWindows(Some(visit), LPARAM((&mut search as *mut Search) as isize));
            if !search.hwnd.is_invalid() {
                if IsIconic(search.hwnd).as_bool() {
                    let _ = ShowWindowAsync(search.hwnd, SW_RESTORE);
                }
                ensure!(
                    GetForegroundWindow() == search.hwnd
                        || SetForegroundWindow(search.hwnd).as_bool(),
                    "Windows declined to foreground workspace window {pid}"
                );
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            bail!("Workspace PID {pid} has no visible terminal window on this desktop");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_distinguishes_workspace_and_config_and_resolves_aliases() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("workspaces.json");
        let b = root.path().join("profiles.json");
        let key = workspace_key(&a, &b, "Desk").unwrap();
        assert_eq!(
            workspace_key(&a, &root.path().join(".").join("profiles.json"), "Desk").unwrap(),
            key
        );
        assert_ne!(workspace_key(&a, &b, "desk").unwrap(), key);
        assert_ne!(workspace_key(&b, &a, "Desk").unwrap(), key);
        assert_ne!(workspace_key(&a, &a, "Desk").unwrap(), key);
    }
}
