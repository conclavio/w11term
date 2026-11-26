use super::*;
use w11term::pane_layout::{Axis, Layout};

const DIVIDER: f64 = 6.0;
fn native_pointer_shape(
    shape: wezterm_term::PointerShape,
) -> windows_reactor::InputSystemCursorShape {
    use wezterm_term::PointerShape::*;
    use windows_reactor::InputSystemCursorShape as Native;
    match shape {
        Default => Native::Arrow,
        Crosshair | Cell => Native::Cross,
        Pointer | Grab => Native::Hand,
        Help => Native::Help,
        Text => Native::IBeam,
        Move | Grabbing => Native::SizeAll,
        Northeast | Southwest | NortheastSouthwest => Native::SizeNortheastSouthwest,
        North | South | NorthSouth => Native::SizeNorthSouth,
        Northwest | Southeast | NorthwestSoutheast => Native::SizeNorthwestSoutheast,
        East | West | EastWest => Native::SizeWestEast,
        NotAllowed | NoDrop => Native::UniversalNo,
        Wait => Native::Wait,
        Progress => Native::AppStarting,
    }
}

type CanvasArea = Rc<Cell<Option<(f32, f32, f32)>>>;

/// Locate a leaf using the measured canvases on both sides of every divider.
/// The header occupies the space above the bottom-aligned terminal layout.
pub(super) struct PaneGeometry {
    layout: Layout,
    focused: TabId,
    areas: BTreeMap<TabId, CanvasArea>,
}

impl PaneGeometry {
    fn rect(&self, client_height: i32) -> Option<(i32, i32, i32, i32)> {
        self.rect_for(self.focused, client_height)
    }

    fn rect_for(&self, pane: TabId, client_height: i32) -> Option<(i32, i32, i32, i32)> {
        let decorated = matches!(self.layout, Layout::Split { .. });
        let border = if decorated { 1.0 } else { 0.0 };
        fn measure(
            node: &Layout,
            data: &PaneGeometry,
            border: f32,
            pane: TabId,
            target: &mut Option<(f32, f32)>,
        ) -> Option<(f32, f32)> {
            match node {
                Layout::Leaf(id) => {
                    if *id == pane {
                        *target = Some((border, border));
                    }
                    let (w, h, _) = data.areas.get(id)?.get()?;
                    Some((w + 2.0 * border, h + 2.0 * border))
                }
                Layout::Split {
                    axis,
                    first,
                    second,
                    ..
                } => {
                    let a = measure(first, data, border, pane, target)?;
                    let mut second_origin = None;
                    let b = measure(second, data, border, pane, &mut second_origin)?;
                    if let Some((x, y)) = second_origin {
                        *target = Some(match axis {
                            Axis::Columns => (a.0 + DIVIDER as f32 + x, y),
                            Axis::Rows => (x, a.1 + DIVIDER as f32 + y),
                        });
                    }
                    Some(match axis {
                        Axis::Columns => (a.0 + DIVIDER as f32 + b.0, a.1.max(b.1)),
                        Axis::Rows => (a.0.max(b.0), a.1 + DIVIDER as f32 + b.1),
                    })
                }
            }
        }
        let mut origin = None;
        let (_, total_h) = measure(&self.layout, self, border, pane, &mut origin)?;
        let (x, y) = origin?;
        let (w, h, scale) = self.areas.get(&pane)?.get()?;
        let px = |v: f32| (v * scale).round() as i32;
        Some((px(x), client_height - px(total_h - y), px(w), px(h)))
    }

    fn contains(&self, pane: TabId, point: (i32, i32), client_height: i32) -> bool {
        self.rect_for(pane, client_height)
            .is_some_and(|(x, y, w, h)| {
                (x..x + w).contains(&point.0) && (y..y + h).contains(&point.1)
            })
    }
}

/// A stationary pointer may receive no native enter/move when its pane changes shape.
/// Keep only a previous, positive canvas hit until all the affected leaves are remeasured.
pub(super) struct RotationHover {
    point: (i32, i32),
    before: BTreeMap<TabId, (f32, f32, f32)>,
    remeasuring: bool,
}

impl RotationHover {
    fn capture(
        geometry: &PaneGeometry,
        panes: &[TabId],
        hovered: TabId,
        point: (i32, i32),
        client_height: i32,
    ) -> Option<Self> {
        if !panes.contains(&hovered) || !geometry.contains(hovered, point, client_height) {
            return None;
        }
        let before = panes
            .iter()
            .map(|id| Some((*id, geometry.areas.get(id)?.get()?)))
            .collect::<Option<_>>()?;
        Some(Self {
            point,
            before,
            remeasuring: false,
        })
    }

    fn measured(&self, geometry: &PaneGeometry) -> bool {
        self.before.iter().all(|(id, old)| {
            geometry
                .areas
                .get(id)
                .and_then(|area| area.get())
                .is_some_and(|new| self.remeasuring || (new.0, new.1) != (old.0, old.1))
        })
    }

