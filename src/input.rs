//! Raw terminal keys are routed only to the selected session. Forms have no terminal target;
//! native popup windows are excluded by the root-window check. Hooks are UI-thread-local.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use tokio::sync::mpsc;
use w11term::{
    interaction::{Interaction, TabSwitcherMode},
    session::Command,
    terminal::FrameStore,
};
use wezterm_term::{KeyCode, KeyModifiers};
use windows::Win32::{
    Foundation::*,
    System::Threading::GetCurrentThreadId,
    UI::{
        Input::KeyboardAndMouse::*,
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};

thread_local! { static INPUT: RefCell<Option<State>> = const { RefCell::new(None) }; }

struct State {
    target: Option<Target>,
    shortcut: Box<dyn Fn(Shortcut)>,
    swallowed: HashSet<u32>,
    pressed: HashMap<u32, KeyCode>,
    surrogate: Option<u16>,
    preferences: Interaction,
    selection: bool,
    switcher: bool,
    grid: Option<Grid>,
    window: Option<HWND>,
    search: bool,
    wheel_remainder: i32,
    zoom_remainder: i32,
    modal: bool,
    game_mode: bool,
    close_handler: Option<Box<dyn Fn()>>,
}

struct Target {
    sender: mpsc::UnboundedSender<Command>,
    frames: FrameStore,
}

#[derive(Clone, Copy, Debug)]
pub enum Shortcut {
    Fullscreen,
    RememberWindow,
    NextTab,
    PreviousTab,
    NewTab,
    CloseTab,
    Select(usize),
    Copy,
    Paste,
    PasteRaw,
    ClearSelection,
    Zoom(i32),
    ResetZoom,
    CommitTab,
    CancelTab,
    Find,
    FindNext,
    FindPrevious,
    FindClose,
}

pub struct KeyboardBridge {
    messages: HHOOK,
    focus: HHOOK,
    mouse: HHOOK,
}

pub fn window() -> Option<HWND> {
    INPUT.with(|s| s.borrow().as_ref().and_then(|s| s.window))
}

impl KeyboardBridge {
    pub fn install(shortcut: impl Fn(Shortcut) + 'static) -> windows::core::Result<Self> {
        // SAFETY: callback lives for the program's lifetime; hook is limited to the current thread.
        let hook =
            unsafe { SetWindowsHookExW(WH_GETMESSAGE, Some(hook), None, GetCurrentThreadId())? };
        let focus = match unsafe {
            SetWindowsHookExW(WH_CALLWNDPROC, Some(focus_hook), None, GetCurrentThreadId())
        } {
            Ok(focus) => focus,
            Err(error) => {
                unsafe {
                    let _ = UnhookWindowsHookEx(hook);
                }
                return Err(error);
            }
        };
        let mouse = match unsafe {
            SetWindowsHookExW(WH_MOUSE, Some(mouse_hook), None, GetCurrentThreadId())
        } {
            Ok(mouse) => mouse,
            Err(error) => {
                unsafe {
                    let _ = UnhookWindowsHookEx(hook);
                    let _ = UnhookWindowsHookEx(focus);
                }
                return Err(error);
            }
        };
        INPUT.with(|input| {
            *input.borrow_mut() = Some(State {
                target: None,
                shortcut: Box::new(shortcut),
                swallowed: HashSet::new(),
                pressed: HashMap::new(),
                surrogate: None,
                preferences: Interaction::default(),
                selection: false,
                switcher: false,
                grid: None,
                window: None,
                search: false,
                wheel_remainder: 0,
                zoom_remainder: 0,
                modal: false,
                game_mode: false,
                close_handler: None,
            })
        });
        Ok(Self {
            messages: hook,
            focus,
            mouse,
        })
    }

    pub fn set_target(&self, target: Option<(mpsc::UnboundedSender<Command>, FrameStore)>) {
        INPUT.with(|input| {
            if let Some(state) = input.borrow_mut().as_mut() {
                state.retarget(target.map(|(sender, frames)| Target { sender, frames }));
            }
        });
    }
    pub fn set_preferences(&self, preferences: Interaction) {
        INPUT.with(|i| {
            if let Some(s) = i.borrow_mut().as_mut() {
                s.preferences = preferences;
            }
        });
    }
    pub fn set_selection(&self, selected: bool) {
        INPUT.with(|i| {
            if let Some(s) = i.borrow_mut().as_mut() {
                s.selection = selected;
            }
        });
    }
    pub fn set_switcher(&self, visible: bool) {
        INPUT.with(|i| {
            if let Some(s) = i.borrow_mut().as_mut() {
                s.switcher = visible;
            }
        });
    }
    pub fn set_search(&self, visible: bool) {
        INPUT.with(|i| {
            if let Some(s) = i.borrow_mut().as_mut() {
                if visible {
                    s.release_keys();
                }
                s.search = visible;
                if let Some(target) = &s.target {
                    let _ = target.sender.send(Command::Focus(!visible));
                }
            }
        });
    }
    pub fn set_modal(&self, visible: bool) {
        INPUT.with(|i| {
            if let Some(s) = i.borrow_mut().as_mut() {
                if visible {
                    s.release_keys();
                }
                s.modal = visible;
            }
        });
    }
    pub fn on_close(&self, handler: impl Fn() + 'static) {
        INPUT.with(|state| {
            if let Some(state) = state.borrow_mut().as_mut() {
                state.close_handler = Some(Box::new(handler));
            }
        });
    }
    pub fn reset_keys(&self) {
        INPUT.with(|i| {
            if let Some(s) = i.borrow_mut().as_mut() {
                s.pressed.clear();
                s.surrogate = None;
            }
        });
    }
    pub fn set_game_mode(&self, game: bool) {
        INPUT.with(|i| {
            if let Some(s) = i.borrow_mut().as_mut() {
                s.game_mode = game;
            }
        });
    }

    #[cfg(feature = "ui-smoke")]
    pub fn smoke_wheel(&self, delta: i16) -> bool {
        let hwnd = INPUT
            .with(|i| i.borrow().as_ref().and_then(|s| s.window))
            .expect("terminal HWND");
        let data = MOUSEHOOKSTRUCTEX {
            Base: MOUSEHOOKSTRUCT {
                hwnd,
                ..Default::default()
            },
            mouseData: (delta as u16 as u32) << 16,
        };
        unsafe {
            mouse_hook(
                HC_ACTION as i32,
                WPARAM(WM_MOUSEWHEEL as usize),
                LPARAM((&data as *const MOUSEHOOKSTRUCTEX) as isize),
            ) == LRESULT(1)
        }
    }
    #[cfg(feature = "ui-smoke")]
    pub fn has_target(&self) -> bool {
        INPUT.with(|input| {
            input
                .borrow()
                .as_ref()
                .is_some_and(|state| state.target.is_some())
        })
    }
}

impl Drop for KeyboardBridge {
    fn drop(&mut self) {
        // SAFETY: this object owns the hook, and is destroyed on its installing thread.
        unsafe {
            let _ = UnhookWindowsHookEx(self.messages);
            let _ = UnhookWindowsHookEx(self.focus);
            let _ = UnhookWindowsHookEx(self.mouse);
        }
        INPUT.with(|input| {
            if let Some(state) = input.borrow_mut().take()
                && let Some(hwnd) = state.window
            {
                unsafe {
                    let _ = RemoveWindowSubclass(hwnd, Some(window_proc), 1);
                }
            }
        });
    }
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && wparam.0 as u32 == WM_MOUSEWHEEL {
        // Thread-local WH_MOUSE sees real wheel input before WinUI translates it.
        let mouse = unsafe { &*(lparam.0 as *const MOUSEHOOKSTRUCTEX) };
        if is_terminal(mouse.Base.hwnd) {
            let consumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                INPUT.with(|i| {
                    i.try_borrow_mut()
                        .ok()
                        .and_then(|mut i| {
                            i.as_mut().map(|s| {
                                s.wheel((mouse.mouseData >> 16) as u16 as i16, modifiers())
                            })
                        })
                        .unwrap_or(false)
                })
            }))
            .unwrap_or(false);
            if consumed {
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn focus_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: WH_CALLWNDPROC supplies a CWPSTRUCT valid for this callback.
        let message = unsafe { &*(lparam.0 as *const CWPSTRUCT) };
        if message.message == WM_ACTIVATEAPP && is_terminal(message.hwnd) {
            INPUT.with(|input| {
                if let Ok(mut input) = input.try_borrow_mut()
                    && let Some(state) = input.as_mut()
                {
                    let focused = message.wParam.0 != 0;
                    if !focused {
                        state.release_keys();
                        if state.switcher {
                            state.switcher = false;
                            (state.shortcut)(Shortcut::CancelTab);
                        }
                    }
                    if let Some(target) = &state.target {
                        let _ = target.sender.send(Command::Focus(focused && !state.search));
                    }
                }
            });
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam.0 == PM_REMOVE.0 as usize {
        // SAFETY: WH_GETMESSAGE provides a valid MSG for this callback invocation.
        let message = unsafe { &mut *(lparam.0 as *mut MSG) };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if !is_terminal(message.hwnd) {
                return;
            }
            INPUT.with(|input| {
                if let Ok(mut input) = input.try_borrow_mut()
                    && let Some(state) = input.as_mut()
                    && state.handle(message)
                {
                    message.message = WM_NULL;
                }
            });
        }));
    }
    // SAFETY: chain the original hook invocation regardless of whether a message was consumed.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn is_terminal(hwnd: HWND) -> bool {
    // Popup menus and dialogs have their own root window; never consume their keyboard input.
    unsafe {
        let root = GetAncestor(hwnd, GA_ROOT);
        let mut title = [0u16; 256];
        let len = GetWindowTextW(root, &mut title);
        len > 0 && String::from_utf16_lossy(&title[..len as usize]).starts_with("w11term")
    }
}

