use super::*;
use plot::Plot;
use std::path::PathBuf;
use w11term::cpu_profile::flame_graph::{Differential, ProfileSide, ROW_HEIGHT, Range};
use w11term::cpu_profile::{FilteredComparison, Profile, Search, sample_share};
use w11term::diagnostic_history::Kind as ArtifactKind;
use w11term::ssh::{CaptureStop, CpuProfileCaptureReport, CpuProfileSummary};

mod capture;
mod compare;
mod plot;
mod report;
pub(super) use capture::Connection;

pub(super) const LABEL: &str = "CPU flame graph";

pub enum Action {
    Open,
    Close,
    OpenFile(Option<PathBuf>),
    CompareFile,
    CompareRepeat,
    Compare(compare::Action),
    Differential,
    DifferentialWidth(ProfileSide, Arc<()>),
    ReturnToComparison,
    Export(Arc<()>),
    Report(report::Action),
    Capture(capture::Action),
    Stop(Arc<()>),
    Graph(Arc<()>, GraphAction),
    Finished(Arc<()>, Result<Option<Outcome>, String>),
}

#[derive(Clone)]
pub enum GraphAction {
    Query(String),
    Match(bool),
    Zoom(usize),
    Up,
    Depth(usize),
    Hover(Option<usize>),
}

pub enum Outcome {
    Opened(Arc<Profile>, PathBuf),
    Compared {
        view: Box<compare::ViewState>,
        install_baseline: bool,
    },
    ComparisonFiltered(FilteredComparison),
    ComparisonReport(ProfileSide, Arc<CpuProfileCaptureReport>),
    Reviewed(Box<capture::Review>),
    Destination(Box<capture::Recording>),
    Captured(Arc<Profile>, PathBuf, Box<CapturedProfile>),
    Exported,
    Report(report::Outcome),
}

pub struct CapturedProfile {
    summary: CpuProfileSummary,
    repeat: capture::Repeat,
    original: Option<ProfileSource>,
}

#[derive(Clone)]
struct ProfileSource {
    profile: Arc<Profile>,
    path: PathBuf,
    report: Option<Arc<CpuProfileCaptureReport>>,
}

impl ProfileSource {
    fn open(path: PathBuf) -> anyhow::Result<Self> {
        let profile = Arc::new(
            Profile::open(&path)
                .with_context(|| format!("Opening CPU profile {}", path.display()))?,
        );
        Ok(Self {
            profile,
            path,
            report: None,
        })
    }
}

struct Loaded {
    profile: Arc<Profile>,
    path: PathBuf,
    plot: Plot,
    capture: Option<Box<CapturedProfile>>,
    verified_report: Option<report::Verified>,
    differential: Option<Differential>,
    ticket: Arc<()>,
    zoom: usize,
    hovered: Option<usize>,
    first_depth: usize,
    query: String,
    search: Arc<Search>,
    current_match: Option<usize>,
}

struct Inspection {
    side: ProfileSide,
    source: Loaded,
    alternate: Option<Loaded>,
}

#[derive(Default)]
pub(super) struct Panel {
    pub open: bool,
    pub(super) activity: Option<diagnostics::Activity>,
    loaded: Option<Loaded>,
    comparison: Option<compare::ViewState>,
    inspection: Option<Inspection>,
    pub(super) pending: Option<Arc<()>>,
    stop: Option<CaptureStop>,
    status: Option<String>,
    capture: Option<capture::Form>,
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

    fn load(
        &mut self,
        profile: Arc<Profile>,
        path: PathBuf,
        capture: Option<Box<CapturedProfile>>,
    ) {
        self.comparison = None;
        self.inspection = None;
        self.loaded = Some(Loaded::new(profile, path, capture, 0));
    }

    fn visible_graph(&self) -> Option<&Loaded> {
        if self.capture.is_some() {
            None
        } else if let Some(inspection) = &self.inspection {
            Some(&inspection.source)
        } else if self.comparison.is_none() {
            self.loaded.as_ref()
        } else {
            None
        }
    }

