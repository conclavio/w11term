use super::*;
use w11term::ssh::{DurableAction, DurablePhase};

pub enum Action {
    Profile(TabId, bool),
    Run(DurableAction),
    ConfirmEnd(bool),
}

pub(super) fn menu_action(label: &str) -> Option<Action> {
    Some(match label {
        "Detach from remote session" => Action::Run(DurableAction::Detach),
        "Reattach to remote session" => Action::Run(DurableAction::Reattach),
        "End remote session…" => Action::ConfirmEnd(true),
        "Cancel ending session" => Action::ConfirmEnd(false),
        "Confirm end remote session" => Action::Run(DurableAction::End),
        "Start new durable shell" => Action::Run(DurableAction::StartNew),
        "Start plain SSH shell" => Action::Run(DurableAction::UseDirectShell),
        _ => return None,
    })
}

pub(super) fn pane_menu(live: &LiveTab) -> Option<MenuItem> {
    let state = live.session.durable_state()?;
    let mut actions = Vec::new();
    if state.phase == DurablePhase::Direct {
        return None;
    }
    for (action, key, label) in [
        (
            DurableAction::Detach,
            "detach",
            "Detach from remote session",
        ),
        (
            DurableAction::Reattach,
            "reattach",
            "Reattach to remote session",
        ),
        (
            DurableAction::StartNew,
            "start-managed",
            "Start new durable shell",
        ),
        (
            DurableAction::UseDirectShell,
            "start-plain",
            "Start plain SSH shell",
        ),
    ] {
        if state.permits(action) {
            actions.push(MenuItem::item(key, label));
        }
    }
    if state.permits(DurableAction::End) {
        if live.confirm_end {
            actions.push(MenuItem::item("end-confirm", "Confirm end remote session"));
            actions.push(MenuItem::item("end-cancel", "Cancel ending session"));
        } else {
            actions.push(MenuItem::item("end", "End remote session…"));
        }
    }
    (!actions.is_empty()).then(|| MenuItem::submenu("durability", "Remote session", actions))
}

impl TerminalApp {
    #[cfg(feature = "ui-smoke")]
    pub(super) fn durable_smoke_step(&mut self, step: u8, context: &ComponentContext<Self>) {
        use super::workspace_view::WorkspaceAction;
        let id = self.tabs.active_id().unwrap();
        let Page::Terminal(live) = &self.tabs.get(id).unwrap().page else {
            panic!("Expected active durable terminal")
        };
        let state = live.session.durable_state().unwrap();
        let next = match (step, state.phase) {
            (0, DurablePhase::Attached) => {
                self.update(
                    tab_menu_message("Detach from remote session".into()),
                    context,
                );
                1
            }
            (1, DurablePhase::Detached) => {
                self.workspace_action(WorkspaceAction::Show, context);
                let manager = self.tabs.active_id().unwrap();
                let name = "durable fixture".to_owned();
                self.workspace_action(WorkspaceAction::Name(manager, name.clone()), context);
                self.workspace_action(WorkspaceAction::Save(manager), context);
                self.close_tab(id, context);
                self.workspace_action(WorkspaceAction::Open(manager, name), context);
                assert_ne!(id, self.tabs.active_id().unwrap());
                2
            }
            (2, DurablePhase::Attached)
                if live
                    .session
                    .frames
                    .load()
                    .rows
                    .iter()
                    .any(|row| row.as_str().contains("restore-screen")) =>
            {
                self.update(tab_menu_message("End remote session…".into()), context);
                self.update(tab_menu_message("Cancel ending session".into()), context);
                self.update(
                    tab_menu_message("Confirm end remote session".into()),
                    context,
                );
                self.update(tab_menu_message("End remote session…".into()), context);
                self.update(
                    tab_menu_message("Confirm end remote session".into()),
                    context,
                );
                3
            }
            (3, DurablePhase::Ended) => {
                eprintln!("Durable workspace smoke passed");
                self.update(Message::CloseWindow, context);
                return;
            }
            (_, DurablePhase::Failed | DurablePhase::Missing | DurablePhase::Unavailable) => {
                panic!("Durable workspace step {step} failed: {}", state.detail)
            }
            _ => step,
        };
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(80));
            Message::Smoke(next)
        });
    }

    pub(super) fn durability_action(&mut self, action: Action) {
        if let Action::Profile(id, value) = action {
            if let Some(AppTab {
                page: Page::NewProfile(draft),
                ..
            }) = self.tabs.get_mut(id)
                && draft.durable != value
            {
                draft.durable = value;
                draft.saved = false;
            }
            return;
        }
        let Some(id) = self.tabs.active_id() else {
            return;
        };
        let Some(AppTab {
            page: Page::Terminal(live),
            ..
        }) = self.tabs.get_mut(id)
        else {
            return;
        };
        match action {
            Action::Run(command) => {
                if matches!(command, DurableAction::End) && !live.confirm_end {
                    return;
                }
                live.confirm_end = false;
                if let Err(error) = live.session.durable_action(command) {
                    live.device_message = Some(format!("Remote session: {error:#}"));
                }
            }
            Action::ConfirmEnd(value) => live.confirm_end = value,
            Action::Profile(..) => unreachable!(),
        }
    }
}
