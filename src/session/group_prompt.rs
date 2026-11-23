use crate::{events::Events, options::GroupPromptPattern};
use termwiz::surface::CursorVisibility;
use wezterm_term::{Line, Terminal};

pub(super) struct Matcher {
    pattern: GroupPromptPattern,
    observed: Option<MatchedLine>,
    sync_since: Option<usize>,
}

#[derive(PartialEq, Eq)]
struct MatchedLine {
    epoch: u64,
    row: isize,
    col: usize,
    text: String,
}

impl Matcher {
    pub(super) fn new(pattern: GroupPromptPattern) -> Self {
        Self {
            pattern,
            observed: None,
            sync_since: None,
        }
    }

    pub(super) fn reset(&mut self) {
        self.observed = None;
        self.sync_since = None;
    }

    pub(super) fn input_sent(&mut self) {
        // A synchronized prompt printed before input cannot authorize another write.
        self.sync_since = None;
    }

    pub(super) fn still_matches(&self, terminal: &Terminal) -> bool {
        self.observed.is_some() && self.candidate(terminal) == self.observed
    }

    pub(super) fn resized(&mut self, terminal: &Terminal, matched_before: bool, events: &Events) {
        // Reflow changes row coordinates. Carry a live observation through the
        // local resize only if the same prompt text is still at the cursor.
        self.observed = if matched_before {
            self.candidate(terminal).filter(|line| {
                self.observed
                    .as_ref()
                    .is_some_and(|old| old.text == line.text)
            })
        } else {
            None
        };
        if self.observed.is_none() {
            events.invalidate_matched_shell_prompt();
        }
    }

    pub(super) fn observe_output(&mut self, terminal: &Terminal, before: usize, events: &Events) {
        let candidate = self.candidate(terminal);
        if candidate != self.observed {
            self.observed = None;
            events.invalidate_matched_shell_prompt();
        }
        if terminal.synchronized_output() {
            self.sync_since.get_or_insert(before);
            return;
        }
        let since = self.sync_since.take().unwrap_or(before);
        if let Some(position) = candidate
            && terminal.last_primary_print().is_some_and(|print| {
                print.seqno > since
                    && position.epoch == print.row_epoch
                    && position.row == print.row
                    && position.col == print.end_col
            })
        {
            self.observed = Some(position);
            events.observe_matched_shell_prompt();
        }
    }

