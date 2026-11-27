use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::Read,
    path::Path,
};

const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DEPTH: usize = 513;
const MAX_FRAMES: usize = 250_000;

#[derive(Default)]
struct Node<'a> {
    name: &'a str,
    samples: u64,
    own_samples: u64,
    children: BTreeMap<&'a str, usize>,
}

pub(super) struct Frame {
    pub name: String,
    pub samples: u64,
    pub own_samples: u64,
    pub parent: usize,
    pub depth: usize,
    pub start: u64,
    pub last_depth: usize,
}

impl Frame {
    pub fn end(&self) -> u64 {
        self.start + self.samples
    }
}

pub struct Profile {
    frames: Vec<Frame>,
    rows: Vec<Vec<usize>>,
    raw_symbols: HashMap<usize, String>,
}

impl Profile {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).context("Opening the folded stacks file")?;
        let metadata = file.metadata()?;
        ensure!(metadata.is_file(), "Choose a regular folded stacks file");
        ensure!(
            metadata.len() <= MAX_FILE_BYTES,
            "The folded stacks file exceeds 16 MiB"
        );
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_FILE_BYTES,
            "The folded stacks file exceeds 16 MiB"
        );
        let input = std::str::from_utf8(&bytes).context("Folded stacks must be UTF-8 text")?;
        Self::parse(input.trim_start_matches('\u{feff}'))
    }

    pub(super) fn parse(input: &str) -> Result<Self> {
        let mut nodes = vec![Node {
            name: "All samples",
            ..Node::default()
        }];
        for (index, line) in input.lines().enumerate() {
            let line = line.trim();
            if !line.is_empty() {
                Self::insert(&mut nodes, line)
                    .with_context(|| format!("Invalid folded stack on line {}", index + 1))?;
            }
        }
        ensure!(
            nodes[0].samples != 0,
            "The folded stacks file has no samples"
        );

        let mut frames: Vec<Frame> = Vec::with_capacity(nodes.len());
        let mut rows = Vec::<Vec<usize>>::new();
        let mut raw_symbols = HashMap::new();
        let mut stack = vec![(0, 0, 0, 0)];
        while let Some((node_id, parent, depth, start)) = stack.pop() {
            let node = &nodes[node_id];
            let id = frames.len();
            let name = display_symbol(node.name);
            if name != node.name {
                raw_symbols.insert(id, node.name.to_owned());
            }
            frames.push(Frame {
                name,
                samples: node.samples,
                own_samples: node.own_samples,
                parent,
                depth,
                start,
                last_depth: depth,
            });
            if rows.len() == depth {
                rows.push(Vec::new());
            }
            rows[depth].push(id);

            // Place children left to right by symbol, leaving self samples above the
            // right edge of their parent. Push them in reverse for depth-first order.
            let mut next_start = start + node.samples - node.own_samples;
            for &child in node.children.values().rev() {
                next_start -= nodes[child].samples;
                stack.push((child, id, depth + 1, next_start));
            }
        }
        for id in (1..frames.len()).rev() {
            let frame = &frames[id];
            let (parent, depth) = (frame.parent, frame.last_depth);
            frames[parent].last_depth = frames[parent].last_depth.max(depth);
        }
        Ok(Self {
            frames,
            rows,
            raw_symbols,
        })
    }

    fn insert<'a>(nodes: &mut Vec<Node<'a>>, line: &'a str) -> Result<()> {
        let separator = line
            .rfind(char::is_whitespace)
            .context("Expected a stack followed by its sample count")?;
        let count: u64 = line[separator..]
            .trim()
            .parse()
            .context("Expected a positive integer sample count")?;
        ensure!(count > 0, "Sample counts must be positive");
        nodes[0].samples = nodes[0]
            .samples
            .checked_add(count)
            .context("Total sample count is too large")?;
        let mut parent = 0;
        for (depth, name) in line[..separator].trim_end().split(';').enumerate() {
            ensure!(depth < MAX_DEPTH, "Stack exceeds {MAX_DEPTH} frames");
            let name = name.trim();
            ensure!(!name.is_empty(), "Stack contains an empty frame");
            let child = match nodes[parent].children.get(name) {
                Some(&child) => child,
                None => {
                    let child = nodes.len();
                    ensure!(child <= MAX_FRAMES, "Profile exceeds {MAX_FRAMES} frames");
                    nodes[parent].children.insert(name, child);
                    nodes.push(Node {
                        name,
                        ..Node::default()
                    });
                    child
                }
            };
            // Every count is part of the already checked profile total.
            nodes[child].samples += count;
            parent = child;
        }
        nodes[parent].own_samples += count;
        Ok(())
    }

    pub(super) fn frame(&self, id: usize) -> &Frame {
        &self.frames[id]
    }

    pub(super) fn symbol(&self, id: usize) -> &str {
        self.raw_symbols
            .get(&id)
            .map_or(&self.frame(id).name, String::as_str)
    }

    pub fn has_frame(&self, id: usize) -> bool {
        id < self.frames.len()
    }

    pub fn row(&self, zoom: usize, depth: usize) -> &[usize] {
        let root = self.frame(zoom);
        let Some(frames) = self.rows.get(root.depth + depth) else {
            return &[];
        };
        let begin = frames.partition_point(|&id| self.frame(id).start < root.start);
        let end = frames.partition_point(|&id| self.frame(id).start < root.end());
        &frames[begin..end]
    }

    pub(super) fn matching_paths(&self, needle: &regex::Regex) -> Vec<bool> {
        let mut matched = vec![false; self.frames.len()];
        for (id, frame) in self.frames.iter().enumerate().skip(1) {
            matched[id] = matched[frame.parent] || needle.is_match(&frame.name);
        }
        matched
    }

    pub(super) fn search(&self, query: &str) -> Search {
        let Some(needle) = symbol_pattern(query) else {
            return Search::default();
        };
        let mut result = Search {
            highlighted: vec![false; self.frames.len()],
            ..Search::default()
        };
        let mut covered_end = 0;
        for (id, frame) in self.frames.iter().enumerate().skip(1) {
            if needle.is_match(&frame.name) {
                result.matches.push(id);
                result.highlighted[id] = true;
                result.samples += frame.end().saturating_sub(covered_end.max(frame.start));
                covered_end = covered_end.max(frame.end());
            }
        }
        result
            .matches
            .sort_unstable_by_key(|&id| (std::cmp::Reverse(self.frame(id).samples), id));
        result
    }
}

