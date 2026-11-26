use super::*;
use std::path::Path;
impl TerminalApp {
    /// Read back the production Direct2D rail, which is outside the public
    /// terminal screenshot's cell rectangle. Only the ui-smoke fixture calls this.
    fn capture_rail(&self, pane: u64, path: &Path) -> anyhow::Result<()> {
        let tab = self
            .tabs
            .entries()
            .iter()
            .find_map(|entry| match &entry.value.page {
                Page::Terminal(tab) if tab.session.pane_id == pane => Some(tab),
                _ => None,
            })
            .expect("fixture pane");
        let (width, height, scale) = tab.surface.area.get().expect("measured fixture pane");
        let metrics = tab.surface.metrics.get();
        let frame = tab.session.frames.load();
        let pixels = |dips: f32| (dips * scale).round() as u32;
        let (w, h) = (pixels(width), pixels(height));
        let device = windows_canvas::GpuDevice::new_or_warp()?;
        let target = device.create_render_target(w, h)?;
        target.draw(|ctx| {
            crate::native_drawing::NativeDrawing::new(ctx)?.set_dpi(96.0 * scale);
            ctx.clear(windows_canvas::ColorF::BLACK);
            tab.surface
                .scrollbar
                .paint(ctx, &frame, width, height, metrics, &mut None)
        })?;
        let mut bgra = target.read_pixels()?;
        for pixel in bgra.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        let image = image::RgbaImage::from_raw(w, h, bgra).expect("render-target dimensions");
        let left = pixels(width - PADDING);
        let top = pixels(PADDING);
        let bottom = pixels(height - STATUS_HEIGHT - PADDING);
        image::imageops::crop_imm(&image, left, top, w - left, bottom - top)
            .to_image()
            .save(path)?;
        Ok(())
    }

