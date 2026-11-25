//! Inline IMM32 composition for terminal canvases. The input window stays with
//! WinUI, including its native candidate lists and input-method switching.
use super::*;
use windows::Win32::{
    Graphics::Gdi::{ClientToScreen, MapWindowPoints},
    UI::Input::Ime::*,
};

#[derive(Clone, Default)]
pub(crate) struct Preedit {
    pub text: String,
    /// UTF-16 index supplied by the IME, also used by DirectWrite.
    pub cursor: u32,
}
pub(crate) struct Surface {
    sender: mpsc::UnboundedSender<Command>,
    invalidator: windows_canvas::Invalidator,
    caret: Box<dyn Fn() -> Option<RECT>>,
}
impl Surface {
    pub(crate) fn new(
        sender: mpsc::UnboundedSender<Command>,
        invalidator: windows_canvas::Invalidator,
        caret: impl Fn() -> Option<RECT> + 'static,
    ) -> Self {
        Self {
            sender,
            invalidator,
            caret: Box::new(caret),
        }
    }
}
#[derive(Default)]
struct Composition {
    surface: Option<Surface>,
    owner: Option<HWND>,
    discarded: Option<HWND>,
    preedit: Preedit,
    offset: i32,
    candidate_mask: u32,
    positioned: Option<RECT>,
}
#[derive(Default)]
pub(super) struct WindowIme {
    composition: RefCell<Composition>,
    subclasses: RefCell<Vec<HWND>>,
    association: std::cell::Cell<Option<(HWND, HIMC)>>,
    native_composition: std::cell::Cell<Option<HWND>>,
}

fn with_composition<R>(f: impl FnOnce(&Composition) -> R) -> R {
    super::window::with_ime(|ime| f(&ime.composition.borrow()))
}

fn with_composition_mut<R>(f: impl FnOnce(&mut Composition) -> R) -> R {
    super::window::with_ime(|ime| f(&mut ime.composition.borrow_mut()))
}
impl Composition {
    fn clear(&mut self) {
        self.owner = None;
        self.preedit = Preedit::default();
        self.offset = 0;
        self.positioned = None;
        self.candidate_mask = 1;
        if let Some(surface) = &self.surface {
            surface.invalidator.invalidate();
        }
    }
    fn update(
        &mut self,
        hwnd: HWND,
        result: Option<String>,
        preedit: Option<String>,
        cursor: Option<u32>,
    ) {
        if self.owner != Some(hwnd) {
            return;
        }
        let Some(surface) = &self.surface else { return };
        if let Some(text) = result {
            if !text.is_empty() {
                let command = Command::Composed(text);
                with_input(|input| {
                    if let Ok(state) = input.try_borrow()
                        && let Some(state) = state.as_ref()
                    {
                        mirror_keyboard(&surface.sender, &state.broadcast, &command);
                    }
                });
                let _ = surface.sender.send(command);
            }
            self.preedit = Preedit::default();
            self.offset = 0;
        }
        if let Some(text) = preedit {
            self.preedit.text = text;
        }
        if let Some(cursor) = cursor {
            self.preedit.cursor = cursor.min(self.preedit.text.encode_utf16().count() as u32);
        }
        surface.invalidator.invalidate();
    }
}

pub(crate) fn set_surface(surface: Option<Surface>) {
    with_composition_mut(|ime| ime.surface = surface);
}
pub(crate) fn preedit(sender: &mpsc::UnboundedSender<Command>) -> Option<Preedit> {
    with_composition(|ime| {
        (ime.owner.is_some() && ime.surface.as_ref()?.sender.same_channel(sender))
            .then(|| ime.preedit.clone())
    })
}
pub(crate) fn active() -> bool {
    with_composition(|ime| ime.owner.is_some())
}