    fn hit(
        &self,
        geometry: &PaneGeometry,
        cursor: Option<(i32, i32)>,
        height: i32,
    ) -> Option<TabId> {
        if cursor != Some(self.point) {
            return None;
        }
        self.before
            .keys()
            .copied()
            .find(|id| geometry.contains(*id, self.point, height))
    }
}

struct DividerDrag {
    path: Vec<bool>,
    origin: f64,
    extent: f64,
}

impl DividerDrag {
    fn ratio(&self, position: f64) -> f64 {
        let min = (40.0 / self.extent).min(0.5);
        ((position - self.origin) / self.extent).clamp(min, 1.0 - min)
    }
}

#[derive(Default)]
pub(super) struct DividerState {
    epoch: u64,
    drag: Option<DividerDrag>,
}

impl DividerState {
    // In-place rotations reuse the same divider path with a different coordinate axis.
    pub(super) fn invalidate(&mut self) {
        self.epoch += 1;
        self.drag = None;
    }

    fn begin(&mut self, epoch: u64, drag: impl FnOnce() -> Option<DividerDrag>) {
        if self.epoch == epoch {
            self.drag = drag();
        }
    }

    fn active(&self, epoch: u64, path: &[bool]) -> Option<&DividerDrag> {
        self.drag
            .as_ref()
            .filter(|drag| self.epoch == epoch && drag.path == path)
    }

    pub(super) fn dragging(&self, epoch: u64, path: &[bool]) -> bool {
        self.active(epoch, path).is_some()
    }

    pub(super) fn end(&mut self, epoch: u64, path: &[bool]) {
        if self.dragging(epoch, path) {
            self.drag = None;
        }
    }
}

#[derive(Clone, Copy, Default)]
enum WindowGrid {
    #[default]
    Off,
    Terminal,
    WithDrawer,
}

#[derive(Default)]
pub(super) struct PaneSurface {
    #[cfg(feature = "ui-smoke")]
    pub(super) paint_count: Rc<Cell<u64>>,
    pub(super) metrics: Rc<Cell<Metrics>>,
    pub(super) invalidator: Invalidator,
    pub(super) scrollbar: Rc<scrollback_bar::ScrollBar>,
    cursor_blink: Rc<RefCell<crate::cursor_blink::CursorBlink>>,
    cursor_guide: Rc<Cell<CursorGuide>>,
    bell: Rc<Cell<visual_bell::Pulse>>,
    pub(super) area: Rc<Cell<Option<(f32, f32, f32)>>>,
    window_grid: Rc<Cell<WindowGrid>>,
    window_size: Rc<Cell<(f64, f64)>>,
    canvas: RefCell<Option<View>>,
}

impl PaneSurface {
    pub(super) fn window_hidden(&self) {
        self.window_grid.set(WindowGrid::Off);
        self.release();
    }

    pub(super) fn release(&self) {
        self.pause_cursor();
        self.update_bell(Instant::now(), false);
        self.scrollbar.configure(false, false, false);
        self.canvas.borrow_mut().take();
    }

    pub(super) fn pause_cursor(&self) {
        self.cursor_blink
            .borrow_mut()
            .set_focus(false, None, &self.invalidator);
    }

    pub(super) fn ring_bell(&self, received: Instant, now: Instant) -> bool {
        let mut pulse = self.bell.get();
        if !pulse.ring(received, now) {
            return false;
        }
        self.bell.set(pulse);
        self.invalidator.invalidate();
        true
    }

    pub(super) fn update_bell(&self, now: Instant, enabled: bool) -> bool {
        let mut pulse = self.bell.get();
        if pulse.visible && (!enabled || pulse.elapsed(now)) {
            pulse.visible = false;
            self.bell.set(pulse);
            self.invalidator.invalidate();
        }
        pulse.visible
    }

    pub(super) fn remeasure(&self, renderer: &mut Renderer) -> Option<TerminalSize> {
        let (width, height, scale) = self.area.get()?;
        let metrics = renderer.measure(width, height, scale);
        self.metrics.set(metrics);
        Some(terminal_size(metrics))
    }
}

fn terminal_size(metrics: Metrics) -> TerminalSize {
    TerminalSize {
        rows: metrics.rows,
        cols: metrics.cols,
        pixel_width: (metrics.cols as f32 * metrics.cell_width * metrics.scale).round() as usize,
        pixel_height: (metrics.rows as f32 * metrics.cell_height * metrics.scale).round() as usize,
        dpi: (96.0 * metrics.scale).round() as u32,
    }
}

fn minimum_canvas(metrics: Metrics, axis: Axis) -> f64 {
    f64::from(
        2.0 * PADDING
            + match axis {
                Axis::Columns => metrics.cell_width,
                Axis::Rows => STATUS_HEIGHT + metrics.cell_height,
            },
    )
}

