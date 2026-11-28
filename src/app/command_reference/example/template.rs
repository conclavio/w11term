use super::*;
use std::ops::Range;

const MAX_COMMAND_BYTES: usize = 4096;

#[derive(Clone)]
pub(super) struct Form {
    source: String,
    spans: Vec<(Range<usize>, usize)>,
    fields: Vec<Field>,
}

#[derive(Clone)]
struct Field {
    label: String,
    value: Value,
}

#[derive(Clone)]
enum Value {
    Text(String),
    Flag {
        short: String,
        long: String,
        use_long: bool,
    },
}

impl Value {
    fn from_label(label: &str) -> Self {
        if let Some(choices) = label
            .strip_prefix('[')
            .and_then(|text| text.strip_suffix(']'))
            && let Some((short, long)) = choices.split_once('|')
            && !short.trim().is_empty()
            && !long.trim().is_empty()
            && !long.contains('|')
        {
            return Self::Flag {
                short: short.into(),
                long: long.into(),
                use_long: true,
            };
        }
        Self::Text(label.into())
    }

    fn raw(&self) -> &str {
        match self {
            Self::Text(text) => text,
            Self::Flag {
                short,
                long,
                use_long,
            } => {
                if *use_long {
                    long
                } else {
                    short
                }
            }
        }
    }
}

fn unescape_braces(text: &str) -> String {
    text.replace(r"\{\{", "{{").replace(r"\}\}", "}}")
}

fn content_end(source: &[u8], mut offset: usize) -> Option<usize> {
    let mut braces = 0;
    while offset < source.len() {
        let rest = &source[offset..];
        if rest.starts_with(br"\{\{") || rest.starts_with(br"\}\}") {
            offset += 4;
            continue;
        }
        match rest[0] {
            b'{' => braces += 1,
            b'}' if braces != 0 => braces -= 1,
            b'}' if rest.starts_with(b"}}") => return Some(offset),
            _ => {}
        }
        offset += 1;
    }
    None
}

impl Form {
    pub(super) fn parse(source: &str) -> Option<Self> {
        if !valid_command_text(source) {
            return None;
        }
        let mut fields: Vec<Field> = Vec::new();
        let mut spans = Vec::new();
        let mut next = 0;
        while let Some(open) = source[next..].find("{{") {
            let open = next + open;
            next = open + 2;
            let Some(close) = content_end(source.as_bytes(), next) else {
                continue;
            };
            let label = &source[next..close];
            next = close + 2;
            if label.trim().is_empty() {
                continue;
            }
            let label = unescape_braces(label);
            let index = if let Some(index) = fields.iter().position(|field| field.label == label) {
                index
            } else {
                let index = fields.len();
                let value = Value::from_label(&label);
                fields.push(Field { label, value });
                index
            };
            spans.push((open..next, index));
        }
        (!fields.is_empty()).then(|| Self {
            source: source.into(),
            spans,
            fields,
        })
    }

    pub(super) fn set_text(&mut self, index: usize, text: String) {
        if let Some(Field {
            value: Value::Text(current),
            ..
        }) = self.fields.get_mut(index)
        {
            // Keep an over-limit value invalid even if its suffix is very large.
            *current = text.chars().take(MAX_COMMAND_BYTES + 1).collect();
        }
    }

    pub(super) fn set_option(&mut self, index: usize, choice: usize) {
        if let Some(Field {
            value: Value::Flag { use_long, .. },
            ..
        }) = self.fields.get_mut(index)
            && choice < 2
        {
            *use_long = choice == 1;
        }
    }

    pub(super) fn preview(&self) -> Option<String> {
        let mut result = String::with_capacity(self.source.len());
        let mut previous = 0;
        for (span, field) in &self.spans {
            result.push_str(&unescape_braces(&self.source[previous..span.start]));
            result.push_str(self.fields[*field].value.raw());
            if result.len() > MAX_COMMAND_BYTES {
                return None;
            }
            previous = span.end;
        }
        result.push_str(&unescape_braces(&self.source[previous..]));
        valid_command_text(&result).then_some(result)
    }

    fn first_text(&self) -> Option<usize> {
        self.fields
            .iter()
            .position(|field| matches!(field.value, Value::Text(_)))
    }