pub(super) fn native_composing(hwnd: HWND) -> bool {
    super::window::with_ime(|ime| ime.native_composition.get() == Some(hwnd))
        || (!hwnd.is_invalid()
            && Context::get(hwnd).is_some_and(|c| unsafe {
                ImmGetCompositionStringW(c.1, GCS_COMPSTR, None, 0) > 0
            }))
}
pub(crate) fn client_height() -> Option<i32> {
    let hwnd = window()?;
    let mut rect = RECT::default();
    unsafe {
        GetClientRect(hwnd, &mut rect).ok()?;
    }
    Some(rect.bottom)
}
pub(crate) fn caret_box(
    (pane_x, pane_y, w, h): (i32, i32, i32, i32),
    metrics: crate::renderer::Metrics,
    col: usize,
    row: i64,
) -> Option<RECT> {
    if w <= 0 || h <= 0 || row < 0 || row as usize >= metrics.rows {
        return None;
    }
    let px = |v: f32| (v * metrics.scale).round() as i32;
    let left = pane_x
        + px(crate::renderer::PADDING
            + col.min(metrics.cols.saturating_sub(1)) as f32 * metrics.cell_width);
    let top = pane_y + px(crate::renderer::PADDING + row as f32 * metrics.cell_height);
    Some(RECT {
        left,
        top,
        right: (left + px(metrics.cell_width)).min(pane_x + w),
        bottom: (top + px(metrics.cell_height)).min(pane_y + h),
    })
}

/// Each context obtained from the IMM belongs to the HWND it was acquired with.
struct Context(HWND, HIMC);
impl Context {
    fn get(hwnd: HWND) -> Option<Self> {
        let himc = unsafe { ImmGetContext(hwnd) };
        (!himc.is_invalid()).then_some(Self(hwnd, himc))
    }
    fn string(&self, kind: IME_COMPOSITION_STRING) -> Option<String> {
        let count = unsafe { ImmGetCompositionStringW(self.1, kind, None, 0) };
        if count < 0 || count % 2 != 0 || count > 65536 {
            return None;
        }
        let mut text = vec![0u16; count as usize / 2];
        let actual = unsafe {
            ImmGetCompositionStringW(self.1, kind, Some(text.as_mut_ptr().cast()), count as u32)
        };
        if actual < 0 || actual > count || actual % 2 != 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&text[..actual as usize / 2]))
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        unsafe {
            let _ = ImmReleaseContext(self.0, self.1);
        }
    }
}

pub(super) fn cancel() {
    let owner = with_composition_mut(|ime| {
        let owner = ime.owner?;
        ime.discarded = Some(owner);
        ime.clear();
        Some(owner)
    });
    if let Some(context) = owner.and_then(Context::get) {
        // ImmNotifyIME can synchronously deliver END/COMPOSITION. Drop all RefCell
        // borrows and forget the old destination before calling into the IME.
        unsafe {
            let _ = ImmNotifyIME(context.1, NI_COMPOSITIONSTR, CPS_CANCEL, 0);
        }
    }
}

fn eligible() -> bool {
    super::window::active()
        && with_input(|state| {
            state.try_borrow().ok().is_some_and(|state| {
                state.as_ref().is_some_and(|s| {
                    s.target.is_some()
                        && !s.is_read_only()
                        && !s.search
                        && !s.modal
                        && !s.drawer_focused
                        && !s.search_field_owns_keys()
                        && !s.switcher
                        && s.picker.is_none()
                        && s.local_mode.is_none()
                })
            })
        })
}

// Some non-editing island windows have no input context. Temporarily attach
// the UI thread's default HIMC there; never replace a native editor's HIMC.
pub(super) fn sync_context() {
    let hwnd = unsafe { GetFocus() };
    let wanted = (!hwnd.is_invalid() && eligible() && is_terminal(hwnd)).then_some(hwnd);
    let current = super::window::with_ime(|ime| ime.association.get());
    if current.is_some_and(|(old, _)| Some(old) == wanted) {
        return;
    }
    restore_context();
    if let Some(hwnd) = wanted
        && Context::get(hwnd).is_none()
        && unsafe { ImmAssociateContextEx(hwnd, HIMC::default(), IACE_DEFAULT) }.as_bool()
        && let Some(context) = Context::get(hwnd)
    {
        super::window::with_ime(|ime| ime.association.set(Some((hwnd, context.1))));
    }
}

