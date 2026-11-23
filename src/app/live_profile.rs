use super::*;
use clipboard_paste::Origin;
use w11term::workspace::SavedSource;

pub(super) const SAVE: &str = "Save SSH connection as profile…";

pub(super) fn open(origin: Option<Origin>) -> Message {
    origin.map_or(Message::ControlIdle, |origin| {
        Message::TabTitle(tab_title::Action::SaveProfile(origin))
    })
}

impl TerminalApp {
    pub(super) fn saveable_ssh_origin(&self) -> Option<Origin> {
        let origin = self.paste_origin()?;
        self.unsaved_ssh(origin)?;
        Some(origin)
    }

    fn unsaved_ssh(&self, origin: Origin) -> Option<(TabId, &Source)> {
        if self.paste_origin() != Some(origin) || self.clipboard.is_open() {
            return None;
        }
        let id = self.tabs.active_id()?;
        let Page::Terminal(live) = &self.tabs.get(id)?.page else {
            return None;
        };
        (matches!(&live.source, Source::Ssh(_))
            && live.profile_name.is_none()
            && !live.device_busy
            && !live.browsing_sessions()
            && !live.files.entry_dialog_open()
            && !live.files.filter_editing()
            && live.session.events.shell_generation() == Some(origin.connection)
            && live.session.frames.load().prompt.is_none()
            && live.session.prompts.current().is_none())
        .then_some((id, &live.source))
    }

    pub(super) fn save_live_ssh_profile(
        &mut self,
        origin: Origin,
        name: &str,
        context: &ComponentContext<Self>,
    ) -> anyhow::Result<()> {
        let (id, source) = self
            .unsaved_ssh(origin)
            .context("The live SSH connection changed. Open the action again.")?;
        // Capture launch options with absolute key paths before changing their base to the catalog.
        let SavedSource::Ssh(options) = SavedSource::capture(source, None)? else {
            unreachable!("the captured pane is an unsaved SSH source");
        };
        self.catalog = self.store.add_to_catalog(name, &options)?;
        self.profile_error = None;
        // The synchronous catalog write cannot change which pane belongs to this tab.
        let Some(AppTab {
            label,
            page: Page::Terminal(live),
        }) = self.tabs.get_mut(id)
        else {
            unreachable!("live pane belongs to the captured tab");
        };
        *label = name.to_owned();
        live.profile_name = Some(name.to_owned());
        self.header.borrow_mut().take();
        self.activate(context);
        Ok(())
    }
}
