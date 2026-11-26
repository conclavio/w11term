use super::*;
use termwiz::input::KeyboardEncoding;
use w11term::events::SshConnectionPhase;
use w11term::session::PaneInspection;
use w11term::ssh::{
    AdditionalSshAuthentication, ConnectionStep, SshKeySource, SshTransport,
    VerifiedSshAuthentication, VerifiedSshPeer, VerifiedSshRoute,
};
use w11term::terminal::TerminalModes;
use wezterm_input_types::KittyKeyboardFlags as Flags;
use wezterm_term::MouseEncoding;

pub(super) struct Inspection {
    pub(super) pane: u64,
    pub(super) current: Option<PaneInspection>,
    ssh_expanded: bool,
    input: tokio::sync::mpsc::UnboundedSender<Command>,
    readout: View,
    pointer: PointerReadout,
    running_timer: Option<crate::ui_timer::RetryTimer>,
}

#[derive(Default)]
struct PointerReadout {
    position: Option<(f64, f64)>,
    cell: Option<(usize, usize)>,
    target: Option<String>,
}

// Keep remote labels to one short line of plain text.
pub(super) fn target_label(uri: &str) -> String {
    let mut chars = uri.chars().map(|c| match c {
        '\u{061c}'
        | '\u{200e}'
        | '\u{200f}'
        | '\u{202a}'..='\u{202e}'
        | '\u{2066}'..='\u{2069}'
        | '\u{2028}'
        | '\u{2029}' => '�',
        c if c.is_control() => '�',
        c => c,
    });
    let mut label: String = chars.by_ref().take(160).collect();
    if chars.next().is_some() {
        label.push('…');
    }
    label
}

impl Drop for Inspection {
    fn drop(&mut self) {
        let _ = self.input.send(Command::Inspect(false));
    }
}

