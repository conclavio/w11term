use super::*;
use tokio::sync::oneshot;

type Reply = oneshot::Sender<Result<serde_json::Value, String>>;

pub struct Completion {
    pane: u64,
    target: (usize, usize),
    visible: bool,
    window_epoch: Option<u64>,
    deadline: Instant,
    reply: Reply,
}

impl Completion {
    fn schedule(self, context: &ComponentContext<TerminalApp>) {
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(50));
            Message::ControlResizeDone(self)
        });
    }

    pub(super) fn observe(self, app: &TerminalApp, context: &ComponentContext<TerminalApp>) {
        if self.reply.is_closed() {
            return;
        }
        let Some(id) = app.tab_for_pane(self.pane) else {
            let _ = self.reply.send(Err("pane was closed or moved".into()));
            return;
        };
        let Some(Page::Terminal(pane)) = app.tabs.get(id).map(|tab| &tab.page) else {
            let _ = self.reply.send(Err("pane was closed".into()));
            return;
        };
        if self.visible != app.tabs.visible(id)
            || self
                .window_epoch
                .is_some_and(|epoch| epoch != app.initial_grid.epoch())
        {
            let _ = self
                .reply
                .send(Err("pane resize was interrupted by a layout change".into()));
            return;
        }

        let frame = pane.session.frames.load();
        let actual = (frame.cols, frame.rows.len());
        let measured = pane.last_size.get();
        let waiting_for_window = self.window_epoch.is_some() && app.initial_grid.get().is_some();
        if actual == self.target
            && (!self.visible || measured == Some(frame.size))
            && !waiting_for_window
        {
            let (cols, rows) = self.target;
            let _ = self.reply.send(Ok(
                serde_json::json!({"pane":self.pane,"cols":cols,"rows":rows}),
            ));
        } else if Instant::now() < self.deadline {
            self.schedule(context);
        } else {
            if self
                .window_epoch
                .is_some_and(|epoch| app.initial_grid.expire(epoch))
            {
                app.invalidator.invalidate();
            }
            let (cols, rows) = measured
                .filter(|_| self.visible)
                .map(|size| (size.cols, size.rows))
                .unwrap_or(actual);
            let _ = self.reply.send(Err(format!(
                "requested grid did not settle in the current window/display; actual {cols}x{rows}"
            )));
        }
    }
}

impl TerminalApp {
    pub(super) fn control_resize(
        &mut self,
        pane: u64,
        cols: usize,
        rows: usize,
        reply: Reply,
        context: &ComponentContext<Self>,
    ) {
        let mut prepare = || -> anyhow::Result<(bool, Option<u64>)> {
            let id = self
                .tab_for_pane(pane)
                .with_context(|| format!("unknown terminal pane {pane}"))?;
            let Some(Page::Terminal(tab)) = self.tabs.get(id).map(|tab| &tab.page) else {
                anyhow::bail!("unknown terminal pane {pane}");
            };
            anyhow::ensure!(
                !matches!(tab.source, Source::TimedReplay(_) | Source::PausedReplay(_)),
                "The recording controls this pane’s grid during timed replay"
            );
            let visible = self.tabs.visible(id);
            let split = visible
                && matches!(
                    self.tabs.active_layout(),
                    Some(w11term::pane_layout::Layout::Split { .. })
                );
            let epoch = if split {
                anyhow::ensure!(
                    !self.tabs.zoomed(),
                    "Unzoom the pane before resizing its split grid"
                );
                self.resize_split(id, cols, rows)?;
                None
            } else if visible {
                anyhow::ensure!(
                    !self.windows.borrow().fullscreen(),
                    "Exit fullscreen before resizing the active window grid"
                );
                let epoch = self.initial_grid.set_for_pane(id, (cols, rows));
                tab.surface.invalidator.invalidate();
                Some(epoch)
            } else {
                // A background pane has no current viewport. Use its most recent
                // measured cell size, or the active grid before it has been drawn.
                let metrics = if tab.surface.area.get().is_some() {
                    tab.surface.metrics.get()
                } else {
                    self.metrics.get()
                };
                tab.session
                    .input
                    .send(Command::Resize(TerminalSize {
                        cols,
                        rows,
                        pixel_width: (cols as f32 * metrics.cell_width * metrics.scale).round()
                            as usize,
                        pixel_height: (rows as f32 * metrics.cell_height * metrics.scale).round()
                            as usize,
                        dpi: (96.0 * metrics.scale).round() as u32,
                    }))
                    .map_err(|_| anyhow::anyhow!("pane was closed"))?;
                // Remount must send the visible grid even if it matches the
                // same viewport the pane had before its background PTY resize.
                tab.last_size.set(None);
                None
            };
            Ok((visible, epoch))
        };
        match prepare() {
            Ok((visible, window_epoch)) => Completion {
                pane,
                target: (cols, rows),
                visible,
                window_epoch,
                deadline: Instant::now() + Duration::from_secs(2),
                reply,
            }
            .schedule(context),
            Err(error) => {
                let _ = reply.send(Err(error.to_string()));
            }
        }
    }
}
