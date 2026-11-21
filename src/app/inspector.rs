use super::*;
use termwiz::input::KeyboardEncoding;
use w11term::terminal::TerminalModes;
use wezterm_input_types::KittyKeyboardFlags as Flags;
use wezterm_term::MouseEncoding;

pub(super) struct Inspection {
    pub(super) pane: u64,
    pub(super) modes: Option<TerminalModes>,
    input: tokio::sync::mpsc::UnboundedSender<Command>,
    readout: View,
}

impl Drop for Inspection {
    fn drop(&mut self) {
        let _ = self.input.send(Command::Inspect(false));
    }
}

fn keyboard_label(encoding: KeyboardEncoding) -> String {
    match encoding {
        KeyboardEncoding::Xterm => "Xterm".into(),
        KeyboardEncoding::CsiU => "CSI u".into(),
        KeyboardEncoding::Win32 => "Win32".into(),
        KeyboardEncoding::Kitty(flags) => {
            let names = [
                (Flags::DISAMBIGUATE_ESCAPE_CODES, "disambiguate"),
                (Flags::REPORT_EVENT_TYPES, "event types"),
                (Flags::REPORT_ALTERNATE_KEYS, "alternate keys"),
                (Flags::REPORT_ALL_KEYS_AS_ESCAPE_CODES, "all keys"),
                (Flags::REPORT_ASSOCIATED_TEXT, "associated text"),
            ];
            let names: Vec<_> = names
                .into_iter()
                .filter_map(|(flag, name)| flags.contains(flag).then_some(name))
                .collect();
            if names.is_empty() {
                "Kitty 0".into()
            } else {
                format!("Kitty {} ({})", flags.bits(), names.join(", "))
            }
        }
    }
}

fn mouse_label(modes: TerminalModes) -> String {
    let modes_enabled: Vec<_> = [
        (modes.mouse_click, "clicks"),
        (modes.mouse_drag, "drag"),
        (modes.mouse_motion, "all motion"),
    ]
    .into_iter()
    .filter_map(|(enabled, label)| enabled.then_some(label))
    .collect();
    let encoding = match modes.mouse_encoding {
        MouseEncoding::X10 => "X10",
        MouseEncoding::Utf8 => "UTF-8",
        MouseEncoding::SGR => "SGR cells",
        MouseEncoding::SgrPixels => "SGR pixels",
    };
    let reporting = if modes_enabled.is_empty() {
        "Off".into()
    } else {
        modes_enabled.join(", ")
    };
    format!("{reporting} ({encoding})")
}

impl Inspection {
    pub(super) fn refresh(&mut self, current: Option<TerminalModes>) {
        if self.modes == current {
            return;
        }
        self.modes = current;
        match current {
            None => self.readout = TextBlock::new().text("Reading terminal modes…").into(),
            Some(modes) => {
                let on_off = |enabled| if enabled { "On" } else { "Off" };
                let screen = if modes.alternate {
                    "Alternate"
                } else {
                    "Primary"
                };
                let rows = [
                    format!("Grid   {} × {}", modes.cols, modes.rows),
                    format!("Screen   {screen}"),
                    format!("Mouse reporting   {}", mouse_label(modes)),
                    format!("Keyboard   {}", keyboard_label(modes.keyboard)),
                    format!("Bracketed paste   {}", on_off(modes.bracketed_paste)),
                    format!("Synchronized output   {}", on_off(modes.synchronized)),
                ];
                self.readout = StackPanel::new().spacing(3.0).keyed_children(
                    rows.into_iter().enumerate().map(|(i, row)| {
                        KeyedView::new(
                            i.to_string(),
                            TextBlock::new().text(row).text_wrapping(TextWrapping::Wrap),
                        )
                    }),
                );
            }
        }
    }

    pub(super) fn view(&self, context: &ViewContext<TerminalApp>) -> View {
        let title = format!("Terminal inspector · Pane {}", self.pane);
        Border::new()
            .grid_row(1)
            .horizontal_alignment(HorizontalAlignment::Right)
            .vertical_alignment(VerticalAlignment::Bottom)
            .margin(8.0)
            .padding(12.0)
            .width(310.0)
            .corner_radius(6.0)
            .background(Color::rgb(38, 40, 45))
            .content(
                StackPanel::new().spacing(6.0).children((
                    Grid::new()
                        .columns([GridLength::STAR, GridLength::Auto])
                        .children((
                            TextBlock::new()
                                .text(title)
                                .vertical_alignment(VerticalAlignment::Center),
                            Button::new()
                                .grid_column(1)
                                .style(ButtonStyle::Subtle)
                                .automation_name("Close terminal inspector")
                                .on_click(context.callback(|_| Message::ToggleInspector))
                                .content("×"),
                        )),
                    self.readout.clone(),
                )),
            )
    }
}

impl TerminalApp {
    pub(super) fn sync_inspector(&mut self) {
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.active().filter(|_| self.show_inspector)
        else {
            self.inspection = None;
            return;
        };
        let pane = tab.session.pane_id;
        if self
            .inspection
            .as_ref()
            .is_some_and(|current| current.pane == pane)
        {
            return;
        }
        let input = tab.session.input.clone();
        let status = if input.send(Command::Inspect(true)).is_ok() {
            "Reading terminal modes…"
        } else {
            "Terminal worker has stopped"
        };
        self.inspection = Some(Inspection {
            pane,
            modes: None,
            input,
            readout: TextBlock::new().text(status).into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cleared_observation_advances_the_watcher_baseline() {
        let (input, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut inspector = Inspection {
            pane: 1,
            modes: None,
            input,
            readout: TextBlock::new().text("Waiting").into(),
        };
        let modes = TerminalModes {
            cols: 80,
            rows: 24,
            alternate: false,
            mouse_click: false,
            mouse_drag: false,
            mouse_motion: false,
            mouse_encoding: MouseEncoding::X10,
            keyboard: KeyboardEncoding::Xterm,
            bracketed_paste: false,
            synchronized: false,
        };
        inspector.refresh(Some(modes));
        assert_eq!(inspector.modes, Some(modes));
        inspector.refresh(None);
        assert!(inspector.modes.is_none());
        inspector.refresh(Some(modes));
        assert_eq!(inspector.modes, Some(modes));
    }
}