#[derive(Clone, Copy)]
struct Grid {
    cw: i32,
    ch: i32,
    width: f32,
    height: f32,
    scale: f32,
}
pub fn set_grid(cw: f32, ch: f32, width: f32, height: f32, scale: f32) {
    INPUT.with(|i| {
        if let Ok(mut state) = i.try_borrow_mut()
            && let Some(state) = state.as_mut()
        {
            state.grid = Some(Grid {
                cw: (cw * scale).round().max(1.0) as i32,
                ch: (ch * scale).round().max(1.0) as i32,
                width: width * scale,
                height: height * scale,
                scale,
            });
        }
    });
}
fn snap_extent(value: i32, overhead: i32, cell: i32) -> i32 {
    ((value - overhead) as f64 / cell.max(1) as f64)
        .round()
        .max(1.0) as i32
        * cell.max(1)
        + overhead
}
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    // Persist before native destruction; a queued component message can be
    // retired with the window before it gets a chance to inspect its HWND.
    if message == WM_CLOSE {
        INPUT.with(|state| {
            if let Ok(state) = state.try_borrow()
                && let Some(state) = state.as_ref()
                && let Some(handler) = &state.close_handler
            {
                handler();
            }
        });
    }
    let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    if matches!(message, WM_EXITSIZEMOVE | WM_CLOSE) {
        INPUT.with(|s| {
            if let Ok(s) = s.try_borrow()
                && let Some(s) = s.as_ref()
            {
                (s.shortcut)(Shortcut::RememberWindow);
            }
        });
    }
    if message == WM_SIZING && lparam.0 != 0 {
        INPUT.with(|i| {
            if let Ok(state) = i.try_borrow()
                && let Some(state) = state.as_ref()
                && state.preferences.snap_to_grid_on_resize
                && let Some(grid) = state.grid
            {
                let mut outer = RECT::default();
                if unsafe { GetWindowRect(hwnd, &mut outer) }.is_err() {
                    return;
                }
                let overhead_x = outer.right - outer.left - grid.width.round() as i32
                    + (crate::renderer::PADDING * 2.0 * grid.scale).round() as i32;
                let overhead_y = outer.bottom - outer.top - grid.height.round() as i32
                    + ((crate::renderer::PADDING * 2.0 + crate::renderer::STATUS_HEIGHT)
                        * grid.scale)
                        .round() as i32;
                // SAFETY: WM_SIZING owns a writable RECT for the duration of this call.
                let proposed = unsafe { &mut *(lparam.0 as *mut RECT) };
                let width = snap_extent(proposed.right - proposed.left, overhead_x, grid.cw);
                let height = snap_extent(proposed.bottom - proposed.top, overhead_y, grid.ch);
                let edge = wparam.0 as u32;
                if matches!(edge, 1 | 4 | 7) {
                    proposed.left = proposed.right - width;
                } else if matches!(edge, 2 | 5 | 8) {
                    proposed.right = proposed.left + width;
                }
                if matches!(edge, 3..=5) {
                    proposed.top = proposed.bottom - height;
                } else if matches!(edge, 6..=8) {
                    proposed.bottom = proposed.top + height;
                }
            }
        });
        return LRESULT(1);
    }
    if message == WM_NCDESTROY {
        unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(window_proc), 1);
        }
        INPUT.with(|i| {
            if let Ok(mut s) = i.try_borrow_mut()
                && let Some(s) = s.as_mut()
            {
                s.window = None;
            }
        });
    }
    result
}

