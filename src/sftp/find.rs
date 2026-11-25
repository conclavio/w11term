use super::*;
use std::{
    cmp::Ordering,
    collections::{HashSet, VecDeque},
    ops::RangeInclusive,
};

const MAX_DIRECTORIES: usize = 128;
const MAX_ENTRIES: usize = 16_384;
const MAX_RESULTS: usize = 200;
const MAX_DEPTH: usize = 24;
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
                    || !self.modified_dates.is_unrestricted()))
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
    pub truncated: bool,
    pub cancelled: bool,
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
    cancel: Option<&'a TransferCancel>,
    progress: Option<&'a watch::Sender<SearchProgress>>,
    next_progress: Instant,
    queued: VecDeque<(String, usize)>,
    directories: HashSet<String>,
    matched: HashSet<String>,
    result: FilenameSearch,
}

fn unavailable(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<Error>(),
        Some(Error::Status(status))
            if matches!(status.status_code, StatusCode::NoSuchFile | StatusCode::PermissionDenied)
    )
}

impl Search<'_> {
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
        let reached =
            self.result.scanned_entries == MAX_ENTRIES || self.result.matches.len() == MAX_RESULTS;
        self.result.truncated |= reached;
        reached
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
                    if matches!(name.as_str(), "" | "." | "..")
                        || name.contains(['/', '\\'])
                        || name.chars().any(char::is_control)
                    {
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
                    if self.kind.is_none_or(|want| Some(want) == kind)
                        && self.pattern.matches(candidate)
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
                            && self.matched.insert(child.clone())
                        {
                            self.result.matches.push(FilenameMatch {
                                parent: path.to_owned(),
                                name,
                                kind,
                                mtime: attrs.mtime,
                                size,
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
        let root = self.canonical_remote_path(path).await?;
        ensure!(
            root.len() <= MAX_PATH_BYTES,
            "The search folder path is too long"
        );
        let mut search = Search {
            engine: self,
            pattern,
            match_path: filter.match_path,
            kind: filter.kind,
            exclude_dirs,
            file_size: filter.file_size,
            modified_days,
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
                truncated: false,
                cancelled: false,
            },
        };
        while let Some((path, depth)) = search.queued.pop_front() {
            if search.cancelled() || search.limit_reached() {
                break;
            }
            search.report_progress(&path, true);
            match search.scan(&path, depth).await {
                Ok(true) => search.result.scanned_directories += 1,
                Ok(false) if depth == 0 && !search.cancelled() => {
                    bail!("The search root changed or is not a directory")
                }
                Ok(false) => search.result.skipped_directories += 1,
                Err(error) if depth > 0 && unavailable(&error) => {
                    search.result.skipped_directories += 1
                }
                Err(error) => return Err(error),
            }
        }
        search.result.cancelled = search.cancelled();
        FilenameSort::Folder.apply(&mut search.result.matches);
        Ok(search.result)
    }
}
