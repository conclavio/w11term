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
    workspace::{Workspace, WorkspaceIdentity, WorkspaceLaunch, WorkspaceStore},
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
    fn load(
        store: &WorkspaceStore,
        name: &str,
        profiles: &Path,
        identity: &WorkspaceIdentity,
        key: String,
        lock: File,
    ) -> Result<Rc<Self>> {
        let entry = store.entry(name)?;
        ensure!(
            &entry.identity == identity,
            "Workspace {name} changed. Try opening it again."
        );
        let layout = WorkspaceLaunch::resolve(entry.layout, profiles)?;
        Ok(Rc::new(Self {
            key,
            layout,
            registration_lock: RefCell::new(Some(lock)),
        }))
    }

    pub fn published(&self) {
        self.registration_lock.take();
    }
}

pub type Launch = (Options, Option<Rc<Opening>>);

pub fn prepare(options: Options) -> Result<Option<Launch>> {
    let Some(name) = &options.workspace else {
        return Ok(Some((options, None)));
    };
    let store = WorkspaceStore::open()?;
    let identity = store.entry(name)?.identity;
    let key = workspace_key(store.path(), &options.profiles_path, &identity)?;
    ensure!(
        options
            .expected_workspace_key
            .as_ref()
            .is_none_or(|expected| expected == &key),
        "Workspace {name} changed. Try switching again."
    );
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
    if options.switch_workspace && focus_existing(&key, None)? {
        return Ok(None);
    }
    let opening = Opening::load(&store, name, &options.profiles_path, &identity, key, lock)?;
    Ok(Some((options, Some(opening))))
}

pub fn prepare_copy(name: &str, profiles: &Path) -> Result<Rc<Opening>> {
    let store = WorkspaceStore::open()?;
    let identity = store.entry(name)?.identity;
    let key = workspace_key(store.path(), profiles, &identity)?;
    let lock = registration_lock(&key)?;
    lock.try_lock()
        .context("Another window is registering this workspace. Try opening it again.")?;
    Opening::load(&store, name, profiles, &identity, key, lock)
}

fn owner(key: &str, caller: Option<control::WindowId>) -> Result<Option<control::WorkspaceOwner>> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            tokio::time::timeout(
                Duration::from_secs(4),
                control::workspace_owner_for(key, caller),
            )
            .await
            .context("Timed out verifying the workspace window")?
        })
}

fn focus_existing(key: &str, caller: Option<control::WindowId>) -> Result<bool> {
    for _ in 0..2 {
        let Some(window) = owner(key, caller)? else {
            return Ok(false);
        };
        if activate(&window)? {
            return Ok(true);
        }
        // This verified process exited while its window was being located.
        // Rediscover before deciding whether the saved layout needs restoring.
    }
    bail!("Workspace windows are closing. Try switching again.")
}

