//! Window-hosted read-only UI Automation Text pattern for the focused terminal.
//! The separate child HWND keeps WinUI's own chrome and settings providers intact.
// Preserve the Windows SDK spellings when matching on its constants.
#![allow(non_upper_case_globals)]
mod document;
mod ranges;
use document::{Document, Selection};
use ranges::{TextRange, interfaces};
use std::{
    cell::RefCell,
    ops::Range,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use w11term::terminal::{FrameStore, Snapshot};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::Variant::VARIANT,
        UI::{Accessibility::*, Shell::*, WindowsAndMessaging::*},
    },
    core::*,
};

fn unavailable() -> Error {
    Error::from_hresult(HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32))
}
fn unsupported() -> Error {
    Error::from_hresult(HRESULT(UIA_E_NOTSUPPORTED as i32))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub bounds: (i32, i32, i32, i32),
    pub cell_width: f64,
    pub cell_height: f64,
    pub padding: f64,
}

struct State {
    geometry: Geometry,
    selected: Option<Selection>,
    cached: Option<(u64, Arc<Document>)>,
    requested: Option<SelectionRequest>,
}
#[derive(Clone, Copy)]
pub struct SelectionRequest {
    pub pane: u64,
    pub generation: u64,
    pub span: Option<(w11term::interaction::Point, w11term::interaction::Point)>,
}
pub struct Pane<'a> {
    pub id: u64,
    pub label: &'a str,
    pub frames: &'a FrameStore,
    pub geometry: Geometry,
    pub selection: Option<&'a w11term::interaction::Selection>,
}
struct Source {
    hwnd: usize,
    id: u64,
    name: String,
    alive: AtomicBool,
    frames: FrameStore,
    state: Mutex<State>,
}
impl Source {
    fn check(&self) -> Result<()> {
        if self.alive.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(unavailable())
        }
    }
    fn document(&self) -> Result<Arc<Document>> {
        self.check()?;
        let f = self.frames.load();
        let mut state = self.state.lock().map_err(|_| unavailable())?;
        if let Some((gen_id, doc)) = &state.cached
            && *gen_id == f.generation
        {
            return Ok(doc.clone());
        }
        let doc = Arc::new(Document::from_frame(&f));
        state.cached = Some((f.generation, doc.clone()));
        Ok(doc)
    }
    fn rect(&self) -> Result<(Geometry, RECT)> {
        self.check()?;
        let mut rect = RECT::default();
        unsafe {
            GetWindowRect(HWND(self.hwnd as *mut _), &mut rect)?;
        }
        Ok((self.state.lock().map_err(|_| unavailable())?.geometry, rect))
    }
    fn select(&self, doc: &Document, span: Range<usize>) -> Result<()> {
        self.check()?;
        if self.frames.load().generation != doc.generation {
            return Err(unavailable());
        }
        let request = SelectionRequest {
            pane: self.id,
            generation: doc.generation,
            span: (!span.is_empty()).then(|| (doc.point(span.start), doc.point(span.end - 1))),
        };
        self.state.lock().map_err(|_| unavailable())?.requested = Some(request);
        unsafe {
            PostMessageW(
                Some(HWND(self.hwnd as *mut _)),
                APPLY_SELECTION,
                WPARAM(0),
                LPARAM(0),
            )
        }
    }
    fn focused(&self) -> bool {
        let hwnd = HWND(self.hwnd as *mut _);
        self.alive.load(Ordering::Acquire)
            && unsafe { GetAncestor(hwnd, GA_ROOT) == GetForegroundWindow() }
    }
}

#[implement(IRawElementProviderSimple, ITextProvider2)]
struct Provider {
    source: Arc<Source>,
}

