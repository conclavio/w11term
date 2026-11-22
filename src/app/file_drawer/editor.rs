use super::{text_draft::TextDraft, *};
use std::cell::OnceCell;

pub(super) struct Editor {
    id: u64,
    generation: u64,
    entry: ListedEntry,
    original: String,
    draft: TextDraft,
    open: bool,
    // Each opening gets new callbacks; a delayed Closed from the old dialog is inert.
    serial: u64,
    mode: Mode,
    saving: bool,
    status: String,
    multiline_ready: bool,
    native_text: Cell<Option<String>>,
    input: OnceCell<View>,
}

enum Mode {
    Edit,
    SaveAs(String),
    ConfirmDiscard,
}

#[derive(Clone)]
pub enum Action {
    Mounted,
    Text(String),
    Save,
    SaveAs,
    Name(String),
    Create,
    Back,
    ConfirmDiscard,
    Discard,
    Close,
}

pub(super) struct Save {
    id: u64,
    parent: String,
    name: String,
    new_file: bool,
}

impl Save {
    fn success(&self) -> String {
        format!(
            "{} {}",
            if self.new_file { "Created" } else { "Saved" },
            self.name
        )
    }
}

impl Editor {
    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn serial(&self) -> u64 {
        self.serial
    }

    pub(super) fn stash(&mut self) {
        self.open = false;
        self.mode = Mode::Edit;
        self.multiline_ready = false;
        self.native_text.take();
        self.input.take();
    }

    fn prepare_save(&mut self, generation: u64, new_file: bool) -> Result<(Save, Operation)> {
        ensure!(
            self.generation == generation,
            "The SSH connection changed. Copy this draft or reopen the remote file to edit it."
        );
        let name = match (&self.mode, new_file) {
            (Mode::Edit, false) => &self.entry.name,
            (Mode::SaveAs(name), true) => name,
            _ => bail!("Choose a save action first"),
        };
        let path = entry_path(&self.entry.parent, name)?;
        let text = self.draft.contents();
        ensure!(
            text.len() <= w11term::sftp::TEXT_PREVIEW_MAX_BYTES && !text.contains('\0'),
            "Save supports UTF-8 text up to 256 KiB without NUL bytes"
        );
        let operation = Operation::SaveText {
            path,
            original: (!new_file).then(|| self.original.clone()),
            text,
        };
        self.saving = true;
        self.status = format!("Saving {name}…");
        self.input.take();
        Ok((
            Save {
                id: self.id,
                parent: self.entry.parent.clone(),
                name: name.clone(),
                new_file,
            },
            operation,
        ))
    }

    pub(super) fn failed(&mut self, error: String) {
        self.status = error;
        self.saving = false;
        self.input.take();
    }

