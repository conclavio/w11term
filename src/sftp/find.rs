use super::folder_comparison::InventoryBuilder;
use super::*;
use std::{
    cmp::Ordering,
    collections::{HashSet, VecDeque},
    ops::RangeInclusive,
};

pub(super) const MAX_DIRECTORIES: usize = 128;
pub(super) const MAX_ENTRIES: usize = 16_384;
const MAX_RESULTS: usize = 200;
const MAX_CONTENT_READS: usize = 256;
pub(super) const MAX_DEPTH: usize = 24;
pub(crate) const MAX_PATH_BYTES: usize = 4096;

pub fn valid_filename_query(query: &str) -> bool {
    query.len() <= 256 && !query.trim().is_empty() && !query.chars().any(char::is_control)
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct FilenameFilter {
    pub regex: bool,
    pub match_case: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub match_path: bool,
    pub kind: Option<EntryKind>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub exclude_dir_regex: String,
    #[serde(skip_serializing_if = "FileSizeRange::is_unrestricted")]
    pub file_size: FileSizeRange,
    #[serde(skip_serializing_if = "ModifiedDateRange::is_unrestricted")]
    pub modified_dates: ModifiedDateRange,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub content_query: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModifiedDateRange {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub from: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub through: String,
}

impl ModifiedDateRange {
    pub fn is_unrestricted(&self) -> bool {
        self.from.trim().is_empty() && self.through.trim().is_empty()
    }

    pub fn validate(&self) -> Result<()> {
        self.compile().map(|_| ())
    }

    fn compile(&self) -> Result<Option<RangeInclusive<i64>>> {
        if self.is_unrestricted() {
            return Ok(None);
        }
        let from = parse_utc_day(&self.from)
            .context("Modified from")?
            .unwrap_or(i64::MIN);
        let through = parse_utc_day(&self.through)
            .context("Modified through")?
            .unwrap_or(i64::MAX);
        ensure!(
            from <= through,
            "Modified from must be on or before Modified through"
        );
        Ok(Some(from..=through))
    }
}

fn parse_utc_day(text: &str) -> Result<Option<i64>> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    ensure!(
        text.len() == 10
            && text.bytes().enumerate().all(|(i, byte)| {
                if matches!(i, 4 | 7) {
                    byte == b'-'
                } else {
                    byte.is_ascii_digit()
                }
            }),
        "Use a UTC date in YYYY-MM-DD format"
    );
    let date = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .context("Use a valid UTC calendar date")?;
    Ok(Some(
        date.signed_duration_since(chrono::DateTime::UNIX_EPOCH.date_naive())
            .num_days(),
    ))
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileSizeRange {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum: Option<u64>,
}

impl FileSizeRange {
    pub fn parse(minimum: &str, maximum: &str) -> Result<Self> {
        Ok(Self {
            minimum: parse_size_bound(minimum).context("Minimum file size")?,
            maximum: parse_size_bound(maximum).context("Maximum file size")?,
        })
    }

    pub fn is_unrestricted(&self) -> bool {
        self.minimum.is_none() && self.maximum.is_none()
    }

    pub fn validate(self, kind: Option<EntryKind>) -> Result<()> {
        ensure!(
            !matches!((self.minimum, self.maximum), (Some(min), Some(max)) if min > max),
            "Minimum file size must be at most the maximum"
        );
        ensure!(
            self.is_unrestricted() || matches!(kind, None | Some(EntryKind::File)),
            "File size limits require Files or All types"
        );
        Ok(())
    }

    fn matches(self, size: Option<u64>) -> bool {
        self.is_unrestricted()
            || size.is_some_and(|size| {
                self.minimum.is_none_or(|min| size >= min)
                    && self.maximum.is_none_or(|max| size <= max)
            })
    }
}

fn parse_size_bound(text: &str) -> Result<Option<u64>> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let boundary = text.bytes().take_while(u8::is_ascii_digit).count();
    let (number, suffix) = text.split_at(boundary);
    let bytes: u64 = number
        .parse()
        .context("Expected a whole, nonnegative file size")?;
    let scale = match suffix.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kib" => 1 << 10,
        "mib" => 1 << 20,
        "gib" => 1 << 30,
        "tib" => 1 << 40,
        "pib" => 1 << 50,
        "eib" => 1 << 60,
        _ => bail!("Use bytes or a binary unit such as KiB, MiB, or GiB"),
    };
    bytes
        .checked_mul(scale)
        .map(Some)
        .context("File size is too large")
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FilenameSort {
    #[default]
    Folder,
    Name,
    Newest,
    Largest,
}

impl FilenameSort {
    pub fn apply(self, matches: &mut [FilenameMatch]) {
        matches.sort_unstable_by(|a, b| {
            match self {
                Self::Folder => Ordering::Equal,
                Self::Name => a.name.cmp(&b.name),
                Self::Newest => b.mtime.cmp(&a.mtime),
                Self::Largest => b.size.cmp(&a.size),
            }
            .then_with(|| (&a.parent, &a.name).cmp(&(&b.parent, &b.name)))
        });
    }
}

impl FilenameFilter {
    pub fn valid_query(&self, query: &str) -> bool {
        valid_filename_query(query)
            || (query.is_empty()
                && (self.kind.is_some()
                    || !self.exclude_dir_regex.is_empty()
                    || !self.file_size.is_unrestricted()
                    || !self.modified_dates.is_unrestricted()
                    || !self.content_query.is_empty()))
    }

    /// Find each matching line in freshly read text using the recursive scan's rules.
    pub fn match_file_text_lines(&self, text: &str) -> Result<Vec<FileTextMatch>> {
        let Some(pattern) = self.compile_content_pattern()? else {
            return Ok(Vec::new());
        };
        Ok(matching_file_lines(text, &pattern)
            .map(|(location, _)| location)
            .collect())
    }

    pub(crate) fn compile_content_pattern(&self) -> Result<Option<regex::Regex>> {
        if self.content_query.is_empty() {
            return Ok(None);
        }
        ensure!(
            valid_filename_query(&self.content_query),
            "Enter a file text query up to 256 bytes without control characters"
        );
        ensure!(
            matches!(self.kind, None | Some(EntryKind::File)),
            "File text search requires Files or All types"
        );
        let pattern = if self.regex {
            std::borrow::Cow::Borrowed(self.content_query.as_str())
        } else {
            std::borrow::Cow::Owned(regex::escape(&self.content_query))
        };
        crate::search::compile_regex(&pattern, self.match_case)
            .map(Some)
            .map_err(|error| anyhow::anyhow!("Invalid file text regex. {error}"))
    }

    pub(crate) fn compile_excluded_dirs(&self) -> Result<Option<regex::Regex>> {
        if self.exclude_dir_regex.is_empty() {
            return Ok(None);
        }
        ensure!(
            valid_filename_query(&self.exclude_dir_regex),
            "Enter a folder exclusion regex up to 256 bytes without control characters"
        );
        crate::search::compile_regex(&self.exclude_dir_regex, self.match_case)
            .map(Some)
            .map_err(|error| anyhow::anyhow!("Invalid folder exclusion regex. {error}"))
    }
}

enum FilenamePattern {
    All,
    Literal(String),
    Folded(String),
    Regex(regex::Regex),
}

impl FilenamePattern {
    fn new(query: &str, filter: &FilenameFilter) -> Result<Self> {
        ensure!(
            filter.valid_query(query),
            "Enter a search pattern up to 256 bytes or add a search filter"
        );
        if query.is_empty() {
            return Ok(Self::All);
        }
        if filter.regex {
            return crate::search::compile_regex(query, filter.match_case)
                .map(Self::Regex)
                .map_err(|error| anyhow::anyhow!("Invalid filename regex. {error}"));
        }
        Ok(if filter.match_case {
            Self::Literal(query.to_owned())
        } else {
            Self::Folded(query.to_lowercase())
        })
    }

    fn matches(&self, candidate: &str) -> bool {
        match self {
            Self::All => true,
            Self::Literal(query) => candidate.contains(query),
            Self::Folded(query) => candidate.to_lowercase().contains(query),
            Self::Regex(regex) => regex.is_match(candidate),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FilenameMatch {
    pub parent: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<EntryKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_match: Option<ContentMatch>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContentMatch {
    pub line: usize,
    pub excerpt: String,
}

/// A matching line in a previously read file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileTextMatch {
    pub line: usize,
    /// UTF-8 byte offset of this line in the original string, excluding any leading BOM.
    pub line_start: usize,
    match_start: usize,
}

impl FileTextMatch {
    pub fn excerpt(&self, original: &str) -> Option<String> {
        let line = original.get(self.line_start..)?.lines().next()?;
        line.get(..self.match_start)?;
        Some(content_excerpt(line, self.match_start))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ContentScan {
    pub inspected: usize,
    pub skipped: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FilenameSearch {
    pub root: String,
    pub matches: Vec<FilenameMatch>,
    pub scanned_directories: usize,
    pub scanned_entries: usize,
    pub skipped_directories: usize,
    #[serde(default)]
    pub excluded_directories: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_scan: Option<ContentScan>,
    pub truncated: bool,
    pub cancelled: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FolderContents {
    pub logical_bytes: String,
    pub files: usize,
    pub files_without_size: usize,
    pub links: usize,
    pub unclassified_entries: usize,
    pub scanned_directories: usize,
    pub skipped_directories: usize,
    pub truncated: bool,
    pub cancelled: bool,
}

#[derive(Default)]
struct Measurement {
    logical_bytes: u128,
    files: usize,
    files_without_size: usize,
    links: usize,
    unclassified_entries: usize,
}

/// A snapshot of a running scan. Totals are unknown until the search ends.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchProgress {
    pub folder: String,
    pub scanned_directories: usize,
    pub scanned_entries: usize,
    pub matches: usize,
}

struct Search<'a> {
    engine: &'a Engine,
    pattern: FilenamePattern,
    match_path: bool,
    kind: Option<EntryKind>,
    exclude_dirs: Option<regex::Regex>,
    file_size: FileSizeRange,
    modified_days: Option<RangeInclusive<i64>>,
    content_pattern: Option<regex::Regex>,
    content_reads: usize,
    content_limit_hit: bool,
    cancel: Option<&'a TransferCancel>,
    progress: Option<&'a watch::Sender<SearchProgress>>,
    next_progress: Instant,
    queued: VecDeque<(String, usize)>,
    directories: HashSet<String>,
    matched: HashSet<String>,
    result: FilenameSearch,
    measurement: Option<Measurement>,
    inventory: Option<InventoryBuilder>,
}

fn unavailable(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<Error>(),
        Some(Error::Status(status))
            if matches!(status.status_code, StatusCode::NoSuchFile | StatusCode::PermissionDenied)
    )
}

fn matching_file_lines<'a>(
    original: &'a str,
    pattern: &'a regex::Regex,
) -> impl Iterator<Item = (FileTextMatch, &'a str)> + 'a {
    let text = original.strip_prefix('\u{feff}').unwrap_or(original);
    let mut offset = original.len() - text.len();
    text.split_inclusive('\n')
        .enumerate()
        .filter_map(move |(index, raw)| {
            let line_start = offset;
            offset += raw.len();
            let line = raw.lines().next()?;
            let matched = pattern.find(line)?;
            Some((
                FileTextMatch {
                    line: index + 1,
                    line_start,
                    match_start: matched.start(),
                },
                line,
            ))
        })
}

fn first_content_match(text: &str, pattern: &regex::Regex) -> Option<ContentMatch> {
    let (location, line) = matching_file_lines(text, pattern).next()?;
    Some(ContentMatch {
        line: location.line,
        excerpt: content_excerpt(line, location.match_start),
    })
}

fn content_excerpt(line: &str, match_start: usize) -> String {
    let start = line[..match_start]
        .char_indices()
        .rev()
        .nth(47)
        .map_or(0, |(index, _)| index);
    let mut excerpt = String::new();
    if start > 0 {
        excerpt.push('…');
    }
    let mut chars = line[start..].chars();
    excerpt.extend(
        chars
            .by_ref()
            .take(160)
            .map(|ch| if ch.is_control() { ' ' } else { ch }),
    );
    if chars.next().is_some() {
        excerpt.push('…');
    }
    excerpt
}

impl<'a> Search<'a> {
    fn new(
        engine: &'a Engine,
        root: String,
        cancel: Option<&'a TransferCancel>,
        progress: Option<&'a watch::Sender<SearchProgress>>,
    ) -> Self {
        Self {
            engine,
            pattern: FilenamePattern::All,
            match_path: false,
            kind: None,
            exclude_dirs: None,
            file_size: FileSizeRange::default(),
            modified_days: None,
            content_pattern: None,
            content_reads: 0,
            content_limit_hit: false,
            cancel,
            progress,
            next_progress: Instant::now(),
            queued: VecDeque::from([(root.clone(), 0)]),
            directories: HashSet::from([root.clone()]),
            matched: HashSet::new(),
            result: FilenameSearch {
                root,
                matches: Vec::new(),
                scanned_directories: 0,
                scanned_entries: 0,
                skipped_directories: 0,
                excluded_directories: 0,
                content_scan: None,
                truncated: false,
                cancelled: false,
            },
            measurement: None,
            inventory: None,
        }
    }

    async fn run(&mut self) -> Result<()> {
        while let Some((path, depth)) = self.queued.pop_front() {
            if self.cancelled() || self.limit_reached() {
                break;
            }
            self.report_progress(&path, true);
            match self.scan(&path, depth).await {
                Ok(true) => self.result.scanned_directories += 1,
                Ok(false) if depth == 0 && !self.cancelled() => {
                    bail!("The search root changed or is not a directory")
                }
                Ok(false) => self.result.skipped_directories += 1,
                Err(error) if depth > 0 && unavailable(&error) => {
                    self.result.skipped_directories += 1
                }
                Err(error) => return Err(error),
            }
        }
        self.result.cancelled = self.cancelled();
        Ok(())
    }

    async fn measure_entry(
        &mut self,
        path: &str,
        attrs: &FileAttributes,
        missing_type: bool,
    ) -> Result<()> {
        let measurement = self.measurement.as_mut().unwrap();
        match EntryKind::from_permissions(attrs.permissions) {
            Some(EntryKind::File) => {
                measurement.files += 1;
                let mut size = attrs.size;
                if size.is_none() && !missing_type {
                    match self.engine.raw.lstat(path).await {
                        Ok(reply)
                            if EntryKind::from_permissions(reply.attrs.permissions)
                                == Some(EntryKind::File) =>
                        {
                            size = reply.attrs.size;
                        }
                        Err(error) if transport_error(&error) => return Err(error.into()),
                        _ => {}
                    }
                }
                if let Some(size) = size {
                    measurement.logical_bytes += u128::from(size);
                } else {
                    measurement.files_without_size += 1;
                }
            }
            Some(EntryKind::Symlink) => measurement.links += 1,
            None if attrs.permissions.is_none_or(|mode| mode & 0o170000 == 0) => {
                measurement.unclassified_entries += 1;
            }
            _ => {}
        }
        Ok(())
    }

    fn report_progress(&mut self, folder: &str, new_folder: bool) {
        let Some(progress) = self.progress else {
            return;
        };
        let now = Instant::now();
        if !new_folder && now < self.next_progress {
            return;
        }
        self.next_progress = now + Duration::from_millis(100);
        progress.send_if_modified(|current| {
            let matches = self.result.matches.len();
            let directories = self.result.scanned_directories;
            let entries = self.result.scanned_entries;
            if current.folder == folder
                && current.matches == matches
                && current.scanned_directories == directories
                && current.scanned_entries == entries
            {
                return false;
            }
            folder.clone_into(&mut current.folder);
            current.matches = matches;
            current.scanned_directories = directories;
            current.scanned_entries = entries;
            true
        });
    }

    fn cancelled(&self) -> bool {
        self.cancel.is_some_and(TransferCancel::is_cancelled)
    }

    fn limit_reached(&mut self) -> bool {
        let reached = self.content_limit_hit
            || self.result.scanned_entries == MAX_ENTRIES
            || self.result.matches.len() == MAX_RESULTS;
        self.result.truncated |= reached;
        reached
    }

    async fn search_file_text(
        &mut self,
        folder: &str,
        path: &str,
        size: Option<u64>,
    ) -> Result<Option<ContentMatch>> {
        if size.is_some_and(|size| size > TEXT_PREVIEW_MAX_BYTES as u64) {
            self.result.content_scan.as_mut().unwrap().skipped += 1;
            return Ok(None);
        }
        if self.content_reads == MAX_CONTENT_READS {
            self.content_limit_hit = true;
            self.result.truncated = true;
            return Ok(None);
        }
        self.content_reads += 1;
        self.report_progress(folder, false);
        let read = async {
            ensure!(
                self.engine.canonical_remote_path(folder).await? == folder,
                "The containing search folder changed"
            );
            self.engine.read_preview_text(path, self.cancel).await
        }
        .await;
        match read {
            Ok(text) => {
                self.result.content_scan.as_mut().unwrap().inspected += 1;
                Ok(first_content_match(
                    &text,
                    self.content_pattern.as_ref().unwrap(),
                ))
            }
            Err(error) if error.downcast_ref::<Error>().is_some_and(transport_error) => Err(error),
            Err(_) => {
                if !self.cancelled() {
                    self.result.content_scan.as_mut().unwrap().skipped += 1;
                }
                Ok(None)
            }
        }
    }

    async fn scan(&mut self, path: &str, depth: usize) -> Result<bool> {
        let mode = self.engine.raw.lstat(path).await?.attrs.permissions;
        if EntryKind::from_permissions(mode) != Some(EntryKind::Directory)
            || self.engine.canonical_remote_path(path).await? != path
        {
            return Ok(false);
        }
        if self.cancelled() {
            return Ok(false);
        }
        let handle = self.engine.raw.opendir(path).await?.handle;
        let scanned = async {
            // Recheck the path after OPEN(DIR) before using the handle.
            if self.engine.canonical_remote_path(path).await? != path {
                return Ok(false);
            }
            'pages: while !self.cancelled() {
                self.report_progress(path, false);
                let page = match self.engine.raw.readdir(&handle).await {
                    Ok(reply) => reply.files,
                    Err(error) if eof(&error) => break,
                    Err(error) => return Err(error.into()),
                };
                ensure!(!page.is_empty(), "empty SFTP directory page without EOF");
                for entry in page {
                    if self.cancelled() || self.limit_reached() {
                        break 'pages;
                    }
                    self.result.scanned_entries += 1;
                    let name = entry.filename;
                    if matches!(name.as_str(), "." | "..") {
                        continue;
                    }
                    if name.is_empty()
                        || name.contains(['/', '\\'])
                        || name.chars().any(char::is_control)
                    {
                        ensure!(
                            self.inventory.is_none(),
                            "The server returned an invalid entry name"
                        );
                        if let Some(measurement) = &mut self.measurement {
                            measurement.unclassified_entries += 1;
                        }
                        continue;
                    }
                    let prefix = path.trim_end_matches('/');
                    if prefix.len() + 1 + name.len() > MAX_PATH_BYTES {
                        self.result.truncated = true;
                        continue;
                    }
                    let child = format!("{prefix}/{name}");
                    // READDIR may omit the type. Resolve it without following links.
                    let mut attrs = entry.attrs;
                    let missing_type = !attrs.permissions.is_some_and(|mode| mode & 0o170000 != 0);
                    if missing_type {
                        match self.engine.raw.lstat(&child).await {
                            Ok(reply) => {
                                attrs.permissions = reply.attrs.permissions;
                                attrs.mtime = reply.attrs.mtime.or(attrs.mtime);
                                attrs.size = reply.attrs.size.or(attrs.size);
                            }
                            Err(error) if transport_error(&error) => return Err(error.into()),
                            Err(_) => {}
                        }
                    }
                    let kind = EntryKind::from_permissions(attrs.permissions);
                    let directory = kind == Some(EntryKind::Directory);
                    let relative_path =
                        if self.match_path || (directory && self.exclude_dirs.is_some()) {
                            child
                                .strip_prefix(self.result.root.trim_end_matches('/'))
                                .and_then(|suffix| suffix.strip_prefix('/'))
                                .context("SFTP search entry is outside the requested root")?
                        } else {
                            &name
                        };
                    if directory
                        && self
                            .exclude_dirs
                            .as_ref()
                            .is_some_and(|regex| regex.is_match(relative_path))
                    {
                        self.result.excluded_directories += 1;
                        continue;
                    }
                    let candidate = if self.match_path {
                        relative_path
                    } else {
                        &name
                    };
                    if let Some(inventory) = &mut self.inventory {
                        inventory.insert(relative_path, &attrs)?;
                    } else if self.measurement.is_some() {
                        if !directory && self.matched.insert(child.clone()) {
                            self.measure_entry(&child, &attrs, missing_type).await?;
                        }
                    } else if self.kind.is_none_or(|want| Some(want) == kind)
                        && self.pattern.matches(candidate)
                        && (self.content_pattern.is_none() || kind == Some(EntryKind::File))
                    {
                        let mut size = attrs.size.filter(|_| kind == Some(EntryKind::File));
                        let need_size = !self.file_size.is_unrestricted()
                            && size.is_none()
                            && kind == Some(EntryKind::File);
                        let need_mtime = self.modified_days.is_some() && attrs.mtime.is_none();
                        if !missing_type && (need_size || need_mtime) {
                            match self.engine.raw.lstat(&child).await {
                                Ok(reply)
                                    if EntryKind::from_permissions(reply.attrs.permissions)
                                        == kind =>
                                {
                                    attrs.mtime = reply.attrs.mtime.or(attrs.mtime);
                                    size = reply
                                        .attrs
                                        .size
                                        .filter(|_| kind == Some(EntryKind::File))
                                        .or(size);
                                }
                                Err(error) if transport_error(&error) => return Err(error.into()),
                                _ => {}
                            }
                        }
                        let date_matches = self.modified_days.as_ref().is_none_or(|range| {
                            attrs
                                .mtime
                                .is_some_and(|time| range.contains(&(i64::from(time) / 86_400)))
                        });
                        if date_matches
                            && self.file_size.matches(size)
                            && !self.matched.contains(&child)
                        {
                            let content_match = if self.content_pattern.is_some() {
                                match self.search_file_text(path, &child, size).await? {
                                    Some(content) => Some(content),
                                    None if self.content_limit_hit => break 'pages,
                                    None => continue,
                                }
                            } else {
                                None
                            };
                            self.matched.insert(child.clone());
                            self.result.matches.push(FilenameMatch {
                                parent: path.to_owned(),
                                name,
                                kind,
                                mtime: attrs.mtime,
                                size,
                                content_match,
                            });
                        }
                    }
                    if directory && !self.directories.contains(&child) {
                        if depth == MAX_DEPTH || self.directories.len() == MAX_DIRECTORIES {
                            self.result.truncated = true;
                        } else {
                            self.directories.insert(child.clone());
                            self.queued.push_back((child, depth + 1));
                        }
                    }
                }
            }
            Ok::<_, anyhow::Error>(true)
        }
        .await;
        self.engine.raw.close(handle).await?;
        scanned
    }
}

impl Engine {
    pub(super) async fn find_filenames(
        &self,
        path: &str,
        query: &str,
        filter: FilenameFilter,
        cancel: Option<&TransferCancel>,
        progress: Option<&watch::Sender<SearchProgress>>,
    ) -> Result<FilenameSearch> {
        let pattern = FilenamePattern::new(query, &filter)?;
        filter.file_size.validate(filter.kind)?;
        let modified_days = filter.modified_dates.compile()?;
        let exclude_dirs = filter.compile_excluded_dirs()?;
        let content_pattern = filter.compile_content_pattern()?;
        let content_scan = content_pattern.as_ref().map(|_| ContentScan::default());
        let root = self.canonical_remote_path(path).await?;
        ensure!(
            root.len() <= MAX_PATH_BYTES,
            "The search folder path is too long"
        );
        let mut search = Search {
            pattern,
            match_path: filter.match_path,
            kind: filter.kind,
            exclude_dirs,
            file_size: filter.file_size,
            modified_days,
            content_pattern,
            ..Search::new(self, root, cancel, progress)
        };
        search.result.content_scan = content_scan;
        search.run().await?;
        FilenameSort::Folder.apply(&mut search.result.matches);
        Ok(search.result)
    }

    pub(super) async fn measure_folder(
        &self,
        path: &str,
        cancel: Option<&TransferCancel>,
        progress: Option<&watch::Sender<SearchProgress>>,
    ) -> Result<FolderContents> {
        let root = self.canonical_remote_path(path).await?;
        ensure!(
            root == path,
            "The measured folder changed. Refresh the drawer"
        );
        ensure!(root.len() <= MAX_PATH_BYTES, "The folder path is too long");
        let mut scan = Search {
            measurement: Some(Measurement::default()),
            ..Search::new(self, root, cancel, progress)
        };
        scan.run().await?;
        let measurement = scan.measurement.unwrap();
        Ok(FolderContents {
            logical_bytes: measurement.logical_bytes.to_string(),
            files: measurement.files,
            files_without_size: measurement.files_without_size,
            links: measurement.links,
            unclassified_entries: measurement.unclassified_entries,
            scanned_directories: scan.result.scanned_directories,
            skipped_directories: scan.result.skipped_directories,
            truncated: scan.result.truncated,
            cancelled: scan.result.cancelled,
        })
    }

    pub(super) async fn inventory_folder(
        &self,
        path: &str,
        cancel: Option<&TransferCancel>,
        progress: Option<&watch::Sender<SearchProgress>>,
    ) -> Result<FolderInventory> {
        let root = self.canonical_remote_path(path).await?;
        ensure!(
            root == path,
            "The comparison folder changed. Refresh the drawer"
        );
        ensure!(root.len() <= MAX_PATH_BYTES, "The folder path is too long");
        let mut scan = Search {
            inventory: Some(InventoryBuilder::default()),
            match_path: true,
            ..Search::new(self, root, cancel, progress)
        };
        scan.run().await?;
        ensure!(!scan.result.cancelled, "Folder comparison scan stopped");
        ensure!(
            !scan.result.truncated,
            "The folder exceeded the scan limits"
        );
        ensure!(
            scan.result.skipped_directories == 0,
            "Some folders could not be listed. A complete inventory is required"
        );
        Ok(scan
            .inventory
            .unwrap()
            .finish(scan.result.root, FolderOrigin::Remote))
    }
}
