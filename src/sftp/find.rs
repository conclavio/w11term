use super::*;
use std::collections::{HashSet, VecDeque};

const MAX_DIRECTORIES: usize = 128;
const MAX_ENTRIES: usize = 16_384;
const MAX_RESULTS: usize = 200;
const MAX_DEPTH: usize = 24;
const MAX_PATH_BYTES: usize = 4096;

pub fn valid_filename_query(query: &str) -> bool {
    query.len() <= 256 && !query.trim().is_empty() && !query.chars().any(char::is_control)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FilenameMatch {
    pub parent: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<EntryKind>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FilenameSearch {
    pub root: String,
    pub matches: Vec<FilenameMatch>,
    pub scanned_directories: usize,
    pub scanned_entries: usize,
    pub skipped_directories: usize,
    pub truncated: bool,
    pub cancelled: bool,
}

struct Search<'a> {
    engine: &'a Engine,
    query: String,
    cancel: Option<&'a TransferCancel>,
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
                    let mode = match entry.attrs.permissions {
                        Some(mode) if mode & 0o170000 != 0 => Some(mode),
                        _ => match self.engine.raw.lstat(&child).await {
                            Ok(reply) => reply.attrs.permissions,
                            Err(error) if transport_error(&error) => return Err(error.into()),
                            Err(_) => None,
                        },
                    };
                    let kind = EntryKind::from_permissions(mode);
                    if name.to_lowercase().contains(&self.query)
                        && self.matched.insert(child.clone())
                    {
                        self.result.matches.push(FilenameMatch {
                            parent: path.to_owned(),
                            name,
                            kind,
                        });
                    }
                    if kind == Some(EntryKind::Directory) && !self.directories.contains(&child) {
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
        cancel: Option<&TransferCancel>,
    ) -> Result<FilenameSearch> {
        ensure!(
            valid_filename_query(query),
            "Enter a filename fragment up to 256 bytes"
        );
        let root = self.canonical_remote_path(path).await?;
        ensure!(
            root.len() <= MAX_PATH_BYTES,
            "The search folder path is too long"
        );
        let mut search = Search {
            engine: self,
            query: query.to_lowercase(),
            cancel,
            queued: VecDeque::from([(root.clone(), 0)]),
            directories: HashSet::from([root.clone()]),
            matched: HashSet::new(),
            result: FilenameSearch {
                root,
                matches: Vec::new(),
                scanned_directories: 0,
                scanned_entries: 0,
                skipped_directories: 0,
                truncated: false,
                cancelled: false,
            },
        };
        while let Some((path, depth)) = search.queued.pop_front() {
            if search.cancelled() || search.limit_reached() {
                break;
            }
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
        search
            .result
            .matches
            .sort_by(|a, b| (&a.parent, &a.name).cmp(&(&b.parent, &b.name)));
        Ok(search.result)
    }
}