impl Provider_Impl {
    fn range(&self, doc: Arc<Document>, span: Range<usize>) -> ITextRangeProvider {
        TextRange::create(self.to_interface(), self.source.clone(), doc, span)
    }
}
impl IRawElementProviderSimple_Impl for Provider_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }
    fn GetPatternProvider(&self, id: UIA_PATTERN_ID) -> Result<IUnknown> {
        self.source.check()?;
        if id == UIA_TextPatternId || id == UIA_TextPattern2Id {
            self.to_interface::<ITextProvider2>().cast()
        } else {
            Err(Error::empty())
        }
    }
    fn GetPropertyValue(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        self.source.check()?;
        Ok(match id {
            UIA_ControlTypePropertyId => UIA_DocumentControlTypeId.0.into(),
            UIA_NamePropertyId => self.source.name.as_str().into(),
            UIA_AutomationIdPropertyId => {
                format!("w11term-pane-{}", self.source.id).as_str().into()
            }
            UIA_IsContentElementPropertyId
            | UIA_IsControlElementPropertyId
            | UIA_IsKeyboardFocusablePropertyId
            | UIA_IsTextPatternAvailablePropertyId
            | UIA_IsTextPattern2AvailablePropertyId => true.into(),
            UIA_HasKeyboardFocusPropertyId => self.source.focused().into(),
            _ => VARIANT::default(),
        })
    }
    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        self.source.check()?;
        unsafe { UiaHostProviderFromHwnd(HWND(self.source.hwnd as *mut _)) }
    }
}
impl ITextProvider_Impl for Provider_Impl {
    fn GetSelection(&self) -> Result<*mut windows::Win32::System::Com::SAFEARRAY> {
        let doc = self.source.document()?;
        let selected = self
            .source
            .state
            .lock()
            .map_err(|_| unavailable())?
            .selected;
        interfaces(
            doc.selected(selected)
                .into_iter()
                .map(|r| self.range(doc.clone(), r)),
        )
    }
    fn GetVisibleRanges(&self) -> Result<*mut windows::Win32::System::Com::SAFEARRAY> {
        interfaces(std::iter::once(self.DocumentRange()?))
    }
    fn RangeFromChild(&self, _: Ref<IRawElementProviderSimple>) -> Result<ITextRangeProvider> {
        Err(Error::from_hresult(E_INVALIDARG))
    }
    fn RangeFromPoint(&self, p: &UiaPoint) -> Result<ITextRangeProvider> {
        let (g, r) = self.source.rect()?;
        if !p.x.is_finite() || !p.y.is_finite() {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        let doc = self.source.document()?;
        let row = ((p.y - f64::from(r.top) - g.padding) / g.cell_height) as usize;
        let col = ((p.x - f64::from(r.left) - g.padding) / g.cell_width) as usize;
        let logical = doc.cell_maps.get(row).map_or(col, |map| map.logical(col));
        let i = doc.position(row, logical);
        Ok(self.range(doc, i..i))
    }
    fn DocumentRange(&self) -> Result<ITextRangeProvider> {
        let doc = self.source.document()?;
        let end = doc.len();
        Ok(self.range(doc, 0..end))
    }
    fn SupportedTextSelection(&self) -> Result<SupportedTextSelection> {
        Ok(SupportedTextSelection_Single)
    }
}
impl ITextProvider2_Impl for Provider_Impl {
    fn RangeFromAnnotation(&self, _: Ref<IRawElementProviderSimple>) -> Result<ITextRangeProvider> {
        Err(Error::from_hresult(E_INVALIDARG))
    }
    fn GetCaretRange(&self, active: *mut BOOL) -> Result<ITextRangeProvider> {
        if active.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        let doc = self.source.document()?;
        unsafe {
            *active = (self.source.focused() && self.source.frames.load().cursor_visible).into();
        }
        let i = doc.caret;
        Ok(self.range(doc, i..i))
    }
}

struct Host {
    hwnd: HWND,
    parent: HWND,
    source: Arc<Source>,
    provider: IRawElementProviderSimple,
    last_frame: Option<Arc<Snapshot>>,
    last_caret: Option<(usize, i64, bool)>,
}
const APPLY_SELECTION: u32 = WM_APP + 17;
struct Site {
    provider: IRawElementProviderSimple,
    source: Arc<Source>,
    select: Box<dyn Fn()>,
}
thread_local! { static PROVIDERS: RefCell<std::collections::HashMap<isize, Site>> = RefCell::default(); }
unsafe extern "system" fn subclass(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    if msg == WM_GETOBJECT && l.0 as i32 == UiaRootObjectId {
        let p = PROVIDERS.with(|entries| {
            entries
                .borrow()
                .get(&(hwnd.0 as isize))
                .map(|s| s.provider.clone())
        });
        if let Some(p) = p {
            return unsafe { UiaReturnRawElementProvider(hwnd, w, l, &p) };
        }
    }
    match msg {
        APPLY_SELECTION => {
            PROVIDERS.with(|entries| {
                if let Some(site) = entries.borrow().get(&(hwnd.0 as isize)) {
                    (site.select)();
                }
            });
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            unsafe {
                let _ = ValidateRect(Some(hwnd), None);
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            if let Some(site) =
                PROVIDERS.with(|entries| entries.borrow_mut().remove(&(hwnd.0 as isize)))
            {
                site.source.alive.store(false, Ordering::Release);
            }
            unsafe {
                UiaReturnRawElementProvider(hwnd, WPARAM(0), LPARAM(0), None);
                let _ = RemoveWindowSubclass(hwnd, Some(subclass), 1);
                DefSubclassProc(hwnd, msg, w, l)
            }
        }
        _ => unsafe { DefSubclassProc(hwnd, msg, w, l) },
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        // Closing the parent can already have destroyed its children. A
        // recycled HWND must never tear down another window's provider.
        if self.source.alive.swap(false, Ordering::AcqRel) {
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

#[derive(Default)]
pub struct Bridge {
    host: RefCell<Option<Host>>,
}
impl Bridge {
    pub fn clear(&self) {
        self.host.borrow_mut().take();
    }
    pub fn update(
        &self,
        parent: HWND,
        pane: Pane<'_>,
        on_select: impl Fn(SelectionRequest) + 'static,
    ) {
        let Pane {
            id: pane,
            label,
            frames,
            geometry: g,
            selection,
        } = pane;
        let selected = selection.filter(|s| s.active).map(|s| Selection {
            anchor: s.anchor,
            end: s.end,
            block: s.block,
        });
        let mut slot = self.host.borrow_mut();
        let changed_pane = slot.as_ref().is_none_or(|h| {
            h.source.id != pane || h.parent != parent || !h.source.alive.load(Ordering::Acquire)
        });
        if changed_pane {
            slot.take();
            let created = unsafe {
                CreateWindowExW(
                    WS_EX_TRANSPARENT | WS_EX_NOACTIVATE,
                    w!("STATIC"),
                    w!("Terminal"),
                    WS_CHILD | WS_CLIPSIBLINGS,
                    g.bounds.0,
                    g.bounds.1,
                    g.bounds.2,
                    g.bounds.3,
                    Some(parent),
                    None,
                    None,
                    None,
                )
            };
            let Ok(hwnd) = created else { return };
            let source = Arc::new(Source {
                hwnd: hwnd.0 as usize,
                id: pane,
                name: format!("Terminal · {label}"),
                alive: AtomicBool::new(true),
                frames: frames.clone(),
                state: Mutex::new(State {
                    geometry: g,
                    selected,
                    cached: None,
                    requested: None,
                }),
            });
            let provider: IRawElementProviderSimple = Provider {
                source: source.clone(),
            }
            .into();
            if !unsafe { SetWindowSubclass(hwnd, Some(subclass), 1, 0) }.as_bool() {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return;
            }
            let live = source.clone();
            PROVIDERS.with(|entries| {
                entries.borrow_mut().insert(
                    hwnd.0 as isize,
                    Site {
                        provider: provider.clone(),
                        source: source.clone(),
                        select: Box::new(move || {
                            if let Ok(mut state) = live.state.lock()
                                && let Some(request) = state.requested.take()
                            {
                                on_select(request);
                            }
                        }),
                    },
                )
            });
            *slot = Some(Host {
                hwnd,
                parent,
                source,
                provider,
                last_frame: None,
                last_caret: None,
            });
        }
        let host = slot.as_mut().unwrap();
        let (moved, selection_changed) = {
            let mut state = host.source.state.lock().unwrap();
            let moved = state.geometry != g;
            let changed = state.selected != selected;
            state.selected = selected;
            state.geometry = g;
            (moved, changed)
        };
        if moved || changed_pane {
            unsafe {
                let _ = SetWindowPos(
                    host.hwnd,
                    Some(HWND_BOTTOM),
                    g.bounds.0,
                    g.bounds.1,
                    g.bounds.2,
                    g.bounds.3,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
        if !unsafe { UiaClientsAreListening() }.as_bool() {
            host.last_frame = None;
            return;
        }
        let frame = frames.load();
        let text_changed = host.last_frame.as_ref().is_none_or(|last| {
            last.generation != frame.generation
                && (last.rows.len() != frame.rows.len()
                    || last.cols != frame.cols
                    || last
                        .rows
                        .iter()
                        .zip(&frame.rows)
                        .any(|(a, b)| !Arc::ptr_eq(a, b) && a.as_str() != b.as_str()))
        });
        let caret = (frame.cursor.x, frame.cursor.y, frame.cursor_visible);
        let caret_changed = host.last_caret.replace(caret) != Some(caret);
        host.last_frame = Some(frame);
        let provider = host.provider.clone();
        let focused = host.source.focused();
        drop(slot);
        unsafe {
            if changed_pane && focused {
                let _ = UiaRaiseAutomationEvent(&provider, UIA_AutomationFocusChangedEventId);
            }
            if text_changed {
                let _ = UiaRaiseAutomationEvent(&provider, UIA_Text_TextChangedEventId);
            }
            if caret_changed || selection_changed {
                let _ = UiaRaiseAutomationEvent(&provider, UIA_Text_TextSelectionChangedEventId);
            }
        }
    }
}