    pub(super) fn dialog(
        &self,
        pane: u64,
        generation: u64,
        context: &ViewContext<TerminalApp>,
    ) -> View {
        let serial = self.serial;
        let message = move |action| Message::Files(FileAction::TextEditor(pane, serial, action));
        let button = |label: &str, enabled: bool, action: Action| {
            Button::new()
                .automation_name(label)
                .is_enabled(enabled)
                .on_click(context.callback(move |_| message(action.clone())))
                .content(label)
        };
        // Reuse the native TextChanged string for the next render. The controlled
        // TextBox then keeps its caret, even when other terminal frames arrive.
        let input = self.input.get_or_init(|| {
            // Reactor applies Text before AcceptsReturn when mounting a TextBox.
            // Mount it empty first so WinUI cannot truncate the seed at line one.
            if !self.multiline_ready {
                context.sender().send(message(Action::Mounted));
            }
            StackPanel::new().keyed_children([KeyedView::new(
                "input",
                TextBox::new()
                    .automation_name("File text")
                    .automation_id("sftp-editor-text")
                    .accepts_return(true)
                    .text_wrapping(TextWrapping::Wrap)
                    .min_height(100.0)
                    .max_height(320.0)
                    .is_enabled(self.multiline_ready && !self.saving)
                    .text_optional(self.multiline_ready.then(|| {
                        self.native_text
                            .take()
                            .unwrap_or_else(|| self.draft.visible().replace('\n', "\r"))
                    }))
                    .on_text_changed(context.callback(move |s| message(Action::Text(s)))),
            )])
        });
        let writable = self.multiline_ready && self.generation == generation && !self.saving;
        let mut fields = vec![
            KeyedView::new(
                "path",
                TextBlock::new()
                    .text(&self.entry.parent)
                    .text_wrapping(TextWrapping::Wrap),
            ),
            KeyedView::new("document", input.clone()),
        ];
        if !self.status.is_empty() {
            fields.push(KeyedView::new(
                "status",
                TextBlock::new()
                    .automation_id("sftp-editor-status")
                    .text(&self.status)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        let controls = match &self.mode {
            Mode::Edit => vec![
                button("Save", writable, Action::Save),
                button("Save as…", writable, Action::SaveAs),
                button("Discard draft…", !self.saving, Action::ConfirmDiscard),
            ],
            Mode::SaveAs(name) => {
                fields.push(KeyedView::new(
                    "name",
                    TextBox::new()
                        .automation_name("New filename")
                        .automation_id("sftp-editor-filename")
                        .placeholder_text("New filename")
                        .is_enabled(!self.saving)
                        .text(name)
                        .on_text_changed(context.callback(move |name| message(Action::Name(name)))),
                ));
                vec![
                    button(
                        "Create file",
                        writable && entry_path(&self.entry.parent, name).is_ok(),
                        Action::Create,
                    ),
                    button("Back to editor", !self.saving, Action::Back),
                ]
            }
            Mode::ConfirmDiscard => {
                fields.push(KeyedView::new(
                    "confirm",
                    TextBlock::new().text("Discard this draft and its unsaved changes?"),
                ));
                vec![
                    button("Discard draft", true, Action::Discard),
                    button("Back to editor", true, Action::Back),
                ]
            }
        };
        fields.push(KeyedView::new(
            "actions",
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .keyed_children(
                    controls
                        .into_iter()
                        .enumerate()
                        .map(|(i, b)| KeyedView::new(i.to_string(), b)),
                ),
        ));
        ContentDialog::new()
            .title(&self.entry.name)
            .close_button_text("Close editor")
            .is_open(true)
            .on_closed(context.callback(move |_| message(Action::Close)))
            .content(
                StackPanel::new()
                    .min_width(280.0)
                    .spacing(8.0)
                    .keyed_children(fields),
            )
    }
}

impl FileDrawer {
    pub(super) fn start_editor(&mut self, serial: u64) {
        if self.editor.is_some() {
            return;
        }
        let Some((entry, original)) = self.take_preview_for_edit(serial) else {
            return;
        };
        self.dialog_serial = self.dialog_serial.wrapping_add(1);
        self.editor = Some(Editor {
            id: self.dialog_serial,
            serial: self.dialog_serial,
            generation: self.generation,
            entry,
            draft: TextDraft::new(&original),
            original,
            open: true,
            mode: Mode::Edit,
            saving: false,
            status: String::new(),
            multiline_ready: false,
            native_text: Cell::default(),
            input: OnceCell::new(),
        });
    }

    pub(super) fn resume_editor(&mut self, serial: u64) {
        if !self.open || self.entry_dialog_open() || self.picking.is_some() {
            return;
        }
        if let Some(editor) = self.editor.as_mut().filter(|e| e.serial == serial) {
            self.dialog_serial = self.dialog_serial.wrapping_add(1);
            editor.serial = self.dialog_serial;
            editor.open = true;
            self.end_filter_edit();
        }
    }

    pub(super) fn editor_action(
        &mut self,
        serial: u64,
        action: Action,
    ) -> Option<(Save, Operation)> {
        let editor = self
            .editor
            .as_mut()
            .filter(|e| e.open && e.serial == serial)?;
        match action {
            Action::Close => editor.stash(),
            Action::Mounted => {
                if !editor.multiline_ready {
                    editor.multiline_ready = true;
                    editor.input.take();
                }
            }
            _ if editor.saving => {}
            Action::Text(text) if editor.multiline_ready => {
                editor.draft.edit(&text);
                editor.native_text.set(Some(text));
                editor.input.take();
            }
            Action::Text(_) => {}
            Action::SaveAs if editor.generation == self.generation => {
                editor.mode = Mode::SaveAs(String::new())
            }
            Action::Name(value) => {
                if let Mode::SaveAs(name) = &mut editor.mode {
                    *name = value;
                }
            }
            Action::Back => editor.mode = Mode::Edit,
            Action::ConfirmDiscard => editor.mode = Mode::ConfirmDiscard,
            Action::Discard if matches!(editor.mode, Mode::ConfirmDiscard) => self.editor = None,
            Action::Save | Action::Create => {
                if self.pending.is_some() || self.parsing {
                    editor.status = "Wait for the current SFTP request to finish.".into();
                } else {
                    match editor.prepare_save(self.generation, matches!(action, Action::Create)) {
                        Ok(request) => return Some(request),
                        Err(err) => editor.status = err.to_string(),
                    }
                }
            }
            Action::Discard | Action::SaveAs => {}
        }
        None
    }

    pub(super) fn finish_save(
        &mut self,
        save: Save,
        result: Result<Value, String>,
    ) -> Option<(String, String)> {
        let editor = self.editor.as_mut().filter(|e| e.id == save.id)?;
        if let Err(error) = result {
            if !editor.open {
                self.status = format!(
                    "Could not save {}. Resume draft to review: {error}",
                    save.name
                );
            }
            editor.failed(error);
            return None;
        }
        self.status = save.success();
        self.editor = None;
        (save.new_file && self.open && self.directory.as_ref() == Some(&save.parent))
            .then_some((save.parent, save.name))
    }

    pub(super) fn detach_editor(&mut self) -> Option<Editor> {
        let mut editor = self.editor.take()?;
        let tail =
            "The SSH connection changed. Copy this draft or reopen the remote file to edit it.";
        editor.status = match &mut self.pending {
            Some(Pending {
                step: Step::SaveText(save),
                reply,
                ..
            }) if editor.id == save.id => match reply.try_recv() {
                Ok(Ok(_)) => format!("{} before SSH reconnected. {tail}", save.success()),
                Ok(Err(error)) => format!("{error}. {tail}"),
                Err(_) => format!("The save outcome is unknown. Check the remote file. {tail}"),
            },
            _ => tail.into(),
        };
        editor.saving = false;
        editor.stash();
        Some(editor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ORIGINAL: &str = "\u{feff}first\r\nsecond\n";

    fn opened() -> (FileDrawer, u64) {
        let kind = ListedKind::Known(EntryKind::File);
        let name = "log [雪].txt";
        let mut files = FileDrawer {
            generation: 7,
            open: true,
            ..Default::default()
        };
        files.accept_listing(
            None,
            "/var/test".into(),
            vec![Entry {
                name: name.into(),
                kind,
            }],
        );
        files.selected = Some(name.into());
        let entry = ListedEntry {
            parent: "/var/test".into(),
            name: name.into(),
            kind,
            revision: files.listing_revision,
        };
        let (preview, _) = files.begin_preview(entry).unwrap();
        files.finish_preview(preview, Ok(json!({"text":ORIGINAL})));
        files.start_editor(preview);
        let serial = files.editor.as_ref().unwrap().serial;
        files.editor_action(serial, Action::Mounted);
        (files, serial)
    }

    #[test]
    fn mounting_the_multiline_input_preserves_the_recovered_document() {
        let (mut files, previous) = opened();
        files.editor_action(previous, Action::Close);
        files.resume_editor(previous);
        let current = files.editor.as_ref().unwrap().serial;
        files.editor_action(previous, Action::Mounted);
        files.editor_action(current, Action::Text(String::new()));
        let editor = files.editor.as_ref().unwrap();
        assert!(!editor.multiline_ready);
        assert_eq!(editor.draft.contents(), ORIGINAL);
        files.editor_action(current, Action::Mounted);
        files.editor_action(current, Action::Text("changed\rsecond\r".into()));
        assert_eq!(
            files.editor.as_ref().unwrap().draft.contents(),
            "\u{feff}changed\r\nsecond\n"
        );
    }

    #[test]
    fn conflicts_preserve_the_draft_and_save_as_only_creates_a_sibling() {
        let (mut files, serial) = opened();
        files.editor_action(serial, Action::Text("first\rSECOND\r".into()));
        let (save, operation) = files.editor_action(serial, Action::Save).unwrap();
        let changed = "\u{feff}first\r\nSECOND\n";
        assert!(
            matches!(operation, Operation::SaveText { path, original: Some(base), text } if path == "/var/test/log [雪].txt" && base == ORIGINAL && text == changed)
        );
        files.finish_save(save, Err("The remote file changed".into()));
        let editor = files.editor.as_ref().unwrap();
        assert!(editor.open && !editor.saving);
        assert_eq!(editor.draft.contents(), changed);
        assert_eq!(editor.original, ORIGINAL);
        files.editor_action(serial, Action::SaveAs);
        files.editor_action(serial, Action::Name("../other".into()));
        assert!(files.editor_action(serial, Action::Create).is_none());
        files.editor_action(serial, Action::Name("saved copy.txt".into()));
        let (save, operation) = files.editor_action(serial, Action::Create).unwrap();
        assert!(
            matches!(operation, Operation::SaveText { path, original: None, text } if path == "/var/test/saved copy.txt" && text == changed)
        );
        assert_eq!(
            files.finish_save(save, Ok(json!({}))),
            Some(("/var/test".into(), "saved copy.txt".into()))
        );
        assert!(files.editor.is_none());
        assert_eq!(files.status, "Created saved copy.txt");
    }

    #[test]
    fn closing_and_switching_preserve_drafts_and_expire_previous_dialog_callbacks() {
        let (mut files, serial) = opened();
        files.editor_action(serial, Action::Text("unsaved".into()));
        files.close();
        assert!(!files.entry_dialog_open());
        assert!(files.editor.is_some());
        files.open = true;
        files.resume_editor(serial);
        let resumed = files.editor.as_ref().unwrap().serial;
        assert_ne!(serial, resumed);
        files.editor_action(serial, Action::Text("obsolete callback".into()));
        files.editor_action(serial, Action::Close);
        assert!(files.entry_dialog_open());
        assert_eq!(files.editor.as_ref().unwrap().draft.visible(), "unsaved");
        files.cancel_entry_dialog();
        assert!(!files.entry_dialog_open());
        files.resume_editor(resumed);
        let latest = files.editor.as_ref().unwrap().serial;
        files.editor_action(latest, Action::Discard);
        assert!(files.editor.is_some());
        files.editor_action(latest, Action::ConfirmDiscard);
        files.editor_action(latest, Action::Discard);
        assert!(files.editor.is_none());
    }

    #[test]
    fn reconnected_draft_remains_recoverable_without_writes_to_the_new_generation() {
        let (mut files, serial) = opened();
        files.editor_action(serial, Action::Text("keep this".into()));
        files.observe_connection(8);
        assert!(!files.entry_dialog_open());
        files.resume_editor(serial);
        let editor = files.editor.as_ref().unwrap();
        let serial = editor.serial;
        assert!(editor.status.contains("SSH connection changed"));
        assert_eq!(editor.draft.visible(), "keep this");
        assert!(files.editor_action(serial, Action::Save).is_none());
        files.editor_action(serial, Action::SaveAs);
        files.editor_action(serial, Action::Name("new.txt".into()));
        assert!(files.editor_action(serial, Action::Create).is_none());
        assert!(files.entry_dialog_open());
    }
}