fn restore_context() {
    if let Some((hwnd, bound)) = super::window::with_ime(|ime| ime.association.take())
        && Context::get(hwnd).is_some_and(|context| context.1 == bound)
    {
        unsafe {
            ImmAssociateContext(hwnd, HIMC::default());
        }
    }
}
pub(crate) fn position_after_draw(offset: f32, scale: f32) {
    with_composition_mut(|ime| ime.offset = (offset * scale).round() as i32);
    position(false);
}

pub(crate) fn position_after_layout() {
    position(false);
}
fn position(force: bool) {
    let update = with_composition_mut(|ime| {
        let hwnd = ime.owner?;
        let mut rect = (ime.surface.as_ref()?.caret)()?;
        rect.left += ime.offset;
        rect.right = rect.left + 1;
        if !force && ime.positioned == Some(rect) {
            return None;
        }
        ime.positioned = Some(rect);
        Some((hwnd, rect, ime.candidate_mask))
    });
    let Some((hwnd, rect, mask)) = update else {
        return;
    };
    let Some(context) = Context::get(hwnd) else {
        return;
    };
    let Some(root) = window() else { return };
    let mut points = [
        POINT {
            x: rect.left,
            y: rect.top,
        },
        POINT {
            x: rect.right,
            y: rect.bottom,
        },
    ];
    unsafe {
        MapWindowPoints(Some(root), Some(hwnd), &mut points);
    }
    let area = RECT {
        left: points[0].x,
        top: points[0].y,
        right: points[1].x,
        bottom: points[1].y,
    };
    for i in 0..4 {
        if mask & (1 << i) != 0 {
            let form = CANDIDATEFORM {
                dwIndex: i,
                dwStyle: CFS_EXCLUDE,
                ptCurrentPos: points[0],
                rcArea: area,
            };
            unsafe {
                let _ = ImmSetCandidateWindow(context.1, &form);
            }
        }
    }
}

