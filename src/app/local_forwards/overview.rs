use super::*;

pub(in crate::app) const LABEL: &str = "Active port forwards";

#[derive(Clone, Copy)]
pub struct Target {
    origin: Origin,
    route: u64,
}

pub enum Edit {
    Refresh,
    Focus(Target),
    Copy(Target),
    Stop(Target),
}

fn action(id: TabId, edit: Edit) -> Message {
    Message::LocalForward(Action::Overview(id, edit))
}

impl TerminalApp {
    pub(super) fn open_forward_overview(&mut self, ctx: &ComponentContext<Self>) {
        let existing = self.tabs.entries().iter().find_map(|entry| {
            matches!(entry.value.page, Page::ForwardOverview(_)).then_some(entry.id)
        });
        if let Some(id) = existing {
            self.tabs.select(id);
            self.sync_selection(ctx);
            self.activate(ctx);
        } else {
            self.insert_tab(
                AppTab {
                    label: LABEL.into(),
                    page: Page::ForwardOverview(String::new()),
                },
                ctx,
            );
        }
    }

    fn window_forwards(&self) -> impl Iterator<Item = (&str, &LiveTab, Origin, &Route)> {
        self.tabs
            .entries()
            .iter()
            .filter_map(|entry| {
                let Page::Terminal(tab) = &entry.value.page else {
                    return None;
                };
                if tab.forwards.entries.is_empty() {
                    return None;
                }
                Some((entry.value.label.as_str(), tab, Origin::from_tab(tab)?))
            })
            .flat_map(|(label, tab, origin)| {
                tab.forwards
                    .entries
                    .iter()
                    .filter(move |route| {
                        route.generation == origin.generation && route.handle.status().running
                    })
                    .map(move |route| (label, tab, origin, route))
            })
    }

    pub(super) fn watch_forward_overview(&mut self, id: TabId, ctx: &ComponentContext<Self>) {
        let listeners = self
            .window_forwards()
            .map(|(_, _, _, route)| route.handle.subscribe())
            .collect();
        self.forward_watch = watch_updates(ctx, Vec::new(), listeners, move || {
            action(id, Edit::Refresh)
        });
    }

    pub(super) fn edit_forward_overview(
        &mut self,
        id: TabId,
        edit: Edit,
        ctx: &ComponentContext<Self>,
    ) {
        if self.tabs.active_id() != Some(id)
            || !matches!(
                self.tabs.get(id).map(|tab| &tab.page),
                Some(Page::ForwardOverview(_))
            )
        {
            return;
        }
        let target = match edit {
            Edit::Refresh => {
                if self.window_forwards().next().is_none() {
                    self.forward_watch.take();
                }
                return;
            }
            Edit::Focus(t) | Edit::Copy(t) | Edit::Stop(t) => t,
        };
        let Some(source) = self.forward_source(target.origin) else {
            return;
        };
        let Some(Page::Terminal(tab)) = self.tabs.get_mut(source).map(|tab| &mut tab.page) else {
            return;
        };
        let Some(route) = tab.forwards.entries.iter_mut().find(|route| {
            route.id == target.route
                && route.generation == target.origin.generation
                && route.handle.status().running
        }) else {
            return;
        };
        let feedback = match edit {
            Edit::Focus(_) => {
                self.tabs.select(source);
                self.sync_selection(ctx);
                self.activate(ctx);
                return;
            }
            Edit::Copy(_) => {
                let Some(feedback) = route.copy_address() else {
                    return;
                };
                feedback
            }
            Edit::Stop(_) if !route.stopping => {
                route.stop();
                String::new()
            }
            _ => return,
        };
        if let Some(Page::ForwardOverview(current)) = self.tabs.get_mut(id).map(|tab| &mut tab.page)
        {
            *current = feedback;
        }
    }

    pub(in crate::app) fn forward_overview(
        &self,
        id: TabId,
        feedback: &str,
        ctx: &ViewContext<Self>,
    ) -> View {
        let mut rows = Vec::new();
        for (label, tab, origin, route) in self.window_forwards() {
            let Some(link) = tab.session.ssh_connection_at(origin.generation) else {
                continue;
            };
            let endpoint = profile_query::address(link.options());
            let pane_label = tab.pane_name.as_ref().map_or(label, |name| name.as_str());
            let port = route.handle.local_port();
            let remote = route.handle.remote_port();
            let status = route.handle.status();
            let target = Target {
                origin,
                route: route.id,
            };
            let mut details = vec![
                KeyedView::new(
                    "source",
                    TextBlock::new()
                        .text(format!("{pane_label} · Pane {} · {endpoint}", origin.pane))
                        .text_wrapping(TextWrapping::Wrap),
                ),
                KeyedView::new(
                    "route",
                    TextBlock::new()
                        .text(format!(
                            "Windows {} → remote 127.0.0.1:{remote}",
                            windows_address(port)
                        ))
                        .text_wrapping(TextWrapping::Wrap),
                ),
                KeyedView::new("status", TextBlock::new().text(route.state_label(&status))),
            ];
            if let Some(error) = status.last_error {
                details.push(KeyedView::new(
                    "error",
                    TextBlock::new()
                        .text(error)
                        .text_wrapping(TextWrapping::Wrap),
                ));
            }
            details.push(KeyedView::new(
                "actions",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .automation_name(format!("Focus pane {} for port {port}", origin.pane))
                            .on_click(ctx.callback(move |_| action(id, Edit::Focus(target))))
                            .content("Focus pane"),
                        Button::new()
                            .automation_name(format!("Copy Windows address for port {port}"))
                            .is_enabled(status.running && !route.stopping)
                            .on_click(ctx.callback(move |_| action(id, Edit::Copy(target))))
                            .content("Copy address"),
                        Button::new()
                            .automation_name(format!("Stop forwarding port {port}"))
                            .is_enabled(status.running && !route.stopping)
                            .on_click(ctx.callback(move |_| action(id, Edit::Stop(target))))
                            .content("Stop"),
                    )),
            ));
            rows.push(KeyedView::new(
                route.id,
                StackPanel::new().spacing(4.0).keyed_children(details),
            ));
        }
        if rows.is_empty() {
            rows.push(KeyedView::new(
                "empty",
                TextBlock::new().text("No active port forwards in this window"),
            ));
        }
        if !feedback.is_empty() {
            rows.push(KeyedView::new(
                "feedback",
                TextBlock::new()
                    .text(feedback)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        Grid::new()
            .margin(16.0)
            .row_spacing(12.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                TextBlock::new().text(LABEL).font_size(24.0),
                ScrollViewer::new()
                    .grid_row(1)
                    .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                    .content(StackPanel::new().spacing(12.0).keyed_children(rows)),
            ))
    }
}
