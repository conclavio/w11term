use super::*;

struct Choice {
    pane: u64,
    title: String,
    label: String,
    profile: Option<String>,
    destination: String,
    tab_number: usize,
    pane_number: usize,
}

impl Choice {
    fn rank(&self, query: &str) -> Option<u8> {
        if query.is_empty() {
            return Some(0);
        }
        [&self.title, &self.label, &self.destination]
            .into_iter()
            .chain(self.profile.iter())
            .filter_map(|field| {
                let field = field.to_lowercase();
                if field == query {
                    Some(0)
                } else if field.starts_with(query) {
                    Some(1)
                } else {
                    field.contains(query).then_some(2)
                }
            })
            .min()
    }

    fn caption(&self) -> String {
        // The engine uses this title until a process sets a title of its own.
        let title = if self.title.is_empty() || self.title == "wezterm" {
            &self.label
        } else {
            &self.title
        };
        let mut fields = vec![title.as_str()];
        for text in self
            .profile
            .iter()
            .chain(std::iter::once(&self.destination))
        {
            if !text.is_empty() && !fields.contains(&text.as_str()) {
                fields.push(text);
            }
        }
        format!(
            "{}  ·  Tab {} / pane {}",
            fields.join("  ·  "),
            self.tab_number,
            self.pane_number
        )
    }
}

pub(super) struct Picker {
    query: String,
    input: ElementRef<TextBox>,
    matches: Vec<Choice>,
    selected: usize,
}

impl Picker {
    fn filter(choices: Vec<Choice>, query: &str) -> Vec<Choice> {
        let needle = query.trim().to_lowercase();
        let mut matches: Vec<_> = choices
            .into_iter()
            .filter_map(|choice| Some((choice.rank(&needle)?, choice)))
            .collect();
        matches.sort_by_key(|(rank, _)| *rank);
        matches.into_iter().map(|(_, choice)| choice).collect()
    }

    pub(super) fn move_by(&mut self, amount: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(amount)
            .min(self.matches.len().saturating_sub(1));
    }

    pub(super) fn view(&self, app: &TerminalApp, ctx: &ViewContext<TerminalApp>) -> View {
        let page_size = ((app.window_height - 145.0) / 42.0).clamp(1.0, 8.0) as usize;
        let first = self.selected / page_size * page_size;
        let mut children = vec![KeyedView::new(
            "search",
            Grid::new()
                .columns([GridLength::STAR, GridLength::Auto])
                .children((
                    TextBox::new()
                        .element_ref(&self.input)
                        .text(&self.query)
                        .placeholder_text("Search by title, profile or user@host")
                        .automation_name("Find pane")
                        .on_text_changed(ctx.callback(Message::PickerQuery)),
                    Button::new()
                        .grid_column(1)
                        .style(ButtonStyle::Subtle)
                        .automation_name("Close pane picker")
                        .on_click(ctx.callback(|_| Message::Shortcut(Shortcut::PickerCancel)))
                        .content(TextBlock::new().text("×")),
                )),
        )];
        for (i, row) in self.matches.iter().enumerate().skip(first).take(page_size) {
            let id = row.pane;
            let caption = row.caption();
            children.push(KeyedView::new(
                format!("pane-{id}"),
                Button::new()
                    .automation_name(&caption)
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .style(if i == self.selected {
                        ButtonStyle::Accent
                    } else {
                        ButtonStyle::Subtle
                    })
                    .on_click(ctx.callback(move |_| Message::PickerChoose(id)))
                    .content(TextBlock::new().text(caption)),
            ));
        }
        if self.matches.is_empty() {
            children.push(KeyedView::new(
                "empty",
                TextBlock::new().text("No matching panes"),
            ));
        } else if self.matches.len() > page_size {
            let last = (first + page_size).min(self.matches.len());
            children.push(KeyedView::new(
                "pages",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .is_enabled(first > 0)
                            .automation_name("Previous pane results")
                            .on_click(ctx.callback(move |_| {
                                Message::Shortcut(Shortcut::PickerMove(-(page_size as isize)))
                            }))
                            .content(TextBlock::new().text("‹")),
                        TextBlock::new()
                            .vertical_alignment(VerticalAlignment::Center)
                            .text(format!("{}–{last} of {}", first + 1, self.matches.len())),
                        Button::new()
                            .is_enabled(last < self.matches.len())
                            .automation_name("Next pane results")
                            .on_click(ctx.callback(move |_| {
                                Message::Shortcut(Shortcut::PickerMove(page_size as isize))
                            }))
                            .content(TextBlock::new().text("›")),
                    )),
            ));
        }
        Border::new()
            .grid_row_span(2)
            .background(Color::argb(112, 0, 0, 0))
            .content(
                Border::new()
                    .vertical_alignment(VerticalAlignment::Top)
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .width((app.width - 32.0).clamp(240.0, 660.0))
                    .margin(16.0)
                    .padding(12.0)
                    .corner_radius(8.0)
                    .background(Color::rgb(40, 42, 48))
                    .content(StackPanel::new().spacing(4.0).keyed_children(children)),
            )
    }
}

