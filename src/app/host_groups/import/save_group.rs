use super::*;
use anyhow::Context;
use w11term::profiles::ImportedGroupMember;

pub enum Action {
    Name(String),
    Profile(usize, String),
    Save,
    Back,
}

pub(super) struct Editor {
    name: String,
    members: Vec<Member>,
}

struct Member {
    index: usize,
    name: String,
    address: String,
    create: bool,
}

impl Editor {
    fn new(review: &Review, catalog: &Profiles) -> Self {
        let mut occupied: BTreeSet<String> = catalog
            .profiles
            .keys()
            .chain(catalog.local.keys())
            .cloned()
            .collect();
        let mut members = Vec::with_capacity(review.checked.len());
        for &index in &review.checked {
            let Some(target) = review
                .rows
                .get(index)
                .and_then(|row| row.target.as_ref().ok())
            else {
                continue;
            };
            let address = list::address(&target.ssh);
            let name = target.profile.clone().unwrap_or_else(|| {
                if occupied.insert(address.clone()) {
                    return address.clone();
                }
                (2..)
                    .map(|suffix| format!("{address} ({suffix})"))
                    .find(|name| occupied.insert(name.clone()))
                    .expect("there are finitely many saved profiles")
            });
            members.push(Member {
                index,
                name,
                address,
                create: target.profile.is_none(),
            });
        }
        Self {
            name: String::new(),
            members,
        }
    }

    pub(super) fn edit(&mut self, action: Action) {
        match action {
            Action::Name(name) => self.name = name,
            Action::Profile(index, name) => {
                if let Some(member) = self
                    .members
                    .iter_mut()
                    .find(|member| member.index == index && member.create)
                {
                    member.name = name;
                }
            }
            _ => {}
        }
    }

    fn entries<'a>(&'a self, review: &'a Review) -> Result<Vec<ImportedGroupMember<'a>>> {
        self.members
            .iter()
            .map(|member| {
                let target = review
                    .rows
                    .get(member.index)
                    .and_then(|row| row.target.as_ref().ok())
                    .context("The selected hosts changed. Preview the host list again.")?;
                Ok(match target.profile.as_deref() {
                    Some(name) => ImportedGroupMember::Existing {
                        name,
                        reviewed: &target.ssh,
                    },
                    None => ImportedGroupMember::New {
                        name: member.name.trim(),
                        options: &target.ssh,
                    },
                })
            })
            .collect()
    }

    fn verify(&self, review: &Review, current: &[list::Row]) -> Result<()> {
        ensure!(
            self.members
                .iter()
                .all(|member| { current.get(member.index) == review.rows.get(member.index) }),
            "SSH profiles changed. Preview the host list again."
        );
        Ok(())
    }

    pub(super) fn view(
        &self,
        action: impl Fn(Change) -> Message + Copy + 'static,
        cancel: View,
        error: View,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let action = move |change| action(Change::SaveGroup(change));
        let valid = !self.name.trim().is_empty()
            && self
                .members
                .iter()
                .all(|member| !member.name.trim().is_empty());
        let rows = self.members.iter().map(|member| {
            let id = member.index;
            let view = if member.create {
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new()
                        .text(format!("New profile for {}", member.address))
                        .text_wrapping(TextWrapping::Wrap),
                    TextBox::new()
                        .automation_name(format!("Profile name for {}", member.address))
                        .text(&member.name)
                        .on_text_changed(
                            ctx.callback(move |name| action(Action::Profile(id, name))),
                        ),
                ))
            } else {
                TextBlock::new()
                    .text(format!(
                        "Use saved profile {} · {}",
                        member.name, member.address
                    ))
                    .text_wrapping(TextWrapping::Wrap)
                    .into()
            };
            KeyedView::new(id, view)
        });
        Grid::new()
            .margin(16.0)
            .row_spacing(8.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                StackPanel::new().spacing(6.0).children((
                    TextBlock::new()
                        .font_size(24.0)
                        .text("Save imported SSH group"),
                    TextBox::new()
                        .automation_name("Imported SSH group name")
                        .placeholder_text("Group name")
                        .text(&self.name)
                        .on_text_changed(ctx.callback(move |name| action(Action::Name(name)))),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .style(ButtonStyle::Accent)
                                .is_enabled(valid)
                                .on_click(ctx.callback(move |_| action(Action::Save)))
                                .content("Save SSH group"),
                            Button::new()
                                .on_click(ctx.callback(move |_| action(Action::Back)))
                                .content("Back to host review"),
                            cancel,
                        )),
                    error,
                )),
                ScrollViewer::new()
                    .grid_row(1)
                    .content(StackPanel::new().spacing(8.0).keyed_children(rows)),
            ))
    }
}

impl TerminalApp {
    pub(super) fn begin_import_group_save(
        &mut self,
        id: TabId,
        revision: u64,
        checked: BTreeSet<usize>,
    ) {
        let Some(review) = self
            .import_draft(id, revision)
            .and_then(|draft| draft.review.as_ref())
            .filter(|review| {
                review.save.is_none() && !checked.is_empty() && review.checked == checked
            })
        else {
            return;
        };
        match self.store.catalog() {
            Ok(catalog) => {
                let editor = Editor::new(review, &catalog);
                if let Some(page) = self.host_group_mut(id)
                    && let Some(draft) = &mut page.import.draft
                    && let Some(review) = &mut draft.review
                {
                    review.save = Some(editor);
                    draft.error = None;
                    page.import.revision += 1;
                }
            }
            Err(error) => self.report_import_error(id, error),
        }
    }

    pub(super) fn save_import_group(&mut self, id: TabId, revision: u64) {
        let Some(draft) = self.import_draft(id, revision) else {
            return;
        };
        let Some((review, editor)) = draft
            .review
            .as_ref()
            .and_then(|review| review.save.as_ref().map(|editor| (review, editor)))
        else {
            return;
        };
        let name = editor.name.trim().to_owned();
        let result = editor.entries(review).and_then(|entries| {
            self.store
                .save_imported_ssh_group(&name, &entries, |catalog| {
                    let current = draft.rows(catalog, self.store.path())?;
                    editor.verify(review, &current)
                })
        });
        match result {
            Ok(catalog) => {
                self.use_group_catalog(catalog);
                if let Some(page) = self.host_group_mut(id) {
                    page.import.draft = None;
                    page.import.revision += 1;
                    page.import.feedback = format!("Saved SSH group {name}");
                    page.saved.select_imported_group(name);
                }
            }
            Err(error) => self.report_import_error(id, error),
        }
    }
}