pub fn modifiers() -> KeyModifiers {
    let mut mods = KeyModifiers::NONE;
    // SAFETY: GetKeyState reads the installing UI thread's keyboard queue state.
    unsafe {
        if GetKeyState(VK_SHIFT.0 as i32) < 0 {
            mods |= KeyModifiers::SHIFT;
        }
        if GetKeyState(VK_CONTROL.0 as i32) < 0 {
            mods |= KeyModifiers::CTRL;
        }
        if GetKeyState(VK_MENU.0 as i32) < 0 {
            mods |= KeyModifiers::ALT;
        }
        if GetKeyState(VK_LWIN.0 as i32) < 0 || GetKeyState(VK_RWIN.0 as i32) < 0 {
            mods |= KeyModifiers::SUPER;
        }
    }
    mods
}

impl State {
    fn release_keys(&mut self) {
        if let Some(target) = &self.target {
            for (_, key) in self.pressed.drain() {
                let _ = target.sender.send(Command::Key {
                    key,
                    modifiers: KeyModifiers::NONE,
                    down: false,
                    repeat: false,
                });
            }
        } else {
            self.pressed.clear();
        }
        self.surrogate = None;
    }

    fn retarget(&mut self, target: Option<Target>) {
        self.release_keys();
        if let Some(old) = self.target.take() {
            let _ = old.sender.send(Command::Focus(false));
        }
        self.target = target;
        self.wheel_remainder = 0;
        self.zoom_remainder = 0;
        if let Some(target) = &self.target {
            let _ = target.sender.send(Command::Focus(true));
        }
    }