fn keyboard_label(modes: TerminalModes) -> String {
    match modes.keyboard {
        KeyboardEncoding::Xterm if modes.xterm_other_keys != (0, false) => {
            let (level, csi_u) = modes.xterm_other_keys;
            let format = if csi_u { "CSI u" } else { "CSI 27" };
            format!("Xterm · modifyOtherKeys {level} · {format}")
        }
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

fn text_area_label(width: usize, height: usize, dpi: u32) -> String {
    match (width, height, dpi) {
        (0, _, _) | (_, 0, _) => "Text area   Awaiting measurement".into(),
        (_, _, 0) => format!("Text area   {width} × {height} px"),
        _ => format!("Text area   {width} × {height} px at {dpi} DPI"),
    }
}

fn ssh_status_view(value: &PaneInspection) -> View {
    use SshConnectionPhase::*;
    let Some(phase) = &value.ssh_connection else {
        return View::empty();
    };
    let phase_label = match phase {
        Queued => "Queued".into(),
        Connecting => "Connecting".into(),
        Reconnecting {
            attempt: Some(attempt),
        } => format!("Reconnecting · attempt {attempt}"),
        Reconnecting { attempt: None } => "Reconnecting".into(),
        Connected => "Connected".into(),
        Disconnected => "Disconnected".into(),
        Stopped => "Stopped".into(),
        Failed(_) => "Failed".into(),
    };
    let step = value
        .ssh_progress
        .as_ref()
        .map_or_else(View::empty, |progress| {
            let action = match progress.step {
                ConnectionStep::OpeningTcp => "Opening TCP connection",
                ConnectionStep::OpeningJumpChannel => "Opening tunnel through the previous jump",
                ConnectionStep::KeyExchange => "Exchanging SSH keys",
                ConnectionStep::Authenticating => "Authenticating SSH user",
                ConnectionStep::OpeningSessionChannel => "Opening SSH session channel",
                ConnectionStep::RequestingTerminal => "Requesting PTY and shell",
                ConnectionStep::PreparingRemoteProgram => "Preparing remote session",
            };
            let description = if matches!(phase, Failed(_)) {
                format!("Last step   {action}")
            } else {
                action.to_owned()
            };
            let role = progress
                .jump
                .map_or_else(|| "Destination".into(), |i| format!("Jump {i}"));
            let address = host_groups::ssh_address(
                &target_label(&progress.user),
                &target_label(&progress.host),
                progress.port,
            );
            StackPanel::new().spacing(3.0).children((
                TextBlock::new()
                    .text(description)
                    .text_wrapping(TextWrapping::Wrap),
                TextBlock::new()
                    .text(format!("{role}   {address}"))
                    .text_wrapping(TextWrapping::Wrap),
            ))
        });
    StackPanel::new().spacing(3.0).children((
        TextBlock::new()
            .text(format!("SSH connection   {phase_label}"))
            .text_wrapping(TextWrapping::Wrap),
        step,
        if let Failed(reason) = phase {
            TextBlock::new()
                .text(reason.as_ref())
                .text_wrapping(TextWrapping::Wrap)
                .into()
        } else {
            View::empty()
        },
    ))
}

fn ssh_authentication_view(authentication: &VerifiedSshAuthentication) -> View {
    let row = |label: &str, value: &str| {
        TextBlock::new()
            .text(format!("{label}   {value}"))
            .text_wrapping(TextWrapping::Wrap)
    };
    let method = match authentication {
        VerifiedSshAuthentication::None => "None",
        VerifiedSshAuthentication::Password => "Password",
        VerifiedSshAuthentication::KeyboardInteractive => "Keyboard-interactive",
        VerifiedSshAuthentication::PublicKey {
            source,
            key_fingerprint,
            certificate_authority_fingerprint,
            additional_methods,
        } => {
            let key_source = match source {
                SshKeySource::File => "key file",
                SshKeySource::Agent => "SSH agent",
            };
            let kind = if certificate_authority_fingerprint.is_some() {
                "User certificate"
            } else {
                "Public key"
            };
            let mut method = format!("{kind} ({key_source})");
            for additional in additional_methods {
                method.push_str(match additional {
                    AdditionalSshAuthentication::Password => " + Password",
                    AdditionalSshAuthentication::KeyboardInteractive => " + Keyboard-interactive",
                });
            }
            return StackPanel::new().spacing(3.0).children((
                row("Authentication", &method),
                row("User key", key_fingerprint),
                certificate_authority_fingerprint
                    .as_deref()
                    .map_or_else(View::empty, |fingerprint| {
                        row("User certificate CA key", fingerprint).into()
                    }),
            ));
        }
    };
    row("Authentication", method).into()
}

fn ssh_route_view(
    route: &VerifiedSshRoute,
    pane: u64,
    expanded: bool,
    context: &ViewContext<TerminalApp>,
) -> View {
    const LABEL: &str = "Verified SSH route";
    let peer = |role: &str, peer: &VerifiedSshPeer| {
        let address = host_groups::ssh_address(&peer.user, &peer.host, peer.port);
        StackPanel::new().spacing(3.0).children((
            TextBlock::new()
                .text(format!("{role}   {address}"))
                .text_wrapping(TextWrapping::Wrap),
            TextBlock::new()
                .text(format!("Host key   {}", peer.key_fingerprint))
                .text_wrapping(TextWrapping::Wrap),
            ssh_authentication_view(&peer.authentication),
            peer.host_key_alias
                .as_ref()
                .map_or_else(View::empty, |alias| {
                    TextBlock::new()
                        .text(format!("Host key alias   {alias}"))
                        .text_wrapping(TextWrapping::Wrap)
                        .into()
                }),
            peer.initial_algorithms
                .as_ref()
                .map_or_else(View::empty, |algorithms| {
                    let strict = if algorithms.strict_key_exchange {
                        "On"
                    } else {
                        "Off"
                    };
                    StackPanel::new().spacing(3.0).keyed_children(
                        [
                            ("kex", format!("Key exchange   {}", algorithms.key_exchange)),
                            (
                                "host-key-algorithm",
                                format!("Host-key algorithm   {}", algorithms.host_key),
                            ),
                            ("cipher", format!("Cipher   {}", algorithms.cipher)),
                            ("strict-kex", format!("Strict key exchange   {strict}")),
                        ]
                        .into_iter()
                        .map(|(key, label)| {
                            KeyedView::new(
                                key,
                                TextBlock::new()
                                    .text(label)
                                    .text_wrapping(TextWrapping::Wrap),
                            )
                        }),
                    )
                }),
        ))
    };
    let hosts = std::iter::once(KeyedView::new(
        "destination",
        peer("Destination", &route.destination),
    ))
    .chain(
        route
            .jump_hosts
            .iter()
            .enumerate()
            .map(|(i, hop)| KeyedView::new(i, peer(&format!("Jump {}", i + 1), hop))),
    );
    Expander::new()
        .automation_name(LABEL)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .is_expanded(expanded)
        .on_is_expanded_changed(context.callback(move |open| Message::InspectSshRoute(pane, open)))
        .slots([
            SlotView::new(ExpanderSlot::Header, TextBlock::new().text(LABEL)),
            SlotView::new(
                ExpanderSlot::Content,
                StackPanel::new().spacing(8.0).keyed_children(hosts),
            ),
        ])
}

fn ssh_transport_view(transport: &SshTransport, pane: u64) -> View {
    let others = transport
        .pane_ids
        .iter()
        .filter(|&&member| member != pane)
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let members = if others.is_empty() { "None" } else { &others };
    StackPanel::new().spacing(3.0).children((
        TextBlock::new().text(format!("SSH transport   #{}", transport.id)),
        TextBlock::new()
            .text(format!("Other panes   {members}"))
            .text_wrapping(TextWrapping::Wrap),
    ))
}

fn running_command_view(command: &wezterm_term::RunningCommand) -> View {
    let label = command
        .command
        .as_deref()
        .map_or_else(|| "Command line unavailable".into(), target_label);
    let started = command_history::timestamp_label(command.started_unix_ms).map_or_else(
        View::empty,
        |timestamp| {
            TextBlock::new()
                .text(format!("Started   {timestamp}"))
                .into()
        },
    );
    let directory = command
        .reported_directory
        .as_deref()
        .map_or_else(View::empty, |name| {
            TextBlock::new()
                .text(format!("Starting directory   {}", target_label(name)))
                .text_wrapping(TextWrapping::Wrap)
                .into()
        });
    StackPanel::new().spacing(3.0).children((
        TextBlock::new()
            .text(format!("Running command   {label}"))
            .text_wrapping(TextWrapping::Wrap),
        started,
        directory,
        TextBlock::new().text(format!("Elapsed   {} s", command.elapsed_ms() / 1000)),
    ))
}

impl Inspection {
    pub(super) fn sync_running_timer(&mut self, context: &ComponentContext<TerminalApp>) {
        if self
            .current
            .as_ref()
            .and_then(|value| value.running_command.as_ref())
            .is_none()
        {
            self.running_timer = None;
        } else if self.running_timer.is_none() {
            let sender = context.sender();
            self.running_timer = crate::ui_timer::RetryTimer::with_next_delay(1000, move || {
                sender.send(Message::InspectorTick);
                1000
            })
            .ok();
        }
    }

    pub(super) fn set_ssh_expanded(&mut self, expanded: bool) {
        self.ssh_expanded = expanded
            && self
                .current
                .as_ref()
                .is_some_and(|value| value.ssh.is_some());
    }

    pub(super) fn clear_pointer(&mut self) {
        self.pointer = Default::default();
    }

    pub(super) fn observe_pointer(
        &mut self,
        pos: (f64, f64),
        hit: &crate::renderer::CellHit,
        frame: &w11term::terminal::Snapshot,
        link: Option<&Link>,
    ) {
        self.pointer.position = Some(pos);
        if hit.inside && hit.col < frame.cols && hit.row < frame.rows.len() {
            self.pointer.cell = Some((hit.col + 1, hit.row + 1));
            self.pointer.target = link.map(|value| target_label(&value.uri));
        } else {
            self.pointer.cell = None;
            self.pointer.target = None;
        }
    }

    pub(super) fn refresh_pointer(
        &mut self,
        metrics: Metrics,
        frame: &w11term::terminal::Snapshot,
        detect_urls: bool,
    ) {
        if let Some((x, y)) = self.pointer.position {
            let hit = metrics.hit(x, y);
            let link = if hit.inside {
                link_at(frame, hit.point(frame), detect_urls)
            } else {
                None
            };
            self.observe_pointer((x, y), &hit, frame, link.as_ref());
        }
    }

    pub(super) fn refresh(&mut self, current: Option<PaneInspection>) {
        let previous_modes = self.current.as_ref().map(|previous| previous.modes);
        let modes = current.as_ref().map(|inspection| inspection.modes);
        self.current = current;
        self.set_ssh_expanded(self.ssh_expanded);
        if previous_modes == modes {
            return;
        }
        match modes {
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
                    text_area_label(modes.pixel_width, modes.pixel_height, modes.dpi),
                    format!("Screen   {screen}"),
                    format!("Mouse reporting   {}", mouse_label(modes)),
                    format!("Keyboard   {}", keyboard_label(modes)),
                    format!("Bracketed paste   {}", on_off(modes.bracketed_paste)),
                    format!("Synchronized output   {}", on_off(modes.synchronized)),
                    format!("In-band resize   {}", on_off(modes.in_band_resize)),
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

    pub(super) fn view(&self, window_height: f64, context: &ViewContext<TerminalApp>) -> View {
        let title = format!("Terminal inspector · Pane {}", self.pane);
        let ssh = self
            .current
            .as_ref()
            .and_then(|value| value.ssh.as_deref())
            .map_or_else(View::empty, |route| {
                ssh_route_view(route, self.pane, self.ssh_expanded, context)
            });
        let transport = self
            .current
            .as_ref()
            .and_then(|value| value.ssh_transport.as_deref())
            .map_or_else(View::empty, |transport| {
                ssh_transport_view(transport, self.pane)
            });
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
                    ScrollViewer::new()
                        .max_height((window_height - 130.0).max(80.0))
                        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                        .content(
                            StackPanel::new().spacing(6.0).children((
                                self.current
                                    .as_ref()
                                    .map_or_else(View::empty, ssh_status_view),
                                self.current
                                    .as_ref()
                                    .and_then(|value| value.running_command.as_ref())
                                    .map_or_else(View::empty, running_command_view),
                                transport,
                                ssh,
                                self.readout.clone(),
                                TextBlock::new().text(self.pointer.cell.map_or_else(
                                    || "Pointer cell   Outside grid".into(),
                                    |(col, row)| format!("Pointer cell   {col}, {row}"),
                                )),
                                TextBlock::new()
                                    .text(format!(
                                        "Hyperlink   {}",
                                        self.pointer.target.as_deref().unwrap_or("None")
                                    ))
                                    .text_wrapping(TextWrapping::Wrap),
                            )),
                        ),
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
            current: None,
            ssh_expanded: false,
            input,
            readout: TextBlock::new().text(status).into(),
            pointer: Default::default(),
            running_timer: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_area_displays_available_measurements() {
        assert_eq!(
            text_area_label(0, 480, 96),
            "Text area   Awaiting measurement"
        );
        assert_eq!(
            text_area_label(800, 0, 0),
            "Text area   Awaiting measurement"
        );
        assert_eq!(text_area_label(800, 480, 0), "Text area   800 × 480 px");
        assert_eq!(
            text_area_label(800, 480, 144),
            "Text area   800 × 480 px at 144 DPI"
        );
    }

    #[test]
    fn viewport_cells_use_pane_dips_and_resolve_the_visible_scrollback_frame() {
        let (input, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut inspector = Inspection {
            pane: 1,
            current: None,
            ssh_expanded: false,
            input,
            readout: TextBlock::new().into(),
            pointer: Default::default(),
            running_timer: None,
        };
        let (writer, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = w11term::terminal::create(
            TerminalSize {
                rows: 3,
                cols: 40,
                ..Default::default()
            },
            writer,
        );
        terminal.advance_bytes(b"one\r\ntwo\r\nthree\r\nhttps://scroll.example/\r\nlive\r\nend");
        let frame = |offset| {
            w11term::terminal::Snapshot::capture(&terminal, None, 1, offset, String::new(), 0)
        };
        let scrolled = frame(1);
        assert!(scrolled.first_stable_row > 0);
        let metrics = Metrics {
            cell_width: 8.5,
            cell_height: 16.25,
            cols: 40,
            rows: 3,
            scale: 1.5,
        };
        let pos = (
            (PADDING + 2.5 * metrics.cell_width) as f64,
            (PADDING + 1.5 * metrics.cell_height) as f64,
        );
        let hit = metrics.hit(pos.0, pos.1);
        assert_eq!(hit.point(&scrolled).row, scrolled.first_stable_row + 1);
        inspector.pointer.position = Some(pos);
        inspector.refresh_pointer(metrics, &scrolled, true);
        assert_eq!(inspector.pointer.cell, Some((3, 2)));
        assert_eq!(
            inspector.pointer.target.as_deref(),
            Some("https://scroll.example/")
        );
        inspector.refresh_pointer(metrics, &scrolled, false);
        assert!(inspector.pointer.target.is_none());
        inspector.refresh_pointer(metrics, &frame(0), true);
        assert!(inspector.pointer.target.is_none());
        for (x, y) in [
            (pos.0, 0.0),
            (0.0, pos.1),
            (pos.0, (PADDING + 3.0 * metrics.cell_height) as f64),
            ((PADDING + 40.0 * metrics.cell_width) as f64, pos.1),
        ] {
            inspector.pointer.position = Some((x, y));
            inspector.refresh_pointer(metrics, &scrolled, true);
            assert!(inspector.pointer.cell.is_none());
        }
        inspector.clear_pointer();
        assert!(inspector.pointer.position.is_none());
    }

    #[test]
    fn remote_targets_are_bounded_single_line_plain_text() {
        let label = target_label(&format!("https://example/\u{202e}\n{}", "é".repeat(400)));
        assert_eq!(label.chars().count(), 161);
        assert!(label.starts_with("https://example/��"));
        assert!(label.ends_with('…'));
    }

    #[test]
    fn a_cleared_observation_advances_the_watcher_baseline() {
        let (input, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut inspector = Inspection {
            pane: 1,
            current: None,
            ssh_expanded: false,
            input,
            readout: TextBlock::new().text("Waiting").into(),
            pointer: Default::default(),
            running_timer: None,
        };
        let modes = TerminalModes {
            cols: 80,
            rows: 24,
            pixel_width: 640,
            pixel_height: 480,
            dpi: 96,
            alternate: false,
            mouse_click: false,
            mouse_drag: false,
            mouse_motion: false,
            mouse_encoding: MouseEncoding::X10,
            keyboard: KeyboardEncoding::Xterm,
            xterm_other_keys: (0, false),
            bracketed_paste: false,
            synchronized: false,
            in_band_resize: false,
        };
        let observation = PaneInspection {
            connection_generation: 0,
            modes,
            running_command: None,
            ssh: None,
            ssh_transport: None,
            ssh_connection: None,
            ssh_progress: None,
        };
        inspector.refresh(Some(observation.clone()));
        assert_eq!(inspector.current, Some(observation.clone()));
        inspector.refresh(None);
        assert!(inspector.current.is_none());
        inspector.refresh(Some(observation.clone()));
        assert_eq!(inspector.current, Some(observation));
    }
}
