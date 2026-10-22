//! UI-thread retry timer, independent of Reactor's bounded background worker pool.
use std::{cell::RefCell, collections::HashMap};
use windows::Win32::{Foundation::*, UI::WindowsAndMessaging::*};
type Callback = Box<dyn FnMut()>;
thread_local! { static CALLBACKS: RefCell<HashMap<usize, Callback>> = RefCell::default(); }
pub struct RetryTimer(usize);
impl RetryTimer {
    pub fn new(callback: impl FnMut() + 'static) -> windows::core::Result<Self> {
        let id = unsafe { SetTimer(None, 0, 100, Some(tick)) };
        if id == 0 {
            return Err(windows::core::Error::from_thread());
        }
        CALLBACKS.with(|entries| entries.borrow_mut().insert(id, Box::new(callback)));
        Ok(Self(id))
    }
}
impl Drop for RetryTimer {
    fn drop(&mut self) {
        unsafe {
            let _ = KillTimer(None, self.0);
        }
        CALLBACKS.with(|entries| {
            entries.borrow_mut().remove(&self.0);
        });
    }
}
unsafe extern "system" fn tick(_: HWND, _: u32, id: usize, _: u32) {
    // Never unwind through the Win32 callback boundary or hold a borrow while calling user code.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let callback = CALLBACKS.with(|entries| entries.borrow_mut().remove(&id));
        if let Some(mut callback) = callback {
            callback();
            CALLBACKS.with(|entries| entries.borrow_mut().insert(id, callback));
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_timer_runs_without_a_background_slot_and_cancels_on_drop() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let count = calls.clone();
        let timer = RetryTimer::new(move || count.set(count.get() + 1)).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while calls.get() == 0 && std::time::Instant::now() < deadline {
            let mut message = MSG::default();
            while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() } {
                unsafe {
                    DispatchMessageW(&message);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(calls.get() > 0);
        drop(timer);
        assert!(CALLBACKS.with(|entries| entries.borrow().is_empty()));
    }
}
