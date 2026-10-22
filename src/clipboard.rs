//! Plain Unicode clipboard access on a dedicated message-only window. No focus changes.
use anyhow::{Context, Result, ensure};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;
use windows::{
    Win32::{
        Foundation::*,
        System::{DataExchange::*, Memory::*},
        UI::WindowsAndMessaging::*,
    },
    core::w,
};

const MAX_TEXT: usize = 1024 * 1024;
enum Request {
    Read(mpsc::SyncSender<Result<String>>),
    Write(String, Option<mpsc::SyncSender<Result<()>>>),
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
                                let result = write_native(hwnd, &text);
                                if let Some(reply) = reply {
                                    let _ = reply.send(result);
                                } else if let Err(e) = result {
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
    let (tx, rx) = mpsc::sync_channel(1);
    service()?
        .try_send(Request::Write(text.into(), Some(tx)))
        .context("Clipboard service is busy")?;
    rx.recv_timeout(Duration::from_secs(2))
        .context("Clipboard write timed out")?
}

pub struct RemoteClipboard(pub Arc<AtomicBool>);
impl wezterm_term::Clipboard for RemoteClipboard {
    fn set_contents(
        &self,
        _selection: wezterm_term::ClipboardSelection,
        data: Option<String>,
    ) -> Result<()> {
        if !self.0.load(Ordering::Relaxed) {
            return Ok(());
        }
        let text = data.unwrap_or_default();
        ensure!(
            text.len() <= MAX_TEXT,
            "OSC 52 clipboard text exceeds 1 MiB"
        );
        service()?
            .try_send(Request::Write(text, None))
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
fn write_native(hwnd: HWND, text: &str) -> Result<()> {
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
        let result = (|| -> Result<()> {
            let _open = open(hwnd)?;
            EmptyClipboard()?;
            SetClipboardData(13, Some(HANDLE(memory.0)))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = GlobalFree(Some(memory));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let disabled = RemoteClipboard(Arc::new(AtomicBool::new(false)));
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
