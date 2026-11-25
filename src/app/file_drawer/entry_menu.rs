use super::{toolbar::ActionMenu, *};

const LABEL: &str = "Selected entry actions";
const GLYPH: &str = "\u{e8bc}";

impl FileDrawer {
    pub(super) fn selected_entry_menu(
        &self,
        pane: u64,
        entry: Option<&ListedEntry>,
        context: &ViewContext<TerminalApp>,
    ) -> View {
        use FileAction::*;
        let mut menu = ActionMenu::default();
        let Some(entry) = entry.filter(|_| !self.busy()) else {
            let tip = if entry.is_some() {
                LABEL
            } else {
                "Select a file or folder to see its actions"
            };
            return menu.button(LABEL, GLYPH, tip.into(), false, [], context);
        };
        let generation = self.generation;
        let writable = self.directory.is_some();
        let regular = entry.kind == ListedKind::Known(EntryKind::File);
        let directory = entry.kind == ListedKind::Known(EntryKind::Directory);
        let supported = entry.kind != ListedKind::Unsupported;
        let valid_path = entry_path(&entry.parent, &entry.name).is_ok();
        let can_copy = writable
            && generation != 0
            && matches!(
                entry.kind,
                ListedKind::Known(EntryKind::File) | ListedKind::Missing
            )
            && valid_path;
        let target = || FileTarget::Listed(entry.clone());
        let preview = |kind| Preview(pane, generation, target(), kind);
        let mut items = vec![menu.item(
            "properties",
            "Properties…",
            writable,
            preview(PreviewKind::Properties),
        )];
        if regular {
            items.extend([
                menu.item(
                    "preview-text",
                    "Preview text…",
                    writable,
                    preview(PreviewKind::Text),
                ),
                menu.item(
                    "tail",
                    "Tail text file…",
                    writable,
                    preview(PreviewKind::Tail),
                ),
            ]);
            if image_preview_name(&entry.name) {
                items.push(menu.item(
                    "preview-image",
                    "Preview image…",
                    writable,
                    preview(PreviewKind::Image),
                ));
            }
            if let Some(reference) = self.reference_id() {
                items.push(menu.item(
                    "compare",
                    "Compare selected with reference…",
                    writable,
                    TextReference(
                        pane,
                        generation,
                        preview::ReferenceAction::Open(target(), reference),
                    ),
                ));
            }
        }
        items.extend([
            MenuItem::separator("inspect-end"),
            menu.item(
                "copy-path",
                "Copy selected path",
                writable,
                CopyPath(pane, generation, entry.revision, Some(entry.clone())),
            ),
            menu.item(
                "verify-download",
                "Download and verify…",
                !directory,
                VerifyDownload(pane, generation, entry.clone()),
            ),
            MenuItem::separator("transfer-end"),
            menu.item(
                "move",
                "Move to folder…",
                writable && generation != 0 && supported && valid_path,
                MoveEntry(pane, generation, target()),
            ),
            menu.item(
                "copy",
                "Copy to folder…",
                can_copy,
                CopyEntry(pane, generation, target(), CopyFileMode::ToFolder),
            ),
            menu.item(
                "duplicate",
                "Duplicate as…",
                can_copy,
                CopyEntry(pane, generation, target(), CopyFileMode::Duplicate),
            ),
            menu.item(
                "rename",
                "Rename…",
                writable && valid_path,
                Rename(pane, generation, entry.clone()),
            ),
            menu.item(
                "delete",
                "Delete…",
                writable && supported && delete_path(&entry.parent, &entry.name).is_ok(),
                Delete(pane, generation, entry.clone()),
            ),
        ]);
        menu.button(
            LABEL,
            GLYPH,
            format!("Actions for {}", entry.name),
            true,
            items,
            context,
        )
    }
}
