use super::*;
use crate::input::ime::{self, smoke};

impl TerminalApp {
    pub(super) fn ime_smoke_step(&mut self, step: u8, context: &ComponentContext<Self>) {
        const COMMIT: &str = "仮名🦀";
        let pane_text = |id: TabId| {
            let Page::Terminal(tab) = &self.tabs.get(id).unwrap().page else {
                panic!("expected lab")
            };
            tab.session
                .frames
                .load()
                .rows
                .iter()
                .map(|row| row.as_str().into_owned())
                .collect::<Vec<_>>()
                .join("\n")
        };
        match step {
            0 => self.update(Message::SplitPane(Axis::Columns, None), context),
            1 => {
                let Page::Terminal(tab) = &self.tabs.active().unwrap().page else {
                    panic!("expected lab")
                };
                tab.session
                    .input
                    .send(Command::Paste("\x1b[2J\x1b[HAאב12ג\x1b[2G".into()))
                    .unwrap();
            }
            2 => {
                smoke::start();
                smoke::update(None, Some("かな"), 2);
            }
            3 => {
                assert!(!pane_text(2).contains("かな"));
                let (x, y, w, h) = self.focused_pane_rect().unwrap();
                assert!(x > 0 && y > 0);
                let start = smoke::query(0);
                let candidate = smoke::query(2);
                let Page::Terminal(tab) = &self.tabs.active().unwrap().page else {
                    panic!("expected lab")
                };
                let metrics = tab.surface.metrics.get();
                // The logical cursor is at column 1 (Alef), displayed at column 5.
                let expected = ime::caret_box((x, y, w, h), metrics, 5, 0).unwrap();
                assert_eq!(
                    (start.x, start.y),
                    (expected.left, expected.top),
                    "IME origin in mixed-direction text"
                );
                assert!(start.x >= x && candidate.x > start.x && candidate.x < x + w);
                assert!(candidate.y >= y && candidate.y < y + h);
                smoke::update(Some(COMMIT), None, 0);
                smoke::end();
            }
            4 => {
                assert_eq!(pane_text(2).matches(COMMIT).count(), 1);
                assert!(!pane_text(1).contains(COMMIT));
                smoke::start();
                smoke::update(None, Some("discard"), 3);
                self.update(Message::PaneFocus(1), context);
                smoke::update(Some("late-result"), None, 0);
                smoke::end();
                self.utility(true, context);
            }
            5 => {
                assert!(
                    !pane_text(1).contains("late-result") && !pane_text(2).contains("late-result")
                );
                smoke::start();
                assert!(!ime::active());
                smoke::end();
                eprintln!("IME native smoke passed");
                self.update(Message::CloseWindow, context);
                return;
            }
            _ => panic!("unexpected IME smoke step"),
        }
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(500));
            Message::Smoke(step + 1)
        });
    }
}
