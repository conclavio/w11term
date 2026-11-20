use super::*;
use w11term::control::Request;

thread_local! {
    static PAINT_COUNTS: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
}

impl TerminalApp {
    fn split_lab(&self, entry: TabId) -> &LiveTab {
        match &self.tabs.get(entry).expect("smoke pane").page {
            Page::Terminal(tab) if matches!(tab.source, Source::Demo) => tab,
            _ => panic!("split smoke requires local labs"),
        }
    }

    fn split_smoke_grid(&self, entry: TabId) -> (usize, usize) {
        let tab = self.split_lab(entry);
        let (width, height, _) = tab.surface.area.get().expect("canvas has painted");
        assert!(width > 0.0 && height > 0.0);
        let measured = tab.surface.metrics.get();
        let frame = tab.session.frames.load();
        assert_eq!(
            (frame.cols, frame.size.rows),
            (measured.cols, measured.rows)
        );
        (measured.cols, measured.rows)
    }

    pub(super) fn split_smoke_step(&mut self, step: u8, context: &ComponentContext<Self>) {
        eprintln!("Split UI smoke step {step}");
        match step {
            0 => {
                assert_eq!(self.tabs.entries().len(), 1);
                let left = self.split_lab(1);
                left.session
                    .input
                    .send(Command::Text(format!("left-{}\r", left.session.pane_id)))
                    .unwrap();
                self.update(Message::SplitPane(Axis::Columns, None), context);
            }
            1 => {
                assert_eq!(self.tabs.active_id(), Some(2));
                let right = self.split_lab(2);
                right
                    .session
                    .input
                    .send(Command::Text(format!("right-{}\r", right.session.pane_id)))
                    .unwrap();
                let epoch = self.watch_epoch;
                self.update(Message::DividerPress(epoch, vec![], 3.0), context);
                self.update(Message::PaneRatio(epoch, vec![], 0.65), context);
                self.update(Message::DividerEnd(epoch, vec![]), context);
            }
            2 => {
                assert!(self.tabs.visible(1) && self.tabs.visible(2));
                let (left_cols, left_rows) = self.split_smoke_grid(1);
                let (right_cols, right_rows) = self.split_smoke_grid(2);
                assert!(left_cols > right_cols && right_cols > 4);
                assert_eq!(left_rows, right_rows);
                let left = self.split_lab(1).session.pane_id;
                let right = self.split_lab(2).session.pane_id;
                // A resize travels back through the UI. Keep its message loop free.
                context.spawn_background(move |_| {
                    let result = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap()
                        .block_on(check_split_control(left, right, left_cols - 3, left_rows));
                    match result {
                        Ok(()) => Message::Smoke(3),
                        Err(error) => {
                            eprintln!("Split control smoke failed: {error:#}");
                            Message::Smoke(u8::MAX)
                        }
                    }
                });
                return;
            }
            3 => {
                assert_eq!(self.tabs.active_id(), Some(2));
                self.update(
                    Message::Shortcut(Shortcut::FocusPane(Direction::Left)),
                    context,
                );
                assert_eq!(self.tabs.active_id(), Some(1));
                self.update(Message::Shortcut(Shortcut::ZoomPane), context);
            }
            4 => {
                assert!(self.tabs.zoomed() && self.tabs.visible(1) && !self.tabs.visible(2));
                let (zoomed, _) = self.split_smoke_grid(1);
                assert!(zoomed > self.split_lab(2).session.frames.load().cols);
                self.update(Message::Shortcut(Shortcut::ZoomPane), context);
                self.update(Message::Shortcut(Shortcut::SplitBelow), context);
            }
            5 => {
                assert_eq!(self.tabs.active_id(), Some(3));
                assert_eq!(self.tabs.headers().len(), 1);
                let left = self.split_smoke_grid(1);
                let right = self.split_smoke_grid(2);
                let below = self.split_smoke_grid(3);
                assert_eq!(left.0, below.0);
                assert!(left.1 < right.1 && below.1 < right.1);
                self.update(Message::Shortcut(Shortcut::ClosePane), context);
                assert!(self.tabs.get(3).is_none());
                self.open("Background lab".into(), Source::Demo, context);
            }
            6 => {
                assert_eq!(self.tabs.active_id(), Some(4));
                assert!(!self.tabs.visible(1) && !self.tabs.visible(2));
                self.update(Message::Shortcut(Shortcut::Select(0)), context);
                assert!(self.tabs.visible(1) && self.tabs.visible(2));
            }
            7 => {
                for (entry, prefix) in [(1, "left"), (2, "right")] {
                    let tab = self.split_lab(entry);
                    let id = tab.session.pane_id;
                    assert_eq!(self.tab_for_pane(id), Some(entry));
                    let marker = format!("{prefix}-{id}");
                    assert!(
                        tab.session
                            .frames
                            .load()
                            .rows
                            .iter()
                            .any(|line| line.as_str().contains(&marker))
                    );
                }
                let left = self.split_lab(1);
                let right = self.split_lab(2);
                PAINT_COUNTS.set((
                    left.surface.paint_count.get(),
                    right.surface.paint_count.get(),
                ));
                right
                    .session
                    .input
                    .send(Command::Text("fresh-right-frame".into()))
                    .unwrap();
            }
            8 => {
                let (left_before, right_before) = PAINT_COUNTS.get();
                assert_eq!(
                    self.split_lab(1).surface.paint_count.get(),
                    left_before,
                    "idle sibling repainted"
                );
                assert!(
                    self.split_lab(2).surface.paint_count.get() > right_before,
                    "visible pane did not repaint"
                );
                let stable_id = self.split_lab(2).session.pane_id;
                let old_group = self.tabs.group_id(2);
                self.update(Message::PaneFocus(2), context);
                self.update(Message::MovePaneToTab, context);
                assert_ne!(self.tabs.group_id(2), old_group);
                assert_eq!(self.tabs.active_id(), Some(2));
                assert_eq!(self.tab_for_pane(stable_id), Some(2));
                self.close_tab(4, context);
                self.close_tab(2, context);
                self.close_tab(1, context);
                eprintln!("Split pane smoke passed");
                return;
            }
            _ => panic!("split UI smoke failed"),
        }
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(600));
            Message::Smoke(step + 1)
        });
    }
}