    pub(super) fn options_only(&self) -> bool {
        self.first_text().is_none()
    }

    pub(super) fn view(
        &self,
        text_field: &ElementRef<TextBox>,
        option_field: &ElementRef<ComboBox>,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let first_text = self.first_text();
        let mut rows: Vec<_> = self
            .fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let editor = match &field.value {
                    Value::Text(text) => {
                        let mut input = TextBox::new()
                            .automation_name(format!("Value for {}", field.label))
                            .text(text)
                            .on_text_changed(
                                ctx.callback(move |text| message(Action::FillText(index, text))),
                            );
                        if first_text == Some(index) {
                            input = input.element_ref(text_field);
                        }
                        View::from(input)
                    }
                    Value::Flag {
                        short,
                        long,
                        use_long,
                    } => {
                        let mut input = ComboBox::new()
                            .automation_name(format!("Option {}", field.label))
                            .items_source([short.clone(), long.clone()])
                            .selected_index(usize::from(*use_long))
                            .on_selection_changed(ctx.callback(move |choice: Option<usize>| {
                                choice.map_or(Message::ControlIdle, |choice| {
                                    message(Action::FillOption(index, choice))
                                })
                            }));
                        if first_text.is_none() && index == 0 {
                            input = input.element_ref(option_field);
                        }
                        View::from(input)
                    }
                };
                KeyedView::new(
                    index,
                    StackPanel::new().spacing(4.0).children((
                        TextBlock::new()
                            .text(&field.label)
                            .text_wrapping(TextWrapping::Wrap),
                        editor,
                    )),
                )
            })
            .collect();
        let preview = self.preview();
        rows.extend([
            KeyedView::new("preview-label", TextBlock::new().text("Command preview")),
            KeyedView::new(
                "preview",
                TextBlock::new()
                    .automation_id("reference-command-preview")
                    .is_text_selection_enabled(true)
                    .text_wrapping(TextWrapping::Wrap)
                    .text(
                        preview
                            .as_deref()
                            .unwrap_or("Use a single command line up to 4,096 UTF-8 bytes."),
                    ),
            ),
            KeyedView::new(
                "apply",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .is_enabled(preview.is_some())
                            .on_click(ctx.callback(|_| message(Action::ApplyFill)))
                            .content("Apply values"),
                        Button::new()
                            .automation_name("Cancel placeholder values")
                            .on_click(ctx.callback(|_| message(Action::CancelFill)))
                            .content("Cancel"),
                    )),
            ),
        ]);
        StackPanel::new().spacing(8.0).keyed_children(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_values_and_flag_choices_preserve_the_rest_of_the_command() {
        let mut form =
            Form::parse(r#"tool {{[-S|--sort]}} "{{name}}" {{name}} {{[-S|--sort]}}"#).unwrap();
        assert_eq!(form.fields.len(), 2);
        assert_eq!(
            form.preview().as_deref(),
            Some(r#"tool --sort "name" name --sort"#)
        );
        form.set_option(0, 0);
        form.set_text(1, "{{literal}}".into());
        assert_eq!(
            form.preview().as_deref(),
            Some(r#"tool -S "{{literal}}" {{literal}} -S"#)
        );
    }

    #[test]
    fn literal_and_nested_braces_follow_the_example_syntax() {
        let form = Form::parse(r"tool '\{\{.ID\}\}' {{stash@{0}}} \{{path}} {{clé}}").unwrap();
        assert_eq!(form.fields.len(), 3);
        assert_eq!(
            form.preview().as_deref(),
            Some(r"tool '{{.ID}}' stash@{0} \path clé")
        );
        let json = Form::parse(r#"tool '{{{"name":"bob"}}}' {{target}}"#).unwrap();
        assert_eq!(
            json.preview().as_deref(),
            Some(r#"tool '{"name":"bob"}' target"#)
        );
        assert!(Form::parse(r"tool '\{\{.ID\}\}'").is_none());
    }

    #[test]
    fn rejects_invalid_lines_including_a_repeated_value_beyond_the_byte_limit() {
        let mut form = Form::parse("tool {{value}} {{value}}").unwrap();
        form.set_text(0, "é".repeat(1200));
        assert!(form.preview().is_none());
        form.set_text(0, "line\nbreak".into());
        assert!(form.preview().is_none());
    }
}
