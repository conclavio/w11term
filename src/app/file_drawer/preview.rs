use super::*;
use std::cell::OnceCell;
use w11term::sftp::FileTextMatch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    Text,
    Properties,
    Checksum(u64),
}

pub(super) struct EntryPreview {
    serial: u64,
    target: FileTarget,
    kind: PreviewKind,
    content: Contents,
    text_match: Option<MatchPreview>,
    full_text: bool,
    checksum_available: bool,
    copy_status: Option<String>,
    pub(super) cancel: TransferCancel,
    // Terminal output can re-render the window while the user selects text.
    view: OnceCell<View>,
}

enum Contents {
    Loading,
    Ready(String),
    Error(String),
}

struct MatchPreview {
    lines: Vec<FileTextMatch>,
    selected: usize,
}

struct MatchContext {
    excerpt: String,
    before: Vec<String>,
    after: Vec<String>,
}

impl MatchContext {
    fn from_file(text: &str, hit: FileTextMatch) -> Option<Self> {
        const CONTEXT_LINES: usize = 2;
        let (before_text, from_line) = text.split_at_checked(hit.line_start)?;
        let before_text = before_text.strip_prefix('\u{feff}').unwrap_or(before_text);
        let mut before: Vec<_> = before_text
            .lines()
            .rev()
            .take(CONTEXT_LINES)
            .map(context_line)
            .collect();
        before.reverse();
        let after = from_line
            .lines()
            .skip(1)
            .take(CONTEXT_LINES)
            .map(context_line)
            .collect();
        Some(Self {
            excerpt: hit.excerpt(text)?,
            before,
            after,
        })
    }

    fn view(self, title: String) -> View {
        let Self {
            excerpt,
            before,
            after,
        } = self;
        let preceding = before.len();
        let mut fields = vec![KeyedView::new("line", TextBlock::new().text(title))];
        for (index, text) in before.into_iter().chain([excerpt]).chain(after).enumerate() {
            let mut block = TextBlock::new()
                .text(text)
                .text_wrapping(TextWrapping::Wrap)
                .min_height(18.0)
                .is_text_selection_enabled(true);
            if index == preceding {
                block = block
                    .automation_id("sftp-preview-match-line")
                    .font_weight(windows_reactor::FontWeight::SEMI_BOLD);
            }
            fields.push(KeyedView::new(format!("context-{index}"), block));
        }
        ScrollViewer::new()
            .max_height(420.0)
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .content(StackPanel::new().keyed_children(fields))
    }
}

fn context_line(text: &str) -> String {
    let mut chars = text.chars();
    let mut context: String = chars
        .by_ref()
        .take(160)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    if chars.next().is_some() {
        context.push('…');
    }
    context
}

impl Drop for EntryPreview {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl EntryPreview {
    fn editable(&self) -> bool {
        self.kind == PreviewKind::Text && matches!(self.content, Contents::Ready(_))
    }