async fn split_control(request: Request) -> anyhow::Result<serde_json::Value> {
    let reply = w11term::control::call(Some(std::process::id()), &request).await?;
    anyhow::ensure!(reply.ok, "{}", reply.error.unwrap_or_default());
    reply
        .result
        .ok_or_else(|| anyhow::anyhow!("missing control result"))
}

async fn check_split_control(
    left: u64,
    right: u64,
    cols: usize,
    rows: usize,
) -> anyhow::Result<()> {
    let listing = split_control(Request::List).await?;
    let sessions = listing["sessions"].as_array().unwrap();
    let left_entry = sessions.iter().find(|s| s["id"] == left).unwrap();
    let right_entry = sessions.iter().find(|s| s["id"] == right).unwrap();
    anyhow::ensure!(left_entry["tab_id"] == right_entry["tab_id"]);
    anyhow::ensure!(left_entry["visible"] == true && right_entry["visible"] == true);
    anyhow::ensure!(left_entry["active"] == false && right_entry["active"] == true);

    split_control(Request::Resize {
        pane: left,
        cols,
        rows,
    })
    .await?;
    let left_text = split_control(Request::GetText { pane: left }).await?;
    anyhow::ensure!(left_text["cols"] == cols && left_text["rows"] == rows);
    let left_marker = format!("left-{left}");
    let right_marker = format!("right-{right}");
    let text = left_text["text"].as_str().unwrap();
    anyhow::ensure!(text.contains(&left_marker) && !text.contains(&right_marker));

    let directory = tempfile::tempdir()?;
    let path = directory.path().join("unfocused-pane.png");
    let capture = split_control(Request::Screenshot {
        pane: left,
        path: path.clone(),
    })
    .await?;
    let (width, height) = image::image_dimensions(path)?;
    anyhow::ensure!(width > 0 && height > 0);
    anyhow::ensure!(capture["width"] == width && capture["height"] == height);
    Ok(())
}
