use super::*;
use plot::{Plot, Range};
use profile::{Profile, Search};
use std::path::PathBuf;
use w11term::ssh::CpuProfileSummary;

mod capture;
mod plot;
mod profile;
pub(super) use capture::Connection;

pub(super) const LABEL: &str = "CPU flame graph";

pub enum Action {
    Open,
    Close,
    OpenFile,
    Export,
    Capture(capture::Action),
    Query(String),
    Match(bool),
    Zoom(usize),
    Up,
    Depth(usize),
    Hover(Option<usize>),
    Finished(Arc<()>, Result<Option<Outcome>, String>),
}

pub enum Outcome {
    Opened(Arc<Profile>, PathBuf),
    Reviewed(Box<capture::Review>),
    Destination(Box<capture::Recording>),
    Captured(Arc<Profile>, PathBuf, CpuProfileSummary),
    Exported,
}

struct Loaded {
    profile: Arc<Profile>,
    path: PathBuf,
    plot: Plot,
    capture: Option<CpuProfileSummary>,
}

#[derive(Default)]
pub(super) struct Panel {
    pub open: bool,
    pub(super) activity: Option<diagnostics::Activity>,
    loaded: Option<Loaded>,
    pending: Option<Arc<()>>,
    status: Option<String>,
    capture: Option<capture::Form>,
    zoom: usize,
    hovered: Option<usize>,
    first_depth: usize,
    query: String,
    search: Arc<Search>,
    current_match: Option<usize>,
}

fn message(action: Action) -> Message {
    Message::FlameGraph(action)
}

impl Panel {
    fn begin(&mut self, status: &str) -> Arc<()> {
        let ticket = Arc::new(());
        self.pending = Some(ticket.clone());
        self.status = Some(status.into());
        ticket
    }

    fn load(&mut self, profile: Arc<Profile>, path: PathBuf, capture: Option<CpuProfileSummary>) {
        self.zoom(0);
        self.query.clear();
        self.search = Arc::default();
        self.current_match = None;
        let plot = Plot::new(profile.clone(), self.search.clone());
        self.loaded = Some(Loaded {
            profile,
            path,
            plot,
            capture,
        });
    }

    fn visible_rows(window_height: f64) -> usize {
        (((window_height - 285.0) / f64::from(plot::ROW_HEIGHT)).floor() as usize).clamp(3, 50)
    }

    fn range(&self, profile: &Profile, window_height: f64) -> (Range, usize) {
        let root = profile.frame(self.zoom);
        let depth = root.last_depth - root.depth + 1;
        let rows = (depth - self.first_depth).min(Self::visible_rows(window_height));
        (
            Range {
                zoom: self.zoom,
                first_depth: self.first_depth,
                rows,
            },
            depth,
        )
    }

    fn zoom(&mut self, id: usize) {
        self.zoom = id;
        self.first_depth = 0;
        self.hovered = None;
    }

