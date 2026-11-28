use super::*;
use std::{fmt::Write as _, ops::Range};

#[derive(Clone)]
pub(super) struct Source {
    pub(super) ticket: Arc<()>,
    label: String,
    text: String,
    lines: usize,
    first: String,
    last: String,
    status: Option<String>,
    menu: OnceCell<Menu>,
}

impl Source {
    pub(super) fn new(endpoint: &str, path: String, text: String) -> anyhow::Result<Self> {
        let text = text
            .strip_prefix('\u{feff}')
            .unwrap_or(&text)
            .replace("\r\n", "\n");
        let lines = text.lines().count();
        anyhow::ensure!(lines != 0, "The SSH text preview is empty");
        let path = path.replace(char::is_control, " ");
        Ok(Self {
            ticket: Arc::new(()),
            label: format!("SSH excerpt from {endpoint} · {path} · {lines} lines"),
            text,
            lines,
            first: "1".into(),
            last: lines.to_string(),
            status: None,
            menu: OnceCell::new(),
        })
    }

    fn excerpt(&self) -> anyhow::Result<(usize, usize, Range<usize>)> {
        let first = self.first.trim().parse::<usize>().ok();
        let last = self.last.trim().parse::<usize>().ok();
        let (first, last) = first
            .zip(last)
            .filter(|&(first, last)| first != 0 && first <= last && last <= self.lines)
            .with_context(|| format!("Enter a line range within 1–{}", self.lines))?;
        let start: usize = self
            .text
            .split_inclusive('\n')
            .take(first - 1)
            .map(str::len)
            .sum();
        let bytes: usize = self.text[start..]
            .split_inclusive('\n')
            .take(last - first + 1)
            .map(str::len)
            .sum();
        Ok((first, last, start..start + bytes))
    }

    pub(super) fn edit(&mut self, action: ExcerptAction, fragments: &mut [Option<String>; 3]) {
        match action {
            ExcerptAction::First(value) => {
                self.first = value;
                self.status = None;
            }
            ExcerptAction::Last(value) => {
                self.last = value;
                self.status = None;
            }
            ExcerptAction::Append(index) => {
                let result = self.excerpt().and_then(|(first, last, range)| {
                    let existing = fragment_text(fragments[index].as_deref().unwrap_or_default());
                    let merged =
                        DotfileBundle::append_startup_excerpt(&existing, &self.text, range)?;
                    fragments[index] = Some(editor_text(&merged));
                    Ok(format!(
                        "Appended SSH lines {first}–{last} to {}",
                        FRAGMENTS[index]
                    ))
                });
                self.status = Some(result.unwrap_or_else(|error| format!("{error:#}")));
            }
            ExcerptAction::Close => unreachable!(),
        }
    }

    pub(super) fn view(
        &self,
        id: TabId,
        ticket: &Arc<()>,
        ready: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let range_field = |name: &'static str, value: &str, action: fn(String) -> ExcerptAction| {
            let ticket = ticket.clone();
            let source = self.ticket.clone();
            StackPanel::new().spacing(4.0).children((
                TextBlock::new().text(name),
                TextBox::new()
                    .automation_name(name)
                    .text(value)
                    .min_width(105.0)
                    .is_enabled(ready)
                    .on_text_changed(ctx.callback(move |value| {
                        message(
                            id,
                            Action::Excerpt(ticket.clone(), source.clone(), action(value)),
                        )
                    })),
            ))
        };
        let menu = self.menu.get_or_init(|| {
            let ticket = ticket.clone();
            let source = self.ticket.clone();
            Menu::new(
                IMPORT_ITEMS.map(|name| MenuItem::item(name, name)),
                ctx.callback(move |name: String| {
                    IMPORT_ITEMS.iter().position(|item| *item == name).map_or(
                        Message::ControlIdle,
                        |index| {
                            message(
                                id,
                                Action::Excerpt(
                                    ticket.clone(),
                                    source.clone(),
                                    ExcerptAction::Append(index),
                                ),
                            )
                        },
                    )
                }),
            )
        });
        let close = ticket.clone();
        let source = self.ticket.clone();
        let mut fields = vec![
            KeyedView::new(
                "source",
                TextBlock::new()
                    .text(&self.label)
                    .text_wrapping(TextWrapping::Wrap),
            ),
            KeyedView::new(
                "range",
                StackPanel::new()
                    .spacing(8.0)
                    .orientation(Orientation::Horizontal)
                    .children((
                        range_field("First SSH line", &self.first, ExcerptAction::First),
                        range_field("Last SSH line", &self.last, ExcerptAction::Last),
                    )),
            ),
        ];
        if let Ok((first, last, range)) = self.excerpt() {
            let text = &self.text[range];
            let prefix = &text[..text.floor_char_boundary(2048)];
            let mut preview = String::new();
            for (offset, line) in prefix.lines().take(12).enumerate() {
                let _ = writeln!(preview, "{}  {line}", first + offset);
            }
            if prefix.len() != text.len() || last - first >= 12 {
                preview.push('…');
            }
            fields.push(KeyedView::new(
                "preview",
                ScrollViewer::new()
                    .max_height(150.0)
                    .horizontal_scroll_bar_visibility(ScrollBarVisibility::Auto)
                    .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                    .content(
                        TextBlock::new()
                            .automation_name("Selected SSH lines")
                            .is_text_selection_enabled(true)
                            .text(preview),
                    ),
            ));
        }
        if let Some(status) = &self.status {
            fields.push(KeyedView::new(
                "status",
                TextBlock::new()
                    .text(status)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        fields.push(KeyedView::new(
            "actions",
            StackPanel::new()
                .spacing(8.0)
                .orientation(Orientation::Horizontal)
                .children((
                    Button::new()
                        .is_enabled(ready)
                        .automation_name("Append SSH lines")
                        .content("Append SSH lines")
                        .menu(menu.clone()),
                    Button::new()
                        .is_enabled(ready)
                        .on_click(ctx.callback(move |_| {
                            message(
                                id,
                                Action::Excerpt(
                                    close.clone(),
                                    source.clone(),
                                    ExcerptAction::Close,
                                ),
                            )
                        }))
                        .content("Close SSH excerpt"),
                )),
        ));
        StackPanel::new().spacing(8.0).keyed_children(fields)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_only_the_inclusive_range_and_preserves_the_last_line_ending() {
        let mut source = Source::new(
            "fixture",
            "/home/dev/.bashrc".into(),
            "\u{feff}one\r\n雪\r\nthree".into(),
        )
        .unwrap();
        source.first = "2".into();
        source.last = "2".into();
        let (first, last, range) = source.excerpt().unwrap();
        assert_eq!((first, last, &source.text[range]), (2, 2, "雪\n"));
        source.last = "3".into();
        let (first, last, range) = source.excerpt().unwrap();
        assert_eq!((first, last, &source.text[range]), (2, 3, "雪\nthree"));
        source.first = "4".into();
        assert!(source.excerpt().is_err());
        source.first = "0".into();
        assert!(source.excerpt().is_err());
    }
}
