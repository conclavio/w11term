use super::*;
use std::collections::BTreeMap;

#[derive(Debug)]
pub struct SearchDownload {
    root: String,
    tree: Directory,
}

type Directory = BTreeMap<WindowsName, (String, Item)>;

#[derive(Debug)]
enum Item {
    Partial(Directory),
    Whole,
    File,
}

impl SearchDownload {
    pub fn new<'a>(
        root: &str,
        matches: impl IntoIterator<Item = &'a FilenameMatch>,
    ) -> Result<Self> {
        ensure!(
            root.starts_with('/')
                && !root.contains(['\0', '\\'])
                && (root == "/" || !root[1..].split('/').any(|p| matches!(p, "" | "." | ".."))),
            "The search root is not a canonical remote folder"
        );
        let prefix = format!("{}/", root.trim_end_matches('/'));
        let mut tree = Directory::new();
        for hit in matches {
            let tail = if hit.parent == root {
                ""
            } else {
                hit.parent
                    .strip_prefix(&prefix)
                    .filter(|tail| !tail.is_empty())
                    .context("Search match is outside the search folder")?
            };
            let components: Vec<_> = (!tail.is_empty())
                .then(|| tail.split('/'))
                .into_iter()
                .flatten()
                .chain(std::iter::once(hit.name.as_str()))
                .collect();
            ensure!(components.len() <= MAX_DEPTH, "Selected path is too deep");
            for name in &components {
                validate_download_name(name)?;
            }
            let kind = match hit.kind {
                Some(EntryKind::File) => Item::File,
                Some(EntryKind::Directory) => Item::Whole,
                _ => bail!("Mark a regular file or directory"),
            };
            insert(&mut tree, &components, kind)?;
        }
        ensure!(
            !tree.is_empty(),
            "Mark at least one search result to download"
        );
        Ok(Self {
            root: root.into(),
            tree,
        })
    }

    pub fn root(&self) -> &str {
        &self.root
    }
}

fn insert(tree: &mut Directory, path: &[&str], target: Item) -> Result<()> {
    use std::collections::btree_map::Entry;
    let (name, rest) = path.split_first().unwrap();
    let entry = match tree.entry(WindowsName(name.encode_utf16().collect())) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) if rest.is_empty() => {
            entry.insert(((*name).to_owned(), target));
            return Ok(());
        }
        Entry::Vacant(entry) => entry.insert(((*name).to_owned(), Item::Partial(Directory::new()))),
    };
    ensure!(
        entry.0 == *name,
        "Selected paths conflict on Windows: {:?} and {name:?}",
        entry.0
    );
    if rest.is_empty() {
        match (&entry.1, &target) {
            (Item::Partial(_), Item::Whole) => entry.1 = target,
            (Item::Whole, Item::Whole) | (Item::File, Item::File) => {}
            _ => bail!("Conflicting entry types at {name:?}"),
        }
        return Ok(());
    }
    match &mut entry.1 {
        Item::Partial(children) => insert(children, rest, target),
        Item::Whole => Ok(()), // A marked directory already includes this match.
        Item::File => bail!("Expected a directory at {name:?}"),
    }
}

impl Download<'_> {
    pub(super) async fn selected(
        &mut self,
        remote: &str,
        local: &Path,
        selection: &SearchDownload,
        depth: usize,
    ) -> Result<()> {
        self.selected_directory(remote, local, &selection.tree, depth)
            .await
    }

    async fn selected_directory(
        &mut self,
        remote: &str,
        local: &Path,
        tree: &Directory,
        depth: usize,
    ) -> Result<()> {
        if self.cancelled() {
            return Ok(());
        }
        let pin = pin_directory_for_download(local, FILE_SHARE_READ, self.preserve_mtime)?;
        let attrs = self.engine.raw.lstat(remote).await?.attrs;
        ensure!(
            EntryKind::from_permissions(attrs.permissions) == Some(EntryKind::Directory),
            "expected a real remote directory: {remote}"
        );
        self.check_path(remote).await?;
        for (name, item) in tree.values() {
            if self.cancelled() {
                return Ok(());
            }
            self.count.entries += 1;
            self.count.name_bytes += name.len();
            ensure!(
                self.count.entries <= MAX_ENTRIES && self.count.name_bytes <= MAX_NAME_BYTES,
                "selected paths exceed the folder download limits"
            );
            let child = format!("{}/{name}", remote.trim_end_matches('/'));
            let path = local.join(name);
            match item {
                Item::File => {
                    let attrs = self.engine.raw.lstat(&child).await?.attrs;
                    ensure!(
                        EntryKind::from_permissions(attrs.permissions) == Some(EntryKind::File),
                        "expected a regular remote file: {child}"
                    );
                    self.file(&child, &path, attrs.size, attrs.mtime).await?;
                }
                Item::Whole | Item::Partial(_) => {
                    std::fs::create_dir(&path)
                        .with_context(|| format!("creating staged directory {name:?}"))?;
                    match item {
                        Item::Partial(children) => {
                            Box::pin(self.selected_directory(&child, &path, children, depth + 1))
                                .await?
                        }
                        _ => self.directory(&child, &path, depth + 1).await?,
                    }
                    self.count.directories += 1;
                }
            }
        }
        if !self.cancelled() {
            set_download_mtime(&pin, attrs.mtime.filter(|_| self.preserve_mtime))?;
        }
        Ok(())
    }
}
