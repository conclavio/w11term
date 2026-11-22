use super::*;
use std::cell::OnceCell;

pub(super) struct TextPreview {
    serial: u64,
    entry: ListedEntry,
    content: Contents,
    pub(super) cancel: TransferCancel,
    // Terminal output can re-render the window while the user selects text.
    view: OnceCell<View>,
}

enum Contents {
    Loading,
    Ready(String),
    Error(String),
}

impl Drop for TextPreview {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl TextPreview {
    pub(super) fn dialog(
        &self,
        pane: u64,
        generation: u64,
        can_edit: bool,
        context: &ViewContext<TerminalApp>,
    ) -> View {
        self.view
            .get_or_init(|| {
                let body = match &self.content {
                    Contents::Loading => TextBlock::new().text("Loading preview…").into(),
                    Contents::Error(error) => TextBlock::new()
                        .automation_id("sftp-preview-error")
                        .text(format!("Could not preview this file. {error}"))
                        .text_wrapping(TextWrapping::Wrap)
                        .into(),
                    Contents::Ready(text) => {
                        let mut fields = vec![KeyedView::new(
                            "contents",
                            TextBlock::new()
                                .automation_name("File contents")
                                .automation_id("sftp-preview-text")
                                .is_text_selection_enabled(true)
                                .text(text),
                        )];
                        if text.is_empty() {
                            fields
                                .push(KeyedView::new("empty", TextBlock::new().text("Empty file")));
                        }
                        ScrollViewer::new()
                            .max_height(420.0)
                            .horizontal_scroll_bar_visibility(ScrollBarVisibility::Auto)
                            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                            .content(StackPanel::new().keyed_children(fields))
                    }
                };
                let serial = self.serial;
                let mut fields = vec![KeyedView::new("document", body)];
                if matches!(self.content, Contents::Ready(_)) && can_edit {
                    fields.push(KeyedView::new(
                        "edit",
                        Button::new()
                            .automation_name("Edit")
                            .on_click(context.callback(move |_| {
                                Message::Files(FileAction::StartTextEditor(
                                    pane, generation, serial,
                                ))
                            }))
                            .content("Edit"),
                    ));
                }
                ContentDialog::new()
                    .title(&self.entry.name)
                    .close_button_text("Close preview")
                    .is_open(true)
                    .on_closed(context.callback(move |_| {
                        Message::Files(FileAction::ClosePreview(pane, generation, serial))
                    }))
                    .content(
                        StackPanel::new()
                            .min_width(280.0)
                            .spacing(8.0)
                            .keyed_children(fields),
                    )
            })
            .clone()
    }
}

impl FileDrawer {
    pub(super) fn take_preview_for_edit(&mut self, serial: u64) -> Option<(ListedEntry, String)> {
        let preview = self.preview.as_ref()?;
        if !self.open
            || preview.serial != serial
            || self.check_selected(&preview.entry).is_err()
            || !matches!(preview.content, Contents::Ready(_))
        {
            return None;
        }
        let mut preview = self.preview.take()?;
        let Contents::Ready(text) = std::mem::replace(&mut preview.content, Contents::Loading)
        else {
            unreachable!()
        };
        Some((preview.entry.clone(), text))
    }
    pub(super) fn begin_preview(&mut self, entry: ListedEntry) -> Result<(u64, Operation)> {
        ensure!(
            self.open && self.generation != 0 && !self.busy() && self.bookmarks.is_none(),
            "Open the file listing before starting a preview"
        );
        ensure!(
            entry.kind == ListedKind::Known(EntryKind::File),
            "Select a regular file to preview"
        );
        self.check_selected(&entry)?;
        let path = entry_path(&entry.parent, &entry.name)?;
        self.end_filter_edit();
        self.dialog_serial = self.dialog_serial.wrapping_add(1);
        self.preview = Some(TextPreview {
            serial: self.dialog_serial,
            entry,
            content: Contents::Loading,
            cancel: TransferCancel::default(),
            view: OnceCell::new(),
        });
        Ok((self.dialog_serial, Operation::PreviewText { path }))
    }

    pub(super) fn finish_preview(&mut self, serial: u64, reply: Result<Value, String>) {
        let Some(current) = self.preview.as_ref().filter(|p| p.serial == serial) else {
            return;
        };
        if !self.open || self.check_selected(&current.entry).is_err() {
            self.cancel_preview();
            return;
        }
        let preview = self.preview.as_mut().unwrap();
        preview.content = match reply {
            Ok(mut value) => match value.get_mut("text").map(Value::take) {
                Some(Value::String(text)) => Contents::Ready(text),
                _ => Contents::Error("Missing text in the SFTP preview reply".into()),
            },
            Err(error) => Contents::Error(error),
        };
        preview.view.take();
    }