pub(super) fn observe(hwnd: HWND, msg: u32) {
    if !matches!(
        msg,
        WM_SETFOCUS | WM_IME_STARTCOMPOSITION | WM_IME_SETCONTEXT | WM_IME_COMPOSITION
    ) || !is_terminal(hwnd)
    {
        return;
    }
    super::window::with_ime(|ime| {
        let mut hooks = ime.subclasses.borrow_mut();
        if !hooks.contains(&hwnd) && unsafe { SetWindowSubclass(hwnd, Some(proc), 2, 0) }.as_bool()
        {
            hooks.push(hwnd);
        }
    });
}
pub(super) fn uninstall() {
    cancel();
    restore_context();
    super::window::with_ime(|ime| {
        let mut hooks = ime.subclasses.borrow_mut();
        for hwnd in hooks.drain(..).filter(|hwnd| is_terminal(*hwnd)) {
            unsafe {
                let _ = RemoveWindowSubclass(hwnd, Some(proc), 2);
            }
        }
    });
    with_composition_mut(|ime| *ime = Composition::default());
}
unsafe extern "system" fn proc(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    let Some(scope) = WindowScope::for_window(hwnd) else {
        return unsafe { DefSubclassProc(hwnd, msg, w, l) };
    };
    let _guard = scope.enter();
    let handled =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(hwnd, msg, w, l)))
            .ok()
            .flatten();
    handled.unwrap_or_else(|| unsafe { DefSubclassProc(hwnd, msg, w, l) })
}
fn handle(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> Option<LRESULT> {
    let zero = Some(LRESULT(0));
    if matches!(
        msg,
        WM_IME_ENDCOMPOSITION | WM_INPUTLANGCHANGE | WM_KILLFOCUS | WM_NCDESTROY
    ) {
        super::window::with_ime(|ime| {
            if ime.native_composition.get() == Some(hwnd) {
                ime.native_composition.set(None);
            }
        });
    }
    if msg == WM_SETFOCUS {
        sync_context();
    }
    if matches!(msg, WM_KILLFOCUS | WM_INPUTLANGCHANGE | WM_NCDESTROY)
        && with_composition(|i| i.owner == Some(hwnd))
    {
        cancel();
    }
    if matches!(msg, WM_KILLFOCUS | WM_NCDESTROY)
        && super::window::with_ime(|ime| ime.association.get())
            .is_some_and(|(owner, _)| owner == hwnd)
    {
        restore_context();
    }
    if msg == WM_NCDESTROY {
        super::window::with_ime(|ime| ime.subclasses.borrow_mut().retain(|h| *h != hwnd));
        unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(proc), 2);
        }
        return None;
    }
    if !matches!(
        msg,
        WM_IME_STARTCOMPOSITION
            | WM_IME_COMPOSITION
            | WM_IME_ENDCOMPOSITION
            | WM_IME_SETCONTEXT
            | WM_IME_REQUEST
            | WM_IME_NOTIFY
    ) {
        return None;
    }
    if msg == WM_IME_STARTCOMPOSITION {
        with_composition_mut(|i| i.discarded = None);
        if !eligible() {
            super::window::with_ime(|ime| ime.native_composition.set(Some(hwnd)));
            return None;
        }
        with_composition_mut(|i| {
            i.clear();
            i.owner = Some(hwnd);
        });
        with_input(|state| {
            if let Ok(state) = state.try_borrow()
                && let Some(target) = state.as_ref().and_then(|s| s.target.as_ref())
                && target.frames.load().scroll_offset > 0
            {
                let _ = target.sender.send(Command::ScrollEdge(false));
            }
        });
        position(true);
        return zero;
    }
    if msg == WM_IME_COMPOSITION && with_composition(|i| i.discarded == Some(hwnd)) {
        return zero;
    }
    if msg == WM_IME_SETCONTEXT && w.0 != 0 && eligible() {
        return Some(unsafe {
            DefSubclassProc(
                hwnd,
                msg,
                w,
                LPARAM(l.0 & !(ISC_SHOWUICOMPOSITIONWINDOW as isize)),
            )
        });
    }
    if !with_composition(|i| i.owner == Some(hwnd)) {
        return None;
    }
    match msg {
        WM_IME_COMPOSITION => {
            let flags = l.0 as u32;
            #[cfg(feature = "ui-smoke")]
            if let Some((result, text, cursor)) = smoke::READ.with_borrow_mut(Option::take) {
                with_composition_mut(|i| i.update(hwnd, result, text, cursor));
                return zero;
            }
            let context = Context::get(hwnd)?;
            let has = |kind: IME_COMPOSITION_STRING| flags & kind.0 != 0;
            let result = has(GCS_RESULTSTR)
                .then(|| context.string(GCS_RESULTSTR))
                .flatten();
            let text = if flags == 0 {
                Some(String::new())
            } else {
                has(GCS_COMPSTR)
                    .then(|| context.string(GCS_COMPSTR))
                    .flatten()
            };
            let cursor = has(GCS_CURSORPOS).then(|| {
                unsafe { ImmGetCompositionStringW(context.1, GCS_CURSORPOS, None, 0) }.max(0) as u32
            });
            with_composition_mut(|i| i.update(hwnd, result, text, cursor));
            // Owning the result prevents the default procedure from generating
            // a second WM_IME_CHAR / WM_CHAR for these same UTF-16 units.
            zero
        }
        WM_IME_ENDCOMPOSITION => {
            with_composition_mut(|i| i.clear());
            zero
        }
        WM_IME_NOTIFY if matches!(w.0 as u32, IMN_OPENCANDIDATE | IMN_CHANGECANDIDATE) => {
            with_composition_mut(|i| i.candidate_mask = (l.0 as u32 & 15).max(1));
            position(true);
            None
        }
        WM_IME_REQUEST if w.0 as u32 == IMR_QUERYCHARPOSITION && l.0 != 0 => {
            // The operating system supplies this writable record for the call.
            let query = unsafe { &mut *(l.0 as *mut IMECHARPOSITION) };
            if query.dwSize < (std::mem::size_of::<IMECHARPOSITION>() as u32) {
                return None;
            }
            let (rect, offset, cursor) = with_composition(|i| {
                Some(((i.surface.as_ref()?.caret)()?, i.offset, i.preedit.cursor))
            })?;
            if query.dwCharPos != 0 && query.dwCharPos != cursor {
                return None;
            }
            let mut point = POINT {
                x: rect.left + if query.dwCharPos == 0 { 0 } else { offset },
                y: rect.top,
            };
            let root = window()?;
            unsafe {
                ClientToScreen(root, &mut point).ok().ok()?;
            }
            query.pt = point;
            query.cLineHeight = (rect.bottom - rect.top).max(1) as u32;
            unsafe {
                GetWindowRect(root, &mut query.rcDocument).ok()?;
            }
            Some(LRESULT(1))
        }
        _ => None,
    }
}