    pub(super) fn scrollbar_pointer_smoke(
        &mut self,
        state: &Path,
        context: &ComponentContext<Self>,
    ) {
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(80));
            Message::Smoke(0)
        });
        let capture = state.join("rail-capture");
        if capture.is_file() {
            let pane = std::fs::read_to_string(&capture).unwrap().parse().unwrap();
            self.capture_rail(pane, &state.join("rail.png")).unwrap();
            std::fs::remove_file(capture).unwrap();
        }
        let request = state.join("rail-pointer");
        if request.is_file() {
            let gesture = std::fs::read_to_string(&request).unwrap();
            let (id, gesture) = gesture.split_once(' ').unwrap();
            let id = id.parse().unwrap();
            let pointer = PointerEventInfo {
                x: f64::from(PADDING) + 1.0,
                y: f64::from(PADDING) + 1.0,
                is_left_button_pressed: gesture == "press",
                capture_succeeded: true,
                ..Default::default()
            };
            let boundary = match gesture {
                "enter" => Some(Message::HoverPane(id)),
                "leave" => Some(Message::PointerExit(id)),
                "move" => Some(Message::Pointer(id, pointer, MouseEventKind::Move)),
                "press" => Some(Message::Pointer(id, pointer, MouseEventKind::Press)),
                "release" => Some(Message::Pointer(id, pointer, MouseEventKind::Release)),
                _ => None,
            };
            if let Some(message) = boundary {
                self.update(message, context);
            } else {
                let Page::Terminal(tab) = &self.tabs.get(id).unwrap().page else {
                    panic!("scrollbar probe must target a fixture pane")
                };
                let (x, press, drag) = if let Some(pixel) = gesture.strip_prefix("failure:") {
                    let (width, _, scale) = tab.surface.area.get().unwrap();
                    (
                        width - PADDING + 1.5,
                        ((PADDING * scale).round() + pixel.parse::<f32>().unwrap()) / scale,
                        None,
                    )
                } else {
                    // UI Automation can observe scroll changes before the swap chain
                    // paints the thumb. Wait for the rail that will receive the press.
                    let Some((track, top, bottom)) = tab
                        .surface
                        .scrollbar
                        .painted_track(&tab.session.frames.load())
                    else {
                        return;
                    };
                    match gesture {
                        "drag-up" => (track, bottom, Some(top)),
                        "drag-down" => (track, top, Some(bottom)),
                        "page-up" => (track, top, None),
                        _ => panic!("unknown scrollbar probe"),
                    }
                };
                let event = |y: f32, held| PointerEventInfo {
                    x: f64::from(x),
                    y: y as f64,
                    is_left_button_pressed: held,
                    capture_succeeded: true,
                    ..Default::default()
                };
                // Only the specified pane receives Reactor's normal pointer messages.
                self.update(
                    Message::Pointer(id, event(press, true), MouseEventKind::Press),
                    context,
                );
                if let Some(y) = drag {
                    self.update(
                        Message::Pointer(id, event(y, true), MouseEventKind::Move),
                        context,
                    );
                }
                self.update(
                    Message::Pointer(
                        id,
                        event(drag.unwrap_or(press), false),
                        MouseEventKind::Release,
                    ),
                    context,
                );
            }
            std::fs::remove_file(request).unwrap();
        }
    }

    pub(super) fn word_selection_smoke(&mut self, state: &Path, context: &ComponentContext<Self>) {
        let request = state.join("double-click");
        if request.is_file() {
            let needle = std::fs::read_to_string(&request).unwrap();
            let id = self.tabs.active_id().unwrap();
            let Page::Terminal(tab) = &self.tabs.get(id).unwrap().page else {
                panic!("double-click requested outside fixture pane");
            };
            let frame = tab.session.frames.load();
            let (row, col) = frame
                .rows
                .iter()
                .enumerate()
                .find_map(|(row, line)| {
                    // The fixture puts the ASCII path on its own row.
                    line.as_str().find(&needle).map(|col| (row, col))
                })
                .expect("requested text on the fixture's SSH pane");
            let metrics = tab.surface.metrics.get();
            let event = |pressed| PointerEventInfo {
                x: (PADDING + (col as f32 + 0.5) * metrics.cell_width) as f64,
                y: (PADDING + (row as f32 + 0.5) * metrics.cell_height) as f64,
                is_left_button_pressed: pressed,
                ..Default::default()
            };
            // Use the same pane-local callback as native pointer input. No
            // system cursor movement or input to another window is involved.
            for _ in 0..2 {
                self.update(
                    Message::Pointer(id, event(true), MouseEventKind::Press),
                    context,
                );
                self.update(
                    Message::Pointer(id, event(false), MouseEventKind::Release),
                    context,
                );
            }
            std::fs::remove_file(request).unwrap();
        }
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(80));
            Message::Smoke(0)
        });
    }

    pub(super) fn accessibility_smoke(
        &mut self,
        step: u8,
        state: &Path,
        context: &ComponentContext<Self>,
    ) {
        if state.join("finish").exists() {
            let _ = context.window().request_close();
            return;
        }
        let next = match step {
            0 => {
                let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.active()
                else {
                    panic!("fixture lab")
                };
                assert!(matches!(tab.source, Source::Demo));
                self.split_pane(Axis::Columns, None, context);
                1
            }
            1 | 3 => {
                let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.active_id().and_then(|id| self.tabs.get_mut(id))
                else {
                    panic!("active fixture pane")
                };
                let frame = tab.session.frames.load();
                let marker = if step == 1 {
                    "right-reader"
                } else {
                    "left-reader"
                };
                if let Some((row, col)) = frame
                    .rows
                    .iter()
                    .enumerate()
                    .find_map(|(i, line)| line.as_str().find(marker).map(|c| (i, c)))
                    && let Some(hwnd) = crate::input::window()
                {
                    if step == 1 {
                        let p = Point {
                            row: frame.first_stable_row + row as isize,
                            col,
                        };
                        let mut s = Selection::new(p, false, frame.alternate);
                        s.update(Point {
                            row: p.row,
                            col: p.col + 4,
                        });
                        s.dragging = false;
                        s.observe(&frame);
                        *tab.selection.borrow_mut() = Some(s);
                    }
                    std::fs::write(
                        state.join(marker),
                        format!("{} {}", hwnd.0 as usize, tab.session.pane_id),
                    )
                    .unwrap();
                    step + 1
                } else {
                    if tab
                        .last_size
                        .get()
                        .is_some_and(|s| s.cols == frame.cols && s.rows == frame.rows.len())
                    {
                        let links = concat!(
                            "\r\n\x1b]8;;https://osc.example/test\x1b\\",
                            "labelled\x1b]8;;\x1b\\",
                            "\r\nhttps://text.example/path "
                        );
                        let before_seam = "x".repeat(frame.cols.saturating_sub(4));
                        tab.session
                            .input
                            .send(Command::Paste(format!(
                                "\x1b[2J\x1b[H{marker} Aאב12ג{links}\r\n{before_seam}seamless界\r\nhard-break"
                            )))
                            .unwrap();
                    }
                    step
                }
            }
            2 if state.join("switch").exists() => {
                self.update(Message::PaneNeighbor(Direction::Left), context);
                3
            }
            2 if state.join("pointer").exists() => {
                // A pointer event addressed to this fixture only, through the same
                // update path as Reactor's native pane-local pointer callback.
                let path = state.join("pointer");
                let request = std::fs::read_to_string(&path).unwrap();
                std::fs::remove_file(path).unwrap();
                if !self.show_inspector {
                    self.update(Message::ToggleInspector, context);
                }
                let focused = self.tabs.active_id().unwrap();
                let (id, row) = match request.as_str() {
                    "plain" => (focused, 0),
                    "osc" => (focused, 1),
                    "detected" => (focused, 2),
                    "neighbor" => (
                        self.tabs
                            .entries()
                            .iter()
                            .find(|t| t.id != focused)
                            .unwrap()
                            .id,
                        0,
                    ),
                    _ => panic!("unexpected pointer probe"),
                };
                let Page::Terminal(live) = &self.tabs.get(id).unwrap().page else {
                    panic!("missing fixture pane")
                };
                let metrics = live.surface.metrics.get();
                let pointer = PointerEventInfo {
                    x: (PADDING + 0.5 * metrics.cell_width) as f64,
                    y: (PADDING + (row as f32 + 0.5) * metrics.cell_height) as f64,
                    ..Default::default()
                };
                self.update(Message::Pointer(id, pointer, MouseEventKind::Move), context);
                assert_eq!(self.tabs.active_id(), Some(focused));
                2
            }
            2 if state.join("append").exists() || state.join("append-neighbor").exists() => {
                let active = self.tabs.active_id().unwrap();
                let neighbor = state.join("append-neighbor");
                let (path, pane) = if neighbor.is_file() {
                    (
                        neighbor,
                        self.tabs
                            .entries()
                            .iter()
                            .find(|entry| entry.id != active)
                            .unwrap()
                            .id,
                    )
                } else {
                    (state.join("append"), active)
                };
                let Page::Terminal(tab) = &self.tabs.get(pane).unwrap().page else {
                    panic!("missing test pane");
                };
                let output = std::fs::read_to_string(&path).unwrap();
                std::fs::remove_file(path).unwrap();
                let command = if output.is_empty() {
                    Command::Text("new-output".into())
                } else {
                    Command::Paste(output)
                };
                tab.session.input.send(command).unwrap();
                2
            }
            _ => step,
        };
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(100));
            Message::Smoke(next)
        });
    }
}
