use super::*;
use std::collections::BTreeSet;
use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::*};
use windows::core::w;

pub type WindowId = u64;
const LEGACY_WINDOW: WindowId = 1;

#[derive(Default)]
struct Window {
    workspace: Option<String>,
    hwnd: usize,
    ui: Option<UiHandler>,
    capture: Option<CapturePng>,
    details: Option<WindowDetails>,
}

struct WindowDetails {
    name: Option<String>,
    active_tab_title: Option<String>,
    tab_count: usize,
}

#[derive(Serialize)]
struct WindowListing<'a> {
    window_id: WindowId,
    name: Option<&'a str>,
    active_tab_title: Option<&'a str>,
    tab_count: usize,
    pane_count: usize,
    active: bool,
}

#[derive(Default)]
struct State {
    windows: BTreeMap<WindowId, Window>,
    used: BTreeSet<WindowId>,
    recent: Vec<WindowId>,
    // Each pane has one authoritative owner, including through a live move.
    panes: BTreeMap<u64, (WindowId, Pane)>,
    moving: BTreeMap<u64, bool>, // true once removed from the source's local view
    pending_pane_requests: BTreeMap<u64, Vec<UiRequest>>,
    manifest: Option<PathBuf>,
}

impl State {
    fn legacy_workspace(&self) -> Option<String> {
        (self.windows.len() == 1)
            .then(|| self.windows.values().next()?.workspace.clone())
            .flatten()
    }

    fn publish(&self) -> Result<()> {
        let Some(manifest) = &self.manifest else {
            return Ok(());
        };
        let pid = std::process::id();
        let record = Instance {
            version: VERSION,
            pid,
            pipe: pipe_name(pid),
            workspace: self.legacy_workspace(),
            workspace_windows: Some(
                self.windows
                    .iter()
                    .filter_map(|(id, w)| {
                        Some(WorkspaceHint {
                            window_id: *id,
                            workspace: w.workspace.clone()?,
                        })
                    })
                    .collect(),
            ),
        };
        let mut file = tempfile::NamedTempFile::new_in(
            manifest.parent().context("Invalid discovery directory")?,
        )?;
        serde_json::to_writer(&mut file, &record)?;
        file.persist(manifest)?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct Registry {
    id: WindowId,
    state: Arc<Mutex<State>>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::for_workspace(None)
    }
}

pub struct PaneTransfer {
    state: Arc<Mutex<State>>,
    ids: Vec<u64>,
}
impl Drop for PaneTransfer {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        let mut pending = Vec::new();
        for id in &self.ids {
            if state.moving.remove(id) == Some(true) {
                state.panes.remove(id);
            }
            if let Some(requests) = state.pending_pane_requests.remove(id) {
                let ui = state
                    .panes
                    .get(id)
                    .and_then(|(owner, _)| state.windows.get(owner))
                    .and_then(|w| w.ui.clone());
                pending.push((ui, requests));
            }
        }
        drop(state);
        for (ui, requests) in pending {
            for request in requests {
                if request.pane_reply_closed() {
                    continue;
                }
                match &ui {
                    Some(handler) => handler(request),
                    None => request.fail("Pane closed during its window transfer"),
                }
            }
        }
    }
}

impl Registry {
    pub fn for_workspace(key: Option<String>) -> Self {
        Self::window(LEGACY_WINDOW, key)
    }

    pub fn window(id: WindowId, workspace: Option<String>) -> Self {
        assert_ne!(id, 0, "window ID zero is reserved");
        let mut state = State::default();
        state.windows.insert(
            id,
            Window {
                workspace,
                ..Window::default()
            },
        );
        state.used.insert(id);
        state.recent.push(id);
        Self {
            id,
            state: Arc::new(Mutex::new(state)),
        }
    }

    pub fn window_id(&self) -> WindowId {
        self.id
    }

    pub fn register_window(&self, id: WindowId, workspace: Option<String>) -> Result<Self> {
        let mut state = self.state.lock().unwrap();
        ensure!(
            id != 0 && state.used.insert(id),
            "Window ID {id} already used or invalid"
        );
        state.windows.insert(
            id,
            Window {
                workspace,
                ..Window::default()
            },
        );
        state.recent.insert(0, id);
        if let Err(error) = state.publish() {
            state.windows.remove(&id);
            state.used.remove(&id);
            state.recent.retain(|w| *w != id);
            return Err(error);
        }
        Ok(Self {
            id,
            state: self.state.clone(),
        })
    }

