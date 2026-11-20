use super::*;
use w11term::pane_layout::{Axis, Layout};

const DIVIDER: f64 = 6.0;

pub(super) struct DividerDrag {
    epoch: u64,
    path: Vec<bool>,
    origin: f64,
    extent: f64,
}

impl DividerDrag {
    fn matches(&self, epoch: u64, path: &[bool]) -> bool {
        self.epoch == epoch && self.path == path
    }

    fn ratio(&self, position: f64) -> f64 {
        let min = (40.0 / self.extent).min(0.5);
        ((position - self.origin) / self.extent).clamp(min, 1.0 - min)
    }
}

#[derive(Default)]
pub(super) struct PaneSurface {
    #[cfg(feature = "ui-smoke")]
    pub(super) paint_count: Rc<Cell<u64>>,
    pub(super) metrics: Rc<Cell<Metrics>>,
    pub(super) invalidator: Invalidator,
    pub(super) area: Rc<Cell<Option<(f32, f32, f32)>>>,
    window_grid: Rc<Cell<bool>>,
    window_size: Rc<Cell<(f64, f64)>>,
    canvas: RefCell<Option<View>>,
}

impl PaneSurface {
    pub(super) fn window_hidden(&self) {
        self.window_grid.set(false);
        self.release();
    }

    pub(super) fn release(&self) {
        self.canvas.borrow_mut().take();
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

impl TerminalApp {
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
        let renderer = RefCell::new(Renderer::with_size(
            &appearance.font_family,
            appearance.ligatures,
            appearance.font_size,
        )?);
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
        let area = surface.area.clone();
        let selection = tab.selection.clone();
        let hover = tab.hover.clone();
        #[cfg(feature = "ui-smoke")]
        let paint_count = surface.paint_count.clone();
        let canvas: View = Canvas::invalidated(&surface.invalidator, move |ctx| {
            #[cfg(feature = "ui-smoke")]
            paint_count.set(paint_count.get() + 1);
            let frame = frames.load();
            let mut renderer = renderer.borrow_mut();
            let measured = renderer.draw(ctx, &frame)?;
            renderer.draw_interaction(
                ctx,
                &frame,
                selection.borrow().as_ref(),
                hover.borrow().as_ref(),
            )?;
            metrics.set(measured);
            area.set(Some((ctx.width, ctx.height, measured.scale)));
            if window_grid.get() {
                if initial_grid
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
                if let Some(((cols, rows), first)) =
                    initial_grid.correction((measured.cols, measured.rows))
                {
                    let (width, height) = window_size.get();
                    geometry_sender.send(Message::Geometry(
                        width
                            + (cols as f32 * measured.cell_width + PADDING * 2.0 - ctx.width)
                                as f64,
                        height
                            + (rows as f32 * measured.cell_height + PADDING * 2.0 + STATUS_HEIGHT
                                - ctx.height) as f64,
                        initial_grid.epoch(),
                        first,
                    ));
                    return Ok(());
                }
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
        tab.surface
            .window_grid
            .set(matches!(self.tabs.active_layout(), Some(Layout::Leaf(_))));
        tab.surface
            .window_size
            .set((self.width, self.window_height));
        let canvas = match tab.error.as_deref() {
            Some(reason) => Err(reason.to_owned()),
            None => self
                .pane_canvas(id, tab, context)
                .map_err(|error| error.to_string()),
        };
        let content = match canvas {
            Ok(canvas) => Border::new()
                .background(Color::rgb(14, 16, 20))
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
                )
                .content(canvas),
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
                let epoch = self.watch_epoch;
                let start_path = path.clone();
                let drag_path = path.clone();
                let release_path = path.clone();
                let lost_path = path.clone();
                let sender = context.sender();
                let drag = self.divider_drag.clone();
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
                        if let Some(drag) = drag.borrow().as_ref()
                            && drag.matches(epoch, &drag_path)
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

    pub(super) fn begin_divider(&self, path: &[bool], position: f64) {
        self.divider_drag.borrow_mut().take();
        let Some(mut node) = self.tabs.active_layout() else {
            return;
        };
        for &side in path {
            let Layout::Split { first, second, .. } = node else {
                return;
            };
            node = if side { second } else { first };
        }
        if let Layout::Split { axis, ratio, .. } = node
            && let Some((w, h)) = self.layout_size(node)
        {
            let extent = if *axis == Axis::Columns { w } else { h } - DIVIDER;
            if extent > 0.0 {
                *self.divider_drag.borrow_mut() = Some(DividerDrag {
                    epoch: self.watch_epoch,
                    path: path.to_vec(),
                    origin: position - extent * ratio,
                    extent,
                });
            }
        }
    }

    pub(super) fn dragging_divider(&self, epoch: u64, path: &[bool]) -> bool {
        self.divider_drag
            .borrow()
            .as_ref()
            .is_some_and(|drag| drag.matches(epoch, path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_uses_window_coordinates_and_requires_same_view_and_divider() {
        let drag = DividerDrag {
            epoch: 7,
            path: vec![true],
            origin: 200.0,
            extent: 400.0,
        };
        assert_eq!(drag.ratio(450.0), 0.625);
        assert_eq!(drag.ratio(120.0), 0.1);
        assert_eq!(drag.ratio(999.0), 0.9);
        assert!(drag.matches(7, &[true]));
        assert!(!drag.matches(8, &[true]));
        assert!(!drag.matches(7, &[false]));
    }
}
