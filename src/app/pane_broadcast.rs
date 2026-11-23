use super::*;
use crate::input::BroadcastTarget;

const START: &str = "Broadcast typing to panes";
const STOP: &str = "Stop broadcasting typing";

pub(super) fn is_label(label: &str) -> bool {
    matches!(label, START | STOP)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Start(Plan),
    Stop(Plan),
}

impl Action {
    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::Start(_) => START,
            Self::Stop(_) => STOP,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    group: TabId,
    panes: Vec<(TabId, u64)>,
}

impl Plan {
    pub(super) fn indicator(&self, context: &ViewContext<TerminalApp>) -> View {
        let action = Action::Stop(self.clone());
        Button::new()
            .grid_row(2)
            .style(ButtonStyle::Accent)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .automation_name(STOP)
            .on_click(context.callback(move |_| Message::BroadcastTyping(action.clone())))
            .content(format!(
                "Broadcasting typing to {} panes  ·  Stop",
                self.panes.len()
            ))
    }
}

impl TerminalApp {
    fn broadcast_plan(&self) -> Option<Plan> {
        let group = self.tabs.active_group_id()?;
        let active = self.tabs.active_id()?;
        let panes: Vec<_> = self
            .tabs
            .active_layout()?
            .leaves()
            .into_iter()
            .filter(|&id| self.tabs.visible(id))
            .filter_map(|id| {
                let Page::Terminal(tab) = &self.tabs.get(id)?.page else {
                    return None;
                };
                let frame = tab.session.frames.load();
                let generation = tab.session.events.shell_generation()?;
                (generation == frame.connection_generation
                    && frame.prompt.is_none()
                    && !tab.read_only.get()
                    && !tab.device_busy
                    && !tab.browsing_sessions())
                .then_some((id, generation))
            })
            .collect();
        (panes.len() >= 2 && panes.iter().any(|&(id, _)| id == active))
            .then_some(Plan { group, panes })
    }

    pub(super) fn broadcast_offer(&self) -> Option<Action> {
        if let Some(plan) = &self.broadcast_typing {
            return Some(Action::Stop(plan.clone()));
        }
        self.broadcast_plan().map(Action::Start)
    }

    pub(super) fn broadcast_menu(&self) -> MenuItem {
        let key = "broadcast-typing";
        self.broadcast_offer().map_or_else(
            || MenuItem::disabled(key, START),
            |action| MenuItem::item(key, action.label()),
        )
    }

    pub(super) fn apply_broadcast(&mut self, action: Action) {
        match action {
            Action::Start(plan)
                if self.broadcast_typing.is_none()
                    && self.broadcast_plan().as_ref() == Some(&plan) =>
            {
                let peers = plan
                    .panes
                    .iter()
                    .filter_map(|&(id, connection)| {
                        let Page::Terminal(tab) = &self.tabs.get(id)?.page else {
                            return None;
                        };
                        Some(BroadcastTarget {
                            sender: tab.session.input.clone(),
                            events: tab.session.events.clone(),
                            connection,
                        })
                    })
                    .collect();
                self.keyboard.set_broadcast(peers);
                self.broadcast_typing = Some(plan);
                self.header.borrow_mut().take();
            }
            Action::Stop(plan) if self.broadcast_typing.as_ref() == Some(&plan) => {
                self.stop_broadcast();
            }
            _ => {}
        }
    }

    pub(super) fn stop_broadcast(&mut self) {
        self.keyboard.set_broadcast(Vec::new());
        self.broadcast_typing = None;
        self.header.borrow_mut().take();
    }

    pub(super) fn reconcile_broadcast(&mut self) {
        if self.broadcast_typing.is_some() && self.broadcast_typing != self.broadcast_plan() {
            self.stop_broadcast();
        }
    }
}
