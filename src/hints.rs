//! Keyboard targets in a single committed viewport, in terminal reading order.
use crate::{
    interaction::{safe_url, text_urls},
    terminal::Snapshot,
};
use std::{ops::Range, sync::LazyLock};
mod patterns;
pub use patterns::{MAX_PATTERNS, Patterns};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Url(String),
    Path(String),
    Pattern { name: String, text: String },
}

impl Target {
    pub fn text(&self) -> &str {
        match self {
            Self::Url(s) | Self::Path(s) | Self::Pattern { text: s, .. } => s,
        }
    }

    pub fn file_path(&self) -> Option<&str> {
        match self {
            Self::Path(path) => Some(path),
            Self::Pattern { text, .. } if self.pattern_url().is_none() => path_token(text, true),
            _ => None,
        }
    }

    pub fn pattern_url(&self) -> Option<String> {
        let Self::Pattern { text, .. } = self else {
            return None;
        };
        text_urls(text)
            .find(|(span, _)| span.start == 0 && span.end == text.len())
            .map(|(_, url)| url)
    }
}

#[derive(Debug)]
pub struct Hint {
    pub target: Target,
    pub row: usize,
    pub col: usize,
    pub label: String,
}

struct Cell {
    start: usize,
    row: usize,
    col: usize,
    occupied: bool,
}

fn diagnostic_path(token: &str) -> &str {
    // Strip diagnostic punctuation, including rustc's path:line:column.
    let mut path = token
        .trim_start_matches(['(', '[', '{'])
        .trim_end_matches([')', ']', '}', ',', ';', ':', '!']);
    if !matches!(path.rsplit(['/', '\\']).next(), Some("." | "..")) {
        path = path.trim_end_matches('.');
    }
    for _ in 0..2 {
        if let Some((base, line)) = path.rsplit_once(':')
            && !line.is_empty()
            && line.bytes().all(|b| b.is_ascii_digit())
        {
            path = base;
        }
    }
    path
}

fn path_token(token: &str, quoted: bool) -> Option<&str> {
    let path = if quoted {
        token
    } else {
        diagnostic_path(token)
    };
    let rest = if path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic()
    {
        &path[2..]
    } else {
        path
    };
    if rest.is_empty()
        || rest.contains([':', '=', '*', '?', '|', '<', '>'])
        || rest.chars().any(char::is_control)
        || path.len() > 4096
    {
        return None;
    }
    let filename = rest.rsplit(['/', '\\']).next()?;
    let has_extension = filename.rsplit_once('.').is_some_and(|(stem, ext)| {
        (stem.is_empty() || stem.chars().any(|c| c.is_alphabetic() || c == '_'))
            && ext.starts_with(char::is_alphabetic)
            && ext.chars().all(char::is_alphanumeric)
    });
    (has_extension || (rest.contains(['/', '\\']) && rest.chars().any(char::is_alphanumeric)))
        .then_some(path)
}

// A pattern is local configuration, but the haystack comes from the remote pane.
// Skip oversized logical lines intact to preserve boundary and anchor semantics.
const MAX_PATTERN_LINE: usize = 16 * 1024;
const MAX_PATTERN_VIEW: usize = 256 * 1024;
const MAX_PATTERN_MATCHES: usize = 512;

struct Scan<'a> {
    patterns: &'a Patterns,
    bytes_left: usize,
    matches_left: usize,
}