    pub fn activate(&self) {
        let mut state = self.state.lock().unwrap();
        if state.windows.contains_key(&self.id) {
            state.recent.retain(|w| *w != self.id);
            state.recent.push(self.id);
        }
    }

    pub fn unregister(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let Some(window) = state.windows.remove(&self.id) else {
            return Ok(());
        };
        clear_tag(window.hwnd, self.id);
        state.recent.retain(|w| *w != self.id);
        let State { panes, moving, .. } = &mut *state;
        panes.retain(|id, (owner, pane)| {
            if *owner != self.id {
                return true;
            }
            if let Some(detached) = moving.get_mut(id) {
                *detached = true;
                pane.active = false;
                pane.visible = false;
                return true;
            }
            false
        });
        state.publish()
    }

    pub fn set_hwnd(&self, raw: usize) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let window = state
            .windows
            .get_mut(&self.id)
            .context("Window already closed")?;
        if raw != 0 {
            let hwnd = HWND(raw as _);
            let mut pid = 0;
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                ensure!(
                    pid == std::process::id() && GetAncestor(hwnd, GA_ROOT) == hwnd,
                    "Invalid native window"
                );
                let old = GetPropW(hwnd, w!("w11term.control.windowId.v1"));
                ensure!(
                    old.is_invalid() || old.0 as usize as u64 == self.id,
                    "Native window already registered"
                );
                SetPropW(
                    hwnd,
                    w!("w11term.control.windowId.v1"),
                    Some(HANDLE(usize::try_from(self.id)? as _)),
                )?;
            }
        }
        if window.hwnd != raw {
            clear_tag(window.hwnd, self.id);
            window.hwnd = raw;
        }
        Ok(())
    }

    pub fn set_ui(&self, handler: impl Fn(UiRequest) + Send + Sync + 'static) {
        if let Some(window) = self.state.lock().unwrap().windows.get_mut(&self.id) {
            window.ui = Some(Arc::new(handler));
        }
    }

    pub fn set_window_details(
        &self,
        name: Option<&str>,
        active_tab_title: Option<&str>,
        tab_count: usize,
    ) {
        if let Some(window) = self.state.lock().unwrap().windows.get_mut(&self.id) {
            window.details = Some(WindowDetails {
                name: name.map(str::to_owned),
                active_tab_title: active_tab_title.map(str::to_owned),
                tab_count,
            });
        }
    }

    pub fn set_capture(&self, capture: CapturePng) {
        if let Some(window) = self.state.lock().unwrap().windows.get_mut(&self.id) {
            window.capture = Some(capture);
        }
    }

    pub(super) fn capture_for(&self, id: u64, fallback: CapturePng) -> Result<(Pane, CapturePng)> {
        let state = self.state.lock().unwrap();
        let (owner, pane) = state
            .panes
            .get(&id)
            .with_context(|| format!("unknown pane {id}"))?;
        let capture = pane
            .capture
            .clone()
            .or_else(|| state.windows.get(owner).and_then(|w| w.capture.clone()))
            .unwrap_or(fallback);
        Ok((pane.clone(), capture))
    }

    pub(super) fn active_ui(&self) -> Result<UiHandler> {
        let state = self.state.lock().unwrap();
        let owner = *state.recent.last().context("No terminal windows remain")?;
        state
            .windows
            .get(&owner)
            .and_then(|window| window.ui.clone())
            .context("Window's GUI control handler is unavailable")
    }

    pub(super) fn window_ui(&self, id: WindowId) -> Result<UiHandler> {
        self.state
            .lock()
            .unwrap()
            .windows
            .get(&id)
            .with_context(|| format!("unknown window {id}"))?
            .ui
            .clone()
            .with_context(|| format!("GUI control handler for window {id} is unavailable"))
    }

    pub(super) fn all_window_uis(&self) -> Result<Vec<(WindowId, UiHandler)>> {
        self.state
            .lock()
            .unwrap()
            .windows
            .iter()
            .map(|(&id, window)| {
                let ui = window.ui.clone().with_context(|| {
                    format!("GUI control handler for window {id} is unavailable")
                })?;
                Ok((id, ui))
            })
            .collect()
    }

    /// Recheck an already-queued request on the GUI thread. A transfer may
    /// have completed since the control worker first chose this window.
    pub fn for_this_window(&self, request: UiRequest) -> Option<UiRequest> {
        if let Some(pane) = request.routed_pane() {
            self.route_pane_request(pane, request, true)
        } else {
            Some(request)
        }
    }

    pub(super) fn post_pane_request(&self, id: u64, request: UiRequest) {
        let _ = self.route_pane_request(id, request, false);
    }

    fn route_pane_request(
        &self,
        id: u64,
        request: UiRequest,
        accept_local: bool,
    ) -> Option<UiRequest> {
        if request.pane_reply_closed() {
            return None;
        }
        let mut state = self.state.lock().unwrap();
        let Some(owner) = state.panes.get(&id).map(|(owner, _)| *owner) else {
            request.fail(&format!("unknown pane {id}"));
            return None;
        };
        if state.moving.contains_key(&id) {
            let waiting = state.pending_pane_requests.entry(id).or_default();
            waiting.retain(|request| !request.pane_reply_closed());
            if waiting.len() >= 8 {
                request.fail("Too many requests waiting on the pane transfer");
            } else {
                waiting.push(request);
            }
            return None;
        }
        if accept_local && owner == self.id {
            return Some(request);
        }
        let ui = state.windows.get(&owner).and_then(|w| w.ui.clone());
        drop(state);
        if let Some(ui) = ui {
            ui(request);
        } else {
            request.fail("Window's GUI control handler is unavailable");
        }
        None
    }

    pub fn begin_transfer(&self, ids: &[u64]) -> Result<PaneTransfer> {
        let mut state = self.state.lock().unwrap();
        ensure!(
            !ids.is_empty()
                && ids.iter().all(|id| !state.moving.contains_key(id)
                    && state
                        .panes
                        .get(id)
                        .is_some_and(|(owner, _)| *owner == self.id)),
            "Pane closed, moved, or already in transfer"
        );
        state.moving.extend(ids.iter().map(|id| (*id, false)));
        Ok(PaneTransfer {
            state: self.state.clone(),
            ids: ids.to_vec(),
        })
    }

    pub fn replace(&self, panes: impl IntoIterator<Item = Pane>) {
        // Evaluate UI data outside the global lock.
        let mut next: BTreeMap<_, _> = panes.into_iter().map(|pane| (pane.id, pane)).collect();
        let mut state = self.state.lock().unwrap();
        if !state.windows.contains_key(&self.id) {
            return;
        }
        let State { panes, moving, .. } = &mut *state;
        panes.retain(|id, (owner, old)| {
            if let Some(new) = next.remove(id) {
                if *owner == self.id || moving.contains_key(id) {
                    *owner = self.id;
                    *old = new;
                    if let Some(detached) = moving.get_mut(id) {
                        *detached = false;
                    }
                }
                return true;
            }
            if *owner != self.id {
                return true;
            }
            if let Some(detached) = moving.get_mut(id) {
                *detached = true;
                old.active = false;
                old.visible = false;
                return true;
            }
            false
        });
        panes.extend(next.into_iter().map(|(id, pane)| (id, (self.id, pane))));
    }

    pub(super) fn pane(&self, id: u64) -> Result<Pane> {
        self.state
            .lock()
            .unwrap()
            .panes
            .get(&id)
            .map(|(_, pane)| pane.clone())
            .with_context(|| format!("unknown pane {id}"))
    }

    pub fn pane_location(&self, id: u64) -> Result<(WindowId, u64)> {
        let state = self.state.lock().unwrap();
        ensure!(!state.moving.contains_key(&id), "pane {id} is moving");
        let (window, pane) = state
            .panes
            .get(&id)
            .with_context(|| format!("unknown pane {id}"))?;
        Ok((*window, pane.tab_id))
    }

    pub(super) fn listing(&self) -> Vec<(WindowId, Pane)> {
        let state = self.state.lock().unwrap();
        let active = state.recent.last();
        state
            .panes
            .values()
            .cloned()
            .map(|(id, mut pane)| {
                pane.active &= active == Some(&id);
                (id, pane)
            })
            .collect()
    }

    pub(super) fn list_windows(&self) -> Value {
        let state = self.state.lock().unwrap();
        let mut pane_counts = BTreeMap::new();
        for (owner, _) in state.panes.values() {
            *pane_counts.entry(*owner).or_insert(0) += 1;
        }
        let active = state.recent.last();
        let windows: Vec<_> = state
            .windows
            .iter()
            .filter_map(|(id, window)| {
                let details = window.details.as_ref()?;
                Some(WindowListing {
                    window_id: *id,
                    name: details.name.as_deref(),
                    active_tab_title: details.active_tab_title.as_deref(),
                    tab_count: details.tab_count,
                    pane_count: pane_counts.get(id).copied().unwrap_or(0),
                    active: active == Some(id),
                })
            })
            .collect();
        json!({ "windows": windows })
    }

    pub fn pane_input_in_window(
        &self,
        window: WindowId,
        id: u64,
    ) -> Option<tokio::sync::mpsc::UnboundedSender<Command>> {
        let state = self.state.lock().unwrap();
        let (owner, pane) = state.panes.get(&id)?;
        (*owner == window && !state.moving.contains_key(&id)).then(|| pane.input.clone())
    }

    pub(super) fn legacy_workspace(&self) -> Option<String> {
        self.state.lock().unwrap().legacy_workspace()
    }

    pub(super) fn workspace_windows(&self, key: &str) -> Vec<WorkspaceMatch> {
        self.state
            .lock()
            .unwrap()
            .windows
            .iter()
            .filter(|(_, w)| w.workspace.as_deref() == Some(key))
            .map(|(id, w)| WorkspaceMatch {
                window_id: *id,
                hwnd: w.hwnd,
            })
            .collect()
    }

    pub(super) fn set_manifest(&self, path: PathBuf) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.manifest = Some(path);
        state.publish()
    }

    pub(super) fn stop_publishing(&self) {
        self.state.lock().unwrap().manifest = None;
    }

    pub(super) fn publish_workspace(&self, id: WindowId, workspace: Option<String>) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let window = state
            .windows
            .get_mut(&id)
            .context("Workspace window closed")?;
        let previous = std::mem::replace(&mut window.workspace, workspace);
        if let Err(e) = state.publish() {
            state.windows.get_mut(&id).unwrap().workspace = previous;
            return Err(e);
        }
        Ok(())
    }
}

