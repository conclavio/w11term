use super::*;

#[derive(Clone)]
pub struct Listing {
    directory: String,
    selected: Option<String>,
    entries: Vec<Entry>,
}

impl Listing {
    fn from_reply(reply: Value, order: SortOrder) -> Result<Self> {
        let directory = reply["directory"]
            .as_str()
            .context("Missing target directory")?
            .to_owned();
        let selected = reply["selected"].as_str().map(str::to_owned);
        let entries = directory_entries(&reply, order)?;
        if let Some(name) = &selected {
            ensure!(
                entries.iter().any(|entry| entry.name == *name
                    && matches!(
                        entry.kind,
                        ListedKind::Known(EntryKind::File) | ListedKind::Missing
                    )),
                "The resolved file is no longer in its folder"
            );
        }
        Ok(Self {
            directory,
            selected,
            entries,
        })
    }
}

impl FileDrawer {
    pub(super) fn finish_link_target(
        &mut self,
        pane: u64,
        serial: u64,
        reply: Result<Value, String>,
        context: &ComponentContext<TerminalApp>,
    ) {
        if !self.opening_link_target(serial) {
            return;
        }
        let reply = match reply {
            Ok(reply) => reply,
            Err(error) => {
                self.link_target_error(serial, error);
                return;
            }
        };
        let generation = self.generation;
        let order = self.sort_order;
        self.parsing = true;
        let rejected = Message::Files(FileAction::LinkTargetListed(
            pane,
            generation,
            serial,
            Err("Unable to display the target folder".into()),
        ));
        context.spawn_background_with_rejection(
            move |_| {
                Message::Files(FileAction::LinkTargetListed(
                    pane,
                    generation,
                    serial,
                    Listing::from_reply(reply, order).map_err(|error| format!("{error:#}")),
                ))
            },
            rejected,
        );
    }

    pub(super) fn accept_link_target(&mut self, serial: u64, result: Result<Listing, String>) {
        self.parsing = false;
        if !self.opening_link_target(serial) {
            return;
        }
        let listing = match result {
            Ok(listing) => listing,
            Err(error) => {
                self.link_target_error(serial, error);
                return;
            }
        };
        self.following = false;
        if let Some(search) = self.search.take() {
            self.saved_search = Some(search);
        }
        self.status.clear();
        self.reveal = listing.selected.map(RevealTarget::Existing);
        self.accept_listing(None, listing.directory, listing.entries);
    }
}