    fn view(
        &self,
        window_height: f64,
        active: Option<Connection>,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let send = |action: fn() -> Action| ctx.callback(move |_| message(action()));
        let idle = self.pending.is_none();
        let mut fields = vec![KeyedView::new(
            "header",
            diagnostics::header(LABEL, "Close CPU flame graph", send(|| Action::Close), ctx),
        )];
        if let Some(status) = &self.status {
            fields.push(text("status", status));
        }
        if let Some(capture) = &self.capture {
            fields.extend(capture.fields(active, idle, ctx));
        } else {
            fields.push(KeyedView::new(
                "files",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .is_enabled(idle)
                            .on_click(send(|| Action::OpenFile))
                            .content("Open folded stacks"),
                        Button::new()
                            .is_enabled(idle && self.loaded.is_some())
                            .on_click(send(|| Action::Export))
                            .content("Export SVG"),
                    )),
            ));
            fields.push(KeyedView::new(
                "capture",
                Button::new()
                    .is_enabled(idle && active.is_some())
                    .on_click(send(|| Action::Capture(capture::Action::Open)))
                    .content("Capture from SSH"),
            ));
            if let Some(loaded) = &self.loaded {
                self.graph(loaded, window_height, ctx, &mut fields);
            } else if self.status.is_none() {
                fields.push(text(
                    "empty",
                    "Open a folded stacks file or capture from a connected Linux SSH pane.",
                ));
            }
        }
        ScrollViewer::new().content(
            Border::new()
                .padding(12.0)
                .content(StackPanel::new().spacing(8.0).keyed_children(fields)),
        )
    }

    fn graph(
        &self,
        loaded: &Loaded,
        window_height: f64,
        ctx: &ViewContext<TerminalApp>,
        fields: &mut Vec<KeyedView>,
    ) {
        let profile = &loaded.profile;
        let total = profile.frame(0).samples;
        let (range, depth) = self.range(profile, window_height);
        let root = profile.frame(self.zoom);
        let focus = profile.frame(self.hovered.unwrap_or(self.zoom));
        let name = loaded
            .path
            .file_name()
            .unwrap_or(loaded.path.as_os_str())
            .to_string_lossy();
        fields.push(KeyedView::new(
            "source",
            TextBlock::new()
                .text(name)
                .text_trimming(TextTrimming::CharacterEllipsis),
        ));
        fields.push(text("total", sample_count(total)));
        let match_button = |next, label, column| {
            Button::new()
                .grid_column(column)
                .automation_name(if next {
                    "Next symbol match"
                } else {
                    "Previous symbol match"
                })
                .is_enabled(!self.search.matches.is_empty())
                .on_click(ctx.callback(move |_| message(Action::Match(next))))
                .content(label)
        };
        fields.push(KeyedView::new(
            "search",
            Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Auto, GridLength::Auto])
                .column_spacing(6.0)
                .children((
                    TextBox::new()
                        .automation_name("Search flame graph symbols")
                        .placeholder_text("Search symbols")
                        .text(&self.query)
                        .on_text_changed(ctx.callback(|query| message(Action::Query(query)))),
                    match_button(false, "Previous", 1),
                    match_button(true, "Next", 2),
                )),
        ));
        if !self.query.trim().is_empty() {
            let hits = self.search.matches.len();
            let unit = if hits == 1 { "frame" } else { "frames" };
            let cursor = self
                .current_match
                .map_or(String::new(), |index| format!(" · Match {}", index + 1));
            fields.push(text(
                "matches",
                format!(
                    "{hits} matching {unit} · {:.1}% of samples{cursor}",
                    plot::share(self.search.samples, total),
                ),
            ));
        }
        let page_at = range.first_depth.saturating_add(range.rows);
        let shallow_at = range
            .first_depth
            .saturating_sub(Self::visible_rows(window_height));
        fields.push(KeyedView::new(
            "navigation",
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children((
                    Button::new()
                        .automation_name("Zoom to caller")
                        .is_enabled(self.zoom != 0)
                        .on_click(ctx.callback(|_| message(Action::Up)))
                        .content("Up"),
                    Button::new()
                        .is_enabled(self.zoom != 0 || range.first_depth != 0)
                        .on_click(ctx.callback(|_| message(Action::Zoom(0))))
                        .content("Reset zoom"),
                )),
        ));
        if range.rows != depth {
            fields.push(KeyedView::new(
                "depth",
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new().text(format!(
                        "Showing levels {}–{page_at} of {depth}",
                        range.first_depth + 1
                    )),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(6.0)
                        .children((
                            Button::new()
                                .is_enabled(range.first_depth > 0)
                                .on_click(ctx.callback(move |_| message(Action::Depth(shallow_at))))
                                .content("Shallower"),
                            Button::new()
                                .is_enabled(page_at < depth)
                                .on_click(ctx.callback(move |_| message(Action::Depth(page_at))))
                                .content("Deeper"),
                        )),
                )),
            ));
        }

        let sender = ctx.sender();
        let hover = loaded.plot.pointer();
        let exit = loaded.plot.pointer();
        let click = loaded.plot.pointer();
        let hover_sender = sender.clone();
        fields.push(KeyedView::new(
            "plot",
            Border::new()
                .height(f64::from(range.height()))
                .background(Color::rgb(26, 26, 28))
                .automation_name("CPU flame graph chart")
                .on_pointer_moved(move |pointer: PointerEventInfo| {
                    if let Some(target) = hover(pointer.x, pointer.y, true) {
                        hover_sender.send(message(Action::Hover(target)));
                    }
                })
                .on_pointer_exited(move |_| {
                    if let Some(target) = exit(-1.0, -1.0, true) {
                        sender.send(message(Action::Hover(target)));
                    }
                })
                .on_pointer_pressed(ctx.callback(move |pointer: PointerEventInfo| {
                    if pointer.is_left_button_pressed
                        && let Some(Some(frame)) = click(pointer.x, pointer.y, false)
                    {
                        return message(Action::Zoom(frame));
                    }
                    Message::ControlIdle
                }))
                .content(loaded.plot.canvas(range, &self.search)),
        ));
        if self.zoom != 0 {
            fields.push(text("zoom", format!("Zoomed to {}", root.name)));
        }
        fields.push(text(
            "frame",
            format!(
                "{} · {} · {:.1}% of capture",
                focus.name,
                sample_count(focus.samples),
                plot::share(focus.samples, total)
            ),
        ));
        if focus.parent != 0 {
            fields.push(text(
                "caller",
                format!("Caller {}", profile.frame(focus.parent).name),
            ));
        }
        if let Some(capture) = &loaded.capture {
            if let Some(raw) = &capture.perf_data {
                fields.push(text(
                    "raw-recording",
                    format!("Raw perf recording saved to {}", raw.path.display()),
                ));
            }
            if let Some(warning) = &capture.perf_data_warning {
                fields.push(text("raw-recording-warning", warning));
            }
            if let Some(outcome) = &capture.workload {
                fields.push(text("workload-outcome", outcome.to_string()));
            }
            fields.push(text(
                "stack-resolution",
                format!("{} unresolved frames", capture.unresolved_frames),
            ));
            if !capture.diagnostics.is_empty() {
                fields.push(KeyedView::new(
                    "perf-output",
                    Expander::new().automation_name("Perf output").slots([
                        SlotView::new(ExpanderSlot::Header, TextBlock::new().text("Perf output")),
                        SlotView::new(
                            ExpanderSlot::Content,
                            TextBlock::new()
                                .text(capture.diagnostics.join("\n"))
                                .text_wrapping(TextWrapping::Wrap)
                                .is_text_selection_enabled(true),
                        ),
                    ]),
                ));
            }
        }
    }
}