fn clear_tag(raw: usize, id: WindowId) {
    let mut pid = 0;
    if raw != 0 {
        unsafe {
            GetWindowThreadProcessId(HWND(raw as _), Some(&mut pid));
        }
    }
    if pid == std::process::id() && native_window_matches(raw, id) {
        unsafe {
            let _ = RemovePropW(HWND(raw as _), w!("w11term.control.windowId.v1"));
        }
    }
}

/// Used only for an explicit user workspace switch. The registry never focuses a window.
pub fn native_window_matches(raw: usize, id: WindowId) -> bool {
    let tag = unsafe { GetPropW(HWND(raw as _), w!("w11term.control.windowId.v1")) };
    !tag.is_invalid() && tag.0 as usize as u64 == id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window_control(registry: &Registry) {
        let id = registry.window_id();
        registry.set_ui(move |request| {
            let reply = match request {
                UiRequest::Open { reply, .. }
                | UiRequest::Resize { reply, .. }
                | UiRequest::ClosePane { reply, .. }
                | UiRequest::SplitPane { reply, .. }
                | UiRequest::SelectPaneView { reply, .. } => reply,
                _ => unreachable!("unexpected window control request"),
            };
            let _ = reply.send(Ok(json!(id)));
        });
    }

    fn pane(id: u64) -> Pane {
        Pane {
            id,
            tab_id: id + 10,
            tab_title: None,
            pane_name: None,
            label: "fixture".into(),
            active: true,
            visible: true,
            read_only: false,
            capture: None,
            input: mpsc::unbounded_channel().0,
        }
    }

    fn capture() -> CapturePng {
        Arc::new(|_, _, _| unreachable!())
    }

    #[tokio::test(start_paused = true)]
    async fn inspect_reports_a_missing_closed_or_unresponsive_pane() {
        let registry = Registry::default();
        let inspect = || registry.execute(Request::Inspect { pane: 7 }, capture());
        assert!(
            inspect()
                .await
                .unwrap_err()
                .to_string()
                .contains("unknown pane 7")
        );
        registry.replace([pane(7)]);
        assert!(
            inspect()
                .await
                .unwrap_err()
                .to_string()
                .contains("pane is closed")
        );
        let (input, _unresponsive) = mpsc::unbounded_channel();
        registry.replace([Pane { input, ..pane(7) }]);
        assert!(
            inspect()
                .await
                .unwrap_err()
                .to_string()
                .contains("inspection timed out")
        );
    }

    #[tokio::test]
    async fn open_uses_last_active_window_and_each_window_keeps_its_visible_panes() {
        let one = Registry::default();
        let two = one.register_window(2, None).unwrap();
        window_control(&one);
        window_control(&two);
        one.replace([pane(11)]);
        two.replace([pane(22)]);
        two.activate();
        let list = one.execute(Request::List, capture()).await.unwrap();
        let panels = list["sessions"].as_array().unwrap();
        assert_eq!(panels[0]["window_id"], 1);
        assert_eq!(panels[1]["window_id"], 2);
        assert!(panels.iter().all(|p| p["visible"] == true));
        assert_eq!(panels[0]["active"], false);
        assert_eq!(panels[1]["active"], true);
        let open = |window_id| Request::Open {
            profile: "fixture".into(),
            window_id,
            cols: None,
            rows: None,
            activate: false,
        };
        assert_eq!(one.execute(open(None), capture()).await.unwrap(), 2);
        assert_eq!(one.execute(open(Some(1)), capture()).await.unwrap(), 1);
        assert_eq!(one.execute(open(None), capture()).await.unwrap(), 2);
        assert_eq!(
            two.execute(
                Request::Resize {
                    pane: 11,
                    cols: 80,
                    rows: 32
                },
                capture()
            )
            .await
            .unwrap(),
            1
        );
        two.unregister().unwrap();
        assert_eq!(two.execute(open(None), capture()).await.unwrap(), 1);
        assert_eq!(
            two.execute(open(Some(2)), capture())
                .await
                .unwrap_err()
                .to_string(),
            "unknown window 2"
        );
        assert!(one.register_window(2, None).is_err());
    }

    #[tokio::test]
    async fn window_listing_keeps_utility_only_windows_and_removes_closed_windows() {
        let one = Registry::default();
        let two = one.register_window(2, None).unwrap();
        let starting = one.register_window(3, None).unwrap();
        window_control(&one);
        window_control(&two);
        one.set_window_details(Some("Operations"), Some("Terminal"), 2);
        two.set_window_details(None, Some("Settings"), 1);
        one.replace([pane(11), pane(12)]);
        two.activate();

        let list = one.execute(Request::ListWindows, capture()).await.unwrap();
        assert_eq!(
            list,
            json!({"windows":[
                {"window_id":1,"name":"Operations","active_tab_title":"Terminal","tab_count":2,"pane_count":2,"active":false},
                {"window_id":2,"name":null,"active_tab_title":"Settings","tab_count":1,"pane_count":0,"active":true},
            ]})
        );
        two.unregister().unwrap();
        let list = starting
            .execute(Request::ListWindows, capture())
            .await
            .unwrap();
        assert_eq!(list["windows"].as_array().unwrap().len(), 1);
        assert_eq!(list["windows"][0]["window_id"], 1);
        assert_eq!(list["windows"][0]["active"], true);
    }

    #[tokio::test]
    async fn live_transfer_keeps_sender_and_waits_to_route_resize_even_if_source_closes() {
        let one = Registry::default();
        let two = one.register_window(2, None).unwrap();
        window_control(&two);
        let pane = pane(3);
        one.replace([pane.clone()]);
        let transfer = one.begin_transfer(&[3]).unwrap();
        one.replace([]);
        one.unregister().unwrap();
        assert!(two.pane(3).unwrap().input.same_channel(&pane.input));
        let (reply, mut rx) = oneshot::channel();
        two.post_pane_request(
            3,
            UiRequest::Resize {
                pane: 3,
                cols: 100,
                rows: 40,
                reply,
            },
        );
        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        two.replace([pane.clone()]);
        drop(transfer);
        assert_eq!(rx.await.unwrap().unwrap(), 2);
        assert!(two.pane(3).unwrap().input.same_channel(&pane.input));
        assert_eq!(two.listing()[0].0, 2);
    }

    #[test]
    fn source_rollback_and_abandoned_transfer_release_the_reservation() {
        let source = Registry::default();
        let pane = pane(13);
        source.replace([pane.clone()]);
        let first = source.begin_transfer(&[13]).unwrap();
        assert!(source.begin_transfer(&[13]).is_err());
        source.replace([]);
        source.replace([pane.clone()]);
        drop(first);
        assert!(source.pane(13).unwrap().input.same_channel(&pane.input));
        let abandoned = source.begin_transfer(&[13]).unwrap();
        source.unregister().unwrap();
        drop(abandoned);
        assert!(source.pane(13).is_err());
    }

    #[tokio::test]
    async fn already_queued_pane_requests_follow_a_completed_transfer() {
        let one = Registry::default();
        let two = one.register_window(2, None).unwrap();
        let (tx, mut source_ui) = mpsc::unbounded_channel();
        one.set_ui(move |request| tx.send(request).unwrap());
        window_control(&two);
        let pane = pane(5);
        one.replace([pane.clone()]);
        let (reply, received) = oneshot::channel();
        one.post_pane_request(
            5,
            UiRequest::Resize {
                pane: 5,
                cols: 90,
                rows: 31,
                reply,
            },
        );
        let queued_resize = source_ui.try_recv().unwrap();
        let (reply, closed) = oneshot::channel();
        one.post_pane_request(5, UiRequest::ClosePane { pane: 5, reply });
        let queued_close = source_ui.try_recv().unwrap();
        let (reply, split) = oneshot::channel();
        one.post_pane_request(
            5,
            UiRequest::SplitPane {
                pane: 5,
                profile: "desk".into(),
                axis: crate::pane_layout::Axis::Columns,
                activate: false,
                reply,
            },
        );
        let queued_split = source_ui.try_recv().unwrap();
        let (reply, selection) = oneshot::channel();
        one.post_pane_request(
            5,
            UiRequest::SelectPaneView {
                pane: 5,
                zoom: Some(true),
                reply,
            },
        );
        let queued_selection = source_ui.try_recv().unwrap();
        let transfer = one.begin_transfer(&[5]).unwrap();
        two.replace([pane]);
        one.replace([]);
        drop(transfer);
        assert!(one.for_this_window(queued_resize).is_none());
        assert!(one.for_this_window(queued_close).is_none());
        assert!(one.for_this_window(queued_split).is_none());
        assert!(one.for_this_window(queued_selection).is_none());
        assert_eq!(received.await.unwrap().unwrap(), 2);
        assert_eq!(closed.await.unwrap().unwrap(), 2);
        assert_eq!(split.await.unwrap().unwrap(), 2);
        assert_eq!(selection.await.unwrap().unwrap(), 2);
    }

    #[tokio::test]
    async fn sftp_cancellation_follows_the_pane_and_ignores_abandoned_replies() {
        let source = Registry::default();
        let target = source.register_window(2, None).unwrap();
        let (tx, mut source_ui) = mpsc::unbounded_channel();
        source.set_ui(move |request| tx.send(request).unwrap());
        let (tx, mut target_ui) = mpsc::unbounded_channel();
        target.set_ui(move |request| tx.send(request).unwrap());
        let pane = pane(5);
        source.replace([pane.clone()]);
        let id = crate::sftp::TransferId {
            pane: 5,
            generation: 2,
            serial: 7,
        };
        let cancel = |reply| UiRequest::CancelSftpTransfer {
            transfer_id: id,
            reply,
        };
        let (reply, response) = oneshot::channel();
        source.post_pane_request(5, cancel(reply));
        let queued = source_ui.try_recv().unwrap();
        let moving = source.begin_transfer(&[5]).unwrap();
        assert!(source.for_this_window(queued).is_none());
        let (reply, abandoned) = oneshot::channel();
        source.post_pane_request(5, cancel(reply));
        drop(abandoned);
        target.replace([pane]);
        source.replace([]);
        drop(moving);
        let request = target
            .for_this_window(target_ui.try_recv().unwrap())
            .unwrap();
        let UiRequest::CancelSftpTransfer { transfer_id, reply } = request else {
            panic!("expected cancellation");
        };
        assert_eq!(transfer_id, id);
        reply.send(Ok(json!("cancelling"))).unwrap();
        assert_eq!(response.await.unwrap().unwrap(), "cancelling");
        assert!(target_ui.try_recv().is_err());
    }

    #[tokio::test]
    async fn waiting_for_a_transfer_distinguishes_pane_closure_from_a_recorded_result() {
        use crate::events::Events;
        use crate::sftp::{TransferId, TransferOutcome};
        for finished in [false, true] {
            let registry = Registry::default();
            let (input, mut commands) = mpsc::unbounded_channel();
            registry.replace([Pane { input, ..pane(7) }]);
            let id = TransferId {
                pane: 7,
                generation: 1,
                serial: 1,
            };
            let journal = Events::default();
            journal.sftp_transfer_started(id);
            let waiting = tokio::spawn(async move {
                registry
                    .execute(
                        Request::WaitSftpTransfer {
                            transfer_id: id,
                            timeout_seconds: 10,
                        },
                        capture(),
                    )
                    .await
            });
            let Some(crate::session::Command::Events(reply)) = commands.recv().await else {
                panic!("expected the wait to obtain the pane journal");
            };
            if finished {
                journal.sftp_transfer_finished(
                    id,
                    TransferOutcome::Completed,
                    "done",
                    None,
                    None,
                    false,
                );
            }
            reply.send(journal).unwrap();
            drop(commands);
            let response = waiting.await.unwrap();
            if finished {
                assert_eq!(response.unwrap()["transfer"]["outcome"], "completed");
            } else {
                assert!(
                    response
                        .unwrap_err()
                        .to_string()
                        .contains("Pane closed before")
                );
            }
        }
    }

    #[tokio::test]
    async fn sftp_listing_uses_the_current_owners_across_windows() {
        use crate::control::SftpTransferStatus;
        use crate::sftp::{TransferId, TransferOperation, TransferPhase};
        let one = Registry::default();
        let two = one.register_window(2, None).unwrap();
        one.replace([pane(7)]);
        two.replace([pane(9)]);
        for window in [&one, &two] {
            let window_id = window.window_id();
            window.set_ui(move |request| {
                let UiRequest::ListSftpTransfers { pane, reply } = request else {
                    panic!("expected a transfer listing");
                };
                // Each sample can contain a duplicate when a pane moves.
                let rows = [7, 9]
                    .into_iter()
                    .filter(|id| pane.is_none_or(|pane| pane == *id))
                    .map(|id| SftpTransferStatus {
                        transfer_id: TransferId {
                            pane: id,
                            generation: 1,
                            serial: 2,
                        },
                        pane: id,
                        window_id,
                        tab_id: id + 10,
                        label: "fixture".into(),
                        operation: TransferOperation::DownloadFile,
                        state: TransferPhase::Running,
                        remote: None,
                        progress: None,
                        batch: None,
                    })
                    .collect();
                reply.send(Ok(rows)).unwrap();
            });
        }
        let all = one
            .execute(Request::ListSftpTransfers { pane: None }, capture())
            .await
            .unwrap();
        let rows = all["transfers"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["window_id"], one.window_id());
        assert_eq!(rows[1]["window_id"], two.window_id());
        let filtered = one
            .execute(Request::ListSftpTransfers { pane: Some(9) }, capture())
            .await
            .unwrap();
        assert_eq!(filtered["transfers"].as_array().unwrap().len(), 1);
        assert_eq!(filtered["transfers"][0]["transfer_id"], "9:1:2");
    }

    #[test]
    fn native_window_tags_bind_distinct_handles_and_clear_on_unregister() {
        struct Hidden(HWND);
        impl Hidden {
            fn new() -> Self {
                Self(unsafe {
                    CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        w!("STATIC"),
                        w!("w11term registry test"),
                        WS_POPUP,
                        0,
                        0,
                        1,
                        1,
                        None,
                        None,
                        None,
                        None,
                    )
                    .unwrap()
                })
            }
        }
        impl Drop for Hidden {
            fn drop(&mut self) {
                unsafe {
                    let _ = DestroyWindow(self.0);
                }
            }
        }
        let (first, second) = (Hidden::new(), Hidden::new());
        let one = Registry::default();
        let two = one.register_window(2, None).unwrap();
        let a = first.0.0 as usize;
        let b = second.0.0 as usize;
        one.set_hwnd(a).unwrap();
        two.set_hwnd(b).unwrap();
        assert!(native_window_matches(a, 1) && native_window_matches(b, 2));
        assert!(!native_window_matches(a, 2) && !native_window_matches(b, 1));
        assert!(two.set_hwnd(a).is_err());
        one.unregister().unwrap();
        assert!(!native_window_matches(a, 1) && native_window_matches(b, 2));
        two.set_hwnd(0).unwrap();
        assert!(!native_window_matches(b, 2));
    }
}