    fn graph_input(&mut self, ticket: &Arc<()>) -> Option<&mut Loaded> {
        if !self.open || self.capture.is_some() {
            return None;
        }
        if let Some(inspection) = &mut self.inspection {
            Some(&mut inspection.source)
        } else if self.comparison.is_none() {
            self.loaded.as_mut()
        } else {
            None
        }
        .filter(|source| Arc::ptr_eq(ticket, &source.ticket))
    }

    fn view(
        &self,
        window_height: f64,
        active: Option<Connection>,
        export_idle: bool,
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
        if let Some((stop, ticket)) = self.stop.as_ref().zip(self.pending.as_ref()) {
            let ticket = ticket.clone();
            fields.push(KeyedView::new(
                "stop",
                Button::new()
                    .is_enabled(!stop.is_requested())
                    .on_click(ctx.callback(move |_| message(Action::Stop(ticket.clone()))))
                    .content("Stop and save profile"),
            ));
        }
        if let Some(capture) = &self.capture {
            fields.extend(capture.fields(active, idle, ctx));
        } else if let Some(inspection) = &self.inspection {
            let source = inspection.source.ticket.clone();
            fields.extend([
                KeyedView::new(
                    "return-to-comparison",
                    Button::new()
                        .is_enabled(idle)
                        .on_click(send(|| Action::ReturnToComparison))
                        .content("Back to comparison"),
                ),
                KeyedView::new(
                    "inspection-export",
                    Button::new()
                        .is_enabled(idle)
                        .on_click(ctx.callback(move |_| message(Action::Export(source.clone()))))
                        .content("Export SVG"),
                ),
            ]);
            if inspection.alternate.is_some() {
                let switch = |side: ProfileSide| {
                    let source = inspection.source.ticket.clone();
                    Button::new()
                        .is_enabled(idle && side != inspection.side)
                        .on_click(ctx.callback(move |_| {
                            message(Action::DifferentialWidth(side, source.clone()))
                        }))
                        .content(side.widths())
                };
                fields.extend([
                    text("differential-title", "Differential CPU flame graph"),
                    KeyedView::new("differential-widths", StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(6.0)
                        .children((switch(ProfileSide::Before), switch(ProfileSide::After)))),
                    text("differential-legend", "Full profiles · Exact-stack sample share · Red increased · Blue decreased · Gray unchanged"),
                ]);
            }
            fields.push(text("inspection-side", inspection.side.caption()));
            inspection.source.graph(window_height, ctx, &mut fields);
        } else if let Some(comparison) = &self.comparison {
            fields.extend(comparison.fields(idle, export_idle, ctx));
        } else {
            let source = self.loaded.as_ref().map(|loaded| loaded.ticket.clone());
            fields.push(KeyedView::new(
                "files",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .is_enabled(idle)
                            .on_click(send(|| Action::OpenFile(None)))
                            .content("Open folded stacks"),
                        Button::new()
                            .is_enabled(idle && self.loaded.is_some())
                            .on_click(ctx.callback(move |_| {
                                source.as_ref().map_or(Message::ControlIdle, |source| {
                                    message(Action::Export(source.clone()))
                                })
                            }))
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
                if loaded.capture.is_some() {
                    fields.push(KeyedView::new(
                        "repeat-profile",
                        Button::new()
                            .is_enabled(idle)
                            .on_click(send(|| Action::Capture(capture::Action::Repeat)))
                            .content("Repeat CPU profile…"),
                    ));
                }
                if loaded
                    .capture
                    .as_ref()
                    .is_some_and(|capture| capture.original.is_some())
                {
                    fields.push(KeyedView::new(
                        "compare-repeat",
                        Button::new()
                            .is_enabled(idle)
                            .on_click(send(|| Action::CompareRepeat))
                            .content("Compare with original profile"),
                    ));
                }
                fields.push(KeyedView::new(
                    "compare",
                    Button::new()
                        .is_enabled(idle)
                        .on_click(send(|| Action::CompareFile))
                        .content("Compare profiles"),
                ));
                report::fields(loaded, idle, ctx, &mut fields);
                loaded.graph(window_height, ctx, &mut fields);
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
}

impl Loaded {
    fn source(&self) -> ProfileSource {
        ProfileSource {
            profile: self.profile.clone(),
            path: self.path.clone(),
            report: report::capture_report(self)
                .filter(|report| report.matches_profile(&self.profile))
                .or_else(|| {
                    self.verified_report
                        .as_ref()
                        .map(|verified| &verified.report)
                })
                .cloned(),
        }
    }

    fn new(
        profile: Arc<Profile>,
        path: PathBuf,
        capture: Option<Box<CapturedProfile>>,
        zoom: usize,
    ) -> Self {
        Self::with_mode(profile, path, capture, zoom, None)
    }

    fn differential(comparison: Arc<w11term::cpu_profile::Comparison>, side: ProfileSide) -> Self {
        let index = side.index();
        Self::with_mode(
            comparison.profiles()[index].clone(),
            comparison.paths()[index].clone(),
            None,
            0,
            Some(Differential { comparison, side }),
        )
    }

    fn with_mode(
        profile: Arc<Profile>,
        path: PathBuf,
        capture: Option<Box<CapturedProfile>>,
        zoom: usize,
        differential: Option<Differential>,
    ) -> Self {
        let search = Arc::<Search>::default();
        Self {
            plot: Plot::new(profile.clone(), search.clone(), differential.clone()),
            profile,
            path,
            capture,
            verified_report: None,
            differential,
            ticket: Arc::new(()),
            zoom,
            hovered: None,
            first_depth: 0,
            query: String::new(),
            search,
            current_match: None,
        }
    }

    fn events(&self) -> impl Fn(GraphAction) -> Message + Clone + use<> {
        let ticket = self.ticket.clone();
        move |action| message(Action::Graph(ticket.clone(), action))
    }

    fn visible_rows(window_height: f64) -> usize {
        (((window_height - 285.0) / f64::from(ROW_HEIGHT)).floor() as usize).clamp(3, 50)
    }

    fn range(&self, window_height: f64) -> (Range, usize) {
        let root = self.profile.frame(self.zoom);
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

    fn graph(
        &self,
        window_height: f64,
        ctx: &ViewContext<TerminalApp>,
        fields: &mut Vec<KeyedView>,
    ) {
        let event = self.events();
        let command = |action: GraphAction| {
            let event = event.clone();
            ctx.callback(move |_| event(action.clone()))
        };
        let profile = &self.profile;
        let total = profile.frame(0).samples;
        let (range, depth) = self.range(window_height);
        let root = profile.frame(self.zoom);
        let focus = profile.frame(self.hovered.unwrap_or(self.zoom));
        let name = self
            .path
            .file_name()
            .unwrap_or(self.path.as_os_str())
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
                .on_click(command(GraphAction::Match(next)))
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
                        .on_text_changed({
                            let event = event.clone();
                            ctx.callback(move |query| event(GraphAction::Query(query)))
                        }),
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
                    sample_share(self.search.samples, total),
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
                        .on_click(command(GraphAction::Up))
                        .content("Up"),
                    Button::new()
                        .is_enabled(self.zoom != 0 || range.first_depth != 0)
                        .on_click(command(GraphAction::Zoom(0)))
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
                                .on_click(command(GraphAction::Depth(shallow_at)))
                                .content("Shallower"),
                            Button::new()
                                .is_enabled(page_at < depth)
                                .on_click(command(GraphAction::Depth(page_at)))
                                .content("Deeper"),
                        )),
                )),
            ));
        }

        let sender = ctx.sender();
        let hover = self.plot.pointer();
        let exit = self.plot.pointer();
        let click = self.plot.pointer();
        let hover_sender = sender.clone();
        let hover_event = event.clone();
        let exit_event = event.clone();
        fields.push(KeyedView::new(
            "plot",
            Border::new()
                .height(f64::from(range.height()))
                .background(Color::rgb(26, 26, 28))
                .automation_name("CPU flame graph chart")
                .on_pointer_moved(move |pointer: PointerEventInfo| {
                    if let Some(target) = hover(pointer.x, pointer.y, true) {
                        hover_sender.send(hover_event(GraphAction::Hover(target)));
                    }
                })
                .on_pointer_exited(move |_| {
                    if let Some(target) = exit(-1.0, -1.0, true) {
                        sender.send(exit_event(GraphAction::Hover(target)));
                    }
                })
                .on_pointer_pressed(ctx.callback(move |pointer: PointerEventInfo| {
                    if pointer.is_left_button_pressed
                        && let Some(Some(frame)) = click(pointer.x, pointer.y, false)
                    {
                        return event(GraphAction::Zoom(frame));
                    }
                    Message::ControlIdle
                }))
                .content(self.plot.canvas(range, &self.search)),
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
                sample_share(focus.samples, total)
            ),
        ));
        if let Some(differential) = &self.differential {
            fields.push(text(
                "stack-delta",
                differential.detail(self.hovered.unwrap_or(self.zoom)),
            ));
        }
        if focus.parent != 0 {
            fields.push(text(
                "caller",
                format!("Caller {}", profile.frame(focus.parent).name),
            ));
        }
        if let Some(capture) = &self.capture {
            let capture = &capture.summary;
            if let Some(raw) = &capture.perf_data {
                fields.push(text(
                    "raw-recording",
                    format!("Raw perf recording saved to {}", raw.path.display()),
                ));
            }
            if let Some(warning) = &capture.perf_data_warning {
                fields.push(text("raw-recording-warning", warning));
            }
            if let Some(outcome) = profile_outcome(capture) {
                fields.push(text("workload-outcome", outcome));
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

    fn apply(&mut self, action: GraphAction) {
        match action {
            GraphAction::Query(query) if query != self.query => {
                self.search = Arc::new(self.profile.search(&query));
                self.query = query;
                self.current_match = None;
            }
            GraphAction::Match(next) => {
                let count = self.search.matches.len();
                if count == 0 {
                    return;
                }
                let index = match (next, self.current_match) {
                    (true, None) => 0,
                    (false, None) => count - 1,
                    (true, Some(index)) => (index + 1) % count,
                    (false, Some(index)) => (index + count - 1) % count,
                };
                self.current_match = Some(index);
                self.zoom(self.search.matches[index]);
            }
            GraphAction::Zoom(id) if self.profile.has_frame(id) => self.zoom(id),
            GraphAction::Up => self.zoom(self.profile.frame(self.zoom).parent),
            GraphAction::Depth(depth) => {
                let root = self.profile.frame(self.zoom);
                self.first_depth = depth.min(root.last_depth - root.depth);
                self.hovered = None;
            }
            GraphAction::Hover(id) => {
                self.hovered = id.filter(|&id| self.profile.has_frame(id));
            }
            _ => (),
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

fn profile_outcome(summary: &CpuProfileSummary) -> Option<String> {
    summary
        .workload
        .as_ref()
        .map(ToString::to_string)
        .or_else(|| summary.stopped_by_user.then(|| "Stopped by request".into()))
}

impl TerminalApp {
    pub(super) fn compare_saved_profiles(
        &mut self,
        before: PathBuf,
        after: PathBuf,
        ctx: &ComponentContext<Self>,
    ) -> bool {
        if self.flame_graph.pending.is_some() {
            return false;
        }
        self.set_diagnostics_drawer(Some(diagnostics::Drawer::FlameGraph));
        let ticket = self.flame_graph.begin("Comparing saved CPU profiles…");
        let rejected = message(Action::Finished(
            ticket.clone(),
            Err("CPU comparison worker unavailable".into()),
        ));
        ctx.spawn_background_with_rejection(
            move |scope| {
                let result = (|| -> anyhow::Result<_> {
                    if scope.is_cancelled() {
                        return Ok(None);
                    }
                    let before = ProfileSource::open(before).context("Reading Before profile")?;
                    let after = ProfileSource::open(after).context("Reading After profile")?;
                    Ok(Some(Outcome::Compared {
                        view: Box::new(compare::ViewState::new(before, after)),
                        install_baseline: true,
                    }))
                })()
                .map_err(|error| format!("{error:#}"));
                message(Action::Finished(ticket, result))
            },
            rejected,
        );
        true
    }

    pub(super) fn with_flame_graph(&self, content: View, ctx: &ViewContext<Self>) -> View {
        let review_visible = self.flame_graph.comparison.is_some()
            && self
                .tabs
                .active()
                .is_some_and(|tab| matches!(tab.page, Page::OpenRouter(_)));
        let fraction = if review_visible { 0.5 } else { 0.67 };
        let width = (self.width * fraction).clamp(275.0, 1100.0);
        let panel = self.flame_graph.view(
            self.window_height,
            self.profile_connection(),
            self.export.pending.is_none(),
            ctx,
        );
        self.diagnostics_layout(content, "flame-graph", panel, width)
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
                panel.inspection = None;
                if let Some(loaded) = &mut panel.loaded {
                    loaded.hovered = None;
                }
                self.close_empty_diagnostics_window(ctx);
            }
            Action::ReturnToComparison if panel.pending.is_none() && panel.capture.is_none() => {
                if panel.inspection.take().is_some() {
                    panel.status = None;
                }
            }
            Action::ReturnToComparison => (),
            Action::Differential
                if panel.open
                    && panel.pending.is_none()
                    && panel.capture.is_none()
                    && panel.inspection.is_none() =>
            {
                if let Some(view) = &panel.comparison {
                    let comparison = view.results.comparison().clone();
                    panel.status = None;
                    panel.inspection = Some(Inspection {
                        side: ProfileSide::After,
                        source: Loaded::differential(comparison.clone(), ProfileSide::After),
                        alternate: Some(Loaded::differential(comparison, ProfileSide::Before)),
                    });
                }
            }
            Action::DifferentialWidth(side, ticket)
                if panel.open && panel.pending.is_none() && panel.capture.is_none() =>
            {
                if let Some(inspection) = &mut panel.inspection
                    && inspection.side != side
                    && Arc::ptr_eq(&inspection.source.ticket, &ticket)
                    && let Some(alternate) = &mut inspection.alternate
                {
                    std::mem::swap(&mut inspection.source, alternate);
                    inspection.side = side;
                    inspection.source.hovered = None;
                    panel.status = None;
                }
            }
            Action::Differential | Action::DifferentialWidth(..) => (),
            Action::Stop(ticket) => {
                if panel
                    .pending
                    .as_ref()
                    .is_some_and(|pending| Arc::ptr_eq(pending, &ticket))
                    && let Some(stop) = &panel.stop
                {
                    stop.request();
                    if let Some(activity) = &mut panel.activity {
                        activity.stopping();
                    }
                    panel.status = Some("Stop requested. Saving collected CPU samples…".into());
                }
            }
            Action::CompareRepeat => {
                if !panel.open || panel.pending.is_some() {
                    return;
                }
                let Some(after) = panel.visible_graph() else {
                    return;
                };
                let Some(before) = after
                    .capture
                    .as_ref()
                    .and_then(|capture| capture.original.clone())
                else {
                    return;
                };
                let after = after.source();
                let ticket = panel.begin("Comparing the original and repeated CPU profiles…");
                let rejected = message(Action::Finished(
                    ticket.clone(),
                    Err("CPU comparison worker unavailable".into()),
                ));
                ctx.spawn_background_with_rejection(
                    move |scope| {
                        let outcome = (!scope.is_cancelled()).then(|| Outcome::Compared {
                            view: Box::new(compare::ViewState::new(before, after)),
                            install_baseline: false,
                        });
                        message(Action::Finished(ticket, Ok(outcome)))
                    },
                    rejected,
                );
            }
            Action::Report(action) => self.cpu_report_action(action, ctx),
            Action::OpenFile(_) | Action::CompareFile | Action::Export(_) => {
                let export = matches!(action, Action::Export(_));
                let inspecting = export && panel.inspection.is_some();
                let recent = match &action {
                    Action::OpenFile(path) => path.clone(),
                    _ => None,
                };
                if panel.pending.is_some()
                    || (panel.comparison.is_some() && recent.is_none() && !inspecting)
                {
                    return;
                }
                let Some(owner) = self.keyboard.hwnd().map(|window| window.0 as usize) else {
                    return;
                };
                let loaded = if export {
                    panel.visible_graph()
                } else {
                    panel.loaded.as_ref()
                };
                if let Action::Export(source) = &action
                    && !loaded.is_some_and(|loaded| Arc::ptr_eq(source, &loaded.ticket))
                {
                    return;
                }
                let path =
                    loaded.map_or_else(|| self.store.path().into(), |loaded| loaded.path.clone());
                let compare = matches!(action, Action::CompareFile);
                let before = loaded.filter(|_| compare).map(Loaded::source);
                let profile = loaded.map(|loaded| loaded.profile.clone());
                let zoom = loaded.map_or(0, |loaded| loaded.zoom);
                let differential = loaded.and_then(|loaded| loaded.differential.clone());
                let ticket = panel.begin(if recent.is_some() {
                    "Opening folded stacks…"
                } else if export {
                    "Choose an SVG destination…"
                } else if compare {
                    "Choose the after profile…"
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
                            let file = if let Some(path) = recent {
                                path
                            } else {
                                let Some(file) = file_picker::pick(
                                    owner,
                                    file_picker::Purpose::FlameGraph { path, export },
                                )?
                                else {
                                    return Ok(None);
                                };
                                file.path
                            };
                            if scope.is_cancelled() {
                                return Ok(None);
                            }
                            if export {
                                let profile =
                                    profile.context("Open folded stacks before exporting")?;
                                plot::export(&profile, zoom, &file, differential.as_ref())?;
                                Ok(Some(Outcome::Exported))
                            } else if compare {
                                let before = before.context("Open a before profile to compare")?;
                                let after = ProfileSource::open(file)?;
                                Ok(Some(Outcome::Compared {
                                    view: Box::new(compare::ViewState::new(before, after)),
                                    install_baseline: false,
                                }))
                            } else {
                                Ok(Some(Outcome::Opened(Arc::new(Profile::open(&file)?), file)))
                            }
                        })()
                        .map_err(|error| format!("{error:#}"));
                        message(Action::Finished(ticket, result))
                    },
                    rejected,
                );
            }
            Action::Compare(action) => {
                let Some(view) = panel.comparison.as_mut().filter(|_| {
                    panel.pending.is_none() && panel.inspection.is_none() && panel.capture.is_none()
                }) else {
                    return;
                };
                match action {
                    compare::Action::Back => {
                        panel.comparison = None;
                        panel.status = None;
                    }
                    compare::Action::Query(query) => view.query = query,
                    compare::Action::Page(forward) => view.page(forward),
                    compare::Action::Select(comparison, id) => view.select(&comparison, id),
                    compare::Action::CaptureSettingsExpanded(expanded) => {
                        view.capture_settings_expanded(expanded);
                    }
                    compare::Action::VerifyReport(side) => {
                        self.choose_cpu_comparison_report(side, ctx)
                    }
                    compare::Action::Inspect(side) => {
                        if panel.open
                            && let Some((profile, path, frame)) = view.selected_source(side)
                        {
                            panel.status = None;
                            panel.inspection = Some(Inspection {
                                side,
                                source: Loaded::new(profile, path, None, frame),
                                alternate: None,
                            });
                        }
                    }
                    compare::Action::OpenRouter => {
                        if panel.open
                            && let Some(evidence) = view.openrouter_preview()
                        {
                            self.review_cpu_comparison_with_openrouter(evidence, ctx);
                        }
                    }
                    compare::Action::Export => {
                        let snapshot = view.results.clone();
                        let reports = view.capture_reports();
                        self.begin_text_export(
                            text_export::Subject::CpuComparison,
                            move |output| {
                                snapshot.write_json_with_capture_reports(
                                    output,
                                    reports.each_ref().map(Option::as_deref),
                                )
                            },
                            ctx,
                        );
                    }
                    compare::Action::Filter => {
                        if view.results.matches_query(&view.query) {
                            return;
                        }
                        let query = view.query.clone();
                        let comparison = view.results.comparison().clone();
                        let ticket = panel.begin("Filtering changed call stacks…");
                        let rejected = message(Action::Finished(
                            ticket.clone(),
                            Err("Filter worker unavailable".into()),
                        ));
                        ctx.spawn_background_with_rejection(
                            move |scope| {
                                let outcome = (!scope.is_cancelled()).then(|| {
                                    Outcome::ComparisonFiltered(comparison.filter(&query))
                                });
                                message(Action::Finished(ticket, Ok(outcome)))
                            },
                            rejected,
                        );
                    }
                }
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
                panel.stop = None;
                self.diagnostic_control.finish(&ticket, &result);
                if let Some(activity) = &mut panel.activity {
                    activity.finish(&ticket, &result, |outcome| match outcome {
                        Outcome::Captured(_, path, capture) => {
                            let outcome = profile_outcome(&capture.summary)
                                .map_or_else(String::new, |outcome| format!("\n{outcome}"));
                            Some(format!("Saved CPU profile{outcome}\n{}", path.display()))
                        }
                        _ => None,
                    });
                }
                match result {
                    Ok(Some(Outcome::Opened(profile, path))) => {
                        panel.activity = None;
                        panel.capture = None;
                        panel.load(profile, path.clone(), None);
                        self.remember_diagnostic(ArtifactKind::CpuProfile, [path], ctx);
                    }
                    Ok(Some(Outcome::Compared {
                        view,
                        install_baseline,
                    })) => {
                        let before = if install_baseline {
                            let (profile, path) = view.before_source();
                            panel.activity = None;
                            panel.capture = None;
                            panel.load(profile, path.clone(), None);
                            Some(path)
                        } else {
                            None
                        };
                        let path = view.after_path().to_owned();
                        panel.comparison = Some(*view);
                        panel.inspection = None;
                        if let Some(loaded) = &mut panel.loaded {
                            loaded.hovered = None;
                        }
                        if let Some(before) = before {
                            self.remember_diagnostic(ArtifactKind::CpuProfile, [before], ctx);
                        }
                        self.remember_diagnostic(ArtifactKind::CpuProfile, [path], ctx);
                    }
                    Ok(Some(Outcome::ComparisonFiltered(results))) => {
                        if let Some(comparison) = &mut panel.comparison {
                            comparison.filtered(results);
                        }
                    }
                    Ok(Some(Outcome::ComparisonReport(side, report))) => {
                        if let Some(comparison) = &mut panel.comparison {
                            panel.status = Some(comparison.attach_report(side, report).into());
                        }
                    }
                    Ok(Some(Outcome::Reviewed(review))) => {
                        if let Some(form) = &mut panel.capture {
                            form.review = Some(*review);
                        }
                    }
                    Ok(Some(Outcome::Captured(profile, path, capture))) => {
                        panel.load(profile, path.clone(), Some(capture));
                        panel.capture = None;
                        panel.status = Some("Captured CPU profile".into());
                        self.remember_diagnostic(ArtifactKind::CpuProfile, [path], ctx);
                    }
                    Ok(Some(Outcome::Destination(recording))) => {
                        self.start_cpu_capture(*recording, ctx);
                    }
                    Ok(Some(Outcome::Exported)) => {
                        panel.status = Some("Saved flame graph SVG".into())
                    }
                    Ok(Some(Outcome::Report(result))) => report::finish(panel, result),
                    Ok(None) => (),
                    Err(error) => panel.status = Some(error),
                }
            }
            Action::Graph(ticket, action) => {
                if let Some(graph) = panel.graph_input(&ticket) {
                    graph.apply(action);
                }
            }
            Action::Capture(action) => self.cpu_capture_action(action, ctx),
        }
    }
}