fn rotation_extent(
    area: (f32, f32, f32),
    metrics: Metrics,
    axis: Axis,
    scale: f32,
) -> Option<(f64, f64)> {
    let (width, height, canvas_scale) = area;
    if canvas_scale != scale || metrics.scale != scale || !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let extent = match axis {
        Axis::Columns => width,
        Axis::Rows => height,
    };
    if !extent.is_finite() || extent <= 0.0 {
        return None;
    }
    let scale = f64::from(scale);
    // All split panes have a one-DIP border on each side of the measured canvas.
    // Allow for either neighboring device pixel when layout rounds that border.
    Some((
        (f64::from(extent) * scale).round() + 2.0 * scale.floor(),
        (minimum_canvas(metrics, axis) * scale).ceil() + 2.0 * scale.ceil(),
    ))
}

impl TerminalApp {
    pub(super) fn capture_rotation_hover(&self, panes: &[TabId]) -> Option<RotationHover> {
        RotationHover::capture(
            self.measured_layout.as_ref()?,
            panes,
            self.keyboard.hovered_pane()?,
            crate::input::cursor_client()?,
            crate::input::ime::client_height()?,
        )
    }

    pub(super) fn reset_balance_measurements(&self, panes: &[TabId]) -> Option<RotationHover> {
        let mut hover = (!self.overlay_visible.get())
            .then(|| self.capture_rotation_hover(panes))
            .flatten();
        if let Some(hover) = &mut hover {
            hover.remeasuring = true;
        }
        // A translated pane can keep its size. Clear each old measurement so
        // every invalidated canvas, including unchanged ones, reports its draw.
        for &id in panes {
            if let Some(AppTab {
                page: Page::Terminal(tab),
                ..
            }) = self.tabs.get(id)
            {
                tab.surface.area.set(None);
            }
        }
        hover
    }

    pub(super) fn restore_rotation_hover(&mut self) {
        let Some(pending) = &self.rotation_hover else {
            return;
        };
        let Some(geometry) = &self.measured_layout else {
            return;
        };
        if !pending.measured(geometry) {
            return;
        }
        let pending = self.rotation_hover.take().unwrap();
        if self.keyboard.hovered_pane().is_some()
            || self.hints.borrow().is_some()
            || self.copy_mode.borrow().is_some()
        {
            return;
        }
        let Some(height) = crate::input::ime::client_height() else {
            return;
        };
        if let Some(id) = pending.hit(geometry, crate::input::cursor_client(), height)
            && matches!(self.tabs.get(id).map(|tab| &tab.page), Some(Page::Terminal(live))
                if live.error.is_none() && !live.browsing_sessions())
        {
            self.keyboard.hover_pane(id);
        }
    }
    fn pane_geometry(&self) -> Option<PaneGeometry> {
        let focused = self.tabs.active_id()?;
        let layout = if self.tabs.zoomed() {
            Layout::Leaf(focused)
        } else {
            self.tabs.active_layout()?.clone()
        };
        let areas = layout
            .leaves()
            .into_iter()
            .map(|id| {
                let Page::Terminal(tab) = &self.tabs.get(id)?.page else {
                    return None;
                };
                Some((id, tab.surface.area.clone()))
            })
            .collect::<Option<_>>()?;
        Some(PaneGeometry {
            layout,
            focused,
            areas,
        })
    }

    pub(super) fn focused_pane_rect(&self) -> Option<(i32, i32, i32, i32)> {
        self.measured_layout
            .as_ref()?
            .rect(crate::input::ime::client_height()?)
    }

    pub(super) fn sync_ime_surface(&mut self) {
        self.measured_layout = self.pane_geometry().map(Rc::new);
        let surface = self.measured_layout.clone().and_then(|geometry| {
            let Page::Terminal(tab) = &self.tabs.active()?.page else {
                return None;
            };
            if tab.browsing_sessions() {
                return None;
            }
            let metrics = tab.surface.metrics.clone();
            let frames = tab.session.frames.clone();
            Some(crate::input::ime::Surface::new(
                tab.session.input.clone(),
                tab.surface.invalidator.clone(),
                move || {
                    let rect = geometry.rect(crate::input::ime::client_height()?)?;
                    let frame = frames.load();
                    crate::input::ime::caret_box(
                        rect,
                        metrics.get(),
                        frame.visual_cursor_column(),
                        frame.cursor.y,
                    )
                },
            ))
        });
        crate::input::ime::set_surface(surface);
    }

