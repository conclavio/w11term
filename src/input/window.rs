//! Component callbacks explicitly enter their owning window. Native callbacks
//! resolve the actual message HWND, including during re-entrant Win32 calls.
use super::*;
use std::{
    cell::Cell,
    rc::{Rc, Weak},
};

type Activation = Box<dyn Fn(bool)>;

#[derive(Default)]
struct WindowData {
    input: RefCell<Option<State>>,
    ime: ime::WindowIme,
    hwnd: Cell<Option<HWND>>,
    active: Cell<bool>,
    visible: Cell<bool>,
    on_activation: RefCell<Option<Activation>>,
}

#[derive(Clone, Default)]
pub(crate) struct WindowScope(Rc<WindowData>);

thread_local! {
    // The empty scope has no registered window, input target or IME owner.
    static CURRENT: RefCell<WindowScope> = RefCell::default();
    static ROOTS: RefCell<HashMap<usize, Weak<WindowData>>> = RefCell::default();
    static HOOKS: RefCell<Weak<ThreadHooks>> = RefCell::default();
}

#[must_use]
pub(crate) struct WindowGuard(WindowScope);

impl Drop for WindowGuard {
    fn drop(&mut self) {
        CURRENT.with_borrow_mut(|slot| std::mem::swap(slot, &mut self.0));
    }
}

impl WindowScope {
    pub(crate) fn current() -> Self {
        CURRENT.with_borrow(Clone::clone)
    }

    pub(crate) fn hwnd(&self) -> Option<HWND> {
        self.0.hwnd.get()
    }

    pub(super) fn refresh_visibility(&self, hwnd: HWND) -> bool {
        let visible = unsafe { IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() };
        self.0.visible.replace(visible) != visible
    }

    pub(crate) fn enter(&self) -> WindowGuard {
        WindowGuard(CURRENT.with(|slot| slot.replace(self.clone())))
    }

    pub(super) fn for_window(hwnd: HWND) -> Option<Self> {
        if hwnd.is_invalid() {
            return None;
        }
        let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
        ROOTS.with_borrow(|roots| roots.get(&(root.0 as usize))?.upgrade().map(Self))
    }

    pub(super) fn remove(&self) {
        if let Some(hwnd) = self.0.hwnd.take() {
            let removed = ROOTS.with_borrow_mut(|roots| {
                let key = hwnd.0 as usize;
                if roots
                    .get(&key)
                    .is_some_and(|owner| owner.ptr_eq(&Rc::downgrade(&self.0)))
                {
                    roots.remove(&key);
                    true
                } else {
                    false
                }
            });
            if removed {
                unsafe {
                    let _ = RemoveWindowSubclass(hwnd, Some(window_proc), 1);
                }
            }
        }
    }
}

pub(super) fn with_input<R>(f: impl FnOnce(&RefCell<Option<State>>) -> R) -> R {
    let scope = CURRENT.with_borrow(Clone::clone);
    f(&scope.0.input)
}

pub(super) fn with_input_mut<R>(f: impl FnOnce(&mut Option<State>) -> R) -> R {
    with_input(|input| f(&mut input.borrow_mut()))
}

pub(super) fn with_ime<R>(f: impl FnOnce(&ime::WindowIme) -> R) -> R {
    let scope = CURRENT.with_borrow(Clone::clone);
    f(&scope.0.ime)
}

pub(crate) fn window() -> Option<HWND> {
    CURRENT.with_borrow(|scope| scope.0.hwnd.get())
}

pub(super) fn active() -> bool {
    CURRENT.with_borrow(|scope| scope.0.active.get())
}

pub(super) fn visible() -> bool {
    CURRENT.with_borrow(|scope| scope.0.visible.get())
}

pub(super) fn activated(focused: bool) {
    let scope = CURRENT.with_borrow(Clone::clone);
    if scope.0.active.replace(focused) == focused {
        return;
    }
    if !focused {
        ime::cancel();
    }
    with_input(|input| {
        if let Ok(mut input) = input.try_borrow_mut()
            && let Some(state) = input.as_mut()
        {
            if !focused {
                state.release_keys();
                state.local_held.clear();
                state.swallowed.clear();
                state.held_resize = None;
                if state.switcher {
                    state.switcher = false;
                    (state.shortcut)(Shortcut::CancelTab);
                }
            }
            state.report_focus();
        }
    });
    if let Some(callback) = scope.0.on_activation.borrow().as_ref() {
        callback(focused);
    }
}

