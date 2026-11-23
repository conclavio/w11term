//! Plain Unicode clipboard access on a dedicated message-only window. No focus changes.
use anyhow::{Context, Result, ensure};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;
use windows::{
    Win32::{
        Foundation::*,
        System::{DataExchange::*, Memory::*},
        UI::WindowsAndMessaging::*,
    },
    core::w,
};

pub(crate) const MAX_TEXT: usize = 1024 * 1024;
enum Request {
    Read(mpsc::SyncSender<Result<String>>),
    Write(String, mpsc::SyncSender<Result<()>>),
    RemoteWrite(String, Permission),
}
fn service() -> Result<&'static mpsc::SyncSender<Request>> {
    static SERVICE: OnceLock<Result<mpsc::SyncSender<Request>, String>> = OnceLock::new();
    SERVICE
        .get_or_init(|| {
            let (tx, rx) = mpsc::sync_channel(32);
            let (ready, started) = mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("terminal-clipboard".into())
                .spawn(move || {
                    // STATIC is a built-in class. HWND_MESSAGE never becomes visible or active.
                    let hwnd = match unsafe {
                        CreateWindowExW(
                            WINDOW_EX_STYLE::default(),
                            w!("STATIC"),
                            w!("w11term clipboard"),
                            WINDOW_STYLE::default(),
                            0,
                            0,
                            0,
                            0,
                            Some(HWND_MESSAGE),
                            None,
                            None,
                            None,
                        )
                    } {
                        Ok(hwnd) => hwnd,
                        Err(e) => {
                            let _ = ready.send(Err(e.to_string()));
                            return;
                        }
                    };
                    let _ = ready.send(Ok(()));
                    loop {
                        let mut message = MSG::default();
                        unsafe {
                            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                                let _ = TranslateMessage(&message);
                                DispatchMessageW(&message);
                            }
                        }
                        match rx.recv_timeout(Duration::from_millis(10)) {
                            Ok(Request::Read(reply)) => {
                                let _ = reply.send(read_native(hwnd));
                            }
                            Ok(Request::Write(text, reply)) => {
                                let _ = reply.send(write_native(hwnd, &text, None));
                            }
                            Ok(Request::RemoteWrite(text, permission)) => {
                                if permission.current()
                                    && let Err(e) = write_remote(hwnd, &text, &permission)
                                {
                                    log::warn!("OSC 52 clipboard write: {e:#}");
                                }
                            }
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                })
                .map_err(|e| e.to_string())?;
            started.recv().map_err(|e| e.to_string())??;
            Ok(tx)
        })
        .as_ref()
        .map_err(|e| anyhow::anyhow!("Clipboard service: {e}"))
}
pub fn read() -> Result<String> {
    let (tx, rx) = mpsc::sync_channel(1);
    service()?
        .try_send(Request::Read(tx))
        .context("Clipboard service is busy")?;
    rx.recv_timeout(Duration::from_secs(2))
        .context("Clipboard read timed out")?
}
pub fn write(text: &str) -> Result<()> {
    ensure!(text.len() <= MAX_TEXT, "Clipboard text exceeds 1 MiB");
    #[cfg(feature = "ui-smoke")]
    if let Some(path) = std::env::var_os("W11TERM_TEST_COPY_FILE") {
        return std::fs::write(path, text).context("Writing the clipboard test fixture");
    }
    let (tx, rx) = mpsc::sync_channel(1);
    service()?
        .try_send(Request::Write(text.into(), tx))
        .context("Clipboard service is busy")?;
    rx.recv_timeout(Duration::from_secs(2))
        .context("Clipboard write timed out")?
}