    fn pane_canvas(
        &self,
        id: TabId,
        tab: &LiveTab,
        context: &ViewContext<Self>,
    ) -> windows_canvas::Result<View> {
        let surface = &tab.surface;
        // CanvasHost invalidates when its draw closure changes identity. Keep one
        // view per mounted leaf so that the output of a sibling cannot repaint it.
        if let Some(canvas) = surface.canvas.borrow().as_ref() {
            return Ok(canvas.clone());
        }
        let appearance = self.appearance.read().unwrap();
        let mut renderer = Renderer::with_size(
            &appearance.font_family,
            appearance.ligatures,
            appearance.terminal_font_size_for(tab.font_size_override()),
        )?;
        renderer.live_cursor = Some(surface.cursor_blink.clone());
        let renderer = RefCell::new(renderer);
        let frames = tab.session.frames.clone();
        let metrics = surface.metrics.clone();
        let sender = tab.session.input.clone();
        let last_size = tab.last_size.clone();
        let error_sender = context.sender();
        let geometry_sender = context.sender();
        let initial_grid = self.initial_grid.clone();
        let windows = self.windows.clone();
        let window_size = surface.window_size.clone();
        let window_grid = surface.window_grid.clone();
        let remember_grid = tab.pane_font_size.get().is_none();
        let area = surface.area.clone();
        let selection = tab.selection.clone();
        let read_only = tab.read_only.clone();
        let cursor_guide = surface.cursor_guide.clone();
        let scrollbar = surface.scrollbar.clone();
        let scrollbar_brush = RefCell::new(None);
        let bell = surface.bell.clone();
        let bell_brush = RefCell::new(None);
        let hover = tab.hover.clone();
        let hints = self.hints.clone();
        let copy_mode = self.copy_mode.clone();
        let native_window = self.keyboard.scope();
        #[cfg(feature = "ui-smoke")]
        let paint_count = surface.paint_count.clone();
        let canvas: View = Canvas::invalidated(&surface.invalidator, move |ctx| {
            let _native = native_window.enter();
            #[cfg(feature = "ui-smoke")]
            paint_count.set(paint_count.get() + 1);
            let hints = hints.borrow();
            let hints = hints.as_ref().filter(|h| h.pane == id);
            let frame = hints.map_or_else(|| frames.load(), |h| h.frame.clone());
            let mut renderer = renderer.borrow_mut();
            let mode = copy_mode.borrow();
            let mode = mode
                .as_ref()
                .filter(|m| m.pane == id && m.state.valid(&frame));
            renderer.copy_cursor = mode.map(|m| m.state.cursor);
            renderer.read_only = read_only.get();
            renderer.cursor_guide = cursor_guide.get();
            let marked = mode.and_then(|m| m.state.selection());
            let measured = {
                let mouse_selection = selection.borrow();
                let visible_selection = marked
                    .as_ref()
                    .or(mouse_selection.as_ref())
                    .filter(|selection| selection.survives_scrollback_clear(&frame));
                let measured = renderer.draw(ctx, &frame, visible_selection)?;
                renderer.draw_interaction(
                    ctx,
                    &frame,
                    visible_selection,
                    hover.borrow().as_ref(),
                )?;
                measured
            };
            if let Some(hints) = hints {
                renderer.draw_hint_labels(ctx, &frame, hints.visible_labels())?;
            }
            scrollbar.draw(ctx, &frame, measured, &mut scrollbar_brush.borrow_mut())?;
            let previous_metrics = metrics.replace(measured);
            let bounds = (ctx.width, ctx.height, measured.scale);
            let previous_area = area.replace(Some(bounds));
            if matches!(window_grid.get(), WindowGrid::Off)
                && (previous_area != Some(bounds)
                    || previous_metrics.cell_width != measured.cell_width
                    || previous_metrics.cell_height != measured.cell_height)
            {
                geometry_sender.send(Message::PaneMeasured(id));
            }
            if let Some(preedit) = crate::input::ime::preedit(&sender) {
                let offset = renderer.draw_preedit(ctx, &frame, &preedit.text, preedit.cursor)?;
                crate::input::ime::position_after_draw(offset, measured.scale);
            }
            visual_bell::draw(
                ctx,
                bell.get().visible,
                frame.visual_bell_color.map(w11term::terminal::rgba),
                &mut bell_brush.borrow_mut(),
            );
            let window_mode = window_grid.get();
            if matches!(window_mode, WindowGrid::Terminal) {
                if !remember_grid {
                    // The next launch will use the profile or window text size again.
                    windows.borrow_mut().grid = None;
                } else if initial_grid
                    .get()
                    .is_none_or(|grid| grid == (measured.cols, measured.rows))
                {
                    windows.borrow_mut().grid = Some((measured.cols, measured.rows));
                }
                crate::input::set_grid(
                    measured.cell_width,
                    measured.cell_height,
                    ctx.width,
                    ctx.height,
                    measured.scale,
                );
            }
            if !matches!(window_mode, WindowGrid::Off)
                && let Some(((cols, rows), first)) =
                    initial_grid.correction((measured.cols, measured.rows))
            {
                let (width, height) = window_size.get();
                geometry_sender.send(Message::Geometry(
                    width + (cols as f32 * measured.cell_width + PADDING * 2.0 - ctx.width) as f64,
                    height
                        + (rows as f32 * measured.cell_height + PADDING * 2.0 + STATUS_HEIGHT
                            - ctx.height) as f64,
                    initial_grid.epoch(),
                    first,
                ));
                return Ok(());
            }
            let size = terminal_size(measured);
            if last_size.replace(Some(size)) != Some(size) {
                let _ = sender.send(Command::Resize(size));
            }
            Ok(())
        })
        .on_error(move |error| {
            error_sender.send(Message::RenderError(id, format!("{error:?}")));
        })
        .into();
        *surface.canvas.borrow_mut() = Some(canvas.clone());
        Ok(canvas)
    }