#[cfg(feature = "ui-smoke")]
pub(crate) mod smoke {
    use super::*;
    // These fixtures stand in only for the data a language IME puts into HIMC.
    // START, COMPOSITION, END and query requests still pass through User32.
    type Strings = (Option<String>, Option<String>, Option<u32>);
    thread_local! {pub(super) static READ: RefCell<Option<Strings>> = const{RefCell::new(None)};}
    pub fn start() {
        let hwnd = window().expect("terminal HWND");
        unsafe {
            SendMessageW(hwnd, WM_IME_STARTCOMPOSITION, None, None);
        }
    }
    pub fn update(result: Option<&str>, text: Option<&str>, cursor: u32) {
        READ.with_borrow_mut(|v| {
            *v = Some((
                result.map(str::to_owned),
                text.map(str::to_owned),
                Some(cursor),
            ))
        });
        let flags = GCS_CURSORPOS.0
            | if result.is_some() { GCS_RESULTSTR.0 } else { 0 }
            | if text.is_some() { GCS_COMPSTR.0 } else { 0 };
        unsafe {
            SendMessageW(
                window().unwrap(),
                WM_IME_COMPOSITION,
                None,
                Some(LPARAM(flags as isize)),
            );
        }
        READ.with_borrow_mut(Option::take);
    }
    pub fn query(cursor: u32) -> POINT {
        let mut query = IMECHARPOSITION {
            dwSize: std::mem::size_of::<IMECHARPOSITION>() as u32,
            dwCharPos: cursor,
            ..Default::default()
        };
        let hwnd = window().unwrap();
        let handled = unsafe {
            SendMessageW(
                hwnd,
                WM_IME_REQUEST,
                Some(WPARAM(IMR_QUERYCHARPOSITION as usize)),
                Some(LPARAM((&mut query as *mut IMECHARPOSITION) as isize)),
            )
        };
        assert_eq!(handled, LRESULT(1));
        unsafe {
            windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut query.pt)
                .ok()
                .unwrap();
        }
        query.pt
    }
    pub fn end() {
        unsafe {
            SendMessageW(window().unwrap(), WM_IME_ENDCOMPOSITION, None, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remapped_find_and_enter_leave_composition_and_its_key_release_to_the_native_edit() {
        let _window = WindowScope::default().enter();
        let hwnd = HWND::default();
        let pane = w11term::session::Session::start(w11term::options::Source::Demo);
        let (sender, mut input) = tokio::sync::mpsc::unbounded_channel();
        let actions = std::rc::Rc::new(RefCell::new(Vec::new()));
        let seen = actions.clone();
        let mut bindings = super::super::Shortcuts::default();
        bindings
            .set(super::super::Action::FindNext, Some("F2".parse().unwrap()))
            .unwrap();
        let mut state = State {
            search: true,
            target: Some(super::super::Target {
                sender,
                frames: pane.frames.clone(),
            }),
            keymap: bindings.keymap().unwrap(),
            ..State::new(move |s| seen.borrow_mut().push(s))
        };
        for vk in [0x0d, 0x1b, 0x71] {
            let key = |message| MSG {
                hwnd,
                message,
                wParam: WPARAM(vk),
                ..Default::default()
            };
            handle(hwnd, WM_IME_STARTCOMPOSITION, WPARAM(0), LPARAM(0));
            assert!(!state.handle(&key(WM_KEYDOWN)));
            handle(hwnd, WM_IME_ENDCOMPOSITION, WPARAM(0), LPARAM(0));
            assert!(!state.handle(&key(WM_KEYUP)));
        }
        assert!(actions.borrow().is_empty());
        assert!(input.try_recv().is_err());
        assert!(state.handle(&MSG {
            hwnd,
            message: WM_KEYDOWN,
            wParam: WPARAM(0x71),
            ..Default::default()
        }));
        assert_eq!(*actions.borrow(), [Shortcut::FindNext]);
    }
    #[test]
    fn remote_session_search_enter_commits_native_composition_before_submitting_a_query() {
        let _window = WindowScope::default().enter();
        let hwnd = HWND::default();
        let actions = std::rc::Rc::new(RefCell::new(Vec::new()));
        let seen = actions.clone();
        let mut state = State {
            search_field: Some(crate::input::SearchField::Sessions(1, 2, 3)),
            search_field_focused: true,
            ..State::new(move |s| seen.borrow_mut().push(s))
        };
        let key = |message| MSG {
            hwnd,
            message,
            wParam: WPARAM(0x0d),
            ..Default::default()
        };
        handle(hwnd, WM_IME_STARTCOMPOSITION, WPARAM(0), LPARAM(0));
        assert!(!state.handle(&key(WM_KEYDOWN)));
        handle(hwnd, WM_IME_ENDCOMPOSITION, WPARAM(0), LPARAM(0));
        assert!(!state.handle(&key(WM_KEYUP)));
        assert!(actions.borrow().is_empty());
        assert!(state.handle(&key(WM_KEYDOWN)));
        assert_eq!(
            &*actions.borrow(),
            &[Shortcut::SubmitSessionSearch(1, 2, 3)]
        );
    }

    #[test]
    fn updates_stay_local_until_a_utf16_result_and_cancel_cannot_retarget() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let hwnd = HWND(std::ptr::dangling_mut());
        let mut ime = Composition {
            owner: Some(hwnd),
            surface: Some(Surface::new(tx, Default::default(), || None)),
            ..Default::default()
        };
        ime.update(hwnd, None, Some("かな".into()), Some(2));
        assert_eq!(ime.preedit.text, "かな");
        assert!(rx.try_recv().is_err());
        ime.update(hwnd, Some("仮名🦀".into()), Some("次".into()), Some(1));
        assert!(matches!(rx.try_recv(),Ok(Command::Composed(text)) if text=="仮名🦀"));
        assert_eq!(ime.preedit.text, "次");
        ime.clear();
        ime.update(hwnd, Some("late".into()), None, None);
        assert!(rx.try_recv().is_err());
        assert!(ime.preedit.text.is_empty());
    }
    #[test]
    fn caret_uses_canvas_offsets_and_fractional_dpi() {
        let m = crate::renderer::Metrics {
            cell_width: 8.0,
            cell_height: 16.0,
            cols: 80,
            rows: 24,
            scale: 1.5,
        };
        assert_eq!(
            caret_box((300, 160, 500, 480), m, 2, 3),
            Some(RECT {
                left: 342,
                top: 250,
                right: 354,
                bottom: 274
            })
        );
        assert!(caret_box((0, 0, 500, 480), m, 1, -1).is_none());
    }
}
