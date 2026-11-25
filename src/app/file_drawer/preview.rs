use super::*;
use std::cell::OnceCell;
use w11term::sftp::{FileTextMatch, PermissionReview};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    Text,
    Properties,
    Checksum(u64),
}

#[derive(Clone)]
pub enum PermissionsAction {
    Edit,
    Input(String),
    Apply,
    Back,
}

#[derive(Default)]
enum Permissions {
    #[default]
    Unavailable,
    Ready(PermissionReview),
    Editing(PermissionReview, String),
    Applying,
}

fn parse_mode(input: &str) -> Option<u32> {
    let input = input.trim();
    if !(3..=4).contains(&input.len()) || !input.bytes().all(|c| (b'0'..=b'7').contains(&c)) {
        return None;
    }
    u32::from_str_radix(input, 8).ok()
}

pub(super) struct EntryPreview {
    serial: u64,
    target: FileTarget,
    kind: PreviewKind,
    content: Contents,
    text_match: Option<MatchPreview>,
    full_text: bool,
    checksum_available: bool,
    permissions: Permissions,
    link_target: Option<String>,
    opening_link: bool,
    status: Option<String>,
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
                if self.checksum_available
                    && !matches!(
                        self.permissions,
                        Permissions::Editing(..) | Permissions::Applying
                    )
                {
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
                if let Some(controls) = self.permissions_controls(pane, generation, context) {
                    fields.push(KeyedView::new("permissions", controls));
                }
                if self.link_target.is_some() {
                    fields.push(KeyedView::new(
                        "open-link-target",
                        Button::new()
                            .is_enabled(!self.opening_link)
                            .on_click(context.callback(move |_| {
                                Message::Files(FileAction::OpenLinkTarget(pane, generation, serial))
                            }))
                            .content(if self.opening_link {
                                "Opening link target…"
                            } else {
                                "Open link target"
                            }),
                    ));
                }
                if let Some(status) = &self.status {
                    fields.push(KeyedView::new(
                        "preview-status",
                        TextBlock::new()
                            .automation_id("sftp-preview-status")
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

    fn permissions_controls(
        &self,
        pane: u64,
        generation: u64,
        context: &ViewContext<TerminalApp>,
    ) -> Option<View> {
        let serial = self.serial;
        let action =
            move |action| Message::Files(FileAction::Permissions(pane, generation, serial, action));
        let button = |label, enabled, event: PermissionsAction| {
            Button::new()
                .automation_name(label)
                .is_enabled(enabled)
                .on_click(context.callback(move |_| action(event.clone())))
                .content(label)
        };
        match &self.permissions {
            Permissions::Unavailable => None,
            Permissions::Ready(_) => {
                Some(button("Edit permissions…", true, PermissionsAction::Edit))
            }
            Permissions::Editing(review, input) => {
                let input_field = TextBox::new()
                    .automation_name("Permission mode (octal)")
                    .placeholder_text("Permission mode (octal)")
                    .text(input)
                    .on_text_changed(
                        context.callback(move |mode| action(PermissionsAction::Input(mode))),
                    );
                let help = TextBlock::new()
                    .text("Enter three or four octal digits. The leading digit sets special bits in a four-digit mode.")
                    .text_wrapping(TextWrapping::Wrap);
                let actions = StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(6.0)
                    .children((
                        button(
                            "Apply permissions",
                            parse_mode(input).is_some_and(|mode| mode != review.mode()),
                            PermissionsAction::Apply,
                        ),
                        button("Cancel edit", true, PermissionsAction::Back),
                    ));
                Some(
                    StackPanel::new()
                        .spacing(6.0)
                        .children((input_field, help, actions)),
                )
            }
            Permissions::Applying => Some(TextBlock::new().text("Applying permissions…").into()),
        }
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
            permissions: Permissions::Unavailable,
            link_target: None,
            opening_link: false,
            status: None,
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
                let target = value
                    .get("target")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                super::properties::display(&path, value)
                    .map(|(attrs, text)| {
                        let kind = EntryKind::from_permissions(attrs.permissions);
                        preview.checksum_available = kind == Some(EntryKind::File);
                        if entry_path(preview.target.parent(), preview.target.name()).is_ok() {
                            if let Some(review) = PermissionReview::from_attributes(&attrs) {
                                preview.permissions = Permissions::Ready(review);
                            }
                            if kind == Some(EntryKind::Symlink) {
                                preview.link_target =
                                    target.filter(|path| !path.is_empty() && !path.contains('\0'));
                            }
                        }
                        text
                    })
                    .map_err(|error| format!("{error:#}"))
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

    pub(super) fn begin_link_target(&mut self, serial: u64) -> Option<Operation> {
        let preview = self.preview.as_ref()?;
        if !self.open || preview.serial != serial || preview.opening_link || self.pending.is_some()
        {
            return None;
        }
        let expected_target = preview.link_target.clone()?;
        let path = self.checked_target_path(&preview.target).ok()?;
        let preview = self.preview.as_mut()?;
        preview.opening_link = true;
        preview.status = None;
        preview.view.take();
        Some(Operation::OpenLinkTarget {
            path,
            expected_target,
        })
    }

    pub(super) fn opening_link_target(&self, serial: u64) -> bool {
        self.open
            && self
                .preview
                .as_ref()
                .is_some_and(|preview| preview.serial == serial && preview.opening_link)
    }

    pub(super) fn link_target_error(&mut self, serial: u64, error: String) {
        if let Some(preview) = self
            .preview
            .as_mut()
            .filter(|p| p.serial == serial && p.opening_link)
        {
            preview.opening_link = false;
            preview.status = Some(format!("Could not open link target. {error}"));
            preview.view.take();
        }
    }

    pub(super) fn permissions_action(
        &mut self,
        serial: u64,
        action: PermissionsAction,
    ) -> Option<(u32, Operation)> {
        let current = self.preview.as_ref()?;
        if !self.open || current.serial != serial || self.pending.is_some() {
            return None;
        }
        let path = self.checked_target_path(&current.target).ok()?;
        let preview = self.preview.as_mut()?;
        match (&mut preview.permissions, action) {
            (Permissions::Ready(review), PermissionsAction::Edit) => {
                preview.permissions =
                    Permissions::Editing(*review, format!("{:04o}", review.mode()));
                preview.status = None;
            }
            (Permissions::Editing(_, input), PermissionsAction::Input(value)) => *input = value,
            (Permissions::Editing(review, _), PermissionsAction::Back) => {
                preview.permissions = Permissions::Ready(*review);
            }
            (Permissions::Editing(review, input), PermissionsAction::Apply) => {
                let mode = parse_mode(input).filter(|&mode| mode != review.mode())?;
                let operation = Operation::SetEntryPermissions {
                    path,
                    review: *review,
                    mode,
                };
                preview.permissions = Permissions::Applying;
                preview.view.take();
                return Some((mode, operation));
            }
            _ => return None,
        }
        preview.view.take();
        None
    }

    pub(super) fn finish_permissions(
        &mut self,
        serial: u64,
        requested: u32,
        reply: Result<Value, String>,
    ) {
        let notice = match reply {
            Ok(value) => {
                let notice = match value["attrs"]["permissions"]
                    .as_u64()
                    .map(|mode| mode & 0o7777)
                {
                    Some(mode) if mode == u64::from(requested) => {
                        format!("Permissions updated to {mode:04o}")
                    }
                    Some(mode) => {
                        format!("Server reports mode {mode:04o} after applying {requested:04o}")
                    }
                    None => "Server acknowledged the change and did not report a mode".into(),
                };
                self.finish_preview(serial, Ok(value));
                notice
            }
            Err(error) => format!("Could not verify the requested mode. {error}"),
        };
        if let Some(preview) = self
            .preview
            .as_mut()
            .filter(|preview| preview.serial == serial)
        {
            if matches!(preview.permissions, Permissions::Applying) {
                preview.permissions = Permissions::Unavailable;
            }
            preview.status = Some(notice);
            preview.view.take();
        } else {
            self.status = notice;
        }
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
        preview.status = Some(status);
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
                Some(Step::Preview(_) | Step::OpenLinkTarget(_))
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
    fn permission_edits_require_the_reviewed_entry_and_show_the_reported_mode() {
        let (mut drawer, entry) = selected(ListedKind::Known(EntryKind::File));
        let (serial, _) = drawer
            .begin_preview(FileTarget::Listed(entry), PreviewKind::Properties)
            .unwrap();
        drawer.finish_preview(
            serial,
            Ok(json!({"attrs":{"permissions":0o100640,"uid":1701,"gid":2048}})),
        );
        assert!(
            drawer
                .permissions_action(serial + 1, PermissionsAction::Edit)
                .is_none()
        );
        assert!(matches!(
            drawer.preview.as_ref().unwrap().permissions,
            Permissions::Ready(_)
        ));
        drawer.permissions_action(serial, PermissionsAction::Edit);
        for invalid in ["8", "0999", "0o700", "00000", "640"] {
            drawer.permissions_action(serial, PermissionsAction::Input(invalid.into()));
            assert!(
                drawer
                    .permissions_action(serial, PermissionsAction::Apply)
                    .is_none(),
                "{invalid}"
            );
        }
        drawer.permissions_action(serial, PermissionsAction::Input("4750".into()));
        let (mode, operation) = drawer
            .permissions_action(serial, PermissionsAction::Apply)
            .unwrap();
        assert_eq!(mode, 0o4750);
        assert!(
            matches!(operation, Operation::SetEntryPermissions { mode: 0o4750, review, .. } if review.mode() == 0o640)
        );
        assert!(
            drawer
                .permissions_action(serial, PermissionsAction::Apply)
                .is_none()
        );
        drawer.finish_permissions(
            serial,
            mode,
            Ok(json!({"attrs":{"permissions":0o100750,"uid":1701,"gid":2048}})),
        );
        let preview = drawer.preview.as_ref().unwrap();
        assert!(
            matches!(preview.permissions, Permissions::Ready(review) if review.mode() == 0o750)
        );
        assert_eq!(
            preview.status.as_deref(),
            Some("Server reports mode 0750 after applying 4750")
        );
    }

    #[test]
    fn opening_a_link_uses_the_current_properties_and_keeps_them_on_error() {
        let (mut drawer, entry) = selected(ListedKind::Known(EntryKind::Symlink));
        let (serial, _) = drawer
            .begin_preview(FileTarget::Listed(entry), PreviewKind::Properties)
            .unwrap();
        drawer.finish_preview(
            serial,
            Ok(json!({"attrs":{"permissions":0o120777},"target":"../payload $now.txt"})),
        );
        assert!(drawer.begin_link_target(serial + 1).is_none());
        assert!(matches!(
            drawer.begin_link_target(serial),
            Some(Operation::OpenLinkTarget { expected_target, .. }) if expected_target == "../payload $now.txt"
        ));
        assert!(drawer.opening_link_target(serial));
        assert!(drawer.begin_link_target(serial).is_none());
        drawer.link_target_error(serial, "Missing target".into());
        let preview = drawer.preview.as_ref().unwrap();
        assert!(!preview.opening_link);
        assert!(
            matches!(&preview.content, Contents::Ready(text) if text.contains("../payload $now.txt"))
        );
        assert_eq!(
            preview.status.as_deref(),
            Some("Could not open link target. Missing target")
        );
        drawer.close_preview(serial);
        assert!(!drawer.opening_link_target(serial));
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