    pub(super) fn pane_view(
        &self,
        id: TabId,
        tab: &LiveTab,
        decorate: bool,
        context: &ViewContext<Self>,
    ) -> View {
        let show_scrollbar = tab.error.is_none()
            && !tab.browsing_sessions()
            && !self.hints.borrow().as_ref().is_some_and(|h| h.pane == id)
            && !self
                .copy_mode
                .borrow()
                .as_ref()
                .is_some_and(|m| m.pane == id);
        let (failed_marks, cursor_guide) = {
            let appearance = self.appearance.read().unwrap();
            (
                appearance.interaction.failed_command_marks,
                appearance.cursor_guide,
            )
        };
        let scrollbar_changed =
            tab.surface
                .scrollbar
                .configure(show_scrollbar, tab.find.open, failed_marks);
        let guide_changed = tab.surface.cursor_guide.replace(cursor_guide) != cursor_guide;
        if scrollbar_changed || guide_changed {
            tab.surface.invalidator.invalidate();
        }
        let focused = tab.error.is_none() && self.keyboard.terminal_focused(&tab.session.input);
        let caret_interval = match tab.source {
            Source::TimedReplay(_) | Source::PausedReplay(_)
                if tab.session.events.replay_state()
                    != Some(w11term::session::ReplayState::Playing) =>
            {
                None
            }
            _ => self.caret_interval,
        };
        tab.surface.cursor_blink.borrow_mut().set_focus(
            focused,
            caret_interval,
            &tab.surface.invalidator,
        );
        tab.surface
            .window_grid
            .set(match self.tabs.active_layout() {
                Some(Layout::Leaf(_)) if tab.files.open => WindowGrid::WithDrawer,
                Some(Layout::Leaf(_)) => WindowGrid::Terminal,
                _ => WindowGrid::Off,
            });
        tab.surface
            .window_size
            .set((self.width, self.window_height));
        let canvas = match tab.error.as_deref() {
            Some(reason) => Err(reason.to_owned()),
            None => self
                .pane_canvas(id, tab, context)
                .map_err(|error| error.to_string()),
        };
        let drop_target = self.pane_drop_target(id, tab);
        let content = match canvas {
            Ok(canvas) => {
                let mut border = Border::new()
                    .background(Color::rgb(14, 16, 20))
                    .drop_policy(drop_target.map(|_| {
                        DragDropPolicy::new().storage_items(
                            DragDropAction::new(DragDropOperation::Copy)
                                .caption("Insert file paths"),
                        )
                    }))
                    .on_drop(context.callback(move |data| {
                        drop_target.map_or(Message::ControlIdle, |target| target.data(data))
                    }))
                    .capture_pointer_on_press(true)
                    .on_pointer_capture_lost(context.callback(move |_| Message::CaptureLost(id)))
                    .on_pointer_entered(context.callback(move |_| Message::HoverPane(id)))
                    .on_pointer_exited(context.callback(move |_| Message::PointerExit(id)))
                    .on_pointer_pressed(
                        context.callback(move |p| Message::Pointer(id, p, MouseEventKind::Press)),
                    )
                    .on_pointer_released(
                        context.callback(move |p| Message::Pointer(id, p, MouseEventKind::Release)),
                    )
                    .on_pointer_moved(
                        context.callback(move |p| Message::Pointer(id, p, MouseEventKind::Move)),
                    );
                let local_pointer = tab.find.open
                    || tab.hover.borrow().is_some()
                    || tab.selection.borrow().as_ref().is_some_and(|s| s.dragging)
                    || self
                        .hints
                        .borrow()
                        .as_ref()
                        .is_some_and(|mode| mode.pane == id)
                    || self
                        .copy_mode
                        .borrow()
                        .as_ref()
                        .is_some_and(|mode| mode.pane == id);
                if !local_pointer && let Some(shape) = tab.session.frames.load().pointer_shape {
                    border = border.pointer_cursor(native_pointer_shape(shape));
                }
                border.content(canvas)
            }

            Err(reason) => Border::new()
                .on_pointer_pressed(context.callback(move |_| Message::PaneFocus(id)))
                .content(
                    StackPanel::new().spacing(12.0).children((
                        TextBlock::new().text(format!("Unable to render terminal: {reason}")),
                        Button::new()
                            .on_click(context.callback(move |_| Message::RetryRender(id)))
                            .content(TextBlock::new().text("Retry rendering")),
                    )),
                ),
        };
        #[cfg(feature = "ui-smoke")]
        let content = pane_drop::with_fixture(content, drop_target, context);
        let active = self.tabs.active_id() == Some(id);
        let content = if let Some(browser) =
            session_browser::discovery_view(id, tab, active, &self.keyboard, context)
        {
            // Keep the terminal surface mounted to measure this leaf. Every
            // neighbor in a split uses its dimensions for accessibility and IME.
            let browser = Border::new()
                .background(Color::rgb(14, 16, 20))
                .content(browser);
            let mut layers = vec![
                KeyedView::new("canvas", content),
                KeyedView::new("browser", browser),
            ];
            if !active {
                // A click anywhere in an inactive browser, including over the
                // search field, focuses its pane before text can reach a sibling.
                layers.push(KeyedView::new(
                    "activate",
                    Button::new()
                        .automation_name("Activate remote session browser")
                        .horizontal_alignment(HorizontalAlignment::Stretch)
                        .vertical_alignment(VerticalAlignment::Stretch)
                        .opacity(0.0)
                        .on_click(context.callback(move |_| Message::PaneFocus(id))),
                ));
            }
            Grid::new().keyed_children(layers)
        } else {
            content
        };
        let instructions = self
            .copy_mode
            .borrow()
            .as_ref()
            .filter(|m| m.pane == id)
            .map(|m| m.instructions().to_owned())
            .or_else(|| {
                self.hints
                    .borrow()
                    .as_ref()
                    .filter(|h| h.pane == id)
                    .map(|h| h.instructions())
            });
        let content = if let Some(instructions) = instructions {
            Grid::new().children((
                content,
                Border::new()
                    .background(Color::rgb(28, 34, 41))
                    .padding(4.0)
                    .vertical_alignment(VerticalAlignment::Bottom)
                    .content(
                        TextBlock::new()
                            .text(instructions)
                            .text_wrapping(TextWrapping::Wrap),
                    ),
            ))
        } else {
            content
        };
        let mut layers = vec![KeyedView::new("pane-content", content)];
        if show_scrollbar
            && self
                .appearance
                .read()
                .unwrap()
                .interaction
                .sticky_command_context
        {
            let frame = tab.session.frames.load();
            if let Some(command) = &frame.command_context
                && let Some(target) = frame.command_context_target()
            {
                layers.push(KeyedView::new(
                    "command-context",
                    Button::new()
                        .automation_name(format!("Jump to command output · {}", command.label))
                        .horizontal_alignment(HorizontalAlignment::Stretch)
                        .horizontal_content_alignment(HorizontalAlignment::Left)
                        .vertical_alignment(VerticalAlignment::Top)
                        .height(28.0)
                        .margin(Thickness::new(4.0, 4.0, 18.0, 0.0))
                        .on_click(
                            context.callback(move |_| Message::JumpToCommandContext(id, target)),
                        )
                        .content(
                            TextBlock::new()
                                .font_size(12.0)
                                .text_trimming(TextTrimming::CharacterEllipsis)
                                .text(format!("↑  {}", command.label)),
                        ),
                ));
            }
        }
        let content = Grid::new().keyed_children(layers);
        if decorate {
            let color = if self.tabs.active_id() == Some(id) {
                Color::rgb(104, 166, 213)
            } else {
                Color::rgb(55, 60, 64)
            };
            Border::new()
                .border_thickness(1.0)
                .border_brush(color)
                .content(content)
        } else {
            content
        }
    }