    fn handle(&mut self, message: &MSG) -> bool {
        if self.window.is_none() && message.hwnd != HWND::default() {
            let root = unsafe { GetAncestor(message.hwnd, GA_ROOT) };
            if unsafe { SetWindowSubclass(root, Some(window_proc), 1, 0) }.as_bool() {
                self.window = Some(root);
            }
        }
        let vk = message.wParam.0 as u32;
        if self.modal {
            return false;
        }
        if matches!(message.message, WM_KEYDOWN | WM_SYSKEYDOWN) && vk == VK_F11.0 as u32 {
            if message.lParam.0 & (1 << 30) == 0 {
                (self.shortcut)(Shortcut::Fullscreen);
            }
            self.swallowed.insert(vk);
            return true;
        }
        let mut mods = modifiers();
        if self.switcher {
            if matches!(message.message, WM_KEYUP | WM_SYSKEYUP)
                && matches!(vk, 0x11 | 0xa2 | 0xa3)
                && !mods.contains(KeyModifiers::CTRL)
            {
                self.switcher = false;
                (self.shortcut)(Shortcut::CommitTab);
                return true;
            }
            if matches!(message.message, WM_KEYDOWN | WM_SYSKEYDOWN) {
                let shortcut = match vk {
                    0x26 => Some(Shortcut::PreviousTab),
                    0x28 => Some(Shortcut::NextTab),
                    0x1b => Some(Shortcut::CancelTab),
                    0x0d => Some(Shortcut::CommitTab),
                    0x09 => Some(if mods.contains(KeyModifiers::SHIFT) {
                        Shortcut::PreviousTab
                    } else {
                        Shortcut::NextTab
                    }),
                    _ => None,
                };
                if let Some(shortcut) = shortcut {
                    self.swallowed.insert(vk);
                    if matches!(shortcut, Shortcut::CancelTab | Shortcut::CommitTab) {
                        self.switcher = false;
                    }
                    (self.shortcut)(shortcut);
                    return true;
                }
                // Other keys belong to the switcher until it commits or cancels.
                return true;
            }
        }
        if matches!(message.message, WM_KEYUP | WM_SYSKEYUP) && self.swallowed.remove(&vk) {
            return true;
        }
        if matches!(message.message, WM_KEYDOWN | WM_SYSKEYDOWN) {
            if self.target.is_some() || self.search {
                let find = if mods.contains(KeyModifiers::CTRL | KeyModifiers::SHIFT) && vk == 0x46
                {
                    Some(Shortcut::Find)
                } else if vk == 0x72 || (self.search && vk == 0x0d) {
                    Some(if mods.contains(KeyModifiers::SHIFT) {
                        Shortcut::FindPrevious
                    } else {
                        Shortcut::FindNext
                    })
                } else if self.search && vk == 0x1b {
                    Some(Shortcut::FindClose)
                } else {
                    None
                };
                if let Some(shortcut) = find {
                    self.swallowed.insert(vk);
                    (self.shortcut)(shortcut);
                    return true;
                }
            }
            let shortcut = if mods.contains(KeyModifiers::CTRL) && vk == VK_TAB.0 as u32 {
                Some(if mods.contains(KeyModifiers::SHIFT) {
                    Shortcut::PreviousTab
                } else {
                    Shortcut::NextTab
                })
            } else if mods.contains(KeyModifiers::CTRL | KeyModifiers::SHIFT) && vk == 0x54 {
                Some(Shortcut::NewTab)
            } else if mods.contains(KeyModifiers::CTRL | KeyModifiers::SHIFT) && vk == 0x57 {
                Some(Shortcut::CloseTab)
            } else if mods.contains(KeyModifiers::CTRL | KeyModifiers::ALT)
                && (0x31..=0x39).contains(&vk)
                // AltGr is also represented as Ctrl+Alt; it must remain text input.
                && unsafe { GetKeyState(VK_RMENU.0 as i32) >= 0 }
            {
                Some(Shortcut::Select((vk - 0x31) as usize))
            } else {
                None
            };
            if let Some(shortcut) = shortcut {
                if matches!(shortcut, Shortcut::NextTab | Shortcut::PreviousTab)
                    && self.preferences.tab_switcher_mode != TabSwitcherMode::Disabled
                {
                    self.switcher = true;
                }
                self.swallowed.insert(vk);
                if message.lParam.0 & (1 << 30) == 0 {
                    (self.shortcut)(shortcut);
                }
                return true;
            }
        }
        if self.search && !matches!(message.message, WM_MOUSEWHEEL | WM_POINTERWHEEL) {
            return false;
        }
        let Some(Target { sender, frames }) = &self.target else {
            return false;
        };
        match message.message {
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                // Let Windows translate injected Unicode and IME-owned input into WM_CHAR.
                if vk == VK_PACKET.0 as u32 || vk == VK_PROCESSKEY.0 as u32 {
                    return false;
                }
                // Leave window-system shortcuts to Windows.
                if mods.contains(KeyModifiers::ALT)
                    && (vk == VK_F4.0 as u32 || vk == VK_SPACE.0 as u32)
                {
                    return false;
                }
                let local = if mods.contains(KeyModifiers::CTRL | KeyModifiers::ALT)
                    && vk == 0x56
                    && unsafe { GetKeyState(VK_RMENU.0 as i32) >= 0 }
                {
                    Some(Shortcut::PasteRaw)
                } else if mods.contains(KeyModifiers::CTRL) && !mods.contains(KeyModifiers::ALT) {
                    match vk {
                        0x56 => Some(Shortcut::Paste),
                        0x43 if mods.contains(KeyModifiers::SHIFT) || self.selection => {
                            Some(Shortcut::Copy)
                        }
                        0x2d => Some(Shortcut::Copy),
                        0xbb | 0x6b => Some(Shortcut::Zoom(1)),
                        0xbd | 0x6d => Some(Shortcut::Zoom(-1)),
                        0x30 | 0x60 => Some(Shortcut::ResetZoom),
                        _ => None,
                    }
                } else if mods.contains(KeyModifiers::SHIFT) && vk == 0x2d {
                    Some(Shortcut::Paste)
                } else {
                    None
                };
                if let Some(shortcut) = local {
                    self.swallowed.insert(vk);
                    (self.shortcut)(shortcut);
                    return true;
                }
                if mods.contains(KeyModifiers::SHIFT) && vk == VK_PRIOR.0 as u32 {
                    self.swallowed.insert(vk);
                    let _ = sender.send(Command::Scroll(
                        frames.load().rows.len().saturating_sub(1).max(1) as isize,
                    ));
                    return true;
                }
                if mods.contains(KeyModifiers::SHIFT) && vk == VK_NEXT.0 as u32 {
                    self.swallowed.insert(vk);
                    let _ = sender.send(Command::Scroll(
                        -(frames.load().rows.len().saturating_sub(1).max(1) as isize),
                    ));
                    return true;
                }
                if mods.contains(KeyModifiers::CTRL | KeyModifiers::SHIFT)
                    && matches!(vk, 0x26 | 0x28)
                {
                    self.swallowed.insert(vk);
                    let _ = sender.send(Command::Prompt(if vk == 0x26 { -1 } else { 1 }));
                    return true;
                }
                if mods.contains(KeyModifiers::CTRL | KeyModifiers::SHIFT)
                    && matches!(vk, 0x24 | 0x23)
                {
                    self.swallowed.insert(vk);
                    let _ = sender.send(Command::ScrollEdge(vk == 0x24));
                    return true;
                }
                if vk == VK_F5.0 as u32 && frames.load().status.starts_with("Terminal lab") {
                    let _ = sender.send(Command::ReloadDemo);
                    return true;
                }
                let repeat = self.pressed.contains_key(&vk) || message.lParam.0 & (1 << 30) != 0;
                if repeat {
                    if let Some(key) = self.pressed.get(&vk)
                        && !self.game_mode
                    {
                        let _ = sender.send(Command::Key {
                            key: *key,
                            modifiers: mods,
                            down: true,
                            repeat: true,
                        });
                    }
                    return true;
                }
                let key = special_key(vk).or_else(|| {
                    let mut keyboard = [0u8; 256];
                    unsafe {
                        GetKeyboardState(&mut keyboard).ok()?;
                    }
                    let altgr = keyboard[VK_RMENU.0 as usize] & 0x80 != 0
                        && mods.contains(KeyModifiers::CTRL);
                    if altgr {
                        mods.remove(KeyModifiers::CTRL | KeyModifiers::ALT);
                    } else {
                        for index in [
                            VK_CONTROL,
                            VK_LCONTROL,
                            VK_RCONTROL,
                            VK_MENU,
                            VK_LMENU,
                            VK_RMENU,
                        ] {
                            keyboard[index.0 as usize] = 0;
                        }
                    }
                    let mut translated = [0u16; 8];
                    // This adapter consumes the keydown, so it owns dead-key translation.
                    let count = unsafe {
                        ToUnicodeEx(
                            vk,
                            ((message.lParam.0 >> 16) & 0xff) as u32,
                            &keyboard,
                            &mut translated,
                            0,
                            Some(GetKeyboardLayout(0)),
                        )
                    };
                    if count <= 0 {
                        return None;
                    }
                    let text = String::from_utf16_lossy(&translated[..count as usize]);
                    let mut chars = text.chars();
                    let first = chars.next()?;
                    if chars.next().is_some() {
                        let _ = sender.send(Command::Text(text));
                        None
                    } else {
                        Some(KeyCode::Char(first))
                    }
                });
                if let Some(key) = key {
                    (self.shortcut)(Shortcut::ClearSelection);
                    self.pressed.insert(vk, key);
                    let _ = sender.send(Command::Key {
                        key,
                        modifiers: mods,
                        down: true,
                        repeat: false,
                    });
                    true
                } else {
                    // Modifiers/dead keys must not trigger WinUI keyboard navigation.
                    matches!(vk, 0x10..=0x14 | 0xa0..=0xa5) || (0x20..=0xfe).contains(&vk)
                }
            }
            WM_KEYUP | WM_SYSKEYUP => {
                if let Some(key) = self.pressed.remove(&vk) {
                    let _ = sender.send(Command::Key {
                        key,
                        modifiers: mods,
                        down: false,
                        repeat: false,
                    });
                    return true;
                }
                false
            }
            WM_CHAR => {
                (self.shortcut)(Shortcut::ClearSelection);
                let unit = vk as u16;
                if (0xd800..=0xdbff).contains(&unit) {
                    self.surrogate = Some(unit);
                    return true;
                }
                let units = self
                    .surrogate
                    .take()
                    .map(|high| vec![high, unit])
                    .unwrap_or_else(|| vec![unit]);
                let text = String::from_utf16_lossy(&units);
                let _ = sender.send(Command::Text(text));
                true
            }
            WM_MOUSEWHEEL | WM_POINTERWHEEL => {
                let delta = ((message.wParam.0 >> 16) as u16) as i16;
                self.wheel(delta, mods)
            }
            _ => false,
        }
    }
    fn wheel(&mut self, delta: i16, mods: KeyModifiers) -> bool {
        let Some(target) = &self.target else {
            return false;
        };
        if mods.contains(KeyModifiers::CTRL) && self.preferences.scroll_to_zoom {
            let steps = wheel_steps(&mut self.zoom_remainder, delta);
            if steps != 0 {
                (self.shortcut)(Shortcut::Zoom(steps));
            }
        } else {
            let steps = wheel_steps(&mut self.wheel_remainder, delta);
            if steps != 0 {
                let mut lines = 3u32;
                // Respect Windows' configured lines-per-notch, including page scrolling.
                let _ = unsafe {
                    SystemParametersInfoW(
                        SPI_GETWHEELSCROLLLINES,
                        0,
                        Some((&mut lines as *mut u32).cast()),
                        SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                    )
                };
                let count = if lines == u32::MAX {
                    target.frames.load().rows.len().saturating_sub(1).max(1)
                } else {
                    lines.min(1000) as usize
                };
                if count != 0 {
                    let _ = target
                        .sender
                        .send(Command::Wheel(steps as isize * count as isize, mods));
                }
            }
        }
        true
    }
}
fn wheel_steps(remainder: &mut i32, delta: i16) -> i32 {
    *remainder += delta as i32;
    let steps = *remainder / 120;
    *remainder %= 120;
    steps
}

