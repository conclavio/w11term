//! Keyboard targets in a single committed viewport, in terminal reading order.
use crate::{
    interaction::{safe_url, text_urls},
    terminal::Snapshot,
};
use std::{ops::Range, sync::LazyLock};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Url(String),
    Path(String),
}

impl Target {
    pub fn text(&self) -> &str {
        match self {
            Self::Url(s) | Self::Path(s) => s,
        }
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
    linked: bool,
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

fn line_targets(text: &str, cells: &[Cell], detect_urls: bool, hints: &mut Vec<Hint>) {
    static TOKENS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#""([^"]+)"|'([^']+)'|`([^`]+)`|([^\s"'`<>]+)"#).unwrap()
    });
    let mut urls: Vec<_> = text_urls(text).collect();
    let mut add = |range: Range<usize>, target| {
        let cells = &cells[cells.partition_point(|c| c.start < range.start)
            ..cells.partition_point(|c| c.start < range.end)];
        if let Some(first) = cells.first()
            && !cells.iter().any(|c| c.linked)
        {
            hints.push(Hint {
                target,
                row: first.row,
                col: first.col,
                label: String::new(),
            });
        }
    };
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
    let mut hints = Vec::new();
    let mut text = String::new();
    let mut cells = Vec::new();
    let mut previous_link = None;
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
                linked: link.is_some(),
            });
            text.push_str(cell.str());
        }
        if !line.last_cell_was_wrapped() || row + 1 == frame.rows.len() {
            line_targets(&text, &cells, detect_urls, &mut hints);
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
}