#[derive(Default)]
struct RemoteRateLimit(Option<std::time::Instant>);
impl RemoteRateLimit {
    fn admit(&mut self, now: std::time::Instant) -> Result<()> {
        ensure!(
            self.0.is_none_or(
                |last| now.saturating_duration_since(last) >= Duration::from_millis(250)
            ),
            "OSC 52 rate limited (four writes per second)"
        );
        self.0 = Some(now);
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteClipboardPolicy {
    Disabled,
    AnyPane,
    ForegroundPane,
}

impl From<bool> for RemoteClipboardPolicy {
    fn from(enabled: bool) -> Self {
        if enabled {
            Self::AnyPane
        } else {
            Self::Disabled
        }
    }
}

struct AccessState {
    policy: RemoteClipboardPolicy,
    focused_window: Option<usize>,
    revision: u64,
}

impl AccessState {
    fn allows(&self, foreground_window: Option<usize>) -> bool {
        match self.policy {
            RemoteClipboardPolicy::Disabled => false,
            RemoteClipboardPolicy::AnyPane => true,
            RemoteClipboardPolicy::ForegroundPane => {
                self.focused_window.is_some() && self.focused_window == foreground_window
            }
        }
    }
}

pub struct RemoteClipboardAccess(Mutex<AccessState>);

impl RemoteClipboardAccess {
    pub fn new(policy: RemoteClipboardPolicy) -> Self {
        Self(Mutex::new(AccessState {
            policy,
            focused_window: None,
            revision: 0,
        }))
    }

    pub fn set_policy(&self, policy: RemoteClipboardPolicy) {
        let mut state = self.0.lock().unwrap();
        if state.policy != policy {
            state.policy = policy;
            state.revision = state.revision.wrapping_add(1);
        }
    }

    pub fn writes_enabled(&self) -> bool {
        self.0.lock().unwrap().policy != RemoteClipboardPolicy::Disabled
    }

    pub fn set_focused_window(&self, window: Option<usize>) {
        let mut state = self.0.lock().unwrap();
        if state.focused_window != window {
            state.focused_window = window;
            if state.policy == RemoteClipboardPolicy::ForegroundPane {
                state.revision = state.revision.wrapping_add(1);
            }
        }
    }

    fn permission(self: &Arc<Self>, foreground_window: Option<usize>) -> Option<Permission> {
        let state = self.0.lock().unwrap();
        state.allows(foreground_window).then(|| Permission {
            access: self.clone(),
            revision: state.revision,
        })
    }
}

struct Permission {
    access: Arc<RemoteClipboardAccess>,
    revision: u64,
}

impl Permission {
    fn current(&self) -> bool {
        self.allows(foreground_window())
    }

    fn allows(&self, foreground_window: Option<usize>) -> bool {
        let state = self.access.0.lock().unwrap();
        state.revision == self.revision && state.allows(foreground_window)
    }
}

fn foreground_window() -> Option<usize> {
    let hwnd = unsafe { GetForegroundWindow() };
    (!hwnd.is_invalid()).then_some(hwnd.0 as usize)
}

pub struct RemoteClipboard(pub Arc<RemoteClipboardAccess>);
impl wezterm_term::Clipboard for RemoteClipboard {
    fn writes_enabled(&self) -> bool {
        self.0.writes_enabled()
    }

    fn set_contents(
        &self,
        _selection: wezterm_term::ClipboardSelection,
        data: Option<String>,
    ) -> Result<()> {
        let Some(permission) = self.0.permission(foreground_window()) else {
            return Ok(());
        };
        let text = data.unwrap_or_default();
        ensure!(
            text.len() <= MAX_TEXT,
            "OSC 52 clipboard text exceeds 1 MiB"
        );
        static LIMIT: std::sync::Mutex<RemoteRateLimit> =
            std::sync::Mutex::new(RemoteRateLimit(None));
        LIMIT.lock().unwrap().admit(std::time::Instant::now())?;
        service()?
            .try_send(Request::RemoteWrite(text, permission))
            .context("Clipboard service is busy")?;
        Ok(())
    }
}
struct Open;
impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}
fn open(hwnd: HWND) -> Result<Open> {
    for _ in 0..10 {
        if unsafe { OpenClipboard(Some(hwnd)) }.is_ok() {
            return Ok(Open);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    unsafe { OpenClipboard(Some(hwnd)) }?;
    Ok(Open)
}
fn read_native(hwnd: HWND) -> Result<String> {
    let _open = open(hwnd)?;
    unsafe {
        if !IsClipboardFormatAvailable(13).is_ok() {
            return Ok(String::new());
        }
        let memory = HGLOBAL(GetClipboardData(13)?.0);
        let length = GlobalSize(memory) / 2;
        ensure!(length <= MAX_TEXT * 2, "Clipboard text exceeds limit");
        let ptr = GlobalLock(memory).cast::<u16>();
        ensure!(!ptr.is_null(), "Could not lock clipboard memory");
        let units = std::slice::from_raw_parts(ptr, length);
        let end = units.iter().position(|u| *u == 0).unwrap_or(length);
        let text = String::from_utf16_lossy(&units[..end]);
        let _ = GlobalUnlock(memory);
        ensure!(text.len() <= MAX_TEXT, "Clipboard text exceeds 1 MiB");
        Ok(text)
    }
}
fn write_remote(hwnd: HWND, text: &str, permission: &Permission) -> Result<()> {
    #[cfg(feature = "ui-smoke")]
    if let Some(path) = std::env::var_os("W11TERM_TEST_REMOTE_COPY_FILE") {
        if permission.current() {
            return std::fs::write(path, text).context("Writing the remote clipboard test fixture");
        }
        return Ok(());
    }
    write_native(hwnd, text, Some(permission))
}

fn write_native(hwnd: HWND, text: &str, permission: Option<&Permission>) -> Result<()> {
    let units: Vec<u16> = text
        .split('\0')
        .next()
        .unwrap_or("")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // Allocate before EmptyClipboard so a failed allocation leaves the old contents intact.
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2)?;
        let ptr = GlobalLock(memory).cast::<u16>();
        if ptr.is_null() {
            let _ = GlobalFree(Some(memory));
            anyhow::bail!("Could not lock clipboard memory");
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), ptr, units.len());
        let _ = GlobalUnlock(memory);
        let result = (|| -> Result<bool> {
            let _open = open(hwnd)?;
            if permission.is_some_and(|permission| !permission.current()) {
                return Ok(false);
            }
            EmptyClipboard()?;
            SetClipboardData(13, Some(HANDLE(memory.0)))?;
            Ok(true)
        })();
        if !matches!(result, Ok(true)) {
            let _ = GlobalFree(Some(memory));
        }
        result.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_writes_are_rate_limited_without_touching_the_system_clipboard() {
        let now = std::time::Instant::now();
        let mut limit = RemoteRateLimit::default();
        assert!(limit.admit(now).is_ok());
        assert!(limit.admit(now + Duration::from_millis(249)).is_err());
        assert!(limit.admit(now + Duration::from_millis(250)).is_ok());
    }

    #[test]
    fn foreground_grants_require_the_selected_pane_in_the_foreground_window() {
        use RemoteClipboardPolicy::*;
        let access = Arc::new(RemoteClipboardAccess::new(ForegroundPane));
        assert!(access.permission(None).is_none());
        assert!(access.permission(Some(7)).is_none());
        access.set_focused_window(Some(7));
        assert!(access.permission(None).is_none());
        assert!(access.permission(Some(8)).is_none());
        let allowed = access.permission(Some(7)).unwrap();
        assert!(!allowed.allows(Some(8)));
        assert!(allowed.allows(Some(7)));
        access.set_focused_window(None);
        access.set_focused_window(Some(7));
        assert!(!allowed.allows(Some(7)));
        assert!(access.permission(Some(7)).is_some());
    }

    #[test]
    fn policy_changes_cancel_queued_writes_and_legacy_grants_ignore_focus() {
        use RemoteClipboardPolicy::*;
        let access = Arc::new(RemoteClipboardAccess::new(AnyPane));
        let background = access.permission(None).unwrap();
        access.set_focused_window(Some(5));
        access.set_focused_window(None);
        assert!(background.allows(Some(5)));
        access.set_policy(ForegroundPane);
        assert!(!background.allows(Some(5)));
        assert!(access.permission(Some(5)).is_none());
        access.set_focused_window(Some(5));
        let active = access.permission(Some(5)).unwrap();
        access.set_policy(Disabled);
        assert!(!active.allows(Some(5)));
        assert!(access.permission(Some(5)).is_none());
    }
    #[test]
    #[ignore = "Writes the system clipboard; run explicitly during native verification"]
    fn native_osc52_roundtrip_preserving_existing_plain_text() {
        use wezterm_term::Clipboard;
        // Do not disturb images, rich text, or application-specific clipboard formats.
        unsafe {
            if OpenClipboard(None).is_err() {
                return;
            }
            let _open = Open;
            let mut format = EnumClipboardFormats(0);
            while format != 0 {
                if !matches!(format, 1 | 7 | 13 | 16) {
                    eprintln!(
                        "Native clipboard smoke skipped: existing clipboard has non-text formats"
                    );
                    return;
                }
                format = EnumClipboardFormats(format);
            }
        }
        let previous = read().unwrap();
        const MARKER: &str = "w11term OSC 52 verification 🌍";
        struct Restore(String);
        impl Drop for Restore {
            fn drop(&mut self) {
                if read().is_ok_and(|current| current == MARKER) {
                    let _ = write(&self.0);
                }
            }
        }
        let _restore = Restore(previous);
        let disabled = RemoteClipboard(Arc::new(RemoteClipboardAccess::new(
            RemoteClipboardPolicy::Disabled,
        )));
        disabled
            .set_contents(
                wezterm_term::ClipboardSelection::Clipboard,
                Some(MARKER.into()),
            )
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("osc52.vt");
        use base64::Engine;
        std::fs::write(
            &path,
            format!(
                "\x1b]52;c;{}\x07",
                base64::engine::general_purpose::STANDARD.encode(MARKER)
            ),
        )
        .unwrap();
        let _session = crate::session::Session::start(crate::options::Source::Replay(path));
        let start = std::time::Instant::now();
        while read().unwrap() != MARKER {
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "OSC52 did not reach the Windows clipboard"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        eprintln!("Native OSC 52 clipboard write and Unicode readback passed");
    }
}