fn line_targets(
    text: &str,
    cells: &mut [Cell],
    detect_urls: bool,
    scan: &mut Scan<'_>,
    hints: &mut Vec<Hint>,
) {
    static TOKENS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#""([^"]+)"|'([^']+)'|`([^`]+)`|([^\s"'`<>]+)"#).unwrap()
    });
    let mut urls: Vec<_> = text_urls(text).collect();
    let mut add = |range: Range<usize>, target| {
        let first = cells
            .partition_point(|c| c.start <= range.start)
            .saturating_sub(1);
        let end = cells.partition_point(|c| c.start < range.end);
        let segment = &mut cells[first..end];
        if let Some(first) = segment.first()
            && !segment.iter().any(|c| c.occupied)
        {
            hints.push(Hint {
                target,
                row: first.row,
                col: first.col,
                label: String::new(),
            });
            for cell in segment {
                cell.occupied = true;
            }
        }
    };
    if !scan.patterns.is_empty() && text.len() <= MAX_PATTERN_LINE && text.len() <= scan.bytes_left
    {
        scan.bytes_left -= text.len();
        for pattern in scan.patterns.rules() {
            if scan.matches_left == 0 {
                break;
            }
            for m in pattern.regex.find_iter(text).take(scan.matches_left) {
                scan.matches_left -= 1;
                add(
                    m.range(),
                    Target::Pattern {
                        name: pattern.name.clone(),
                        text: m.as_str().into(),
                    },
                );
            }
        }
    }
    let mut next_url = 0;
    for captures in TOKENS.captures_iter(text) {
        let token = captures.iter().skip(1).flatten().next().unwrap();
        while next_url < urls.len() && urls[next_url].0.end <= token.start() {
            next_url += 1;
        }
        if urls
            .get(next_url)
            .is_some_and(|(r, _)| r.start < token.end())
        {
            continue;
        }
        if let Some(path) = path_token(token.as_str(), captures.get(4).is_none()) {
            let start = token.start() + token.as_str().find(path).unwrap();
            add(start..start + path.len(), Target::Path(path.to_owned()));
        }
    }
    if detect_urls {
        for (range, uri) in urls.drain(..) {
            add(range, Target::Url(uri));
        }
    }
}

pub fn viewport(frame: &Snapshot, detect_urls: bool) -> Vec<Hint> {
    viewport_with_patterns(frame, detect_urls, &Patterns::default())
}

pub fn viewport_with_patterns(
    frame: &Snapshot,
    detect_urls: bool,
    patterns: &Patterns,
) -> Vec<Hint> {
    let mut hints = Vec::new();
    let mut text = String::new();
    let mut cells = Vec::new();
    let mut previous_link = None;
    let mut scan = Scan {
        patterns,
        bytes_left: MAX_PATTERN_VIEW,
        matches_left: MAX_PATTERN_MATCHES,
    };
    for (row, line) in frame.rows.iter().enumerate() {
        for cell in line
            .visible_cells()
            .take_while(|c| c.cell_index() < frame.cols)
        {
            let col = cell.cell_index();
            let link = cell.attrs().hyperlink();
            if link != previous_link.as_ref()
                && let Some(link) = link
                && safe_url(link.uri())
            {
                hints.push(Hint {
                    target: Target::Url(link.uri().to_owned()),
                    row,
                    col,
                    label: String::new(),
                });
            }
            previous_link = link.cloned();
            cells.push(Cell {
                start: text.len(),
                row,
                col,
                occupied: link.is_some(),
            });
            text.push_str(cell.str());
        }
        if !line.last_cell_was_wrapped() || row + 1 == frame.rows.len() {
            line_targets(&text, &mut cells, detect_urls, &mut scan, &mut hints);
            text.clear();
            cells.clear();
            previous_link = None;
        }
    }
    hints.sort_unstable_by_key(|h| (h.row, h.col));
    // Equal-width labels are prefix-free, so selection never needs a timeout.
    let mut width = 1;
    let mut capacity = 26;
    while hints.len() > capacity {
        capacity *= 26;
        width += 1;
    }
    for (i, h) in hints.iter_mut().enumerate() {
        let mut label = vec![b'a'; width];
        let mut value = i;
        for byte in label.iter_mut().rev() {
            *byte += (value % 26) as u8;
            value /= 26;
        }
        h.label = String::from_utf8(label).unwrap();
    }
    hints
}