impl KeyboardBridge {
    pub(crate) fn scope(&self) -> WindowScope {
        self.scope.clone()
    }

    pub(crate) fn bootstrap_title(&self) -> &str {
        &self.bootstrap
    }

    pub(crate) fn hwnd(&self) -> Option<HWND> {
        self.scope.hwnd()
    }

    pub(crate) fn on_activation(&self, callback: impl Fn(bool) + 'static) {
        *self.scope.0.on_activation.borrow_mut() = Some(Box::new(callback));
    }

    /// A published WinUI window has the exact initial caption supplied by its
    /// component. Only the initial bind enumerates, and a duplicate match fails
    /// closed. Routine input routing never consults a title or the foreground.
    pub(crate) fn try_bind(&self) -> Option<HWND> {
        if let Some(hwnd) = self.hwnd() {
            return Some(hwnd);
        }
        struct Search<'a> {
            title: &'a str,
            found: Option<HWND>,
            ambiguous: bool,
        }
        unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
            let search = unsafe { &mut *(lparam.0 as *mut Search<'_>) };
            let mut pid = 0;
            let thread = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            if pid == std::process::id()
                && thread == unsafe { GetCurrentThreadId() }
                && unsafe { GetAncestor(hwnd, GA_ROOT) } == hwnd
                && unsafe { GetWindow(hwnd, GW_OWNER) }.is_err()
            {
                let mut caption = [0; 96];
                let len = unsafe { GetWindowTextW(hwnd, &mut caption) } as usize;
                if caption[..len]
                    .iter()
                    .copied()
                    .eq(search.title.encode_utf16())
                    && search.found.replace(hwnd).is_some()
                {
                    search.ambiguous = true;
                }
            }
            true.into()
        }
        let mut search = Search {
            title: &self.bootstrap,
            found: None,
            ambiguous: false,
        };
        unsafe {
            let _ = EnumThreadWindows(
                GetCurrentThreadId(),
                Some(visit),
                LPARAM((&mut search as *mut Search<'_>) as isize),
            );
        }
        let hwnd = search.found?;
        if search.ambiguous || WindowScope::for_window(hwnd).is_some() {
            return None;
        }
        if !unsafe { SetWindowSubclass(hwnd, Some(window_proc), 1, 0) }.as_bool() {
            return None;
        }
        self.scope.0.hwnd.set(Some(hwnd));
        self.scope.refresh_visibility(hwnd);
        ROOTS.with_borrow_mut(|roots| {
            roots.insert(hwnd.0 as usize, Rc::downgrade(&self.scope.0));
        });
        let _guard = self.scope.enter();
        activated(unsafe { GetActiveWindow() } == hwnd);
        ime::sync_context();
        Some(hwnd)
    }
}

pub(super) fn install_hooks() -> windows::core::Result<Rc<ThreadHooks>> {
    HOOKS.with_borrow_mut(|hooks| {
        if let Some(shared) = hooks.upgrade() {
            return Ok(shared);
        }
        let shared = Rc::new(ThreadHooks::install()?);
        *hooks = Rc::downgrade(&shared);
        Ok(shared)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use w11term::{options::Source, session::Session};
    use windows::core::{HSTRING, w};

    struct HiddenWindow(HWND);

    impl HiddenWindow {
        fn new(title: &str, parent: Option<HWND>) -> Self {
            Self(unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    &HSTRING::from(title),
                    if parent.is_some() {
                        WS_CHILD
                    } else {
                        WS_OVERLAPPEDWINDOW
                    },
                    0,
                    0,
                    100,
                    100,
                    parent,
                    None,
                    None,
                    None,
                )
                .unwrap()
            })
        }
    }
    impl Drop for HiddenWindow {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }

    fn dispatch_character(hwnd: HWND, ch: char) -> u32 {
        unsafe {
            PostMessageW(Some(hwnd), WM_CHAR, WPARAM(ch as usize), LPARAM(0)).unwrap();
            let mut msg = MSG::default();
            assert!(PeekMessageW(&mut msg, Some(hwnd), WM_CHAR, WM_CHAR, PM_REMOVE).as_bool());
            msg.message
        }
    }

    #[test]
    fn hidden_minimized_and_restored_windows_update_cursor_eligibility_without_activation() {
        let changes = Rc::new(Cell::new(0));
        let observed = changes.clone();
        let bridge = KeyboardBridge::install(move |event| {
            if event == Shortcut::CaretPresentationChanged {
                observed.set(observed.get() + 1);
            }
        })
        .unwrap();
        let window = HiddenWindow(unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                w!("STATIC"),
                &HSTRING::from(bridge.bootstrap_title()),
                WS_OVERLAPPEDWINDOW,
                GetSystemMetrics(SM_XVIRTUALSCREEN) - 1024,
                GetSystemMetrics(SM_YVIRTUALSCREEN) - 1024,
                100,
                100,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        });
        assert_eq!(bridge.try_bind(), Some(window.0));
        let scope = bridge.scope();
        let _scope = scope.enter();
        assert!(!visible());
        unsafe {
            let _ = ShowWindow(window.0, SW_SHOWNOACTIVATE);
        }
        assert!(visible());
        let shown = changes.get();
        unsafe {
            let _ = ShowWindow(window.0, SW_SHOWMINNOACTIVE);
        }
        assert!(unsafe { IsIconic(window.0) }.as_bool());
        assert!(!visible());
        assert!(changes.get() > shown);
        unsafe {
            let _ = ShowWindow(window.0, SW_SHOWNOACTIVATE);
        }
        assert!(visible());
        unsafe {
            let _ = ShowWindow(window.0, SW_HIDE);
        }
        assert!(!visible());
    }

    #[test]
    fn registered_roots_route_children_and_survive_either_window_closing() {
        let a = KeyboardBridge::install(|_| {}).unwrap();
        let b = KeyboardBridge::install(|_| {}).unwrap();
        assert!(Rc::ptr_eq(&a._hooks, &b._hooks));
        let wa = HiddenWindow::new(a.bootstrap_title(), None);
        let wb = HiddenWindow::new(b.bootstrap_title(), None);
        let child_b = HiddenWindow::new("child of B", Some(wb.0));
        let duplicate = HiddenWindow::new(a.bootstrap_title(), None);
        assert!(a.try_bind().is_none());
        drop(duplicate);
        assert_eq!(b.try_bind(), Some(wb.0));
        assert_eq!(a.try_bind(), Some(wa.0));
        let decoy = HiddenWindow::new("w11term desktop", None);
        unsafe {
            SetWindowTextW(wb.0, w!("normal caption with no bootstrap token")).unwrap();
        }

        let lab = Session::start(Source::Demo);
        let (tx_a, mut rx_a) = mpsc::unbounded_channel();
        let (tx_b, mut rx_b) = mpsc::unbounded_channel();
        let target = |tx| Some((tx, lab.frames.clone(), Rc::new(Cell::new(false))));
        a.set_target(target(tx_a));
        b.set_target(target(tx_b));
        assert!(matches!(rx_a.try_recv(), Ok(Command::Focus(false))));
        assert!(matches!(rx_b.try_recv(), Ok(Command::Focus(false))));

        // Dispatch B's child message inside an A component callback. The native
        // hook must select B temporarily and restore A after handling it.
        let guard = a.scope().enter();
        assert_eq!(dispatch_character(child_b.0, 'b'), WM_NULL);
        assert_eq!(window(), Some(wa.0));
        assert!(matches!(rx_b.try_recv(), Ok(Command::Text(s)) if s == "b"));
        assert!(rx_a.try_recv().is_err());
        assert_eq!(dispatch_character(decoy.0, 'x'), WM_CHAR);
        assert_eq!(dispatch_character(wa.0, 'a'), WM_NULL);
        assert!(matches!(rx_a.try_recv(), Ok(Command::Text(s)) if s == "a"));
        drop(guard);

        // The app stays active when focus passes to a sibling top-level
        // window. WM_ACTIVATE releases a key held by the old pane.
        unsafe {
            SendMessageW(wa.0, WM_ACTIVATE, Some(WPARAM(WA_ACTIVE as usize)), None);
            PostMessageW(
                Some(wa.0),
                WM_KEYDOWN,
                WPARAM(VK_LEFT.0 as usize),
                LPARAM(0),
            )
            .unwrap();
            let mut msg = MSG::default();
            assert!(
                PeekMessageW(&mut msg, Some(wa.0), WM_KEYDOWN, WM_KEYDOWN, PM_REMOVE).as_bool()
            );
            SendMessageW(wa.0, WM_ACTIVATE, Some(WPARAM(WA_INACTIVE as usize)), None);
        }
        assert!(matches!(rx_a.try_recv(), Ok(Command::Focus(true))));
        assert!(matches!(
            rx_a.try_recv(),
            Ok(Command::Key {
                down: true,
                key: KeyCode::LeftArrow,
                ..
            })
        ));
        assert!(matches!(
            rx_a.try_recv(),
            Ok(Command::Key {
                down: false,
                key: KeyCode::LeftArrow,
                ..
            })
        ));
        assert!(matches!(rx_a.try_recv(), Ok(Command::Focus(false))));
        assert!(rx_b.try_recv().is_err());

        // Native close before component teardown; subsequent messages to B
        // still reach the same channel after A removes its subclass and hooks.
        drop(wa);
        assert!(a.hwnd().is_none());
        drop(a);
        assert_eq!(dispatch_character(child_b.0, 'c'), WM_NULL);
        assert!(matches!(rx_b.try_recv(), Ok(Command::Text(s)) if s == "c"));
        drop(b);
        assert_eq!(dispatch_character(child_b.0, 'd'), WM_CHAR);
    }

    #[test]
    #[cfg(feature = "ui-smoke")]
    fn composition_follows_its_native_owner_through_deactivation_and_late_result() {
        let lab = Session::start(Source::Demo);
        let setup = || {
            let bridge = KeyboardBridge::install(|_| {}).unwrap();
            let window = HiddenWindow::new(bridge.bootstrap_title(), None);
            assert_eq!(bridge.try_bind(), Some(window.0));
            let (tx, mut rx) = mpsc::unbounded_channel();
            let _guard = bridge.scope().enter();
            bridge.set_target(Some((
                tx.clone(),
                lab.frames.clone(),
                Rc::new(Cell::new(false)),
            )));
            ime::set_surface(Some(ime::Surface::new(
                tx.clone(),
                Default::default(),
                || None,
            )));
            assert!(matches!(rx.try_recv(), Ok(Command::Focus(false))));
            (bridge, window, tx, rx)
        };
        let (a, wa, tx_a, mut rx_a) = setup();
        let (b, wb, tx_b, mut rx_b) = setup();
        let activate = |hwnd, value| unsafe {
            SendMessageW(hwnd, WM_ACTIVATE, Some(WPARAM(value as usize)), None);
        };
        let _guard_a = a.scope().enter();
        activate(wa.0, WA_ACTIVE);
        ime::smoke::start();
        ime::smoke::update(None, Some("before restart"), 14);
        a.reset_keys(&tx_b); // A sibling's new transport does not own this edit.
        assert!(ime::active());
        a.reset_keys(&tx_a);
        assert!(!ime::active());
        ime::smoke::update(Some("old transport"), None, 0);
        ime::smoke::end();
        ime::smoke::start();
        ime::smoke::update(None, Some("pending"), 7);
        assert!(ime::active());
        {
            let _guard_b = b.scope().enter();
            assert!(!ime::active());
            activate(wa.0, WA_INACTIVE);
            activate(wb.0, WA_ACTIVE);
            ime::smoke::start();
            ime::smoke::update(Some("second"), None, 0);
            ime::smoke::end();
        }
        ime::smoke::update(Some("late"), None, 0);
        assert!(!ime::active());
        assert!(matches!(rx_a.try_recv(), Ok(Command::Focus(true))));
        assert!(matches!(rx_a.try_recv(), Ok(Command::Focus(false))));
        assert!(rx_a.try_recv().is_err());
        assert!(matches!(rx_b.try_recv(), Ok(Command::Focus(true))));
        assert!(matches!(rx_b.try_recv(), Ok(Command::Composed(s)) if s == "second"));
        assert!(rx_b.try_recv().is_err());
    }
}