    pub(super) fn layout_view(
        &self,
        layout: &Layout,
        path: &mut Vec<bool>,
        context: &ViewContext<Self>,
    ) -> View {
        match layout {
            Layout::Leaf(id) => match self.tabs.get(*id) {
                Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) => self.pane_view(*id, tab, !path.is_empty(), context),
                _ => Border::new().into(),
            },
            Layout::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                path.push(false);
                let first = self.layout_view(first, path, context);
                path.pop();
                path.push(true);
                let second = self.layout_view(second, path, context);
                path.pop();
                let position = |axis, p: PointerEventInfo| {
                    if axis == Axis::Columns {
                        p.window_x
                    } else {
                        p.window_y
                    }
                };
                let axis = *axis;
                let epoch = self.dividers.borrow().epoch;
                let start_path = path.clone();
                let drag_path = path.clone();
                let release_path = path.clone();
                let lost_path = path.clone();
                let sender = context.sender();
                let dividers = self.dividers.clone();
                let divider = Border::new()
                    .background(Color::rgb(72, 73, 75))
                    .capture_pointer_on_press(true)
                    .on_pointer_pressed(context.callback(move |p: PointerEventInfo| {
                        if p.is_left_button_pressed && p.capture_succeeded {
                            Message::DividerPress(epoch, start_path.clone(), position(axis, p))
                        } else {
                            Message::DividerEnd(epoch, start_path.clone())
                        }
                    }))
                    .on_pointer_moved(move |p: PointerEventInfo| {
                        if let Some(drag) = dividers.borrow().active(epoch, &drag_path)
                            && p.is_left_button_pressed
                        {
                            sender.send(Message::PaneRatio(
                                epoch,
                                drag_path.clone(),
                                drag.ratio(position(axis, p)),
                            ));
                        }
                    })
                    .on_pointer_released(
                        context.callback(move |_| Message::DividerEnd(epoch, release_path.clone())),
                    )
                    .on_pointer_capture_lost(
                        context.callback(move |_| Message::DividerEnd(epoch, lost_path.clone())),
                    );
                let defs = [
                    GridLength::Star(*ratio),
                    GridLength::Pixel(DIVIDER),
                    GridLength::Star(1.0 - ratio),
                ];
                let grid = if axis == Axis::Columns {
                    Grid::new().columns(defs)
                } else {
                    Grid::new().rows(defs)
                };
                let place = |index| {
                    if axis == Axis::Columns {
                        Border::new().grid_column(index)
                    } else {
                        Border::new().grid_row(index)
                    }
                };
                grid.keyed_children([
                    KeyedView::new("first", place(0).content(first)),
                    KeyedView::new("divider", place(1).content(divider)),
                    KeyedView::new("second", place(2).content(second)),
                ])
            }
        }
    }

    pub(super) fn layout_size(&self, layout: &Layout) -> Option<(f64, f64)> {
        match layout {
            Layout::Leaf(id) => {
                let Page::Terminal(tab) = &self.tabs.get(*id)?.page else {
                    return None;
                };
                let (w, h, _) = tab.surface.area.get()?;
                Some((f64::from(w) + 2.0, f64::from(h) + 2.0))
            }
            Layout::Split {
                axis,
                first,
                second,
                ..
            } => {
                let (aw, ah) = self.layout_size(first)?;
                let (bw, bh) = self.layout_size(second)?;
                Some(match axis {
                    Axis::Columns => (aw + DIVIDER + bw, ah.max(bh)),
                    Axis::Rows => (aw.max(bw), ah + DIVIDER + bh),
                })
            }
        }
    }

    fn split_scale(&self) -> Option<f32> {
        let Page::Terminal(focused) = &self.tabs.active()?.page else {
            return None;
        };
        if self.tabs.zoomed() {
            return None;
        }
        Some(focused.surface.area.get()?.2)
    }

    fn measured_extent(&self, pane: TabId, axis: Axis, scale: f32) -> Option<(f64, f64)> {
        let Page::Terminal(tab) = &self.tabs.get(pane)?.page else {
            return None;
        };
        if tab.error.is_some() {
            return None;
        }
        rotation_extent(
            tab.surface.area.get()?,
            tab.surface.metrics.get(),
            axis,
            scale,
        )
    }

    pub(super) fn available_balance(&self) -> Option<PaneBalance> {
        let target = self.tabs.pane_balance()?;
        self.measure_balance(&target)?;
        Some(target)
    }

    pub(super) fn measure_balance(&self, target: &PaneBalance) -> Option<BalancedPanes> {
        let scale = self.split_scale()?;
        target.measure(DIVIDER * f64::from(scale), |pane, axis| {
            self.measured_extent(pane, axis, scale)
        })
    }

    pub(super) fn rotation_fits(&self) -> bool {
        let Some(scale) = self.split_scale() else {
            return false;
        };
        let Some(layout) = self.tabs.active_layout() else {
            return false;
        };
        let Some(id) = self.tabs.active_id() else {
            return false;
        };
        layout.rotation_fits(id, DIVIDER * f64::from(scale), |pane, axis| {
            self.measured_extent(pane, axis, scale)
        })
    }

    pub(super) fn keyboard_resize(&mut self, direction: Direction) {
        if self.tabs.zoomed() {
            return;
        }
        let Some(id) = self.tabs.active_id() else {
            return;
        };
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get(id)
        else {
            return;
        };
        let Some(layout) = self.tabs.active_layout() else {
            return;
        };
        let metrics = tab.surface.metrics.get();
        // Each split leaf also has a one-DIP border on both sides of its canvas.
        let minimum = 2.0 + minimum_canvas(metrics, direction.axis());
        let edit = layout.keyboard_resize(id, direction, minimum, DIVIDER, |node| {
            let (w, h) = self.layout_size(node)?;
            Some(match direction.axis() {
                Axis::Columns => w,
                Axis::Rows => h,
            })
        });
        if let Some((path, ratio)) = edit {
            self.tabs.set_ratio(&path, ratio);
        }
    }

    pub(super) fn begin_divider(&self, epoch: u64, path: &[bool], position: f64) {
        self.dividers.borrow_mut().begin(epoch, || {
            let mut node = self.tabs.active_layout()?;
            for &side in path {
                let Layout::Split { first, second, .. } = node else {
                    return None;
                };
                node = if side { second } else { first };
            }
            let Layout::Split { axis, ratio, .. } = node else {
                return None;
            };
            let (w, h) = self.layout_size(node)?;
            let extent = if *axis == Axis::Columns { w } else { h } - DIVIDER;
            (extent > 0.0).then(|| DividerDrag {
                path: path.to_vec(),
                origin: position - extent * ratio,
                extent,
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stationary_cursor_uses_final_canvas_bounds_after_every_rotated_leaf_is_measured() {
        let left = Rc::new(Cell::new(Some((98.0, 98.0, 2.0))));
        let right = Rc::new(Cell::new(left.get()));
        let mut geometry = PaneGeometry {
            layout: Layout::Split {
                axis: Axis::Columns,
                ratio: 0.5,
                first: Box::new(Layout::Leaf(1)),
                second: Box::new(Layout::Leaf(2)),
            },
            focused: 1,
            areas: BTreeMap::from([(1, left.clone()), (2, right.clone())]),
        };
        let point = (150, 200);
        let height = 260;
        assert!(RotationHover::capture(&geometry, &[1, 2], 2, point, height).is_none());
        let pending = RotationHover::capture(&geometry, &[1, 2], 1, point, height).unwrap();
        let same_point = (150, 100);
        let same_pane = RotationHover::capture(&geometry, &[1, 2], 1, same_point, height).unwrap();
        let Layout::Split { axis, .. } = &mut geometry.layout else {
            unreachable!()
        };
        *axis = Axis::Rows;
        left.set(Some((204.0, 45.0, 2.0)));
        assert!(!pending.measured(&geometry));
        right.set(left.get());
        assert!(pending.measured(&geometry));
        assert_eq!(pending.hit(&geometry, Some(point), height), Some(2));
        assert_eq!(same_pane.hit(&geometry, Some(same_point), height), Some(1));
        assert_eq!(pending.hit(&geometry, Some(same_point), height), None);
        assert_eq!(pending.hit(&geometry, None, height), None);
        // Balancing invalidates all measurements; an unchanged-size sibling
        // can still be under the pointer after its origin moves.
        let mut balanced = pending;
        balanced.remeasuring = true;
        right.set(None);
        assert!(!balanced.measured(&geometry));
        right.set(Some((98.0, 98.0, 2.0)));
        assert!(balanced.measured(&geometry));
        right.set(left.get());
        for outside in [(150, 35), (150, 158), (420, 200)] {
            assert!(
                !geometry.contains(1, outside, height) && !geometry.contains(2, outside, height)
            );
        }
    }

    #[test]
    fn rotation_size_uses_cell_padding_footer_and_decorations_in_device_pixels() {
        let area = (160.0, 120.0, 1.25);
        let metrics = Metrics {
            cell_width: 12.0,
            cell_height: 20.0,
            scale: 1.25,
            ..Default::default()
        };
        assert_eq!(
            rotation_extent(area, metrics, Axis::Columns, 1.25),
            Some((202.0, 49.0))
        );
        assert_eq!(
            rotation_extent(area, metrics, Axis::Rows, 1.25),
            Some((152.0, 92.0))
        );
        assert_eq!(rotation_extent(area, metrics, Axis::Rows, 1.5), None);
        assert_eq!(
            rotation_extent((160.0, 0.0, 1.25), metrics, Axis::Rows, 1.25),
            None
        );
    }

    #[test]
    fn a_new_transport_ignores_an_old_drag_until_a_fresh_press() {
        let event = |kind, button| MouseEvent {
            kind,
            button,
            x: 3,
            y: 1,
            x_pixel_offset: 0,
            y_pixel_offset: 0,
            modifiers: KeyModifiers::NONE,
        };
        let mut remote = RemoteMouse::default();
        let drag = event(MouseEventKind::Move, MouseButton::Left);
        let release = event(MouseEventKind::Release, MouseButton::None);
        assert!(remote.event(drag).is_none());
        assert!(remote.event(release).is_none());
        assert!(
            remote
                .event(event(MouseEventKind::Move, MouseButton::None))
                .is_some()
        );
        assert!(
            remote
                .event(event(MouseEventKind::Press, MouseButton::Left))
                .is_some()
        );
        assert!(remote.event(drag).is_some());
        assert_eq!(remote.event(release).unwrap().button, MouseButton::Left);
    }

    #[test]
    fn dragging_uses_window_coordinates_and_requires_same_view_and_divider() {
        let drag = DividerDrag {
            path: vec![true],
            origin: 200.0,
            extent: 400.0,
        };
        assert_eq!(drag.ratio(450.0), 0.625);
        assert_eq!(drag.ratio(120.0), 0.1);
        assert_eq!(drag.ratio(999.0), 0.9);
        let state = DividerState {
            epoch: 7,
            drag: Some(drag),
        };
        assert!(state.dragging(7, &[true]));
        assert!(!state.dragging(8, &[true]));
        assert!(!state.dragging(7, &[false]));
    }

    #[test]
    fn rotation_rejects_old_press_ratio_and_capture_loss_at_the_same_divider_path() {
        let mut state = DividerState::default();
        let path = [false];
        let make_drag = || {
            Some(DividerDrag {
                path: path.to_vec(),
                origin: 20.0,
                extent: 120.0,
            })
        };
        let old = state.epoch;
        state.begin(old, make_drag);
        state.invalidate();
        state.begin(old, || panic!("old axis used to measure a new drag"));
        assert!(!state.dragging(old, &path));
        let new = state.epoch;
        state.begin(new, make_drag);
        state.begin(old, || panic!("old press replaced the new drag"));
        state.end(old, &path);
        assert!(!state.dragging(old, &path));
        assert_eq!(state.active(new, &path).unwrap().ratio(80.0), 0.5);
        state.end(new, &path);
        assert!(!state.dragging(new, &path));
    }
}
