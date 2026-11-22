//! Raw terminal keys are routed only to the selected session. Forms have no terminal target;
//! native popup windows are excluded by the root-window check. Hooks are UI-thread-local.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use tokio::sync::mpsc;
use w11term::{
    interaction::{Interaction, TabSwitcherMode},
    pane_layout::Direction,
    session::Command,
    shortcuts::{Action, KeyMap, Shortcuts},
    terminal::{FrameStore, KeyDetails, KeypadKey},
};
use wezterm_term::{KeyCode, KeyModifiers};
pub(crate) mod ime;
use windows::Win32::{
    Foundation::*,
    System::Threading::GetCurrentThreadId,
    UI::{
        Input::KeyboardAndMouse::*,
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};

mod window;
pub(crate) use window::{WindowScope, window};
use window::{with_input, with_input_mut};

#[derive(Clone, Copy, PartialEq, Eq)]
enum LocalMode {
    Hints,
    Copy,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PickerStage {
    Search,
    Commands,
    Placement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum KeySlot {
    Virtual(u32),
    PrintableScan(char),
    KeypadScan { scan: u32, extended: bool },
    KeypadVirtual(u32),
}

struct HeldKey {
    vk: u32,
    key: KeyCode,
    details: Option<KeyDetails>,
}

struct State {
    read_only: Option<std::rc::Rc<std::cell::Cell<bool>>>,
    target: Option<Target>,
    shortcut: Box<dyn Fn(Shortcut)>,
    swallowed: HashSet<KeySlot>,
    pressed: HashMap<KeySlot, HeldKey>,
    surrogate: Option<u16>,
    preferences: Interaction,
    keymap: KeyMap,
    switcher_trigger: SwitchTrigger,
    selection: bool,
    switcher: bool,
    grid: Option<Grid>,
    search: bool,
    picker: Option<PickerStage>,
    local_mode: Option<LocalMode>,
    // A true entry may repeat a movement first pressed in this copy-mode session.
    local_held: HashMap<KeySlot, bool>,
    held_resize: Option<HeldResize>,
    wheel_panes: WheelPanes,
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

#[derive(Clone, Copy)]
struct HeldResize {
    key: KeySlot,
    repeat: Option<(KeyModifiers, Direction)>,
}

#[derive(Default)]
struct WheelPanes {
    visible: HashMap<u64, (Target, i32)>,
    hovered: Option<u64>,
}

impl WheelPanes {
    fn swapped(&mut self, [first, second]: [u64; 2]) {
        let replacement = match self.hovered {
            Some(id) if id == first => second,
            Some(id) if id == second => first,
            _ => return,
        };
        self.hovered = self
            .visible
            .contains_key(&replacement)
            .then_some(replacement);
    }

    fn replace(
        &mut self,
        panes: impl IntoIterator<Item = (u64, mpsc::UnboundedSender<Command>, FrameStore)>,
    ) {
        self.visible = panes
            .into_iter()
            .map(|(id, sender, frames)| {
                let remainder = self
                    .visible
                    .get(&id)
                    .filter(|(target, _)| target.sender.same_channel(&sender))
                    .map_or(0, |(_, remainder)| *remainder);
                (id, (Target { sender, frames }, remainder))
            })
            .collect();
        self.hovered = self.hovered.filter(|id| self.visible.contains_key(id));
    }

    fn target(&mut self) -> Option<(&Target, &mut i32)> {
        // The single canvas has historically accepted wheel input anywhere
        // in the terminal window. Multiple canvases require a pointer hit.
        let (target, remainder) = if self.visible.len() == 1 {
            self.visible.values_mut().next()?
        } else {
            self.visible.get_mut(&self.hovered?)?
        };
        Some((target, remainder))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    Fullscreen,
    RememberWindow,
    BeginResize,
    TaskbarCreated,
    CaretPresentationChanged,
    NextTab,
    PreviousTab,
    NewTab,
    DuplicateTabLayout,
    CloseTab,
    SplitRight,
    SplitBelow,
    SplitRightInCurrentDirectory,
    SplitBelowInCurrentDirectory,
    FocusPane(Direction),
    ResizePane(Direction),
    SwapPane(Direction),
    ZoomPane,
    RotateSplit,
    ClosePane,
    Select(usize),
    Copy,
    Paste,
    PasteRaw,
    ClearSelection,
    ClearScrollback,
    Zoom(i32),
    ResetZoom,
    CommitTab,
    CancelTab,
    Find,
    FindNext,
    FindPrevious,
    FindClose,
    CommandPalette,
    PanePicker,
    FindAllPanes,
    PickerMove(isize),
    PickerAccept,
    PickerCancel,
    PickerPlace(Direction),
    KeyboardHints,
    HintKey(u32),
    CopyMode,
    CopyKey(w11term::copy_mode::CopyKey),
    MovePane,
    MovePaneToExistingTab,
    Workspaces,
}

pub struct KeyboardBridge {
    scope: WindowScope,
    bootstrap: String,
    _hooks: std::rc::Rc<ThreadHooks>,
}

struct ThreadHooks {
    messages: HHOOK,
    focus: HHOOK,
    mouse: HHOOK,
}
type PaneTarget = (
    mpsc::UnboundedSender<Command>,
    FrameStore,
    std::rc::Rc<std::cell::Cell<bool>>,
);

impl ThreadHooks {
    fn install() -> windows::core::Result<Self> {
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
        Ok(Self {
            messages: hook,
            focus,
            mouse,
        })
    }
}

impl Drop for ThreadHooks {
    fn drop(&mut self) {
        unsafe {
            let _ = UnhookWindowsHookEx(self.messages);
            let _ = UnhookWindowsHookEx(self.focus);
            let _ = UnhookWindowsHookEx(self.mouse);
        }
    }
}

impl KeyboardBridge {
    pub fn terminal_focused(&self, sender: &mpsc::UnboundedSender<Command>) -> bool {
        let _native = self.scope.enter();
        with_input(|input| {
            input.borrow().as_ref().is_some_and(|state| {
                window::visible()
                    && state.terminal_focused()
                    && state
                        .target
                        .as_ref()
                        .is_some_and(|target| target.sender.same_channel(sender))
            })
        })
    }
    pub fn install(shortcut: impl Fn(Shortcut) + 'static) -> windows::core::Result<Self> {
        let scope = WindowScope::default();
        let _guard = scope.enter();
        with_input_mut(|input| *input = Some(State::new(shortcut)));
        Ok(Self {
            scope,
            bootstrap: format!("w11term-window-{:032x}", rand::random::<u128>()),
            _hooks: window::install_hooks()?,
        })
    }

    pub fn set_target(&self, target: Option<PaneTarget>) {
        let _native = self.scope.enter();
        with_input(|input| {
            if let Some(state) = input.borrow_mut().as_mut() {
                state.read_only = target.as_ref().map(|(_, _, locked)| locked.clone());
                state.retarget(target.map(|(sender, frames, _)| Target { sender, frames }));
            }
        });
        ime::sync_context();
    }

    pub fn set_read_only(
        sender: &mpsc::UnboundedSender<Command>,
        flag: &std::cell::Cell<bool>,
        enabled: bool,
    ) {
        if flag.get() == enabled {
            return;
        }
        let cancel_composition = enabled
            && with_input_mut(|input| {
                if let Some(state) = input.as_mut()
                    && state
                        .target
                        .as_ref()
                        .is_some_and(|t| t.sender.same_channel(sender))
                {
                    state
                        .local_held
                        .extend(state.pressed.keys().map(|vk| (*vk, false)));
                    state.release_keys();
                    return true;
                }
                false
            });
        if cancel_composition {
            ime::cancel();
        }
        if sender.send(Command::SetReadOnly(enabled)).is_ok() {
            flag.set(enabled);
        }
        ime::sync_context();
    }
    pub fn set_wheel_panes(
        &self,
        panes: impl IntoIterator<Item = (u64, mpsc::UnboundedSender<Command>, FrameStore)>,
    ) {
        let _native = self.scope.enter();
        with_input(|input| {
            if let Some(state) = input.borrow_mut().as_mut() {
                state.wheel_panes.replace(panes);
            }
        });
    }
    pub fn hover_pane(&self, id: u64) {
        let _native = self.scope.enter();
        with_input(|input| {
            if let Some(state) = input.borrow_mut().as_mut()
                && state.wheel_panes.visible.contains_key(&id)
            {
                state.wheel_panes.hovered = Some(id);
            }
        });
    }
    pub fn hovered_pane(&self) -> Option<u64> {
        let _native = self.scope.enter();
        with_input(|input| input.borrow().as_ref()?.wheel_panes.hovered)
    }
    pub fn panes_swapped(&self, pair: [u64; 2]) {
        let _native = self.scope.enter();
        with_input_mut(|input| {
            if let Some(state) = input.as_mut() {
                state.wheel_panes.swapped(pair);
            }
        });
    }
    pub fn leave_pane(&self, id: u64) {
        let _native = self.scope.enter();
        with_input(|input| {
            if let Some(state) = input.borrow_mut().as_mut()
                && state.wheel_panes.hovered == Some(id)
            {
                state.wheel_panes.hovered = None;
            }
        });
    }
    pub fn set_preferences(&self, preferences: Interaction) {
        let _native = self.scope.enter();
        with_input(|i| {
            if let Some(s) = i.borrow_mut().as_mut() {
                s.preferences = preferences;
            }
        });
    }
    pub fn set_shortcuts(&self, keymap: KeyMap) {
        let _native = self.scope.enter();
        with_input(|i| {
            if let Some(s) = i.borrow_mut().as_mut() {
                s.keymap = keymap;
            }
        });
    }
    pub fn set_selection(&self, selected: bool) {
        let _native = self.scope.enter();
        with_input(|i| {
            if let Some(s) = i.borrow_mut().as_mut() {
                s.selection = selected;
            }
        });
    }
    pub fn set_switcher(&self, visible: bool) {
        let _native = self.scope.enter();
        with_input(|i| {
            if let Some(s) = i.borrow_mut().as_mut() {
                s.switcher = visible;
                if visible {
                    ime::cancel();
                    s.wheel_panes.hovered = None;
                }
            }
        });
        ime::sync_context();
    }
    pub fn set_search(&self, visible: bool) {
        let _native = self.scope.enter();
        with_input(|i| {
            if let Some(s) = i.borrow_mut().as_mut() {
                if visible {
                    ime::cancel();
                    s.release_keys();
                    s.wheel_panes.hovered = None;
                }
                s.search = visible;
                s.report_focus();
            }
        });
        ime::sync_context();
    }
    pub fn set_picker(&self, stage: Option<PickerStage>) {
        let _native = self.scope.enter();
        with_input(|input| {
            if let Some(s) = input.borrow_mut().as_mut() {
                s.picker = stage;
                if stage.is_some() {
                    ime::cancel();
                    s.local_held.extend(s.pressed.keys().map(|&vk| (vk, false)));
                    s.release_keys();
                    s.wheel_panes.hovered = None;
                    if let Some(target) = &s.target {
                        let _ = target.sender.send(Command::Focus(false));
                    }
                }
            }
        });
        ime::sync_context();
    }
    pub fn set_picker_placement(&self) {
        let _native = self.scope.enter();
        with_input(|input| {
            if let Some(s) = input.borrow_mut().as_mut()
                && s.picker.is_some()
            {
                s.picker = Some(PickerStage::Placement);
            }
        });
    }
    pub fn set_hints(&self, visible: bool) {
        self.set_local_mode(visible.then_some(LocalMode::Hints));
    }
    pub fn set_copy_mode(&self, visible: bool) {
        self.set_local_mode(visible.then_some(LocalMode::Copy));
    }
    fn set_local_mode(&self, mode: Option<LocalMode>) {
        let _native = self.scope.enter();
        with_input(|input| {
            if let Some(s) = input.borrow_mut().as_mut() {
                s.local_mode = mode;
                if mode.is_some() {
                    ime::cancel();
                    for owned in s.local_held.values_mut() {
                        *owned = false;
                    }
                    s.local_held.extend(s.pressed.keys().map(|&vk| (vk, false)));
                    s.release_keys();
                    s.wheel_panes.hovered = None;
                }
                s.report_focus();
            }
        });
        ime::sync_context();
    }
    pub fn set_modal(&self, visible: bool) {
        let _native = self.scope.enter();
        let changed = with_input(|i| {
            if let Some(s) = i.borrow_mut().as_mut() {
                if s.modal == visible {
                    return false;
                }
                if visible {
                    ime::cancel();
                    s.release_keys();
                    s.wheel_panes.hovered = None;
                }
                s.modal = visible;
                return true;
            }
            false
        });
        if changed {
            ime::sync_context();
        }
    }
    pub fn on_close(&self, handler: impl Fn() + 'static) {
        let _native = self.scope.enter();
        with_input(|state| {
            if let Some(state) = state.borrow_mut().as_mut() {
                state.close_handler = Some(Box::new(handler));
            }
        });
    }
    pub fn reset_keys(&self, sender: &mpsc::UnboundedSender<Command>) {
        let _native = self.scope.enter();
        let owns_input = with_input_mut(|input| {
            if let Some(s) = input
                && s.target
                    .as_ref()
                    .is_some_and(|target| target.sender.same_channel(sender))
            {
                s.pressed.clear();
                s.surrogate = None;
                return true;
            }
            false
        });
        if owns_input {
            ime::cancel();
        }
    }
    pub fn set_game_mode(&self, game: bool) {
        let _native = self.scope.enter();
        with_input(|i| {
            if let Some(s) = i.borrow_mut().as_mut() {
                s.game_mode = game;
            }
        });
    }

    #[cfg(feature = "ui-smoke")]
    pub fn smoke_wheel(&self, delta: i16) -> bool {
        let hwnd = self.hwnd().expect("terminal HWND");
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
        let _native = self.scope.enter();
        with_input(|input| {
            input
                .borrow()
                .as_ref()
                .is_some_and(|state| state.target.is_some())
        })
    }
}

impl Drop for KeyboardBridge {
    fn drop(&mut self) {
        let _guard = self.scope.enter();
        ime::uninstall();
        if let Some(mut state) = with_input_mut(Option::take) {
            state.release_keys();
        }
        self.scope.remove();
    }
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && wparam.0 as u32 == WM_MOUSEWHEEL {
        // Thread-local WH_MOUSE sees real wheel input before WinUI translates it.
        let mouse = unsafe { &*(lparam.0 as *const MOUSEHOOKSTRUCTEX) };
        if let Some(scope) = WindowScope::for_window(mouse.Base.hwnd) {
            let _guard = scope.enter();
            let consumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                with_input(|i| {
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
        if matches!(
            message.message,
            WM_SETFOCUS | WM_IME_STARTCOMPOSITION | WM_IME_COMPOSITION | WM_IME_SETCONTEXT
        ) && let Some(scope) = WindowScope::for_window(message.hwnd)
        {
            let _guard = scope.enter();
            ime::observe(message.hwnd, message.message);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam.0 == PM_REMOVE.0 as usize {
        // SAFETY: WH_GETMESSAGE provides a valid MSG for this callback invocation.
        let message = unsafe { &mut *(lparam.0 as *mut MSG) };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if !matches!(
                message.message,
                WM_KEYDOWN
                    | WM_KEYUP
                    | WM_SYSKEYDOWN
                    | WM_SYSKEYUP
                    | WM_CHAR
                    | WM_SYSCHAR
                    | WM_DEADCHAR
                    | WM_SYSDEADCHAR
                    | WM_MOUSEWHEEL
                    | WM_POINTERWHEEL
            ) {
                return;
            }
            let Some(scope) = WindowScope::for_window(message.hwnd) else {
                return;
            };
            let _guard = scope.enter();
            with_input(|input| {
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
    window().is_some_and(|root| unsafe { GetAncestor(hwnd, GA_ROOT) } == root)
}

/// Current physical client pixel, provided that this window is actually under the cursor.
pub(crate) fn cursor_client() -> Option<(i32, i32)> {
    let hwnd = window()?;
    let mut point = POINT::default();
    unsafe {
        GetCursorPos(&mut point).ok()?;
        if !is_terminal(WindowFromPoint(point))
            || !windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut point).as_bool()
        {
            return None;
        }
    }
    Some((point.x, point.y))
}

#[derive(Clone, Copy)]
struct Grid {
    cw: i32,
    ch: i32,
    width: f32,
    height: f32,
    scale: f32,
}
pub fn clear_grid() {
    with_input(|i| {
        if let Ok(mut state) = i.try_borrow_mut()
            && let Some(state) = state.as_mut()
        {
            state.grid = None;
        }
    });
}
pub fn set_grid(cw: f32, ch: f32, width: f32, height: f32, scale: f32) {
    with_input(|i| {
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
    static TASKBAR_CREATED: std::sync::LazyLock<u32> = std::sync::LazyLock::new(|| unsafe {
        RegisterWindowMessageW(windows::core::w!("TaskbarButtonCreated"))
    });
    let taskbar_created = message != 0 && message == *TASKBAR_CREATED;
    if !matches!(
        message,
        WM_ACTIVATE
            | WM_SETTINGCHANGE
            | WM_SIZE
            | WM_WINDOWPOSCHANGED
            | WM_CLOSE
            | WM_ENTERSIZEMOVE
            | WM_EXITSIZEMOVE
            | WM_SIZING
            | WM_NCDESTROY
    ) && !taskbar_created
    {
        return unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    }
    let Some(scope) = WindowScope::for_window(hwnd) else {
        return unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    };
    let _guard = scope.enter();
    if message == WM_ACTIVATE {
        window::activated((wparam.0 & 0xffff) != WA_INACTIVE as usize);
    }
    // Persist before native destruction; a queued component message can be
    // retired with the window before it gets a chance to inspect its HWND.
    if message == WM_CLOSE {
        with_input(|state| {
            if let Ok(state) = state.try_borrow()
                && let Some(state) = state.as_ref()
                && let Some(handler) = &state.close_handler
            {
                handler();
            }
        });
    }
    if message == WM_ENTERSIZEMOVE {
        with_input(|s| {
            if let Ok(s) = s.try_borrow()
                && let Some(s) = s.as_ref()
            {
                (s.shortcut)(Shortcut::BeginResize);
            }
        });
    }
    let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    let visibility_changed =
        matches!(message, WM_SIZE | WM_WINDOWPOSCHANGED) && scope.refresh_visibility(hwnd);
    if taskbar_created || message == WM_SETTINGCHANGE || visibility_changed {
        with_input(|s| {
            if let Ok(s) = s.try_borrow()
                && let Some(s) = s.as_ref()
            {
                (s.shortcut)(if taskbar_created {
                    Shortcut::TaskbarCreated
                } else {
                    Shortcut::CaretPresentationChanged
                });
            }
        });
    }
    if matches!(message, WM_EXITSIZEMOVE | WM_CLOSE) {
        with_input(|s| {
            if let Ok(s) = s.try_borrow()
                && let Some(s) = s.as_ref()
            {
                (s.shortcut)(Shortcut::RememberWindow);
            }
        });
    }
    if message == WM_SIZING && lparam.0 != 0 {
        with_input(|i| {
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
        scope.remove();
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

fn emit_key(
    sender: &mpsc::UnboundedSender<Command>,
    key: KeyCode,
    details: Option<&KeyDetails>,
    mut modifiers: KeyModifiers,
    down: bool,
    repeat_scan: Option<(u32, u32)>,
) {
    let repeat = repeat_scan.is_some();
    let command = if let Some(details) = details {
        let mut details = details.clone();
        if details.altgr
            && modifiers.contains(KeyModifiers::CTRL | KeyModifiers::ALT)
            && unsafe { GetKeyState(VK_RMENU.0.into()) < 0 }
        {
            modifiers.remove(KeyModifiers::CTRL | KeyModifiers::ALT);
        }
        if details.keypad.is_some() || details.physical.is_some() {
            // Lock modifiers describe this event, including repeats and releases
            // after the lock changed while the physical key was held.
            use wezterm_input_types::KeyboardLedStatus as Leds;
            unsafe {
                details
                    .leds
                    .set(Leds::NUM_LOCK, GetKeyState(VK_NUMLOCK.0.into()) & 1 != 0);
                details
                    .leds
                    .set(Leds::CAPS_LOCK, GetKeyState(VK_CAPITAL.0.into()) & 1 != 0);
            }
        }
        if let Some((vk, scan)) = repeat_scan
            && details.physical.is_some()
            && details.text.is_none()
        {
            // The physical identity still belongs to the initial press, but typing
            // Shift/Caps Lock while holding it can change subsequent associated text.
            let mut state = [0; 256];
            if unsafe { GetKeyboardState(&mut state) }.is_ok() {
                if !details.altgr {
                    clear_command_modifiers(&mut state);
                }
                details.repeat_text = layout_text(vk, scan, &state);
            }
        }
        Command::DetailedKey {
            key,
            details,
            modifiers,
            down,
            repeat,
        }
    } else {
        Command::Key {
            key,
            modifiers,
            down,
            repeat,
        }
    };
    let _ = sender.send(command);
}
fn clear_command_modifiers(keyboard: &mut [u8; 256]) {
    for modifier in [
        VK_CONTROL,
        VK_LCONTROL,
        VK_RCONTROL,
        VK_MENU,
        VK_LMENU,
        VK_RMENU,
    ] {
        keyboard[modifier.0 as usize] = 0;
    }
}

// Flag 4 asks Windows to translate without consuming its pending dead key.
fn layout_text(vk: u32, scan: u32, keyboard: &[u8; 256]) -> Option<String> {
    let mut result = [0u16; 8];
    let count = unsafe {
        ToUnicodeEx(
            vk,
            scan,
            keyboard,
            &mut result,
            4,
            Some(GetKeyboardLayout(0)),
        )
    };
    if count <= 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&result[..count as usize]))
}

fn layout_level(vk: u32, scan: u32, mut keyboard: [u8; 256], shift: bool) -> Option<char> {
    keyboard[VK_RSHIFT.0 as usize] = 0;
    let state = if shift { 0x80 } else { 0 };
    keyboard[VK_SHIFT.0 as usize] = state;
    keyboard[VK_LSHIFT.0 as usize] = state;
    keyboard[VK_CAPITAL.0 as usize] = 0;
    let text = layout_text(vk, scan, &keyboard)?;
    let mut chars = text.chars();
    let first = chars.next()?;
    (chars.next().is_none() && !first.is_control()).then_some(first)
}

fn translated_key(
    count: i32,
    translated: &[u16],
    base: Option<char>,
    position: Option<char>,
    altgr: bool,
) -> Option<(KeyCode, KeyDetails)> {
    // A negative ToUnicodeEx result is a pending dead key. Its spacing
    // character in the output buffer is not committed text or a key event.
    let units = translated.get(..usize::try_from(count).ok()?)?;
    let text = String::from_utf16_lossy(units);
    let mut chars = text.chars();
    let first = chars.next()?;
    let composed = chars.next().is_some();
    Some((
        KeyCode::Char(first),
        KeyDetails {
            base,
            physical: position.filter(|&p| p != base.unwrap_or(first)),
            altgr,
            text: composed.then_some(text),
            ..KeyDetails::default()
        },
    ))
}

impl State {
    fn is_read_only(&self) -> bool {
        self.read_only.as_ref().is_some_and(|locked| locked.get())
    }
    fn new(shortcut: impl Fn(Shortcut) + 'static) -> Self {
        Self {
            target: None,
            shortcut: Box::new(shortcut),
            swallowed: HashSet::new(),
            read_only: None,
            pressed: HashMap::new(),
            surrogate: None,
            preferences: Interaction::default(),
            keymap: Shortcuts::default()
                .keymap()
                .expect("unique default bindings"),
            switcher_trigger: SwitchTrigger::Modifier(KeyModifiers::CTRL),
            selection: false,
            switcher: false,
            grid: None,
            search: false,
            picker: None,
            local_mode: None,
            local_held: HashMap::new(),
            held_resize: None,
            wheel_panes: WheelPanes::default(),
            wheel_remainder: 0,
            zoom_remainder: 0,
            modal: false,
            game_mode: false,
            close_handler: None,
        }
    }

    fn release_keys(&mut self) {
        // The physical key still belongs to the old shortcut until key-up.
        // A picker or native text field can receive repeat messages before then.
        if let Some(held) = &mut self.held_resize {
            held.repeat = None;
        }
        if let Some(target) = &self.target {
            for (_, held) in self.pressed.drain() {
                emit_key(
                    &target.sender,
                    held.key,
                    held.details.as_ref(),
                    KeyModifiers::NONE,
                    false,
                    None,
                );
            }
        } else {
            self.pressed.clear();
        }
        self.surrogate = None;
    }

    fn retarget(&mut self, target: Option<Target>) {
        if !self
            .target
            .as_ref()
            .zip(target.as_ref())
            .is_some_and(|(a, b)| a.sender.same_channel(&b.sender))
        {
            ime::cancel();
        }
        self.release_keys();
        if let Some(old) = self.target.take() {
            let _ = old.sender.send(Command::Focus(false));
        }
        self.target = target;
        self.wheel_remainder = 0;
        self.zoom_remainder = 0;
        self.report_focus();
    }

    fn terminal_focused(&self) -> bool {
        window::active()
            && !self.search
            && !self.modal
            && !self.switcher
            && self.picker.is_none()
            && self.local_mode.is_none()
    }
    fn report_focus(&self) {
        if let Some(target) = &self.target {
            let _ = target.sender.send(Command::Focus(self.terminal_focused()));
        }
    }

    fn handle(&mut self, message: &MSG) -> bool {
        let raw_vk = message.wParam.0 as u32;
        let vk = if matches!(
            message.message,
            WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP
        ) {
            key_identity(raw_vk, message.lParam.0)
        } else {
            raw_vk
        };
        let scan = ((message.lParam.0 >> 16) & 0xff) as u32;
        let extended = message.lParam.0 & (1 << 24) != 0;
        let keypad = match message.message {
            WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => keypad_key(vk, scan, extended),
            WM_CHAR | WM_SYSCHAR if vk == 0x0d && extended => Some(KeypadKey::Enter),
            _ => None,
        };
        let physical = pc101_printable(scan, extended);
        // Shortcut lookup uses the logical VK. Ownership follows the physical
        // position, including when Windows changes VK on a Num Lock transition.
        let slot = match (keypad, physical) {
            (Some(_), _) if scan == 0 => KeySlot::KeypadVirtual(vk),
            (Some(_), _) => KeySlot::KeypadScan { scan, extended },
            (None, Some(position)) => KeySlot::PrintableScan(position),
            (None, None) => KeySlot::Virtual(vk),
        };
        if let Some(held) = self.held_resize
            && held.key == slot
        {
            match message.message {
                WM_KEYDOWN | WM_SYSKEYDOWN if message.lParam.0 & (1 << 30) != 0 => {
                    if let Some((required, direction)) = held.repeat
                        && self.target.is_some()
                        && !self.modal
                        && self.local_mode.is_none()
                        && self.picker.is_none()
                        && !self.switcher
                        && !self.search
                        && modifiers() == required
                    {
                        (self.shortcut)(Shortcut::ResizePane(direction));
                    }
                    return true;
                }
                WM_KEYUP | WM_SYSKEYUP => {
                    self.held_resize = None;
                    self.swallowed.remove(&slot);
                    return true;
                }
                WM_KEYDOWN | WM_SYSKEYDOWN => self.held_resize = None,
                _ => {}
            }
        }
        if self.modal {
            return false;
        }
        if let Some(&repeat_owned) = self.local_held.get(&slot) {
            match message.message {
                // A translated control character can arrive after Enter or Escape closed the overlay.
                WM_CHAR | WM_SYSCHAR if matches!(vk, 0x0d | 0x1b) => return true,
                WM_KEYUP | WM_SYSKEYUP => {
                    self.local_held.remove(&slot);
                    self.swallowed.remove(&slot);
                    return true;
                }
                WM_KEYDOWN | WM_SYSKEYDOWN if message.lParam.0 & (1 << 30) != 0 => {
                    if self.local_mode != Some(LocalMode::Copy) || !repeat_owned {
                        return true;
                    }
                }
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    self.local_held.remove(&slot);
                }
                _ => {}
            }
        }
        if let Some(mode) = self.local_mode {
            match message.message {
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    if vk == VK_F4.0 as u32 && modifiers().contains(KeyModifiers::ALT) {
                        return false;
                    }
                    let repeat = message.lParam.0 & (1 << 30) != 0;
                    if repeat && !self.local_held.contains_key(&slot) {
                        return true;
                    }
                    self.local_held.insert(slot, mode == LocalMode::Copy);
                    if mode == LocalMode::Copy {
                        if let Some(key) = w11term::copy_mode::CopyKey::from_vk(vk, modifiers())
                            && (!repeat || key.repeats())
                        {
                            if key == w11term::copy_mode::CopyKey::Cancel {
                                // The UI dismisses the overlay asynchronously. Keep Escape's
                                // release local, but let the next key use the terminal shortcuts.
                                self.local_mode = None;
                            }
                            (self.shortcut)(Shortcut::CopyKey(key));
                        }
                    } else if !repeat
                        && matches!(vk, 0x41..=0x5a | 0x08 | 0x1b | 0x0d)
                        && !modifiers().intersects(KeyModifiers::CTRL | KeyModifiers::ALT)
                    {
                        (self.shortcut)(Shortcut::HintKey(vk));
                    }
                    return true;
                }
                WM_KEYUP | WM_SYSKEYUP => {
                    self.swallowed.remove(&slot);
                    return true;
                }
                WM_CHAR | WM_SYSCHAR | WM_DEADCHAR | WM_SYSDEADCHAR => return true,
                _ => {}
            }
        }
        if let Some(stage) = self.picker {
            if matches!(message.message, WM_KEYUP | WM_SYSKEYUP) && self.swallowed.remove(&slot) {
                // Fresh Enter and Space belong to the focused placement button
                // while it is mounted. Space activates native buttons on release.
                return stage != PickerStage::Placement || !matches!(vk, 0x0d | 0x20);
            }
            if matches!(message.message, WM_KEYDOWN | WM_SYSKEYDOWN) {
                let repeat = message.lParam.0 & (1 << 30) != 0;
                let action = match (stage, vk) {
                    (_, 0x1b) => Shortcut::PickerCancel,
                    (PickerStage::Commands, 0x09) => {
                        Shortcut::PickerMove(if modifiers().contains(KeyModifiers::SHIFT) {
                            -1
                        } else {
                            1
                        })
                    }
                    (PickerStage::Search | PickerStage::Commands, 0x0d) => Shortcut::PickerAccept,
                    (PickerStage::Search | PickerStage::Commands, 0x26) => Shortcut::PickerMove(-1),
                    (PickerStage::Search | PickerStage::Commands, 0x28) => Shortcut::PickerMove(1),
                    (PickerStage::Placement, 0x25) => Shortcut::PickerPlace(Direction::Left),
                    (PickerStage::Placement, 0x26) => Shortcut::PickerPlace(Direction::Up),
                    (PickerStage::Placement, 0x27) => Shortcut::PickerPlace(Direction::Right),
                    (PickerStage::Placement, 0x28) => Shortcut::PickerPlace(Direction::Down),
                    (PickerStage::Placement, 0x0d | 0x20) => {
                        if repeat && !self.swallowed.contains(&slot) {
                            // Held in the search field before the buttons appeared.
                            self.local_held.insert(slot, false);
                        } else {
                            self.swallowed.insert(slot);
                        }
                        return repeat;
                    }
                    _ => return false,
                };
                if matches!(action, Shortcut::PickerMove(_)) {
                    self.swallowed.insert(slot);
                    (self.shortcut)(action);
                } else {
                    // A selection, placement or cancel key stays local until its
                    // release, even when its action changes the active stage/pane.
                    self.local_held.insert(slot, false);
                    if !repeat {
                        (self.shortcut)(action);
                    }
                }
                return true;
            }
            // Native text editing owns printable keys, composition and clipboard shortcuts.
            return false;
        }
        if matches!(vk, 0x09 | 0x0d | 0x20 | 0x26 | 0x28) && self.swallowed.contains(&slot) {
            // The button or row that owned the key may have closed the picker.
            match message.message {
                WM_KEYDOWN | WM_SYSKEYDOWN if message.lParam.0 & (1 << 30) != 0 => return true,
                WM_CHAR | WM_SYSCHAR if matches!(vk, 0x09 | 0x0d | 0x20) => return true,
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    self.swallowed.remove(&slot);
                }
                _ => {}
            }
        }
        if ime::active() {
            match message.message {
                WM_CHAR | WM_SYSCHAR => return true,
                WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => return false,
                _ => {}
            }
        }
        if matches!(message.message, WM_KEYDOWN | WM_SYSKEYDOWN) && vk == VK_F11.0 as u32 {
            if message.lParam.0 & (1 << 30) == 0 {
                (self.shortcut)(Shortcut::Fullscreen);
            }
            self.swallowed.insert(slot);
            return true;
        }
        let mods = modifiers();
        if self.switcher {
            if matches!(message.message, WM_KEYUP | WM_SYSKEYUP)
                && self.switcher_trigger.released(slot, mods)
            {
                self.swallowed.remove(&slot);
                self.switcher = false;
                (self.shortcut)(Shortcut::CommitTab);
                return true;
            }
            if matches!(message.message, WM_KEYDOWN | WM_SYSKEYDOWN) {
                let binding = self.keymap.resolve(vk, mods, false, unsafe {
                    GetKeyState(VK_RMENU.0 as i32) < 0
                });
                let shortcut = match (binding, vk) {
                    (Some(Action::PreviousTab), _) | (_, 0x26) => Some(Shortcut::PreviousTab),
                    (Some(Action::NextTab), _) | (_, 0x28) => Some(Shortcut::NextTab),
                    (_, 0x1b) => Some(Shortcut::CancelTab),
                    (_, 0x0d) => Some(Shortcut::CommitTab),
                    _ => None,
                };
                if let Some(shortcut) = shortcut {
                    self.swallowed.insert(slot);
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
        if matches!(message.message, WM_KEYUP | WM_SYSKEYUP) && self.swallowed.remove(&slot) {
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
                    self.swallowed.insert(slot);
                    (self.shortcut)(shortcut);
                    return true;
                }
            }
            let right_alt = unsafe { GetKeyState(VK_RMENU.0 as i32) < 0 };
            let shortcut = self
                .keymap
                .resolve(vk, mods, self.target.is_some() && !self.search, right_alt)
                .map(Shortcut::from);
            if let Some(shortcut) = shortcut {
                if matches!(shortcut, Shortcut::NextTab | Shortcut::PreviousTab)
                    && self.preferences.tab_switcher_mode != TabSwitcherMode::Disabled
                {
                    self.switcher_trigger = SwitchTrigger::new(slot, mods);
                    self.switcher = true;
                }
                self.swallowed.insert(slot);
                if message.lParam.0 & (1 << 30) == 0 {
                    if matches!(
                        shortcut,
                        Shortcut::CommandPalette
                            | Shortcut::PanePicker
                            | Shortcut::FindAllPanes
                            | Shortcut::MovePaneToExistingTab
                    ) {
                        self.picker = Some(if shortcut == Shortcut::CommandPalette {
                            PickerStage::Commands
                        } else {
                            PickerStage::Search
                        });
                        self.local_held.insert(slot, false);
                    }
                    if matches!(shortcut, Shortcut::KeyboardHints) {
                        self.local_mode = Some(LocalMode::Hints);
                    } else if matches!(shortcut, Shortcut::CopyMode) {
                        self.local_mode = Some(LocalMode::Copy);
                    }
                    if let Shortcut::ResizePane(direction) = shortcut {
                        self.held_resize = Some(HeldResize {
                            key: slot,
                            repeat: Some((mods, direction)),
                        });
                    }
                    if shortcut == Shortcut::ClearScrollback {
                        // Bind the destructive action to the pane receiving this keydown.
                        if let Some(target) = &self.target {
                            let _ = target.sender.send(Command::ClearScrollback);
                        }
                    } else {
                        (self.shortcut)(shortcut);
                    }
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
                    self.swallowed.insert(slot);
                    (self.shortcut)(shortcut);
                    return true;
                }
                if mods.contains(KeyModifiers::SHIFT) && vk == VK_PRIOR.0 as u32 {
                    self.swallowed.insert(slot);
                    let _ = sender.send(Command::Scroll(
                        frames.load().rows.len().saturating_sub(1).max(1) as isize,
                    ));
                    return true;
                }
                if mods.contains(KeyModifiers::SHIFT) && vk == VK_NEXT.0 as u32 {
                    self.swallowed.insert(slot);
                    let _ = sender.send(Command::Scroll(
                        -(frames.load().rows.len().saturating_sub(1).max(1) as isize),
                    ));
                    return true;
                }
                if mods == (KeyModifiers::CTRL | KeyModifiers::SHIFT) && matches!(vk, 0x26 | 0x28) {
                    self.swallowed.insert(slot);
                    let _ = sender.send(Command::Prompt(if vk == 0x26 { -1 } else { 1 }));
                    return true;
                }
                if mods == (KeyModifiers::CTRL | KeyModifiers::SHIFT) && matches!(vk, 0x24 | 0x23) {
                    self.swallowed.insert(slot);
                    let _ = sender.send(Command::ScrollEdge(vk == 0x24));
                    return true;
                }
                if vk == VK_F5.0 as u32 && frames.load().status.starts_with("Terminal lab") {
                    let _ = sender.send(Command::ReloadDemo);
                    return true;
                }
                if self.is_read_only() {
                    self.local_held.insert(slot, false);
                    return true;
                }
                if let Some(held) = self.pressed.get(&slot) {
                    if !self.game_mode {
                        emit_key(
                            sender,
                            held.key,
                            held.details.as_ref(),
                            mods,
                            true,
                            Some((vk, scan)),
                        );
                    }
                    return true;
                }
                if message.lParam.0 & (1 << 30) != 0 {
                    // Windows tracks the preceding down by VK. A different
                    // physical key mapped to the same VK can be a fresh press.
                    let sibling_down = self.pressed.values().any(|held| held.vk == vk);
                    if !sibling_down {
                        return true;
                    }
                }
                let mut details = None;
                let key = special_key(vk).or_else(|| {
                    let mut keyboard = [0u8; 256];
                    unsafe {
                        GetKeyboardState(&mut keyboard).ok()?;
                    }
                    let altgr = keyboard[VK_RMENU.0 as usize] & 0x80 != 0
                        && mods.contains(KeyModifiers::CTRL);
                    if !altgr {
                        clear_command_modifiers(&mut keyboard);
                    }
                    let base = (!altgr)
                        .then(|| layout_level(vk, scan, keyboard, false))
                        .flatten();
                    // Shift may change while held. Query its layout level before the
                    // stateful translation consumes any pending dead-key composition.
                    let shifted =
                        base.filter(|&b| physical.is_some_and(|p| p != b))
                            .and_then(|base| {
                                layout_level(vk, scan, keyboard, true).filter(|&s| s != base)
                            });
                    let mut translated = [0u16; 8];
                    // This adapter consumes the keydown, so it owns dead-key translation.
                    let count = unsafe {
                        ToUnicodeEx(
                            vk,
                            scan,
                            &keyboard,
                            &mut translated,
                            0,
                            Some(GetKeyboardLayout(0)),
                        )
                    };
                    let (key, mut translated) =
                        translated_key(count, &translated, base, physical, altgr)?;
                    translated.shifted = shifted;
                    details = Some(translated);
                    Some(key)
                });
                // A keypad function can have a protocol identity even when the
                // active layout produced no text. Clear has no legacy encoding.
                let key = key.or_else(|| keypad.map(|_| KeyCode::Clear));
                if let Some(key) = key {
                    if let Some(keypad) = keypad {
                        details.get_or_insert_with(KeyDetails::default).keypad = Some(keypad);
                    } else if key == KeyCode::Char(' ')
                        && let Some(position) = physical.filter(|&p| p != ' ')
                        && details.is_none()
                    {
                        // Space normally takes the special-key path (including Ctrl+Space).
                        details = Some(KeyDetails {
                            base: Some(' '),
                            physical: Some(position),
                            ..Default::default()
                        });
                    }
                    (self.shortcut)(Shortcut::ClearSelection);
                    emit_key(sender, key, details.as_ref(), mods, true, None);
                    self.pressed.insert(slot, HeldKey { vk, key, details });
                    true
                } else {
                    // Modifiers/dead keys must not trigger WinUI keyboard navigation.
                    matches!(vk, 0x10..=0x14 | 0xa0..=0xa5) || (0x20..=0xfe).contains(&vk)
                }
            }
            WM_KEYUP | WM_SYSKEYUP => {
                if let Some(held) = self.pressed.remove(&slot) {
                    emit_key(sender, held.key, held.details.as_ref(), mods, false, None);
                    return true;
                }
                false
            }
            WM_CHAR => {
                if self.is_read_only() {
                    return true;
                }
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
        if self.modal || self.switcher || self.picker.is_some() || self.local_mode.is_some() {
            return false;
        }
        let Some(target) = &self.target else {
            return false;
        };
        if mods.contains(KeyModifiers::CTRL) && self.preferences.scroll_to_zoom {
            let steps = wheel_steps(&mut self.zoom_remainder, delta);
            if steps != 0 {
                (self.shortcut)(Shortcut::Zoom(steps));
            }
        } else {
            let (target, remainder) = if self.wheel_panes.visible.is_empty() {
                (target, &mut self.wheel_remainder)
            } else if let Some(hit) = self.wheel_panes.target() {
                hit
            } else {
                return false;
            };
            let steps = wheel_steps(remainder, delta);
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
impl From<Action> for Shortcut {
    fn from(action: Action) -> Self {
        match action {
            Action::FocusLeft => Self::FocusPane(Direction::Left),
            Action::FocusRight => Self::FocusPane(Direction::Right),
            Action::FocusUp => Self::FocusPane(Direction::Up),
            Action::FocusDown => Self::FocusPane(Direction::Down),
            Action::ResizeLeft => Self::ResizePane(Direction::Left),
            Action::ResizeRight => Self::ResizePane(Direction::Right),
            Action::ResizeUp => Self::ResizePane(Direction::Up),
            Action::ResizeDown => Self::ResizePane(Direction::Down),
            Action::SwapPaneLeft => Self::SwapPane(Direction::Left),
            Action::SwapPaneRight => Self::SwapPane(Direction::Right),
            Action::SwapPaneUp => Self::SwapPane(Direction::Up),
            Action::SwapPaneDown => Self::SwapPane(Direction::Down),
            Action::SplitRight => Self::SplitRight,
            Action::SplitBelow => Self::SplitBelow,
            Action::SplitRightInCurrentDirectory => Self::SplitRightInCurrentDirectory,
            Action::SplitBelowInCurrentDirectory => Self::SplitBelowInCurrentDirectory,
            Action::ZoomPane => Self::ZoomPane,
            Action::RotateSplit => Self::RotateSplit,
            Action::ClosePane => Self::ClosePane,
            Action::MovePane => Self::MovePane,
            Action::MovePaneToExistingTab => Self::MovePaneToExistingTab,
            Action::Workspaces => Self::Workspaces,
            Action::CommandPalette => Self::CommandPalette,
            Action::PanePicker => Self::PanePicker,
            Action::FindAllPanes => Self::FindAllPanes,
            Action::KeyboardHints => Self::KeyboardHints,
            Action::CopyMode => Self::CopyMode,
            Action::ClearScrollback => Self::ClearScrollback,
            Action::NextTab => Self::NextTab,
            Action::PreviousTab => Self::PreviousTab,
            Action::NewTab => Self::NewTab,
            Action::DuplicateTabLayout => Self::DuplicateTabLayout,
            Action::CloseTab => Self::CloseTab,
            Action::SelectTab(number) => Self::Select(usize::from(number - 1)),
        }
    }
}

#[derive(Clone, Copy)]
enum SwitchTrigger {
    Modifier(KeyModifiers),
    Key(KeySlot),
}
impl SwitchTrigger {
    fn new(slot: KeySlot, mods: KeyModifiers) -> Self {
        [KeyModifiers::CTRL, KeyModifiers::ALT, KeyModifiers::SHIFT]
            .into_iter()
            .find(|flag| mods.contains(*flag))
            .map_or(Self::Key(slot), Self::Modifier)
    }
    fn released(self, slot: KeySlot, mods: KeyModifiers) -> bool {
        match self {
            Self::Key(key) => key == slot,
            Self::Modifier(flag) => {
                !mods.contains(flag)
                    && match flag {
                        KeyModifiers::CTRL => matches!(slot, KeySlot::Virtual(0x11 | 0xa2 | 0xa3)),
                        KeyModifiers::ALT => matches!(slot, KeySlot::Virtual(0x12 | 0xa4 | 0xa5)),
                        _ => matches!(slot, KeySlot::Virtual(0x10 | 0xa0 | 0xa1)),
                    }
            }
        }
    }
}
pub fn cell_pixel_offset(coordinate: f32, cell: f32, scale: f32) -> isize {
    ((coordinate.max(0.0) % cell.max(1.0)) * scale).round() as isize
}
fn wheel_steps(remainder: &mut i32, delta: i16) -> i32 {
    *remainder += delta as i32;
    let steps = *remainder / 120;
    *remainder %= 120;
    steps
}

fn key_identity(vk: u32, data: isize) -> u32 {
    match vk {
        0x10 => {
            if (data >> 16) & 0xff == 0x36 {
                0xa1
            } else {
                0xa0
            }
        }
        0x11 => {
            if data & (1 << 24) != 0 {
                0xa3
            } else {
                0xa2
            }
        }
        0x12 => {
            if data & (1 << 24) != 0 {
                0xa5
            } else {
                0xa4
            }
        }
        _ => vk,
    }
}

/// Set-1 make codes for the printable positions of a standard US PC-101.
/// Extended, keypad, ISO-only and unmapped keys have no printable alternate.
fn pc101_printable(scan: u32, extended: bool) -> Option<char> {
    if extended {
        return None;
    }
    let byte = match scan {
        0x02..=0x0d => b"1234567890-="[(scan - 0x02) as usize],
        0x10..=0x1b => b"qwertyuiop[]"[(scan - 0x10) as usize],
        0x1e..=0x29 => b"asdfghjkl;'`"[(scan - 0x1e) as usize],
        0x2b..=0x35 => b"\\zxcvbnm,./"[(scan - 0x2b) as usize],
        0x39 => b' ',
        _ => return None,
    };
    Some(char::from(byte))
}

fn keypad_key(vk: u32, scan: u32, extended: bool) -> Option<KeypadKey> {
    use KeypadKey::*;
    Some(match vk {
        0x60..=0x69 => [
            Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
        ][(vk - 0x60) as usize],
        0x6a => Multiply,
        0x6b => Add,
        0x6c => Separator,
        // HID Keypad Comma can reach Windows through a layout-specific VK.
        0xbc | 0xc2 if scan == 0x7e => Separator,
        0x6d => Subtract,
        0x6e => Decimal,
        0x6f => Divide,
        0x0d if extended => Enter,
        // 0x92 is shared with the Japanese dictionary key. 0x59 is keypad '='.
        0x92 | 0xbb if scan == 0x59 => Equal,
        // The dedicated navigation cluster uses the same scans with E0 set.
        // No scan in an injected VK event gives us no keypad evidence.
        _ if !extended => match (vk, scan) {
            (0x24, 0x47) => Home,
            (0x26, 0x48) => Up,
            (0x21, 0x49) => PageUp,
            (0x25, 0x4b) => Left,
            (0x0c, 0x4c) => Begin,
            (0x27, 0x4d) => Right,
            (0x23, 0x4f) => End,
            (0x28, 0x50) => Down,
            (0x22, 0x51) => PageDown,
            (0x2d, 0x52) => Insert,
            (0x2e, 0x53) => Delete,
            _ => return None,
        },
        _ => return None,
    })
}

fn special_key(vk: u32) -> Option<KeyCode> {
    Some(match vk {
        0x08 => KeyCode::Char('\u{8}'),
        0x09 => KeyCode::Char('\t'),
        0x0d => KeyCode::Char('\r'),
        0x1b => KeyCode::Char('\u{1b}'),
        0x10 | 0xa0 => KeyCode::LeftShift,
        0xa1 => KeyCode::RightShift,
        0x11 | 0xa2 => KeyCode::LeftControl,
        0xa3 => KeyCode::RightControl,
        0x12 | 0xa4 => KeyCode::LeftAlt,
        0xa5 => KeyCode::RightAlt,
        0x5b => KeyCode::LeftWindows,
        0x5c => KeyCode::RightWindows,
        0x14 => KeyCode::CapsLock,
        0x90 => KeyCode::NumLock,
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
        0x2e => KeyCode::Delete,
        0x70..=0x87 => KeyCode::Function((vk - 0x6f) as u8),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use w11term::{options::Source, session::Session};

    #[test]
    fn physical_printable_positions_use_the_pc101_make_code() {
        let mut keys = (0..=255)
            .filter_map(|scan| pc101_printable(scan, false))
            .collect::<Vec<_>>();
        assert_eq!(keys.len(), 47 + 1); // number row, QWERTY rows, and Space
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), 48);
        for (scan, expected) in [
            (0x02, '1'),
            (0x0d, '='),
            (0x10, 'q'),
            (0x15, 'y'),
            (0x1b, ']'),
            (0x1e, 'a'),
            (0x28, '\''),
            (0x2b, '\\'),
            (0x2e, 'c'),
            (0x35, '/'),
            (0x39, ' '),
        ] {
            assert_eq!(pc101_printable(scan, false), Some(expected));
            assert_eq!(pc101_printable(scan, true), None);
        }
        for scan in [0, 0x0e, 0x1c, 0x36, 0x37, 0x4b, 0x56, 0x7e] {
            assert_eq!(pc101_printable(scan, false), None);
        }
    }

    #[test]
    fn physical_identity_preserves_altgr_and_the_dead_key_translation_result() {
        let position = Some('q');
        assert!(translated_key(-1, &[0xb4], None, position, false).is_none());
        assert!(translated_key(0, &[0xb4], None, position, false).is_none());
        let (key, details) = translated_key(1, &[64], None, position, true).unwrap();
        assert_eq!(key, KeyCode::Char('@'));
        assert!(details.altgr);
        assert_eq!(details.physical, position);
        let (_, details) = translated_key(2, &[0xb4, 113], None, position, false).unwrap();
        assert_eq!(details.text.as_deref(), Some("´q"));
        let (_, details) = translated_key(1, &[90], Some('z'), Some('z'), false).unwrap();
        assert!(
            details.physical.is_none(),
            "Shift does not change the PC-101 identity"
        );
    }

    #[test]
    fn tab_switcher_commits_when_the_remapped_trigger_is_released() {
        let trigger = SwitchTrigger::new(
            KeySlot::Virtual(0x75),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        );
        assert!(!trigger.released(KeySlot::Virtual(0xa0), KeyModifiers::ALT));
        assert!(!trigger.released(KeySlot::Virtual(0xa4), KeyModifiers::ALT));
        assert!(trigger.released(KeySlot::Virtual(0xa4), KeyModifiers::NONE));
        let single = SwitchTrigger::new(KeySlot::Virtual(0x77), KeyModifiers::NONE);
        assert!(!single.released(KeySlot::Virtual(0xa2), KeyModifiers::NONE));
        assert!(single.released(KeySlot::Virtual(0x77), KeyModifiers::NONE));
    }
    #[test]
    fn simultaneous_left_and_right_modifiers_have_distinct_press_identities() {
        assert_ne!(
            key_identity(0x10, 0x2a << 16),
            key_identity(0x10, 0x36 << 16)
        );
        assert_ne!(key_identity(0x11, 0), key_identity(0x11, 1 << 24));
        assert_ne!(key_identity(0x12, 0), key_identity(0x12, 1 << 24));
        assert_eq!(
            special_key(key_identity(0x10, 0x36 << 16)),
            Some(KeyCode::RightShift)
        );
    }
    #[test]
    fn windows_keypad_scans_distinguish_both_clusters_and_all_operators() {
        use KeypadKey::*;
        for (vk, scan, key) in [
            (0x24, 0x47, Home),
            (0x26, 0x48, Up),
            (0x21, 0x49, PageUp),
            (0x25, 0x4b, Left),
            (0x0c, 0x4c, Begin),
            (0x27, 0x4d, Right),
            (0x23, 0x4f, End),
            (0x28, 0x50, Down),
            (0x22, 0x51, PageDown),
            (0x2d, 0x52, Insert),
            (0x2e, 0x53, Delete),
        ] {
            assert_eq!(keypad_key(vk, scan, false), Some(key));
            assert_eq!(keypad_key(vk, scan, true), None);
            assert_eq!(keypad_key(vk, 0, false), None);
        }
        for (index, key) in [
            Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(keypad_key(0x60 + index as u32, 0, false), Some(key));
        }
        for (vk, scan, extended, key) in [
            (0x0d, 0x1c, true, Enter),
            (0x6a, 0x37, false, Multiply),
            (0x6b, 0x4e, false, Add),
            (0x6c, 0x7e, false, Separator),
            (0xc2, 0x7e, false, Separator),
            (0x6d, 0x4a, false, Subtract),
            (0x6e, 0x53, false, Decimal),
            (0x6f, 0x35, true, Divide),
            (0x92, 0x59, false, Equal),
        ] {
            assert_eq!(keypad_key(vk, scan, extended), Some(key));
        }
        assert_eq!(keypad_key(0x0d, 0x1c, false), None);
        assert_eq!(keypad_key(0xbb, 0x0d, false), None); // main '='
        assert_eq!(keypad_key(0x92, 0, false), None); // Fujitsu dictionary
    }

    #[test]
    fn simultaneous_enters_keep_distinct_presses_repeats_and_releases() {
        let lab = Session::start(Source::Demo);
        let (sender, mut pending) = mpsc::unbounded_channel();
        let mut state = State {
            target: Some(Target {
                sender,
                frames: lab.frames.clone(),
            }),
            ..State::new(|_| {})
        };
        for (msg, keypad, previous, down, repeat) in [
            (WM_KEYDOWN, false, false, true, false),
            (WM_KEYDOWN, true, true, true, false),
            (WM_KEYDOWN, true, true, true, true),
            (WM_KEYUP, false, true, false, false),
            (WM_KEYUP, true, true, false, false),
        ] {
            assert!(state.handle(&MSG {
                message: msg,
                wParam: WPARAM(0x0d),
                lParam: LPARAM(
                    (0x1c << 16) | ((keypad as isize) << 24) | ((previous as isize) << 30)
                ),
                ..Default::default()
            }));
            let actual = match pending.try_recv().unwrap() {
                Command::DetailedKey {
                    key: KeyCode::Char('\r'),
                    details,
                    down,
                    repeat,
                    ..
                } => (details.keypad, down, repeat),
                Command::Key {
                    key: KeyCode::Char('\r'),
                    down,
                    repeat,
                    ..
                } => (None, down, repeat),
                _ => panic!("Expected Enter"),
            };
            assert_eq!(actual, (keypad.then_some(KeypadKey::Enter), down, repeat));
        }
        assert!(state.pressed.is_empty());
        assert!(pending.try_recv().is_err());
    }
    #[test]
    fn keypad_game_mode_releases_press_identity_when_num_lock_changes() {
        let lab = Session::start(Source::Demo);
        let (sender, mut pending) = mpsc::unbounded_channel();
        let mut state = State {
            game_mode: true,
            target: Some(Target {
                sender,
                frames: lab.frames.clone(),
            }),
            ..State::new(|_| {})
        };
        for (msg, vk, previous) in [
            (WM_KEYDOWN, VK_LEFT, false),
            (WM_KEYDOWN, VK_LEFT, true),
            (WM_KEYUP, VK_NUMPAD4, true),
        ] {
            assert!(state.handle(&MSG {
                message: msg,
                wParam: WPARAM(vk.0.into()),
                lParam: LPARAM((0x4b << 16) | ((previous as isize) << 30)),
                ..Default::default()
            }));
        }
        for expected in [true, false] {
            assert!(matches!(pending.try_recv(), Ok(Command::DetailedKey {
                key: KeyCode::LeftArrow, details: KeyDetails {keypad: Some(KeypadKey::Left), ..},
                down, repeat: false, ..
            }) if down == expected));
        }
        assert!(pending.try_recv().is_err());
        assert!(state.pressed.is_empty());
    }

    #[test]
    fn sgr_pixel_offsets_use_physical_pixels_at_non_default_dpi() {
        assert_eq!(cell_pixel_offset(13.0, 10.0, 1.0), 3);
        assert_eq!(cell_pixel_offset(13.0, 10.0, 1.5), 5);
        assert_eq!(cell_pixel_offset(13.0, 10.0, 2.0), 6);
        assert_eq!(cell_pixel_offset(-1.0, 10.0, 2.0), 0);
    }
    #[test]
    fn win32_delete_is_a_functional_key() {
        assert_eq!(special_key(0x2e), Some(KeyCode::Delete));
        assert_eq!(special_key(0x08), Some(KeyCode::Char('\x08')));
    }
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
    fn split_wheel_tracks_fractional_scroll_independently_for_each_visible_pane() {
        let lab = Session::start(Source::Demo);
        let (left, _left_input) = mpsc::unbounded_channel();
        let (right, _right_input) = mpsc::unbounded_channel();
        let layout = || {
            [
                (5, left.clone(), lab.frames.clone()),
                (6, right.clone(), lab.frames.clone()),
            ]
        };
        let mut panes = WheelPanes::default();
        panes.replace(layout());
        assert!(panes.target().is_none());
        panes.hovered = Some(6);
        let (target, remainder) = panes.target().unwrap();
        assert!(target.sender.same_channel(&right));
        assert_eq!(wheel_steps(remainder, 60), 0);
        panes.hovered = Some(5);
        let (target, remainder) = panes.target().unwrap();
        assert!(target.sender.same_channel(&left));
        assert_eq!(wheel_steps(remainder, 60), 0);
        panes.replace(layout());
        assert_eq!(wheel_steps(panes.target().unwrap().1, 60), 1);
        panes.hovered = Some(6);
        assert_eq!(wheel_steps(panes.target().unwrap().1, 60), 1);

        // Swapping exchanges the panes under a stationary pointer. Fractional
        // deltas stay with their sessions; swapping elsewhere leaves hover alone.
        panes.swapped([5, 6]);
        assert!(panes.target().unwrap().0.sender.same_channel(&left));
        panes.swapped([6, 7]);
        assert!(panes.target().unwrap().0.sender.same_channel(&left));
        panes.swapped([6, 5]);
        assert!(panes.target().unwrap().0.sender.same_channel(&right));

        panes.replace([(5, left.clone(), lab.frames.clone())]);
        assert_eq!(panes.hovered, None);
        assert!(panes.target().unwrap().0.sender.same_channel(&left));
        panes.replace([]);
        assert!(panes.target().is_none());
    }
    #[test]
    fn grid_snap_rounds_to_cells_with_fixed_window_chrome() {
        assert_eq!(snap_extent(1238, 24, 10), 1234);
        assert_eq!(snap_extent(980, 98, 20), 978);
        assert_eq!(snap_extent(0, 24, 10), 34);
    }

    #[test]
    fn remapped_palette_owns_navigation_before_the_overlay_mounts_and_after_accept() {
        let lab = Session::start(Source::Demo);
        let (sender, mut input) = mpsc::unbounded_channel();
        let actions = std::rc::Rc::new(RefCell::new(Vec::new()));
        let observed = actions.clone();
        let mut bindings = Shortcuts::default();
        bindings
            .set(Action::CommandPalette, Some("F7".parse().unwrap()))
            .unwrap();
        let mut state = State {
            target: Some(Target {
                sender,
                frames: lab.frames.clone(),
            }),
            keymap: bindings.keymap().unwrap(),
            ..State::new(move |action| observed.borrow_mut().push(action))
        };
        let key = |message, vk| MSG {
            message,
            wParam: WPARAM(vk),
            ..Default::default()
        };
        for vk in [0x76, 0x09, 0x28, 0x26] {
            assert!(state.handle(&key(WM_KEYDOWN, vk)));
            assert!(state.handle(&key(WM_KEYUP, vk)));
        }
        assert!(!state.handle(&key(WM_CHAR, 'k' as usize)));
        assert!(state.handle(&key(WM_KEYDOWN, 0x0d)));
        state.picker = None;
        assert!(state.handle(&key(WM_CHAR, 0x0d)));
        assert!(state.handle(&key(WM_KEYUP, 0x0d)));
        assert_eq!(
            *actions.borrow(),
            [
                Shortcut::CommandPalette,
                Shortcut::PickerMove(1),
                Shortcut::PickerMove(1),
                Shortcut::PickerMove(-1),
                Shortcut::PickerAccept
            ]
        );
        assert!(input.try_recv().is_err());
    }

    #[test]
    fn moving_a_pane_routes_only_fresh_picker_actions_and_keeps_button_keys_native() {
        let lab = Session::start(Source::Demo);
        let (sender, mut input) = mpsc::unbounded_channel();
        let actions = std::rc::Rc::new(RefCell::new(Vec::new()));
        let observed = actions.clone();
        let mut bindings = Shortcuts::default();
        bindings
            .set(Action::MovePaneToExistingTab, Some("F8".parse().unwrap()))
            .unwrap();
        let mut state = State {
            keymap: bindings.keymap().unwrap(),
            target: Some(Target {
                sender,
                frames: lab.frames.clone(),
            }),
            ..State::new(move |action| observed.borrow_mut().push(action))
        };
        let key_event = |message, vk: u32, repeat| MSG {
            message,
            wParam: WPARAM(vk as usize),
            lParam: LPARAM(if repeat { 1 << 30 } else { 0 }),
            ..Default::default()
        };
        // The UI has not processed the queued open message yet.
        assert!(state.handle(&key_event(WM_KEYDOWN, VK_F8.0.into(), false)));
        assert!(!state.handle(&key_event(WM_CHAR, u32::from('q'), false)));
        assert!(state.handle(&key_event(WM_KEYUP, VK_F8.0.into(), false)));
        assert_eq!(
            actions.borrow().as_slice(),
            &[Shortcut::MovePaneToExistingTab]
        );
        actions.borrow_mut().clear();
        let enter = |message, repeat| MSG {
            message,
            wParam: WPARAM(VK_RETURN.0 as usize),
            lParam: LPARAM(if repeat { 1 << 30 } else { 0 }),
            ..Default::default()
        };
        assert!(state.handle(&enter(WM_KEYDOWN, false)));
        state.picker = Some(PickerStage::Placement);
        assert!(state.handle(&enter(WM_KEYDOWN, true)));
        assert!(state.handle(&enter(WM_KEYUP, false)));
        assert!(!state.handle(&enter(WM_KEYDOWN, false)));
        assert!(!state.handle(&enter(WM_KEYUP, false)));
        assert!(state.handle(&key_event(WM_KEYDOWN, 0x20, true)));
        assert!(state.handle(&key_event(WM_KEYUP, 0x20, false)));
        assert!(!state.handle(&key_event(WM_KEYDOWN, 0x20, false)));
        assert!(state.handle(&key_event(WM_KEYDOWN, 0x20, true)));
        assert!(!state.handle(&key_event(WM_KEYUP, 0x20, false)));
        assert_eq!(actions.borrow().as_slice(), &[Shortcut::PickerAccept]);
        for (vk, direction) in [
            (0x25, Direction::Left),
            (0x26, Direction::Up),
            (0x27, Direction::Right),
            (0x28, Direction::Down),
        ] {
            state.picker = Some(PickerStage::Search);
            if matches!(vk, 0x25 | 0x27) {
                assert!(
                    !state.handle(&key_event(WM_KEYDOWN, vk, false)),
                    "search caret"
                );
            }
            state.picker = Some(PickerStage::Placement);
            actions.borrow_mut().clear();
            assert!(state.handle(&key_event(WM_KEYDOWN, vk, true)));
            assert!(state.handle(&key_event(WM_KEYUP, vk, false)));
            assert!(
                actions.borrow().is_empty(),
                "a held arrow committed a placement"
            );
            assert!(state.handle(&key_event(WM_KEYDOWN, vk, false)));
            state.picker = None;
            assert!(state.handle(&key_event(WM_KEYDOWN, vk, true)));
            assert!(state.handle(&key_event(WM_KEYUP, vk, false)));
            assert_eq!(
                actions.borrow().as_slice(),
                &[Shortcut::PickerPlace(direction)]
            );
        }
        state.picker = Some(PickerStage::Placement);
        assert!(state.handle(&key_event(WM_KEYDOWN, 0x1b, false)));
        state.picker = None;
        assert!(state.handle(&key_event(WM_KEYDOWN, 0x1b, true)));
        assert!(state.handle(&key_event(WM_KEYUP, 0x1b, false)));
        assert_eq!(actions.borrow().last(), Some(&Shortcut::PickerCancel));
        state.picker = Some(PickerStage::Placement);
        assert!(!state.handle(&enter(WM_KEYDOWN, false)));
        state.picker = None;
        assert!(state.handle(&enter(WM_KEYDOWN, true)));
        assert!(state.handle(&key_event(WM_CHAR, 0x0d, false)));
        assert!(state.handle(&enter(WM_KEYUP, false)));
        assert!(
            input.try_recv().is_err(),
            "picker keys reached the terminal"
        );
    }

    #[test]
    fn locking_a_pane_releases_the_held_key_before_lock_and_swallows_its_remaining_repeats() {
        let lab = Session::start(Source::Demo);
        let (sender, mut pending) = mpsc::unbounded_channel();
        let locked = std::rc::Rc::new(std::cell::Cell::new(false));
        with_input_mut(|slot| {
            *slot = Some(State {
                target: Some(Target {
                    sender: sender.clone(),
                    frames: lab.frames.clone(),
                }),
                read_only: Some(locked.clone()),
                pressed: HashMap::from([(
                    KeySlot::Virtual(0x58),
                    HeldKey {
                        vk: 0x58,
                        key: KeyCode::Char('x'),
                        details: Some(KeyDetails {
                            base: Some('x'),
                            ..KeyDetails::default()
                        }),
                    },
                )]),
                surrogate: Some(0xd800),
                ..State::new(|_| {})
            })
        });
        KeyboardBridge::set_read_only(&sender, &locked, true);
        assert!(matches!(
            pending.try_recv().unwrap(),
            Command::DetailedKey {
                key: KeyCode::Char('x'),
                down: false,
                repeat: false,
                ..
            }
        ));
        assert!(matches!(
            pending.try_recv().unwrap(),
            Command::SetReadOnly(true)
        ));
        KeyboardBridge::set_read_only(&sender, &locked, false);
        assert!(matches!(
            pending.try_recv().unwrap(),
            Command::SetReadOnly(false)
        ));
        with_input_mut(|slot| {
            let mut state = slot.take().unwrap();
            assert!(state.surrogate.is_none());
            assert!(state.handle(&MSG {
                message: WM_KEYDOWN,
                wParam: WPARAM(0x58),
                lParam: LPARAM(1 << 30),
                ..Default::default()
            }));
            assert!(state.handle(&MSG {
                message: WM_KEYUP,
                wParam: WPARAM(0x58),
                ..Default::default()
            }));
            assert!(pending.try_recv().is_err());
            assert!(state.handle(&MSG {
                message: WM_CHAR,
                wParam: WPARAM('x' as usize),
                ..Default::default()
            }));
            assert!(matches!(pending.try_recv().unwrap(), Command::Text(s) if s == "x"));
        });
    }

    #[test]
    fn switching_tabs_releases_held_keys_to_the_old_session_and_forms_receive_native_input() {
        let _guard = WindowScope::default().enter();
        window::activated(true);
        let lab = Session::start(Source::Demo);
        let (old, mut old_input) = mpsc::unbounded_channel();
        let (new, mut new_input) = mpsc::unbounded_channel();
        let mut state = State {
            target: Some(Target {
                sender: old,
                frames: lab.frames.clone(),
            }),
            pressed: HashMap::from([(
                KeySlot::Virtual(0x58),
                HeldKey {
                    vk: 0x58,
                    key: KeyCode::Char('x'),
                    details: None,
                },
            )]),
            surrogate: Some(0xd800),
            ..State::new(|_| {})
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
        state.picker = Some(PickerStage::Search);
        let actions = std::rc::Rc::new(RefCell::new(Vec::new()));
        let observed = actions.clone();
        state.shortcut = Box::new(move |action| observed.borrow_mut().push(action));
        let down = |message, repeat| MSG {
            message,
            wParam: WPARAM(VK_DOWN.0 as usize),
            lParam: LPARAM(if repeat { 1 << 30 } else { 0 }),
            ..Default::default()
        };
        // Opening the picker calls release_keys while Alt+Shift+Down is held.
        state.held_resize = Some(HeldResize {
            key: KeySlot::Virtual(VK_DOWN.0.into()),
            repeat: Some((KeyModifiers::ALT | KeyModifiers::SHIFT, Direction::Down)),
        });
        state.release_keys();
        assert!(state.handle(&down(WM_KEYDOWN, true)));
        assert!(state.handle(&down(WM_KEYUP, false)));
        assert!(
            actions.borrow().is_empty(),
            "resize repeat moved the picker"
        );
        assert!(state.handle(&down(WM_KEYDOWN, false)));
        assert!(state.handle(&down(WM_KEYUP, false)));
        assert_eq!(actions.borrow().as_slice(), &[Shortcut::PickerMove(1)]);
        assert!(!state.handle(&MSG {
            message: WM_CHAR,
            wParam: WPARAM('q' as usize),
            ..Default::default()
        }));
        let enter = |message, repeat| MSG {
            message,
            wParam: WPARAM(VK_RETURN.0 as usize),
            lParam: LPARAM(if repeat { 1 << 30 } else { 0 }),
            ..Default::default()
        };
        assert!(state.handle(&enter(WM_KEYDOWN, false)));
        state.picker = None;
        assert!(state.handle(&enter(WM_KEYDOWN, true)));
        assert!(state.handle(&enter(WM_KEYUP, false)));
        assert!(
            new_input.try_recv().is_err(),
            "picker input reached the shell"
        );
        state.local_mode = Some(LocalMode::Hints);
        let hint = |msg, repeat| MSG {
            message: msg,
            wParam: WPARAM(b'C' as usize),
            lParam: LPARAM(if repeat { 1 << 30 } else { 0 }),
            ..Default::default()
        };
        assert!(state.handle(&hint(WM_KEYDOWN, false)));
        assert!(state.handle(&hint(WM_CHAR, false)));
        state.local_mode = None;
        assert!(state.handle(&hint(WM_KEYDOWN, true)));
        assert!(state.handle(&hint(WM_KEYUP, false)));
        assert!(
            new_input.try_recv().is_err(),
            "a hint key reached the terminal after dismissal"
        );
        state.local_mode = Some(LocalMode::Copy);
        let moves = std::rc::Rc::new(RefCell::new(Vec::new()));
        let got = moves.clone();
        state.shortcut = Box::new(move |event| got.borrow_mut().push(event));
        let key = |msg, vk, repeat| MSG {
            message: msg,
            wParam: WPARAM(vk),
            lParam: LPARAM(if repeat { 1 << 30 } else { 0 }),
            ..Default::default()
        };
        // Held in the shell when the user entered copy mode. Neither a known
        // inherited key nor an orphan repeat can start a movement here.
        state.local_held.insert(KeySlot::Virtual(0x26), false);
        for event in [
            key(WM_KEYDOWN, 0x26, true),
            key(WM_KEYUP, 0x26, false),
            key(WM_KEYDOWN, 0x26, true),
            key(WM_KEYDOWN, 0x26, false),
            key(WM_KEYDOWN, 0x26, true),
            key(WM_KEYUP, 0x26, false),
            key(WM_CHAR, 0x71, false),
            key(WM_KEYDOWN, 0x1b, false),
        ] {
            assert!(state.handle(&event));
        }
        // Escape and a new shortcut may already be in the Windows message queue
        // before the component has processed the copy-mode dismissal.
        let mut shortcuts = Shortcuts::default();
        shortcuts
            .set(Action::CommandPalette, Some("F7".parse().unwrap()))
            .unwrap();
        state.keymap = shortcuts.keymap().unwrap();
        assert!(state.handle(&key(WM_KEYDOWN, 0x1b, true)));
        assert!(state.handle(&key(WM_CHAR, 0x1b, false)));
        assert!(state.handle(&key(WM_KEYUP, 0x1b, false)));
        assert!(state.handle(&key(WM_KEYDOWN, 0x76, false)));
        assert!(state.handle(&key(WM_KEYUP, 0x76, false)));
        use w11term::copy_mode::CopyKey;
        assert_eq!(
            *moves.borrow(),
            [
                Shortcut::CopyKey(CopyKey::Up),
                Shortcut::CopyKey(CopyKey::Up),
                Shortcut::CopyKey(CopyKey::Cancel),
                Shortcut::CommandPalette,
            ]
        );
        assert!(
            new_input.try_recv().is_err(),
            "copy mode or its closing key release reached the shell"
        );
        state.picker = None;
        state.game_mode = true;
        let key_message = |message, repeat| MSG {
            message,
            wParam: WPARAM(0x2e),
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
                key: KeyCode::Delete,
                down: true,
                repeat: false,
                ..
            }
        ));
        assert!(matches!(
            new_input.try_recv().unwrap(),
            Command::Key {
                key: KeyCode::Delete,
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