fn special_key(vk: u32) -> Option<KeyCode> {
    Some(match vk {
        0x08 => KeyCode::Char('\u{8}'),
        0x09 => KeyCode::Char('\t'),
        0x0d => KeyCode::Char('\r'),
        0x1b => KeyCode::Char('\u{1b}'),
        0x20 => KeyCode::Char(' '),
        0x21 => KeyCode::PageUp,
        0x22 => KeyCode::PageDown,
        0x23 => KeyCode::End,
        0x24 => KeyCode::Home,
        0x25 => KeyCode::LeftArrow,
        0x26 => KeyCode::UpArrow,
        0x27 => KeyCode::RightArrow,
        0x28 => KeyCode::DownArrow,
        0x2d => KeyCode::Insert,
        0x2e => KeyCode::Char('\u{7f}'),
        0x70..=0x87 => KeyCode::Function((vk - 0x6f) as u8),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use w11term::{options::Source, session::Session};
    #[test]
    fn fractional_wheel_deltas_accumulate_in_both_directions() {
        let mut remainder = 0;
        for _ in 0..3 {
            assert_eq!(wheel_steps(&mut remainder, 30), 0);
        }
        assert_eq!(wheel_steps(&mut remainder, 30), 1);
        assert_eq!(remainder, 0);
        assert_eq!(wheel_steps(&mut remainder, -40), 0);
        assert_eq!(wheel_steps(&mut remainder, -80), -1);
        assert_eq!(wheel_steps(&mut remainder, 360), 3);
    }
    #[test]
    fn grid_snap_rounds_to_cells_with_fixed_window_chrome() {
        assert_eq!(snap_extent(1238, 24, 10), 1234);
        assert_eq!(snap_extent(980, 98, 20), 978);
        assert_eq!(snap_extent(0, 24, 10), 34);
    }

    #[test]
    fn switching_tabs_releases_held_keys_to_the_old_session_and_forms_receive_native_input() {
        let lab = Session::start(Source::Demo);
        let (old, mut old_input) = mpsc::unbounded_channel();
        let (new, mut new_input) = mpsc::unbounded_channel();
        let mut state = State {
            target: Some(Target {
                sender: old,
                frames: lab.frames.clone(),
            }),
            shortcut: Box::new(|_| {}),
            swallowed: HashSet::new(),
            pressed: HashMap::from([(0x58, KeyCode::Char('x'))]),
            surrogate: Some(0xd800),
            preferences: Interaction::default(),
            selection: false,
            switcher: false,
            grid: None,
            window: None,
            search: false,
            wheel_remainder: 0,
            zoom_remainder: 0,
            modal: false,
            game_mode: false,
            close_handler: None,
        };
        state.retarget(Some(Target {
            sender: new,
            frames: lab.frames.clone(),
        }));
        assert!(matches!(
            old_input.try_recv().unwrap(),
            Command::Key {
                key: KeyCode::Char('x'),
                down: false,
                repeat: false,
                ..
            }
        ));
        assert!(matches!(
            old_input.try_recv().unwrap(),
            Command::Focus(false)
        ));
        assert!(matches!(
            new_input.try_recv().unwrap(),
            Command::Focus(true)
        ));
        assert!(state.pressed.is_empty());
        assert!(state.surrogate.is_none());
        state.search = true;
        assert!(!state.handle(&MSG {
            message: WM_CHAR,
            wParam: WPARAM('q' as usize),
            ..Default::default()
        }));
        assert!(!state.handle(&MSG {
            message: WM_KEYDOWN,
            wParam: WPARAM(0x41),
            ..Default::default()
        }));
        assert!(
            new_input.try_recv().is_err(),
            "find text must stay in the native textbox"
        );
        state.search = false;
        state.game_mode = true;
        let key_message = |message, repeat| MSG {
            message,
            wParam: WPARAM(0x27),
            lParam: LPARAM(if repeat { 1 << 30 } else { 0 }),
            ..Default::default()
        };
        assert!(state.handle(&key_message(WM_KEYDOWN, false)));
        for _ in 0..5 {
            assert!(state.handle(&key_message(WM_KEYDOWN, true)));
        }
        assert!(state.handle(&key_message(WM_KEYUP, false)));
        assert!(matches!(
            new_input.try_recv().unwrap(),
            Command::Key {
                down: true,
                repeat: false,
                ..
            }
        ));
        assert!(matches!(
            new_input.try_recv().unwrap(),
            Command::Key {
                down: false,
                repeat: false,
                ..
            }
        ));
        assert!(
            new_input.try_recv().is_err(),
            "game-mode repeats leaked to SSH"
        );
        state.game_mode = false;
        state.handle(&key_message(WM_KEYDOWN, false));
        state.handle(&key_message(WM_KEYDOWN, true));
        state.handle(&key_message(WM_KEYUP, false));
        assert!(matches!(
            new_input.try_recv().unwrap(),
            Command::Key {
                down: true,
                repeat: false,
                ..
            }
        ));
        assert!(matches!(
            new_input.try_recv().unwrap(),
            Command::Key {
                down: true,
                repeat: true,
                ..
            }
        ));
        assert!(matches!(
            new_input.try_recv().unwrap(),
            Command::Key {
                down: false,
                repeat: false,
                ..
            }
        ));
        state.retarget(None);
        assert!(matches!(
            new_input.try_recv().unwrap(),
            Command::Focus(false)
        ));
        assert!(!state.handle(&MSG {
            message: WM_CHAR,
            wParam: WPARAM('a' as usize),
            ..Default::default()
        }));
        assert!(new_input.try_recv().is_err());
    }
}