pub(super) fn symbol_pattern(query: &str) -> Option<regex::Regex> {
    let query = query.trim();
    let end = query
        .char_indices()
        .nth(256)
        .map_or(query.len(), |(index, _)| index);
    (end != 0).then(|| {
        regex::RegexBuilder::new(&regex::escape(&query[..end]))
            .case_insensitive(true)
            .build()
            .expect("short literal symbol search")
    })
}

#[derive(Default)]
pub(super) struct Search {
    pub matches: Vec<usize>,
    pub samples: u64,
    highlighted: Vec<bool>,
}

impl Search {
    pub fn contains(&self, id: usize) -> bool {
        self.highlighted.get(id).copied().unwrap_or(false)
    }
}

fn display_symbol(name: &str) -> String {
    let mut display = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_control()
            || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{fffe}' | '\u{ffff}')
        {
            display.extend(c.escape_default());
        } else {
            display.push(c);
        }
    }
    display
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_duplicate_stacks_and_preserves_self_samples_and_ancestry() {
        let profile = Profile::parse(concat!(
            "worker;main;hot loop 60\r\n",
            "worker;main;hot loop 10\r\n",
            "worker;main 20\r\n",
            "worker;other;hot loop 5\r\n",
            "other;main 5\r\n",
        ))
        .unwrap();
        assert_eq!(profile.frame(0).samples, 100);
        let worker = profile.row(0, 1)[1];
        let frames = profile.row(worker, 2);
        assert_eq!(frames.len(), 2);
        let hot = profile.frame(frames[0]);
        assert_eq!(
            (hot.name.as_str(), hot.start, hot.samples),
            ("hot loop", 5, 70)
        );
        assert_eq!(profile.frame(hot.parent).samples, 90);
        assert_eq!(profile.frame(frames[1]).start, 95);
        assert_eq!(profile.frame(worker).last_depth, 3);
        let search = profile.search("HOT LOOP");
        assert_eq!((search.matches.len(), search.samples), (2, 75));
        assert_eq!(search.matches[0], frames[0]);
    }

    #[test]
    fn recursive_matches_count_each_sample_only_once() {
        let profile = Profile::parse("app;work;work 35\napp;work;wait 5\napp;idle 60\n").unwrap();
        let search = profile.search("Work");
        assert_eq!((search.matches.len(), search.samples), (2, 40));
    }

    #[test]
    fn rejects_invalid_or_overflowing_folded_stacks() {
        for input in [
            "",
            "app;;main 5",
            "app;main 0",
            "app;main -1",
            "app;main NaN",
        ] {
            assert!(Profile::parse(input).is_err(), "{input}");
        }
        assert!(Profile::parse(&format!("app {}\napp 1", u64::MAX)).is_err());
        assert!(Profile::parse(&format!("{}f 1", "f;".repeat(MAX_DEPTH))).is_err());
    }
}