pub fn switch(name: &str, profiles: &Path, caller: control::WindowId) -> Result<()> {
    let store = WorkspaceStore::open()?;
    let identity = store.entry(name)?.identity;
    let key = workspace_key(store.path(), profiles, &identity)?;
    if focus_existing(&key, Some(caller))? {
        return Ok(());
    }
    // The child rechecks under the registration lock. Two simultaneous clicks
    // may launch two processes, but only one can start terminal sessions.
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .arg("--profiles")
        .arg(profiles)
        .arg("--switch-workspace")
        .arg(name)
        .arg("--expected-workspace-key")
        .arg(&key)
        .creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0)
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("Starting workspace window")?;
    let manifest = control::discovery_dir()?.join(format!("{}.json", child.id()));
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                // Surface missing profiles or invalid saved layouts in the picker.
                let entry = store.entry(name)?;
                ensure!(
                    entry.identity == identity,
                    "Workspace {name} changed. Try switching again."
                );
                WorkspaceLaunch::resolve(entry.layout, profiles)?;
                bail!(
                    "Opening workspace {name} failed ({status}). See {} for details.",
                    w11term::keys::data_dir()?.join("startup.log").display()
                );
            }
            // A concurrent launch can activate the winner and exit successfully.
            ensure!(
                focus_existing(&key, Some(caller))?,
                "Workspace {name} closed before its window was ready"
            );
            return Ok(());
        }
        if manifest.try_exists()?
            && let Some(window) = owner(&key, Some(caller))?
            && window.pid == child.id()
            && activate(&window)?
        {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Workspace {name} has not opened a window yet (PID {}).",
            child.id()
        );
        std::thread::sleep(Duration::from_millis(75));
    }
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
    window_id: control::WindowId,
) -> Result<()> {
    let store = WorkspaceStore::open()?;
    let (key, _lock) = store.save_with(name, layout, |identity| {
        let key = workspace_key(store.path(), profiles, identity)?;
        let lock = registration_lock(&key)?;
        lock.try_lock()
            .context("Another window is launching this workspace. Try saving again.")?;
        Ok((key, lock))
    })?;
    server
        .publish_window_workspace(window_id, Some(key))
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

fn workspace_key(catalog: &Path, profiles: &Path, identity: &WorkspaceIdentity) -> Result<String> {
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
    match identity {
        WorkspaceIdentity::Legacy(name) => hash.update(name.as_bytes()),
        WorkspaceIdentity::Unique(id) => {
            // Legacy names cannot contain a NUL.
            hash.update([0]);
            hash.update(id.as_bytes());
        }
    }
    Ok(URL_SAFE_NO_PAD.encode(hash.finalize()))
}

fn activate(owner: &control::WorkspaceOwner) -> Result<bool> {
    struct Search {
        pid: u32,
        hwnd: HWND,
        window_id: Option<control::WindowId>,
        registered: usize,
        ambiguous: bool,
    }
    unsafe extern "system" fn visit(hwnd: HWND, param: LPARAM) -> BOOL {
        // EnumWindows invokes us synchronously on this thread.
        let search = unsafe { &mut *(param.0 as *mut Search) };
        unsafe {
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == search.pid && IsWindowVisible(hwnd).as_bool() {
                if let Some(id) = search.window_id {
                    if (search.registered == 0 || search.registered == hwnd.0 as usize)
                        && control::native_window_matches(hwnd.0 as usize, id)
                    {
                        search.hwnd = hwnd;
                        return false.into();
                    }
                    return true.into();
                }
                // Previous single-window releases did not publish a logical ID.
                if GetWindow(hwnd, GW_OWNER).is_ok() {
                    return true.into();
                }
                let mut title = [0u16; 16];
                let len = GetWindowTextW(hwnd, &mut title);
                if String::from_utf16_lossy(&title[..len as usize]).starts_with("w11term") {
                    if !search.hwnd.is_invalid() {
                        search.ambiguous = true;
                        return false.into();
                    }
                    search.hwnd = hwnd;
                }
            }
        }
        true.into()
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let pid = owner.pid;
    let mut restore_sent = false;
    let mut requested_foreground = false;
    loop {
        if !owner.is_running()? {
            return Ok(false);
        }
        let mut search = Search {
            pid,
            hwnd: HWND::default(),
            window_id: owner.window_id,
            registered: owner.hwnd,
            ambiguous: false,
        };
        unsafe {
            let _ = EnumWindows(Some(visit), LPARAM((&mut search as *mut Search) as isize));
            ensure!(
                !search.ambiguous,
                "Older workspace process {pid} has multiple terminal windows"
            );
            if !search.hwnd.is_invalid() {
                if IsIconic(search.hwnd).as_bool() {
                    if !restore_sent {
                        ensure!(
                            ShowWindowAsync(search.hwnd, SW_RESTORE).as_bool(),
                            "Windows declined to restore workspace window {pid}"
                        );
                        restore_sent = true;
                    }
                } else if GetForegroundWindow() == search.hwnd {
                    return Ok(true);
                } else if !requested_foreground {
                    ensure!(
                        SetForegroundWindow(search.hwnd).as_bool(),
                        "Windows declined to foreground workspace window {pid}"
                    );
                    requested_foreground = true;
                }
            }
        }
        if Instant::now() >= deadline {
            bail!("Workspace window {pid} did not become visible in the foreground");
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
        let desk = WorkspaceIdentity::Legacy("Desk".into());
        let key = workspace_key(&a, &b, &desk).unwrap();
        assert_eq!(
            workspace_key(&a, &root.path().join(".").join("profiles.json"), &desk).unwrap(),
            key
        );
        assert_ne!(
            workspace_key(&a, &b, &WorkspaceIdentity::Legacy("desk".into())).unwrap(),
            key
        );
        assert_ne!(workspace_key(&b, &a, &desk).unwrap(), key);
        assert_ne!(workspace_key(&a, &a, &desk).unwrap(), key);
        assert_ne!(
            workspace_key(&a, &b, &WorkspaceIdentity::Unique("Desk".into())).unwrap(),
            key
        );
    }
}