    pub(super) fn close_preview(&mut self, serial: u64) {
        if self.preview.as_ref().is_some_and(|p| p.serial == serial) {
            self.cancel_preview();
        }
    }

    pub(super) fn cancel_preview(&mut self) {
        if self.preview.take().is_some() {
            if matches!(
                self.pending.as_ref().map(|p| &p.step),
                Some(Step::Preview(_))
            ) {
                self.pending.take();
            }
            self.cached.get_mut().take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn selected(kind: ListedKind) -> (FileDrawer, ListedEntry) {
        let name = "résumé 100% [pilot].txt";
        let folder = "/srv/雪";
        let mut files = FileDrawer {
            open: true,
            generation: 2,
            ..Default::default()
        };
        files.accept_listing(
            None,
            folder.into(),
            vec![Entry {
                name: name.into(),
                kind,
                mtime: None,
            }],
        );
        files.set_filter("résumé".into());
        files.selected = Some(name.into());
        let entry = ListedEntry {
            parent: folder.into(),
            name: name.into(),
            kind,
            revision: files.listing_revision,
        };
        (files, entry)
    }

    #[test]
    fn preview_keeps_exact_text_and_existing_listing_selection() {
        let (mut files, entry) = selected(ListedKind::Known(EntryKind::File));
        let (serial, request) = files.begin_preview(entry.clone()).unwrap();
        assert!(
            matches!(request, Operation::PreviewText { path } if path == "/srv/雪/résumé 100% [pilot].txt")
        );
        assert!(files.entry_dialog_open());
        assert!(matches!(
            files.preview.as_ref().unwrap().content,
            Contents::Loading
        ));
        let original = "\u{feff}\tαβ ⛵\r\n\nlast line  ";
        files.finish_preview(serial, Ok(json!({"text":original})));
        assert!(
            matches!(&files.preview.as_ref().unwrap().content, Contents::Ready(text) if text == original)
        );
        files.close_preview(serial);
        assert!(!files.busy());
        assert!(files.check_selected(&entry).is_ok());
        assert_eq!(files.filter, "résumé");
        assert_eq!(files.status, "");
    }

    #[test]
    fn preview_refuses_nonfiles_and_stale_entries() {
        for kind in [
            ListedKind::Known(EntryKind::Directory),
            ListedKind::Known(EntryKind::Symlink),
            ListedKind::Missing,
            ListedKind::Unsupported,
        ] {
            let (mut files, entry) = selected(kind);
            assert!(files.begin_preview(entry).is_err());
            assert!(!files.entry_dialog_open());
        }
        let (mut files, entry) = selected(ListedKind::Known(EntryKind::File));
        files.listing_revision += 1;
        assert!(files.begin_preview(entry.clone()).is_err());
        files.listing_revision = entry.revision;
        files.selected = None;
        assert!(files.begin_preview(entry).is_err());
    }

    #[test]
    fn discarded_reads_cannot_reopen_or_replace_another_preview() {
        let (mut files, entry) = selected(ListedKind::Known(EntryKind::File));
        let (first, _) = files.begin_preview(entry.clone()).unwrap();
        let cancellation = files.preview.as_ref().unwrap().cancel.clone();
        files.close_preview(first);
        assert!(cancellation.is_cancelled());
        let (second, _) = files.begin_preview(entry.clone()).unwrap();
        files.close_preview(first);
        files.finish_preview(first, Ok(json!({"text":"wrong file"})));
        assert!(matches!(
            files.preview.as_ref().unwrap().content,
            Contents::Loading
        ));
        files.finish_preview(second, Err("Binary data".into()));
        assert!(
            matches!(&files.preview.as_ref().unwrap().content, Contents::Error(error) if error == "Binary data")
        );
        files.accept_listing(None, "/elsewhere".into(), vec![]);
        files.finish_preview(second, Ok(json!({"text":"late"})));
        assert!(files.preview.is_none());
        assert_eq!(files.directory.as_deref(), Some("/elsewhere"));

        let (mut files, entry) = selected(ListedKind::Known(EntryKind::File));
        let (serial, _) = files.begin_preview(entry).unwrap();
        let cancellation = files.preview.as_ref().unwrap().cancel.clone();
        files.observe_connection(3);
        files.finish_preview(serial, Ok(json!({"text":"old peer"})));
        assert!(cancellation.is_cancelled());
        assert!(files.preview.is_none());
    }
}
