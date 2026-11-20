use super::*;
use std::path::Path;
impl TerminalApp {
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
                        tab.session
                            .input
                            .send(Command::Text(format!("\x1b[2J\x1b[H{marker}")))
                            .unwrap();
                    }
                    step
                }
            }
            2 if state.join("switch").exists() => {
                self.update(Message::PaneNeighbor(Direction::Left), context);
                3
            }
            2 if state.join("append").exists() => {
                std::fs::remove_file(state.join("append")).unwrap();
                let Some(AppTab {
                    page: Page::Terminal(tab),
                    ..
                }) = self.tabs.active()
                else {
                    panic!("missing test pane")
                };
                tab.session
                    .input
                    .send(Command::Text("new-output".into()))
                    .unwrap();
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