#[cfg(test)]
mod tests {
    use super::*;
    use wezterm_term::TerminalSize;
    fn screen(text: &str, cols: usize, rows: usize) -> Snapshot {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let mut t = crate::terminal::create(
            TerminalSize {
                cols,
                rows,
                ..Default::default()
            },
            tx,
        );
        t.advance_bytes(text.as_bytes());
        Snapshot::capture(&t, None, 1, 0, "test".into(), 0)
    }
    #[test]
    fn paths_links_and_labels_follow_visible_logical_lines_and_cells() {
        let f = screen(
            "界 https://example.org/a(b).\r\nsrc/foo.rs:12:8: \"build/my file.rs\"\r\nC:\\work\\code.go:7 a.out",
            12,
            10,
        );
        let hints = viewport(&f, true);
        assert_eq!(
            hints.iter().map(|h| h.target.text()).collect::<Vec<_>>(),
            [
                "https://example.org/a(b)",
                "src/foo.rs",
                "build/my file.rs",
                r"C:\work\code.go",
                "a.out"
            ]
        );
        assert_eq!(
            (hints[0].row, hints[0].col, hints[0].label.as_str()),
            (0, 3, "a")
        );
        assert!(
            viewport(&f, false)
                .iter()
                .all(|h| matches!(h.target, Target::Path(_)))
        );
        let f = screen("old/file.rs\r\n\r\n\r\nnext.rs", 16, 3);
        assert_eq!(viewport(&f, true).len(), 1);
    }
    #[test]
    fn osc8_owns_its_label_and_unsafe_schemes_never_become_paths_or_actions() {
        let f = screen(
            "\x1b]8;;https://example.org\x1b\\src/link.rs\x1b]8;;\x1b\\ \x1b]8;;file:///tools\x1b\\skip/me.rs\x1b]8;;\x1b\\ file:///x.txt javascript:evil",
            80,
            2,
        );
        let h = viewport(&f, false);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].target, Target::Url("https://example.org".into()));
    }
    #[test]
    fn all_hint_labels_have_equal_length_when_one_letter_is_insufficient() {
        let line = (0..27).map(|i| format!("log{i}.txt ")).collect::<String>();
        let h = viewport(&screen(&line, 80, 6), true);
        assert_eq!(h.len(), 27);
        assert_eq!(h[0].label, "aa");
        assert_eq!(h[26].label, "ba");
    }

    #[test]
    fn named_patterns_match_whole_logical_lines_and_reserve_only_their_cells() {
        let mut rules = Patterns::default();
        rules.save(None, "Ticket", r"OPS-[0-9]+").unwrap();
        rules.save(None, "Number", r"[0-9]+").unwrap();
        let frame = screen(
            "界 OPS-\x1b[32m104\x1b[m 7\r\n\x1b]8;;https://example.org\x07OPS-16\x1b]8;;\x07\r\n\x1b]8;;file:///x\x07OPS-17\x1b]8;;\x07",
            7,
            8,
        );
        let hints = viewport_with_patterns(&frame, true, &rules);
        assert_eq!(
            hints.iter().map(|h| h.target.text()).collect::<Vec<_>>(),
            ["OPS-104", "7", "https://example.org"]
        );
        assert_eq!((hints[0].row, hints[0].col), (0, 3));
        assert!(matches!(&hints[0].target, Target::Pattern { name, .. } if name == "Ticket"));
        assert!(matches!(&hints[1].target, Target::Pattern { name, .. } if name == "Number"));
        assert!(
            viewport_with_patterns(&screen("OPS-\r\n104", 10, 2), false, &rules)
                .iter()
                .all(|h| h.target.text() != "OPS-104")
        );
    }

    #[test]
    fn pattern_actions_require_a_whole_recognized_url_or_path_and_preserve_copy_text() {
        let mut rules = Patterns::default();
        rules.save(None, "Item", r"\S+").unwrap();
        let hints = viewport_with_patterns(
            &screen("www.example.org src/a.rs file:///x", 60, 2),
            true,
            &rules,
        );
        assert_eq!(hints[0].target.text(), "www.example.org");
        assert_eq!(
            hints[0].target.pattern_url().as_deref(),
            Some("https://www.example.org")
        );
        assert_eq!(hints[1].target.file_path(), Some("src/a.rs"));
        assert_eq!(hints[2].target.text(), "file:///x");
        assert_eq!(hints[2].target.pattern_url(), None);
        assert_eq!(hints[2].target.file_path(), None);
    }

    #[test]
    fn dense_patterns_and_oversized_logical_lines_respect_the_scan_budget() {
        let mut rules = Patterns::default();
        rules.save(None, "Letter", "x").unwrap();
        let frame = screen(&"x".repeat(MAX_PATTERN_MATCHES + 64), 128, 6);
        assert_eq!(
            viewport_with_patterns(&frame, false, &rules).len(),
            MAX_PATTERN_MATCHES
        );
        let oversized = format!("{}\r\nx", "x".repeat(MAX_PATTERN_LINE + 1));
        let frame = screen(&oversized, 256, 68);
        let hints = viewport_with_patterns(&frame, false, &rules);
        assert_eq!(hints.len(), 1);
        assert!(hints[0].row > 0);
    }
}
