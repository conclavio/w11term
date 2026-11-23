//! Window interaction preferences and cell-based selections, independent of native UI.
use crate::terminal::Snapshot;
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::{collections::BTreeMap, sync::Arc};
mod find;
pub use find::FindSelection;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TabSwitcherMode {
    Disabled,
    #[default]
    InOrder,
    Mru,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MultiLinePasteWarning {
    #[default]
    Automatic,
    Always,
    Never,
}

// Windows Terminal also accepts true for automatic and false for never.
fn multiline_setting<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> Result<MultiLinePasteWarning, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Setting {
        Mode(MultiLinePasteWarning),
        Enabled(bool),
    }
    Ok(match Setting::deserialize(input)? {
        Setting::Mode(mode) => mode,
        Setting::Enabled(true) => MultiLinePasteWarning::Automatic,
        Setting::Enabled(false) => MultiLinePasteWarning::Never,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteWarning {
    MultiLine,
    Large,
    LargeMultiLine,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Interaction {
    pub copy_on_select: bool,
    pub trim_block_selection: bool,
    pub trim_paste: bool,
    #[serde(deserialize_with = "multiline_setting")]
    pub multi_line_paste_warning: MultiLinePasteWarning,
    pub large_paste_warning: bool,
    pub snap_to_grid_on_resize: bool,
    pub tab_switcher_mode: TabSwitcherMode,
    pub application_tab_titles: bool,
    pub scroll_to_zoom: bool,
    pub failed_command_marks: bool,
    pub sticky_command_context: bool,
    pub detect_urls: bool,
    pub osc52: bool,
    pub osc52_foreground_only: bool,
    pub word_delimiters: String,
}
impl Default for Interaction {
    fn default() -> Self {
        Self {
            copy_on_select: true,
            trim_block_selection: true,
            trim_paste: true,
            multi_line_paste_warning: MultiLinePasteWarning::Automatic,
            large_paste_warning: true,
            snap_to_grid_on_resize: true,
            tab_switcher_mode: TabSwitcherMode::InOrder,
            application_tab_titles: false,
            scroll_to_zoom: true,
            failed_command_marks: true,
            sticky_command_context: true,
            detect_urls: true,
            osc52: true,
            osc52_foreground_only: false,
            word_delimiters: " /\\()\"'-:,.;<>~!@#$%^&*|+=[]{}?│".into(),
        }
    }
}
pub fn paste_text(text: &str, trim: bool) -> &str {
    if trim { text.trim_end() } else { text }
}

impl Interaction {
    pub fn remote_clipboard_policy(&self) -> crate::clipboard::RemoteClipboardPolicy {
        use crate::clipboard::RemoteClipboardPolicy::*;
        match (self.osc52, self.osc52_foreground_only) {
            (false, _) => Disabled,
            (true, false) => AnyPane,
            (true, true) => ForegroundPane,
        }
    }

    /// Use the actual clipboard payload, after any trimming, for both the
    /// prompt decision and delivery. Raw paste never has bracket protection.
    pub fn clipboard_paste<'a>(
        &self,
        text: &'a str,
        raw: bool,
        bracketed: bool,
    ) -> (&'a str, Option<PasteWarning>) {
        let bracketed = !raw && bracketed;
        let text = paste_text(text, self.trim_paste && !raw && !bracketed);
        let warn_multiline = match self.multi_line_paste_warning {
            MultiLinePasteWarning::Automatic => !bracketed,
            MultiLinePasteWarning::Always => true,
            MultiLinePasteWarning::Never => false,
        };
        // Windows measures the clipboard's UTF-16 units. Count only far
        // enough to distinguish the last unprompted unit from the first over.
        let large = self.large_paste_warning && text.encode_utf16().nth(5 * 1024).is_some();
        let warning = match (warn_multiline && text.contains(['\r', '\n']), large) {
            (true, true) => Some(PasteWarning::LargeMultiLine),
            (true, false) => Some(PasteWarning::MultiLine),
            (false, true) => Some(PasteWarning::Large),
            (false, false) => None,
        };
        (text, warning)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub row: isize,
    pub col: usize,
}
#[derive(Clone, Debug)]
pub struct Selection {
    pub anchor: Point,
    pub end: Point,
    /// The upper endpoint is an insertion point instead of a selected cell.
    /// Select All uses this at column zero after a final hard line break.
    pub end_exclusive: bool,
    pub block: bool,
    pub alternate: bool,
    pub active: bool,
    pub dragging: bool,
    pub cols: usize,
    lines: BTreeMap<isize, Arc<wezterm_term::Line>>,
    clear_generation: Option<u64>,
    observed_screen: Option<crate::copy_mode::ScreenId>,
}
impl Selection {
    pub fn new(point: Point, block: bool, alternate: bool) -> Self {
        Self {
            anchor: point,
            end: point,
            end_exclusive: false,
            block,
            alternate,
            active: false,
            dragging: true,
            cols: 0,
            lines: BTreeMap::new(),
            clear_generation: None,
            observed_screen: None,
        }
    }
    pub fn survives_scrollback_clear(&self, frame: &Snapshot) -> bool {
        self.alternate
            || self
                .clear_generation
                .is_none_or(|old| old == frame.scrollback_clear_generation)
            || frame
                .scrollback_clear_floor
                .is_none_or(|floor| self.anchor.row.min(self.end.row) >= floor)
    }

    pub fn reconcile_scrollback_clear(&mut self, frame: &Snapshot) -> bool {
        if !self.survives_scrollback_clear(frame) {
            return false;
        }
        if self
            .clear_generation
            .replace(frame.scrollback_clear_generation)
            != Some(frame.scrollback_clear_generation)
            && !self.alternate
            && let Some(floor) = frame.scrollback_clear_floor
        {
            self.lines = self.lines.split_off(&floor);
        }
        true
    }

    pub fn observe(&mut self, frame: &Snapshot) {
        self.observed_screen.get_or_insert_with(|| frame.into());
        if !self.reconcile_scrollback_clear(frame) {
            return;
        }
        if self.cols != 0 && self.cols != frame.cols {
            self.active = false;
            self.dragging = false;
            self.lines.clear();
        }
        self.cols = frame.cols;
        for (i, line) in frame.rows.iter().enumerate() {
            self.lines
                .insert(frame.first_stable_row + i as isize, line.clone());
        }
        while self.lines.len() > 10000 {
            self.lines.pop_first();
        }
    }
    pub fn update(&mut self, point: Point) {
        if point != self.anchor {
            self.active = true;
        }
        self.end = point;
        self.end_exclusive = false;
    }
    pub fn span(&self, row: isize, cols: usize) -> Option<Range<usize>> {
        if !self.active {
            return None;
        }
        let (first, last) = if self.anchor <= self.end {
            (self.anchor, self.end)
        } else {
            (self.end, self.anchor)
        };
        if row < first.row || row > last.row {
            return None;
        }
        let (start, end) = if self.block {
            (
                self.anchor.col.min(self.end.col),
                self.anchor.col.max(self.end.col).saturating_add(1),
            )
        } else {
            (
                if row == first.row { first.col } else { 0 },
                if row == last.row {
                    last.col.saturating_add(usize::from(!self.end_exclusive))
                } else {
                    cols
                },
            )
        };
        Some(start.min(cols)..end.min(cols))
    }
    pub fn can_copy(&self, frame: &Snapshot) -> bool {
        self.active
            && self.alternate == frame.alternate
            && self.cols == frame.cols
            && self.survives_scrollback_clear(frame)
    }
    pub fn text(&self, frame: &Snapshot, trim_block: bool) -> String {
        if !self.can_copy(frame) {
            return String::new();
        }
        let mut lines = self.lines.clone();
        for (i, line) in frame.rows.iter().enumerate() {
            lines.insert(frame.first_stable_row + i as isize, line.clone());
        }
        self.text_from_rows(
            lines.iter().map(|(row, line)| (*row, &**line)),
            trim_block,
            usize::MAX,
        )
        .expect("unlimited selection")
    }

    pub(crate) fn text_from_rows<'a>(
        &self,
        lines: impl Iterator<Item = (isize, &'a wezterm_term::Line)>,
        trim_block: bool,
        limit: usize,
    ) -> Result<String, &'static str> {
        let shape = match (self.block, trim_block) {
            (false, _) => TextShape::Linear,
            (true, true) => TextShape::TrimmedBlock,
            (true, false) => TextShape::PaddedBlock,
        };
        plain_text(
            lines.filter_map(|(row, line)| Some((line, self.span(row, self.cols)?))),
            self.cols,
            shape,
            limit,
            "Selection exceeds the clipboard size limit",
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextShape {
    Linear,
    TrimmedBlock,
    PaddedBlock,
}

/// Copy and export use the same grapheme, wrap and whitespace rules. Check the
/// actual UTF-8 byte count before appending, including column gaps and CRLF.
pub(crate) fn plain_text<'a>(
    rows: impl Iterator<Item = (&'a wezterm_term::Line, Range<usize>)>,
    cols: usize,
    shape: TextShape,
    limit: usize,
    limit_error: &'static str,
) -> Result<String, &'static str> {
    let mut output = String::new();
    let mut hard_break = false;
    for (line, range) in rows {
        if hard_break {
            append_text(&mut output, 0, "\r\n", limit, limit_error)?;
        }
        hard_break =
            shape != TextShape::Linear || range.end != cols || !line.last_cell_was_wrapped();
        let row_cols = (cols / wezterm_term::line_width_scale(line)).max(cols.min(1));
        let range = range.start.min(row_cols)..range.end.min(row_cols);
        let trim = hard_break && shape != TextShape::PaddedBlock;
        let last_text = if trim {
            // Locate the last non-whitespace cell before allocating. A large
            // blank or padded row can legitimately serialize to zero bytes.
            let last = line
                .visible_cells()
                .take_while(|c| c.cell_index() < range.end)
                .filter(|c| {
                    c.cell_index() + c.width() > range.start && !c.str().trim_end().is_empty()
                })
                .last();
            let Some(cell) = last else { continue };
            Some(cell.cell_index())
        } else {
            None
        };
        let mut column = range.start;
        for cell in line
            .visible_cells()
            .take_while(|c| c.cell_index() < range.end)
        {
            let start = cell.cell_index();
            if start + cell.width() <= range.start {
                continue;
            }
            let last = Some(start) == last_text;
            let text = if last {
                cell.str().trim_end()
            } else {
                cell.str()
            };
            append_text(
                &mut output,
                start.saturating_sub(column),
                text,
                limit,
                limit_error,
            )?;
            column = start + cell.width();
            if last {
                break;
            }
        }
        if shape == TextShape::PaddedBlock {
            append_text(
                &mut output,
                range.end.saturating_sub(column),
                "",
                limit,
                limit_error,
            )?;
        }
    }
    Ok(output)
}

fn append_text(
    output: &mut String,
    spaces: usize,
    text: &str,
    limit: usize,
    limit_error: &'static str,
) -> Result<(), &'static str> {
    let size = output
        .len()
        .checked_add(spaces)
        .and_then(|size| size.checked_add(text.len()))
        .filter(|&size| size <= limit)
        .ok_or(limit_error)?;
    if size > output.capacity() {
        let capacity = size
            .max(output.capacity().saturating_mul(2))
            .max(64)
            .min(limit);
        output
            .try_reserve_exact(capacity - output.len())
            .map_err(|_| "Not enough memory to capture terminal text")?;
    }
    output.extend(std::iter::repeat_n(' ', spaces));
    output.push_str(text);
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WordClass {
    Whitespace,
    Delimiter,
    Text,
}

pub fn word_class(grapheme: &str, delimiters: &str) -> WordClass {
    if grapheme.chars().all(char::is_whitespace) {
        WordClass::Whitespace
    } else if grapheme.chars().any(|c| delimiters.contains(c)) {
        WordClass::Delimiter
    } else {
        WordClass::Text
    }
}

pub fn word_range(frame: &Snapshot, point: Point, delimiters: &str) -> Range<usize> {
    let Some(line) = usize::try_from(point.row - frame.first_stable_row)
        .ok()
        .and_then(|i| frame.rows.get(i))
    else {
        return point.col..point.col + 1;
    };
    let cells: Vec<_> = line.visible_cells().collect();
    let Some(index) = cells
        .iter()
        .position(|c| c.cell_index() <= point.col && c.cell_index() + c.width() > point.col)
    else {
        return point.col..point.col + 1;
    };
    let category = |s: &str| word_class(s, delimiters);
    let class = category(cells[index].str());
    let mut first = index;
    let mut last = index;
    while first > 0 && category(cells[first - 1].str()) == class {
        first -= 1;
    }
    while last + 1 < cells.len() && category(cells[last + 1].str()) == class {
        last += 1;
    }
    cells[first].cell_index()..cells[last].cell_index() + cells[last].width()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub uri: String,
    pub spans: Vec<(isize, Range<usize>)>,
}
pub fn safe_url(uri: &str) -> bool {
    let lower = uri.to_ascii_lowercase();
    !uri.chars().any(char::is_control)
        && (lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("mailto:"))
}
pub fn link_at(frame: &Snapshot, point: Point, detect: bool) -> Option<Link> {
    let index = usize::try_from(point.row - frame.first_stable_row).ok()?;
    let line = frame.rows.get(index)?;
    if let Some(cell) = line
        .visible_cells()
        .find(|c| c.cell_index() <= point.col && c.cell_index() + c.width() > point.col)
        && let Some(link) = cell.attrs().hyperlink()
        && safe_url(link.uri())
    {
        let uri = link.uri().to_owned();
        let spans = frame
            .rows
            .iter()
            .enumerate()
            .flat_map(|(y, line)| {
                line.visible_cells()
                    .filter(|c| c.attrs().hyperlink().is_some_and(|link| link.uri() == uri))
                    .map(|c| {
                        (
                            frame.first_stable_row + y as isize,
                            c.cell_index()..c.cell_index() + c.width(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        return Some(Link { uri, spans });
    }
    if !detect {
        return None;
    }
    let mut first = index;
    let mut last = index;
    while first > 0 && frame.rows[first - 1].last_cell_was_wrapped() {
        first -= 1;
    }
    while last + 1 < frame.rows.len() && frame.rows[last].last_cell_was_wrapped() {
        last += 1;
    }
    let mut text = String::new();
    let mut map = Vec::new();
    for y in first..=last {
        for cell in frame.rows[y]
            .visible_cells()
            .take_while(|c| c.cell_index() < frame.cols)
        {
            let start = text.len();
            text.push_str(cell.str());
            map.push((
                start..text.len(),
                Point {
                    row: frame.first_stable_row + y as isize,
                    col: cell.cell_index(),
                },
                cell.width(),
            ));
        }
    }
    for (range, uri) in text_urls(&text) {
        let spans: Vec<_> = map
            .iter()
            .filter(|(r, _, _)| r.start < range.end && r.end > range.start)
            .map(|(_, p, width)| (p.row, p.col..p.col + width))
            .collect();
        if spans
            .iter()
            .any(|(row, cols)| *row == point.row && cols.contains(&point.col))
        {
            return Some(Link { uri, spans });
        }
    }
    None
}

/// Shared by pointer hit testing and keyboard hints, including punctuation rules.
pub(crate) fn text_urls(text: &str) -> impl Iterator<Item = (Range<usize>, String)> + '_ {
    static URL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?i)(?:https?://|mailto:|www\.)[^\s\p{Cc}<>"'`]*"#).unwrap()
    });
    URL.find_iter(text).filter_map(|m| {
        let mut url = m.as_str();
        let surplus = |open, close| {
            url.matches(close)
                .count()
                .saturating_sub(url.matches(open).count())
        };
        let mut excess = [surplus('(', ')'), surplus('[', ']'), surplus('{', '}')];
        while let Some(byte) = url.as_bytes().last() {
            if let Some(i) = b")]}".iter().position(|c| c == byte) {
                if excess[i] == 0 {
                    break;
                }
                excess[i] -= 1;
            } else if !b".,;!?".contains(byte) {
                break;
            }
            url = &url[..url.len() - 1];
        }
        let uri = if url.get(..4).is_some_and(|s| s.eq_ignore_ascii_case("www.")) {
            format!("https://{url}")
        } else {
            url.to_owned()
        };
        safe_url(&uri).then_some((m.start()..m.start() + url.len(), uri))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_osc52_settings_keep_their_scope_and_the_disable_switch_takes_precedence() {
        use crate::clipboard::RemoteClipboardPolicy::*;
        for (json, expected) in [
            (r#"{}"#, AnyPane),
            (r#"{"osc52":true}"#, AnyPane),
            (r#"{"osc52":false}"#, Disabled),
            (r#"{"osc52ForegroundOnly":true}"#, ForegroundPane),
            (r#"{"osc52":false,"osc52ForegroundOnly":true}"#, Disabled),
        ] {
            let settings: Interaction = serde_json::from_str(json).unwrap();
            assert_eq!(settings.remote_clipboard_policy(), expected);
        }
    }
    fn screen(bytes: &[u8], cols: usize) -> Snapshot {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = crate::terminal::create(
            wezterm_term::TerminalSize {
                cols,
                rows: 8,
                ..Default::default()
            },
            tx,
        );
        terminal.advance_bytes(bytes);
        Snapshot::capture(&terminal, None, 1, 0, "test".into(), 0)
    }
    fn selection(
        frame: &Snapshot,
        start: (usize, usize),
        end: (usize, usize),
        block: bool,
    ) -> Selection {
        let mut selection = Selection::new(
            Point {
                row: frame.first_stable_row + start.0 as isize,
                col: start.1,
            },
            block,
            frame.alternate,
        );
        selection.update(Point {
            row: frame.first_stable_row + end.0 as isize,
            col: end.1,
        });
        selection.observe(frame);
        selection
    }
    #[test]
    fn copying_joins_soft_wraps_trims_hard_lines_and_uses_plain_graphemes() {
        let frame = screen(b"\x1b[31m1234567890abc\x1b[0m\r\nnext   ", 10);
        let selected = selection(&frame, (0, 0), (2, 8), false);
        assert_eq!(selected.text(&frame, true), "1234567890abc\r\nnext");
        let frame = screen("a界e\u{301}z".as_bytes(), 20);
        assert_eq!(
            selection(&frame, (0, 2), (0, 3), false).text(&frame, true),
            "界e\u{301}"
        );
    }
    #[test]
    fn rectangular_copy_padding_and_reverse_selection() {
        let frame = screen(b"alpha\r\nb\r\ncat", 20);
        let selected = selection(&frame, (2, 4), (0, 0), true);
        assert_eq!(selected.text(&frame, true), "alpha\r\nb\r\ncat");
        assert_eq!(selected.text(&frame, false), "alpha\r\nb    \r\ncat  ");
        let mut other = frame.clone();
        other.alternate = !other.alternate;
        assert!(selected.text(&other, true).is_empty());
    }
    #[test]
    fn word_selection_and_paste_trim_preserve_internal_whitespace() {
        let frame = screen(b"hello/world  words", 30);
        let range = word_range(
            &frame,
            Point {
                row: frame.first_stable_row,
                col: 8,
            },
            &Interaction::default().word_delimiters,
        );
        assert_eq!(range, 6..11);
        assert_eq!(paste_text("one  \r\n  two \r\n\t ", true), "one  \r\n  two");
        assert_eq!(paste_text("x \r\n", false), "x \r\n");
    }

    #[test]
    fn clipboard_paste_warns_for_the_bytes_that_will_be_sent() {
        let mut settings = Interaction::default();
        let text = "one\r\ntwo \r\n";
        assert_eq!(
            settings.clipboard_paste(text, false, false),
            ("one\r\ntwo", Some(PasteWarning::MultiLine))
        );
        assert_eq!(settings.clipboard_paste(text, false, true), (text, None));
        assert_eq!(
            settings.clipboard_paste(text, true, true),
            (text, Some(PasteWarning::MultiLine))
        );
        assert_eq!(
            settings.clipboard_paste("one\r\n", false, false),
            ("one", None)
        );
        assert_eq!(settings.clipboard_paste("", true, false), ("", None));
        settings.multi_line_paste_warning = MultiLinePasteWarning::Always;
        assert_eq!(
            settings.clipboard_paste(text, false, true).1,
            Some(PasteWarning::MultiLine)
        );
        settings.multi_line_paste_warning = MultiLinePasteWarning::Never;
        settings.trim_paste = false;
        assert_eq!(settings.clipboard_paste(text, false, false), (text, None));
    }

    #[test]
    fn large_paste_counts_windows_clipboard_units_and_combines_warnings() {
        let mut settings = Interaction::default();
        let at_limit = "😀".repeat(2560);
        assert!(
            settings
                .clipboard_paste(&at_limit, false, false)
                .1
                .is_none()
        );
        let over = format!("{at_limit}\n");
        assert_eq!(
            settings.clipboard_paste(&over, false, false),
            (at_limit.as_str(), None)
        );
        assert_eq!(
            settings.clipboard_paste(&over, false, true).1,
            Some(PasteWarning::Large)
        );
        assert_eq!(
            settings.clipboard_paste(&over, true, false).1,
            Some(PasteWarning::LargeMultiLine)
        );
        settings.large_paste_warning = false;
        assert_eq!(
            settings.clipboard_paste(&over, true, false).1,
            Some(PasteWarning::MultiLine)
        );
    }

    #[test]
    fn paste_preferences_accept_legacy_booleans_and_round_trip() {
        for (value, mode) in [
            (serde_json::json!(true), MultiLinePasteWarning::Automatic),
            (serde_json::json!(false), MultiLinePasteWarning::Never),
            (serde_json::json!("always"), MultiLinePasteWarning::Always),
        ] {
            let settings: Interaction = serde_json::from_value(
                serde_json::json!({"multiLinePasteWarning":value, "largePasteWarning":false}),
            )
            .unwrap();
            assert_eq!(settings.multi_line_paste_warning, mode);
            let saved = serde_json::to_value(&settings).unwrap();
            assert_eq!(
                saved["multiLinePasteWarning"],
                serde_json::to_value(mode).unwrap()
            );
            assert_eq!(saved["largePasteWarning"], false);
        }
        let old: Interaction = serde_json::from_str("{}").unwrap();
        assert_eq!(
            old.multi_line_paste_warning,
            MultiLinePasteWarning::Automatic
        );
        assert!(old.large_paste_warning);
    }
    #[test]
    fn urls_cover_wrapped_lines_explicit_links_and_safe_schemes() {
        let frame = screen(b"https://example.com/a(b).", 12);
        let point = Point {
            row: frame.first_stable_row + 1,
            col: 2,
        };
        let link = link_at(&frame, point, true).unwrap();
        assert_eq!(link.uri, "https://example.com/a(b)");
        assert!(
            link.spans
                .iter()
                .any(|(row, _)| *row == frame.first_stable_row)
        );
        assert!(link_at(&frame, point, false).is_none());
        let frame = screen(b"\x1b]8;;https://example.com\x1b\\label\x1b]8;;\x1b\\", 20);
        assert_eq!(
            link_at(
                &frame,
                Point {
                    row: frame.first_stable_row,
                    col: 1
                },
                false
            )
            .unwrap()
            .uri,
            "https://example.com"
        );
        assert!(!safe_url("javascript:alert(1)"));
        assert!(!safe_url("file:///C:/Windows/notepad.exe"));
        assert!(!safe_url("https://example.com\nattack"));
    }
    #[test]
    fn clearing_scrollback_invalidates_only_selections_touching_erased_rows() {
        let original = screen(b"old\r\nkeep\r\nlast", 12);
        let first = original.first_stable_row;
        let mut reversed = selection(&original, (2, 3), (0, 1), true);
        let mut retained = selection(&original, (1, 0), (2, 3), false);
        // "keep" was on screen at clear time, then ordinary output evicted it
        // before the UI observed this frame. Its selection is still copyable.
        let mut frame = screen(b"last\r\nnew", 12);
        frame.first_stable_row = first + 2;
        frame.oldest_stable_row = first + 2;
        frame.scrollback_clear_generation += 1;
        frame.scrollback_clear_floor = Some(first + 1);
        // Copy can run before the UI has received the committed frame notification.
        assert_eq!(reversed.text(&frame, true), "");
        assert!(!reversed.reconcile_scrollback_clear(&frame));
        assert!(retained.reconcile_scrollback_clear(&frame));
        assert!(!retained.lines.contains_key(&first));
        assert_eq!(retained.text(&frame, true), "keep\r\nlast");
    }

    #[test]
    fn selection_remembers_rows_scrolled_out_of_the_viewport() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = crate::terminal::create(
            wezterm_term::TerminalSize {
                cols: 10,
                rows: 2,
                ..Default::default()
            },
            tx,
        );
        terminal.advance_bytes(b"first\r\nsecond");
        let first = Snapshot::capture(&terminal, None, 1, 0, "test".into(), 0);
        let mut selected = selection(&first, (0, 0), (1, 5), false);
        terminal.advance_bytes(b"\r\nthird\r\nfourth");
        let last = Snapshot::capture(&terminal, None, 2, 0, "test".into(), 0);
        selected.observe(&last);
        selected.update(Point {
            row: last.first_stable_row + 1,
            col: 5,
        });
        assert_eq!(
            selected.text(&last, true),
            "first\r\nsecond\r\nthird\r\nfourth"
        );
    }
}