    fn candidate(&self, terminal: &Terminal) -> Option<MatchedLine> {
        const MAX_PROMPT_BYTES: usize = 512;
        if terminal.is_alt_screen_active() || terminal.synchronized_output() {
            return None;
        }
        let cursor = terminal.cursor_pos();
        let screen = terminal.screen();
        let phys = screen.phys_row(cursor.y);
        let line = screen.physical_lines().nth(phys)?;
        if cursor.visibility != CursorVisibility::Visible
            || cursor.x == 0
            || cursor.x > MAX_PROMPT_BYTES
            || line.last_cell_was_wrapped()
            || phys
                .checked_sub(1)
                .and_then(|row| screen.physical_lines().nth(row))
                .is_some_and(Line::last_cell_was_wrapped)
        {
            return None;
        }
        let mut text = String::new();
        let mut end_col = 0;
        for cell in line.visible_cells() {
            if cell.cell_index() >= cursor.x {
                if cell.str() != " " {
                    return None;
                }
                continue;
            }
            if cell.cell_index() + cell.width() > cursor.x
                || text.len() + cell.str().len() > MAX_PROMPT_BYTES
            {
                return None;
            }
            text.push_str(cell.str());
            if cell.str() != " " {
                end_col = cell.cell_index() + cell.width();
            }
        }
        text.truncate(text.trim_end_matches(' ').len());
        if !self.pattern.matches(&text) {
            return None;
        }
        Some(MatchedLine {
            epoch: screen.row_epoch(),
            row: screen.visible_row_to_stable_row(cursor.y),
            col: end_col,
            text,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{GroupExecution, PromptToken, ShellPrompt};
    use tokio::sync::{mpsc, watch};

    fn fixture() -> (Terminal, Matcher, Events) {
        let events = Events::default();
        events.command_stream(Some(1));
        let (writer, _) = mpsc::unbounded_channel();
        let (notices, _) = mpsc::channel(1);
        let mut terminal = crate::terminal::create(Default::default(), writer);
        terminal.set_notification_handler(Box::new(crate::sound::Alerts {
            events: events.clone(),
            notices,
            player: Default::default(),
        }));
        terminal.set_track_primary_prints(true);
        let matcher = Matcher::new("rack-[12]>>".to_owned().try_into().unwrap());
        (terminal, matcher, events)
    }

    fn feed(terminal: &mut Terminal, matcher: &mut Matcher, events: &Events, bytes: &[u8]) {
        let before = terminal.current_seqno();
        terminal.advance_bytes(bytes);
        matcher.observe_output(terminal, before, events);
    }

    fn matched(events: &Events) -> PromptToken {
        let Some(ShellPrompt::Matched(token)) = events.shell_prompt() else {
            panic!(
                "expected text-matched prompt, got {:?}",
                events.shell_prompt()
            );
        };
        token
    }

    #[test]
    fn group_prompt_requires_fresh_primary_text_after_input_and_reconnect() {
        let (mut terminal, mut matcher, events) = fixture();
        feed(
            &mut terminal,
            &mut matcher,
            &events,
            b"logs: rack-1>>\r\nrack-",
        );
        assert_eq!(events.shell_prompt(), Some(ShellPrompt::Unverified));
        feed(&mut terminal, &mut matcher, &events, b"1>>");
        let token = matched(&events);
        feed(&mut terminal, &mut matcher, &events, b" ");
        assert_eq!(matched(&events), token);
        assert!(matcher.still_matches(&terminal));
        assert!(events.claim_shell_prompt(token));
        let (status, execution) = watch::channel(GroupExecution::AwaitingPrompt);
        events.observe_group_command(
            1,
            "show".into(),
            status,
            crate::events::GroupTranscript::default().host(1),
        );
        for bytes in [
            b" ".as_slice(),
            b"\x1b[1C\x1b[1D",
            b"\x1b[?1049hrack-2>> ",
            b"\x1b[?1049l",
        ] {
            feed(&mut terminal, &mut matcher, &events, bytes);
            assert_eq!(events.shell_prompt(), Some(ShellPrompt::InUse));
            assert!(matches!(
                *execution.borrow(),
                GroupExecution::AwaitingPrompt
            ));
        }
        feed(&mut terminal, &mut matcher, &events, b"\r\nrack-2>> ");
        let second = matched(&events);
        assert_ne!(second, token);
        assert!(matches!(
            *execution.borrow(),
            GroupExecution::PromptReturned
        ));

        terminal.reset_connection();
        matcher.reset();
        events.command_stream(Some(2));
        feed(&mut terminal, &mut matcher, &events, b"\x1b[1C\x1b[1D");
        assert_eq!(events.shell_prompt(), Some(ShellPrompt::Unverified));
        feed(&mut terminal, &mut matcher, &events, b"\r\nrack-1>> ");
        assert!(!events.claim_shell_prompt(second));
        assert_eq!(matched(&events).connection, 2);
    }

    #[test]
    fn group_prompt_waits_for_sync_end_and_yields_to_semantic_markers() {
        let (mut terminal, mut matcher, events) = fixture();
        feed(
            &mut terminal,
            &mut matcher,
            &events,
            b"\x1b[?2026hrack-1>> ",
        );
        assert_eq!(events.shell_prompt(), Some(ShellPrompt::Unverified));
        feed(&mut terminal, &mut matcher, &events, b"\x1b[?2026l");
        let heuristic = matched(&events);
        feed(
            &mut terminal,
            &mut matcher,
            &events,
            b"\r\n\x1b]133;A\x07rack-2>> \x1b]133;B\x07",
        );
        assert!(!events.claim_shell_prompt(heuristic));
        let Some(ShellPrompt::Ready(semantic)) = events.shell_prompt() else {
            panic!("expected semantic prompt");
        };
        assert!(events.claim_shell_prompt(semantic));
        feed(&mut terminal, &mut matcher, &events, b"\r\nrack-1>> ");
        assert_eq!(events.shell_prompt(), Some(ShellPrompt::InUse));
    }

    #[test]
    fn group_prompt_sync_can_expire_and_discards_evidence_before_local_input() {
        let (mut terminal, mut matcher, events) = fixture();
        feed(
            &mut terminal,
            &mut matcher,
            &events,
            b"\x1b[?2026hrack-1>> ",
        );
        matcher.input_sent();
        feed(&mut terminal, &mut matcher, &events, b"\x1b[?2026l");
        assert_eq!(events.shell_prompt(), Some(ShellPrompt::Unverified));
        feed(
            &mut terminal,
            &mut matcher,
            &events,
            b"\r\n\x1b[?2026hrack-2>> ",
        );
        let before = terminal.current_seqno();
        terminal.end_synchronized_output();
        matcher.observe_output(&terminal, before, &events);
        assert_eq!(matched(&events).connection, 1);
    }

    #[test]
    fn group_prompt_survives_reflow_while_the_same_prompt_fits() {
        let (mut terminal, mut matcher, events) = fixture();
        feed(&mut terminal, &mut matcher, &events, b"rack-1>> ");
        let token = matched(&events);
        let verified = matcher.still_matches(&terminal);
        let size = terminal.get_size();
        terminal.resize(wezterm_term::TerminalSize { cols: 100, ..size });
        assert!(!matcher.still_matches(&terminal));
        matcher.resized(&terminal, verified, &events);
        assert!(matcher.still_matches(&terminal));
        assert_eq!(matched(&events), token);

        terminal.resize(wezterm_term::TerminalSize { cols: 3, ..size });
        matcher.resized(&terminal, true, &events);
        assert!(!matcher.still_matches(&terminal));
        assert_eq!(events.shell_prompt(), Some(ShellPrompt::InUse));
    }
}