impl TerminalApp {
    fn picker_choices(&self) -> Vec<Choice> {
        let mut choices = vec![];
        for (tab, header) in self.tabs.headers().into_iter().enumerate() {
            if let Some(layout) = self.tabs.layout_for(header.id) {
                for (pane, leaf) in layout.leaves().into_iter().enumerate() {
                    if let Some(AppTab {
                        label,
                        page: Page::Terminal(live),
                    }) = self.tabs.get(leaf)
                    {
                        choices.push(Choice {
                            pane: live.session.pane_id,
                            label: label.clone(),
                            title: live.session.frames.load().title.clone(),
                            destination: source_label(&live.source),
                            profile: live.profile_name.clone(),
                            tab_number: tab + 1,
                            pane_number: pane + 1,
                        });
                    }
                }
            }
        }
        choices
    }

    pub(super) fn open_picker(&mut self, ctx: &ComponentContext<Self>) {
        if self.picker.is_some() {
            return;
        }
        if self.switcher.is_some() {
            self.finish_switcher(false, ctx);
        }
        let matches = self.picker_choices();
        let active = self.tabs.active().and_then(AppTab::pane_id);
        let selected = matches
            .iter()
            .position(|row| Some(row.pane) == active)
            .unwrap_or(0);
        self.picker = Some(Picker {
            input: ElementRef::new(),
            query: String::new(),
            matches,
            selected,
        });
        self.keyboard.set_picker(true);
        self.focus_picker_input(0, ctx);
    }

    pub(super) fn focus_picker_input(&self, attempt: u8, ctx: &ComponentContext<Self>) {
        let Some(picker) = &self.picker else { return };
        let sender = ctx.sender();
        let queued = picker.input.request_focus_result(move |result| {
            if !result.unwrap_or(false) && attempt < 4 {
                sender.send(Message::PickerFocus(attempt + 1));
            }
        });
        if !queued && attempt < 4 {
            ctx.spawn_background(move |_| {
                std::thread::sleep(Duration::from_millis(20));
                Message::PickerFocus(attempt + 1)
            });
        }
    }

    pub(super) fn filter_picker(&mut self, query: String) {
        if self
            .picker
            .as_ref()
            .is_none_or(|picker| picker.query == query)
        {
            return;
        }
        let matches = Picker::filter(self.picker_choices(), &query);
        if let Some(picker) = &mut self.picker {
            picker.query = query;
            picker.matches = matches;
            picker.selected = 0;
        }
    }

    pub(super) fn accept_picker(&mut self, ctx: &ComponentContext<Self>) {
        if let Some(pane) = self
            .picker
            .as_ref()
            .and_then(|picker| picker.matches.get(picker.selected))
            .map(|row| row.pane)
        {
            self.choose_picker_pane(pane, ctx);
        }
    }

    pub(super) fn choose_picker_pane(&mut self, pane: u64, ctx: &ComponentContext<Self>) {
        let id = self.tab_for_pane(pane);
        let Some(picker) = &mut self.picker else {
            return;
        };
        if !picker.matches.iter().any(|row| row.pane == pane) {
            return;
        }
        let Some(id) = id else {
            // A pane can close while its previously rendered result is being chosen.
            picker.matches.retain(|row| row.pane != pane);
            picker.move_by(0);
            return;
        };
        self.tabs.select(id);
        if matches!(self.tabs.get(id), Some(AppTab { page: Page::Terminal(live), .. }) if live.find.open)
        {
            self.find(true, None, ctx);
        }
        self.sync_selection(ctx);
        self.close_picker(ctx);
    }

    pub(super) fn close_picker(&mut self, ctx: &ComponentContext<Self>) {
        if self.picker.is_some() {
            // Searching and moving the highlight never changed the active tab or pane.
            self.activate(ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(pane: u64, title: &str, label: &str, destination: &str) -> Choice {
        Choice {
            pane,
            title: title.into(),
            label: label.into(),
            profile: Some(label.into()),
            destination: destination.into(),
            tab_number: 1,
            pane_number: 1,
        }
    }
    #[test]
    fn search_matches_titles_profiles_and_destinations_with_exact_matches_first() {
        let choices = || {
            vec![
                row(1, "logs", "dev20", "root@host:22"),
                row(2, "BÜILD LOG", "dev2", "pilot@game:6792"),
                row(3, "dev2 cockpit", "play", "pilot@game:6789"),
            ]
        };
        let ids = |query| {
            Picker::filter(choices(), query)
                .into_iter()
                .map(|row| row.pane)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("DEV2"), [2, 1, 3]);
        assert_eq!(ids(" üIlD "), [2]);
        assert_eq!(ids("PILOT@game"), [2, 3]);
        assert!(ids("missing").is_empty());
    }
}