fn text(key: &str, value: impl Into<String>) -> KeyedView {
    KeyedView::new(
        key,
        TextBlock::new()
            .text(value.into())
            .text_wrapping(TextWrapping::Wrap),
    )
}

fn sample_count(samples: u64) -> String {
    let unit = if samples == 1 { "sample" } else { "samples" };
    format!("{samples} {unit}")
}

impl TerminalApp {
    pub(super) fn with_flame_graph(&self, content: View, ctx: &ViewContext<Self>) -> View {
        let width = (self.width * 0.67).clamp(275.0, 1100.0);
        Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Pixel(width)])
            .keyed_children([
                KeyedView::new("terminal", Border::new().content(content)),
                KeyedView::new(
                    "flame-graph",
                    Border::new()
                        .grid_column(1)
                        .background(Color::rgb(35, 35, 35))
                        .content(self.flame_graph.view(
                            self.window_height,
                            self.profile_connection(),
                            ctx,
                        )),
                ),
            ])
    }

    pub(super) fn flame_graph_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        let panel = &mut self.flame_graph;
        match action {
            Action::Open => {
                self.command_reference.close();
                self.set_diagnostics_drawer(Some(diagnostics::Drawer::FlameGraph));
            }
            Action::Close => {
                panel.open = false;
                panel.hovered = None;
            }
            Action::OpenFile | Action::Export => {
                if panel.pending.is_some() {
                    return;
                }
                let Some(owner) = self.keyboard.hwnd().map(|window| window.0 as usize) else {
                    return;
                };
                let path = panel
                    .loaded
                    .as_ref()
                    .map_or_else(|| self.store.path().into(), |loaded| loaded.path.clone());
                let export = matches!(action, Action::Export);
                let profile = panel.loaded.as_ref().map(|loaded| loaded.profile.clone());
                let zoom = panel.zoom;
                let ticket = panel.begin(if export {
                    "Choose an SVG destination…"
                } else {
                    "Choose a folded stacks file…"
                });
                let rejected = message(Action::Finished(
                    ticket.clone(),
                    Err("File worker unavailable".into()),
                ));
                ctx.spawn_background_with_rejection(
                    move |scope| {
                        let result = (|| -> anyhow::Result<_> {
                            let Some(file) = file_picker::pick(
                                owner,
                                file_picker::Purpose::FlameGraph { path, export },
                            )?
                            else {
                                return Ok(None);
                            };
                            if scope.is_cancelled() {
                                return Ok(None);
                            }
                            if export {
                                let profile =
                                    profile.context("Open folded stacks before exporting")?;
                                plot::export(&profile, zoom, &file.path)?;
                                Ok(Some(Outcome::Exported))
                            } else {
                                Ok(Some(Outcome::Opened(
                                    Arc::new(Profile::open(&file.path)?),
                                    file.path,
                                )))
                            }
                        })()
                        .map_err(|error| format!("{error:#}"));
                        message(Action::Finished(ticket, result))
                    },
                    rejected,
                );
            }
            Action::Finished(ticket, result) => {
                if panel
                    .pending
                    .take_if(|pending| Arc::ptr_eq(pending, &ticket))
                    .is_none()
                {
                    return;
                }
                panel.status = None;
                if let Some(activity) = &mut panel.activity {
                    activity.finish(&ticket, &result, |outcome| match outcome {
                        Outcome::Captured(_, path, summary) => {
                            let workload = summary
                                .workload
                                .as_ref()
                                .map_or_else(String::new, |outcome| format!("\n{outcome}"));
                            Some(format!("Saved CPU profile{workload}\n{}", path.display()))
                        }
                        _ => None,
                    });
                }
                match result {
                    Ok(Some(Outcome::Opened(profile, path))) => {
                        panel.activity = None;
                        panel.load(profile, path, None);
                    }
                    Ok(Some(Outcome::Reviewed(review))) => {
                        if let Some(form) = &mut panel.capture {
                            form.review = Some(*review);
                        }
                    }
                    Ok(Some(Outcome::Captured(profile, path, summary))) => {
                        panel.load(profile, path, Some(summary));
                        panel.capture = None;
                        panel.status = Some("Captured CPU profile".into());
                    }
                    Ok(Some(Outcome::Destination(recording))) => {
                        self.start_cpu_capture(*recording, ctx);
                    }
                    Ok(Some(Outcome::Exported)) => {
                        panel.status = Some("Saved flame graph SVG".into())
                    }
                    Ok(None) => (),
                    Err(error) => panel.status = Some(error),
                }
            }
            Action::Query(query) => {
                if let Some(loaded) = &panel.loaded
                    && query != panel.query
                {
                    panel.search = Arc::new(loaded.profile.search(&query));
                    panel.query = query;
                    panel.current_match = None;
                }
            }
            Action::Match(next) => {
                let count = panel.search.matches.len();
                if count > 0 {
                    let index = match (next, panel.current_match) {
                        (true, None) => 0,
                        (false, None) => count - 1,
                        (true, Some(index)) => (index + 1) % count,
                        (false, Some(index)) => (index + count - 1) % count,
                    };
                    panel.current_match = Some(index);
                    panel.zoom(panel.search.matches[index]);
                }
            }
            Action::Zoom(id) => {
                if panel
                    .loaded
                    .as_ref()
                    .is_some_and(|loaded| loaded.profile.has_frame(id))
                {
                    panel.zoom(id);
                }
            }
            Action::Up => {
                if let Some(loaded) = &panel.loaded {
                    panel.zoom(loaded.profile.frame(panel.zoom).parent);
                }
            }
            Action::Depth(depth) => {
                if let Some(loaded) = &panel.loaded {
                    let root = loaded.profile.frame(panel.zoom);
                    panel.first_depth = depth.min(root.last_depth - root.depth);
                    panel.hovered = None;
                }
            }
            Action::Hover(id) => {
                panel.hovered = id.filter(|&id| {
                    panel
                        .loaded
                        .as_ref()
                        .is_some_and(|loaded| loaded.profile.has_frame(id))
                });
            }
            Action::Capture(action) => self.cpu_capture_action(action, ctx),
        }
    }
}