    pub(super) fn dialog(
        &self,
        pane: u64,
        generation: u64,
        can_edit: bool,
        context: &ViewContext<TerminalApp>,
    ) -> View {
        self.view
            .get_or_init(|| {
                let matches = self
                    .text_match
                    .as_ref()
                    .filter(|hits| !hits.lines.is_empty());
                let body = match &self.content {
                    Contents::Loading => TextBlock::new()
                        .text(match self.kind {
                            PreviewKind::Text => "Loading preview…",
                            PreviewKind::Properties => "Loading properties…",
                            PreviewKind::Checksum(_) => "Calculating SHA-256 over SFTP…",
                        })
                        .into(),
                    Contents::Error(error) => {
                        let (id, message) = match self.kind {
                            PreviewKind::Text => {
                                ("sftp-preview-error", "Could not preview this file.")
                            }
                            PreviewKind::Properties => {
                                ("sftp-properties-error", "Could not read properties.")
                            }
                            PreviewKind::Checksum(_) => {
                                ("sftp-checksum-error", "Could not calculate SHA-256.")
                            }
                        };
                        TextBlock::new()
                            .automation_id(id)
                            .text(format!("{message} {error}"))
                            .text_wrapping(TextWrapping::Wrap)
                            .into()
                    }
                    Contents::Ready(text) => {
                        let match_view = matches.filter(|_| !self.full_text).and_then(|hits| {
                            let hit = hits.lines[hits.selected];
                            let title = if hits.lines.len() == 1 {
                                format!("Match on line {}", hit.line)
                            } else {
                                format!(
                                    "Match {} of {} · Line {}",
                                    hits.selected + 1,
                                    hits.lines.len(),
                                    hit.line,
                                )
                            };
                            Some(MatchContext::from_file(text, hit)?.view(title))
                        });
                        match_view.unwrap_or_else(|| self.full_text_view(text))
                    }
                };
                let serial = self.serial;
                let body_key = if self.full_text {
                    "full-document".to_owned()
                } else if let Some(hits) = matches {
                    format!("match-{}", hits.selected)
                } else {
                    "document".to_owned()
                };
                let mut fields = vec![KeyedView::new(body_key, body)];
                if let Some(hits) = matches {
                    if !self.full_text && hits.lines.len() > 1 {
                        let nav_button = |label, forward, enabled| {
                            Button::new()
                                .is_enabled(enabled)
                                .on_click(context.callback(move |_| {
                                    Message::Files(FileAction::MovePreviewMatch(
                                        pane, generation, serial, forward,
                                    ))
                                }))
                                .content(label)
                        };
                        fields.push(KeyedView::new(
                            "match-navigation",
                            StackPanel::new()
                                .orientation(Orientation::Horizontal)
                                .spacing(8.0)
                                .children((
                                    nav_button("Previous match", false, hits.selected > 0),
                                    nav_button(
                                        "Next match",
                                        true,
                                        hits.selected + 1 < hits.lines.len(),
                                    ),
                                )),
                        ));
                    }
                    let full = !self.full_text;
                    fields.push(KeyedView::new(
                        "toggle-match",
                        Button::new()
                            .on_click(context.callback(move |_| {
                                Message::Files(FileAction::ShowFullPreview(
                                    pane, generation, serial, full,
                                ))
                            }))
                            .content(if full {
                                "Show full file"
                            } else {
                                "Back to match"
                            }),
                    ));
                } else if self.text_match.is_some() {
                    fields.insert(
                        0,
                        KeyedView::new(
                            "match-missing",
                            TextBlock::new()
                                .text("The current file no longer matches the search.")
                                .text_wrapping(TextWrapping::Wrap),
                        ),
                    );
                }
                if self.editable() && can_edit {
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
                if self.checksum_available {
                    let target = self.target.clone();
                    fields.push(KeyedView::new(
                        "checksum",
                        Button::new()
                            .automation_name("Calculate SHA-256")
                            .on_click(context.callback(move |_| {
                                Message::Files(FileAction::Preview(
                                    pane,
                                    generation,
                                    target.clone(),
                                    PreviewKind::Checksum(serial),
                                ))
                            }))
                            .content("Calculate SHA-256"),
                    ));
                }
                if matches!(self.kind, PreviewKind::Checksum(_))
                    && matches!(self.content, Contents::Ready(_))
                {
                    fields.push(KeyedView::new(
                        "copy",
                        Button::new()
                            .on_click(context.callback(move |_| {
                                Message::Files(FileAction::CopyChecksum(pane, generation, serial))
                            }))
                            .content("Copy SHA-256"),
                    ));
                    fields.push(KeyedView::new(
                        "verified-download",
                        Button::new()
                            .on_click(context.callback(move |_| {
                                Message::Files(FileAction::VerifyChecksum(pane, generation, serial))
                            }))
                            .content("Use for verified download"),
                    ));
                }
                if let Some(status) = &self.copy_status {
                    fields.push(KeyedView::new(
                        "copy-status",
                        TextBlock::new()
                            .text(status)
                            .text_wrapping(TextWrapping::Wrap),
                    ));
                }
                ContentDialog::new()
                    .title(self.target.name())
                    .close_button_text(match self.kind {
                        PreviewKind::Text => "Close preview",
                        PreviewKind::Properties => "Close properties",
                        PreviewKind::Checksum(_) if matches!(self.content, Contents::Loading) => {
                            "Cancel calculation"
                        }
                        PreviewKind::Checksum(_) => "Close checksum",
                    })
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

    fn full_text_view(&self, text: &str) -> View {
        let (name, id) = match self.kind {
            PreviewKind::Text => ("File contents", "sftp-preview-text"),
            PreviewKind::Properties => ("File properties", "sftp-properties-text"),
            PreviewKind::Checksum(_) => ("SHA-256 digest", "sftp-checksum-text"),
        };
        let mut fields = vec![KeyedView::new(
            "contents",
            TextBlock::new()
                .automation_name(name)
                .automation_id(id)
                .is_text_selection_enabled(true)
                .text(text),
        )];
        if self.kind == PreviewKind::Text && text.is_empty() {
            fields.push(KeyedView::new("empty", TextBlock::new().text("Empty file")));
        }
        ScrollViewer::new()
            .max_height(420.0)
            .horizontal_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .content(StackPanel::new().keyed_children(fields))
    }
}

impl FileDrawer {
    pub(super) fn take_preview_for_edit(&mut self, serial: u64) -> Option<(FileTarget, String)> {
        let preview = self.preview.as_ref()?;
        if !self.open
            || preview.serial != serial
            || !preview.editable()
            || self.checked_target_path(&preview.target).is_err()
        {
            return None;
        }
        let target = preview.target.clone();
        let mut preview = self.preview.take()?;
        let Contents::Ready(text) = std::mem::replace(&mut preview.content, Contents::Loading)
        else {
            unreachable!()
        };
        Some((target, text))
    }
    pub(super) fn begin_preview(
        &mut self,
        target: FileTarget,
        kind: PreviewKind,
    ) -> Result<(u64, Operation)> {
        let idle = if matches!(kind, PreviewKind::Checksum(_)) {
            // A completed Properties dialog is the source of this request.
            self.pending.is_none()
        } else {
            !self.busy()
        };
        ensure!(
            self.open && self.generation != 0 && idle && self.bookmarks.is_none(),
            "Open the file listing before starting a preview"
        );
        let path = self.checked_target_path(&target)?;
        let operation = match kind {
            PreviewKind::Text => {
                ensure!(target.is_regular_file(), "Select a regular file to preview");
                Operation::PreviewText { path }
            }
            PreviewKind::Properties => Operation::EntryProperties { path },
            PreviewKind::Checksum(properties) => {
                ensure!(
                    self.preview
                        .as_ref()
                        .is_some_and(|preview| preview.serial == properties
                            && preview.checksum_available
                            && preview.target == target),
                    "Reopen the selected file's properties to calculate SHA-256"
                );
                Operation::Sha256 {
                    path,
                    max_bytes: None,
                }
            }
        };
        self.end_filter_edit();
        self.dialog_serial = self.dialog_serial.wrapping_add(1);
        self.preview = Some(EntryPreview {
            serial: self.dialog_serial,
            target,
            kind,
            content: Contents::Loading,
            text_match: None,
            full_text: false,
            checksum_available: false,
            copy_status: None,
            cancel: TransferCancel::default(),
            view: OnceCell::new(),
        });
        Ok((self.dialog_serial, operation))
    }

    pub(super) fn finish_preview(&mut self, serial: u64, reply: Result<Value, String>) {
        let Some(current) = self.preview.as_ref().filter(|p| p.serial == serial) else {
            return;
        };
        let Ok(path) = self.checked_target_path(&current.target) else {
            self.cancel_preview();
            return;
        };
        if !self.open {
            self.cancel_preview();
            return;
        }
        let preview = self.preview.as_mut().unwrap();
        let text = reply.and_then(|mut value| match preview.kind {
            PreviewKind::Text => match value.get_mut("text").map(Value::take) {
                Some(Value::String(text)) => Ok(text),
                _ => Err("Missing text in the SFTP preview reply".into()),
            },
            PreviewKind::Properties => {
                let mode = value["attrs"]["permissions"]
                    .as_u64()
                    .and_then(|mode| u32::try_from(mode).ok());
                let text = super::properties::display_text(&path, value)
                    .map_err(|error| format!("{error:#}"));
                preview.checksum_available =
                    text.is_ok() && EntryKind::from_permissions(mode) == Some(EntryKind::File);
                text
            }
            PreviewKind::Checksum(_) => value["sha256"]
                .as_str()
                .ok_or_else(|| "Missing SHA-256 in the SFTP reply".to_owned())
                .and_then(|sha256| normalize_sha256(sha256).map_err(|error| error.to_string())),
        });
        preview.content = match text {
            Ok(text) => {
                if preview.kind == PreviewKind::Text
                    && let FileTarget::Search(_, hit) = &preview.target
                    && hit.content_match.is_some()
                    && let Some(panel) = &self.search
                {
                    preview.text_match = Some(MatchPreview {
                        lines: panel.match_file_text_lines(&text).unwrap_or_default(),
                        selected: 0,
                    });
                }
                Contents::Ready(text)
            }
            Err(error) => Contents::Error(error),
        };
        preview.view.take();
    }

    pub(super) fn show_full_preview(&mut self, serial: u64, full: bool) {
        if let Some(preview) = self.preview.as_mut().filter(|p| p.serial == serial)
            && preview
                .text_match
                .as_ref()
                .is_some_and(|hits| !hits.lines.is_empty())
            && preview.full_text != full
        {
            preview.full_text = full;
            preview.view.take();
        }
    }

    pub(super) fn move_preview_match(&mut self, serial: u64, forward: bool) {
        let Some(preview) = self
            .preview
            .as_mut()
            .filter(|p| p.serial == serial && !p.full_text)
        else {
            return;
        };
        if let Some(hits) = &mut preview.text_match
            && let Some(selected) = hits
                .selected
                .checked_add_signed(if forward { 1 } else { -1 })
            && selected < hits.lines.len()
        {
            hits.selected = selected;
            preview.view.take();
        }
    }

    fn ready_checksum(&self, serial: u64) -> Option<(&FileTarget, &str)> {
        let preview = self.preview.as_ref().filter(|p| {
            p.serial == serial
                && matches!(p.kind, PreviewKind::Checksum(_))
                && self.open
                && self.checked_target_path(&p.target).is_ok()
        })?;
        let Contents::Ready(sha256) = &preview.content else {
            return None;
        };
        Some((&preview.target, sha256))
    }

    pub(super) fn copy_checksum(&mut self, serial: u64) {
        let Some((_, sha256)) = self.ready_checksum(serial) else {
            return;
        };
        let status = match w11term::clipboard::write(sha256) {
            Ok(()) => "Copied SHA-256".to_owned(),
            Err(error) => format!("Could not copy SHA-256. {error}"),
        };
        let preview = self.preview.as_mut().unwrap();
        preview.copy_status = Some(status);
        preview.view.take();
    }

    pub(super) fn verify_checksum(&mut self, serial: u64) -> Result<()> {
        let Some((target, digest)) = self.ready_checksum(serial) else {
            return Ok(());
        };
        let (target, digest) = (target.clone(), digest.to_owned());
        self.cancel_preview();
        self.begin_verified_download(target, digest)
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

    #[test]
    fn match_preview_keeps_nearby_lines_and_caps_long_context() {
        let text = format!(
            "\u{feff}Écho\r\n{}\r\n\r\n\tÉcho Écho\r\nfollowing\r\nlast nearby\r\noutside",
            "雪".repeat(170),
        );
        let filter = w11term::sftp::FilenameFilter {
            content_query: "^\\s*écho".into(),
            regex: true,
            ..Default::default()
        };
        let hits = filter.match_file_text_lines(&text).unwrap();
        assert_eq!(hits.iter().map(|hit| hit.line).collect::<Vec<_>>(), [1, 4]);
        assert_eq!(hits[0].line_start, '\u{feff}'.len_utf8());
        assert!(
            MatchContext::from_file(&text, hits[0])
                .unwrap()
                .before
                .is_empty()
        );
        let MatchContext {
            excerpt,
            before,
            after,
        } = MatchContext::from_file(&text, hits[1]).unwrap();
        assert_eq!(excerpt, " Écho Écho");
        assert_eq!(before, [format!("{}…", "雪".repeat(160)), String::new()]);
        assert_eq!(after, ["following", "last nearby"]);
        assert!(filter.match_file_text_lines("outside").unwrap().is_empty());
    }

    #[test]
    fn line_navigation_stays_in_bounds_and_only_moves_the_current_context() {
        let (mut files, entry) = selected(ListedKind::Known(EntryKind::File));
        let (serial, _) = files
            .begin_preview(FileTarget::Listed(entry), PreviewKind::Text)
            .unwrap();
        let text = "hit\r\n\r\nother\nhit hit\n";
        let filter = w11term::sftp::FilenameFilter {
            content_query: "hit".into(),
            ..Default::default()
        };
        files.finish_preview(serial, Ok(json!({"text": text})));
        files.preview.as_mut().unwrap().text_match = Some(MatchPreview {
            lines: filter.match_file_text_lines(text).unwrap(),
            selected: 0,
        });
        let current = |files: &FileDrawer| {
            files
                .preview
                .as_ref()
                .unwrap()
                .text_match
                .as_ref()
                .unwrap()
                .selected
        };
        files.move_preview_match(serial, false);
        files.move_preview_match(serial.wrapping_add(1), true);
        assert_eq!(current(&files), 0);
        files.move_preview_match(serial, true);
        files.move_preview_match(serial, true);
        assert_eq!(current(&files), 1);
        files.show_full_preview(serial, true);
        files.move_preview_match(serial, false);
        files.show_full_preview(serial, false);
        assert_eq!(current(&files), 1);
        files.move_preview_match(serial, false);
        assert_eq!(current(&files), 0);
        assert_eq!(files.take_preview_for_edit(serial).unwrap().1, text);
    }

    #[test]
    fn verified_download_reuses_only_the_current_checksum_and_selection() {
        let (mut files, entry) = selected(ListedKind::Known(EntryKind::File));
        let (properties, _) = files
            .begin_preview(FileTarget::Listed(entry.clone()), PreviewKind::Properties)
            .unwrap();
        files.finish_preview(properties, Ok(json!({"attrs": {"permissions": 0o100644}})));
        let (checksum, _) = files
            .begin_preview(FileTarget::Listed(entry), PreviewKind::Checksum(properties))
            .unwrap();
        let digest = "f".repeat(64);
        files.finish_preview(checksum, Ok(json!({"sha256": digest})));

        files.verify_checksum(properties).unwrap();
        assert!(files.dialog.is_none());
        let selection = files.selected.take();
        files.verify_checksum(checksum).unwrap();
        assert!(files.dialog.is_none());
        files.selected = selection;

        files.verify_checksum(checksum).unwrap();
        assert!(files.preview.is_none());
        let dialog = files.dialog.as_ref().unwrap();
        assert!(!dialog.resume_download);
        assert_eq!(dialog.destination().unwrap(), digest);
        assert!(
            matches!(&dialog.action, EditAction::VerifyDownload(FileTarget::Listed(entry))
            if entry.name == "résumé 100% [pilot].txt")
        );
    }

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
                size: None,
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
        let (serial, request) = files
            .begin_preview(FileTarget::Listed(entry.clone()), PreviewKind::Text)
            .unwrap();
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
            assert!(
                files
                    .begin_preview(FileTarget::Listed(entry), PreviewKind::Text)
                    .is_err()
            );
            assert!(!files.entry_dialog_open());
        }
        let (mut files, entry) = selected(ListedKind::Known(EntryKind::File));
        files.listing_revision += 1;
        assert!(
            files
                .begin_preview(FileTarget::Listed(entry.clone()), PreviewKind::Text)
                .is_err()
        );
        files.listing_revision = entry.revision;
        files.selected = None;
        assert!(
            files
                .begin_preview(FileTarget::Listed(entry), PreviewKind::Text)
                .is_err()
        );
    }

    #[test]
    fn discarded_reads_cannot_reopen_or_replace_another_preview() {
        let (mut files, entry) = selected(ListedKind::Known(EntryKind::File));
        let (first, _) = files
            .begin_preview(FileTarget::Listed(entry.clone()), PreviewKind::Text)
            .unwrap();
        let cancellation = files.preview.as_ref().unwrap().cancel.clone();
        files.close_preview(first);
        assert!(cancellation.is_cancelled());
        let (second, _) = files
            .begin_preview(FileTarget::Listed(entry.clone()), PreviewKind::Text)
            .unwrap();
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
        let (serial, _) = files
            .begin_preview(FileTarget::Listed(entry), PreviewKind::Text)
            .unwrap();
        let cancellation = files.preview.as_ref().unwrap().cancel.clone();
        files.observe_connection(3);
        files.finish_preview(serial, Ok(json!({"text":"old peer"})));
        assert!(cancellation.is_cancelled());
        assert!(files.preview.is_none());
    }

    #[test]
    fn properties_accept_all_listed_types_and_never_offer_an_editable_document() {
        for kind in [
            ListedKind::Known(EntryKind::File),
            ListedKind::Known(EntryKind::Directory),
            ListedKind::Known(EntryKind::Symlink),
            ListedKind::Unsupported,
            ListedKind::Missing,
        ] {
            let (mut files, entry) = selected(kind);
            let (serial, request) = files
                .begin_preview(FileTarget::Listed(entry.clone()), PreviewKind::Properties)
                .unwrap();
            assert!(
                matches!(request, Operation::EntryProperties { path } if path == "/srv/雪/résumé 100% [pilot].txt")
            );
            files.finish_preview(serial, Ok(json!({"attrs":{"size":17}})));
            let details = files.preview.as_ref().unwrap();
            assert!(matches!(&details.content, Contents::Ready(text) if text.contains("17 bytes")));
            assert!(!details.editable());
            files.start_editor(serial);
            assert!(files.editor.is_none());
            assert!(files.entry_dialog_open());
            files.close_preview(serial);
            assert!(!files.busy());
            assert!(files.check_selected(&entry).is_ok());
        }
    }

    #[test]
    fn stale_properties_callbacks_cannot_replace_or_close_a_text_preview() {
        let (mut files, entry) = selected(ListedKind::Known(EntryKind::File));
        let (details, _) = files
            .begin_preview(FileTarget::Listed(entry.clone()), PreviewKind::Properties)
            .unwrap();
        let cancel = files.preview.as_ref().unwrap().cancel.clone();
        files.close_preview(details);
        assert!(cancel.is_cancelled());
        let (text, _) = files
            .begin_preview(FileTarget::Listed(entry.clone()), PreviewKind::Text)
            .unwrap();
        files.finish_preview(details, Ok(json!({"attrs":{"size":0}})));
        files.close_preview(details);
        assert!(matches!(
            files.preview.as_ref().unwrap().content,
            Contents::Loading
        ));
        files.finish_preview(text, Ok(json!({"text":"actual file"})));
        assert!(files.preview.as_ref().unwrap().editable());
        files.close_preview(text);

        let (details, _) = files
            .begin_preview(FileTarget::Listed(entry.clone()), PreviewKind::Properties)
            .unwrap();
        let cancel = files.preview.as_ref().unwrap().cancel.clone();
        files.listing_revision += 1;
        files.finish_preview(details, Ok(json!({"attrs":{}})));
        assert!(cancel.is_cancelled());
        assert!(files.preview.is_none());
        assert!(
            files
                .begin_preview(FileTarget::Listed(entry), PreviewKind::Properties)
                .is_err()
        );
    }
}
