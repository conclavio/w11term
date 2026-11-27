use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};

const BUNDLED: &[u8] = include_bytes!("../../../assets/command-reference/tldr-pages.en.zip");
pub(super) const MAX_ARCHIVE: usize = 16 * 1024 * 1024;
const MAX_PAGE: usize = 64 * 1024;
const MAX_CONTENT: usize = 32 * 1024 * 1024;
const MAX_PAGES: usize = 20_000;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Platform(pub &'static str, pub &'static str);

pub(super) const COMMON: Platform = Platform("common", "Common");
pub(super) const LINUX: Platform = Platform("linux", "Linux");
pub(super) const WINDOWS: Platform = Platform("windows", "Windows");
pub(super) const PLATFORMS: &[Platform] = &[
    COMMON,
    LINUX,
    WINDOWS,
    Platform("osx", "macOS"),
    Platform("android", "Android"),
    Platform("cisco-ios", "Cisco IOS"),
    Platform("dos", "DOS"),
    Platform("freebsd", "FreeBSD"),
    Platform("netbsd", "NetBSD"),
    Platform("openbsd", "OpenBSD"),
    Platform("sunos", "SunOS"),
];

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Key {
    pub platform: Platform,
    pub name: String,
}

#[derive(Debug)]
pub(super) struct Example {
    pub description: String,
    pub command: String,
}

#[derive(Debug)]
pub(super) struct Page {
    pub title: String,
    pub description: String,
    pub link: Option<String>,
    pub examples: Vec<Example>,
    pub custom: bool,
}

impl Page {
    fn parse(text: &str, custom: bool) -> Option<Self> {
        let mut lines = text.trim_start_matches('\u{feff}').lines().map(str::trim);
        let title = lines.find(|line| !line.is_empty())?.strip_prefix("# ")?;
        let mut description = Vec::new();
        let mut link = None;
        let mut examples = Vec::new();
        let mut label = None;
        for line in lines {
            if let Some(quote) = line.strip_prefix("> ") {
                if let Some(url) = quote
                    .strip_prefix("More information: <")
                    .and_then(|quote| quote.trim_end_matches('.').strip_suffix('>'))
                    .filter(|url| {
                        url.starts_with("https://") && w11term::interaction::safe_url(url)
                    })
                {
                    link = Some(url.to_owned());
                } else {
                    description.push(quote);
                }
            } else if let Some(text) = line.strip_prefix("- ") {
                label = Some(text.strip_suffix(':').unwrap_or(text));
            } else if let Some(command) = line
                .strip_prefix('`')
                .and_then(|line| line.strip_suffix('`'))
                && let Some(description) = label.take()
                && insertable(command)
            {
                examples.push(Example {
                    description: description.to_owned(),
                    command: command.to_owned(),
                });
            }
        }
        Some(Self {
            title: title.to_owned(),
            description: description.join("\n"),
            link,
            examples,
            custom,
        })
    }
}

pub(super) fn insertable(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= 4096 && !text.chars().any(char::is_control)
}

pub(super) fn query(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .to_ascii_lowercase()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !matches!(name, "." | "..")
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '\\'))
}

#[derive(Debug)]
pub(super) struct Catalog {
    pages: BTreeMap<Key, Page>,
}

impl Catalog {
    pub(super) fn page(&self, key: &Key) -> Option<&Page> {
        self.pages.get(key)
    }

    fn archive(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= MAX_ARCHIVE,
            "Command reference archive is too large"
        );
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("Reading the TLDR ZIP")?;
        ensure!(
            zip.len() <= MAX_PAGES,
            "Command reference archive has too many entries"
        );
        let mut pages = BTreeMap::new();
        let mut expanded = 0;
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index)?;
            let path = entry.name()?;
            let Some((platform, file)) = path.split_once('/') else {
                continue;
            };
            let Some(platform) = PLATFORMS.iter().find(|p| p.0 == platform).copied() else {
                continue;
            };
            let Some(name) = file.strip_suffix(".md").filter(|name| valid_name(name)) else {
                continue;
            };
            let name = name.to_ascii_lowercase();
            ensure!(
                entry.is_file() && !entry.is_symlink(),
                "Invalid TLDR page entry"
            );
            let text = read_page(&mut entry)?;
            expanded += text.len();
            ensure!(
                expanded <= MAX_CONTENT,
                "Command reference pages exceed the cache budget"
            );
            let Some(page) = Page::parse(&text, false) else {
                continue;
            };
            let key = Key { platform, name };
            ensure!(pages.insert(key, page).is_none(), "Duplicate TLDR page");
        }
        for required in [COMMON, LINUX, WINDOWS] {
            ensure!(
                pages.keys().any(|key| key.platform == required),
                "TLDR archive is missing {}",
                required.1
            );
        }
        Ok(Self { pages })
    }

    fn custom_pages(&mut self, directory: &Path) -> Result<()> {
        let mut count = 0;
        let mut expanded = 0;
        for &platform in PLATFORMS {
            let entries = match fs::read_dir(directory.join(platform.0)) {
                Ok(entries) => entries,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err.into()),
            };
            for entry in entries {
                let entry = entry?;
                let filename = entry.file_name();
                let Some(name) = filename
                    .to_str()
                    .and_then(|file| file.strip_suffix(".md"))
                    .filter(|name| valid_name(name))
                else {
                    continue;
                };
                if !entry.file_type()?.is_file() {
                    continue;
                }
                count += 1;
                ensure!(count <= 1000, "Custom command reference has too many pages");
                let text = read_page(File::open(entry.path())?)?;
                expanded += text.len();
                ensure!(
                    expanded <= MAX_CONTENT,
                    "Custom command reference exceeds the cache budget"
                );
                if let Some(page) = Page::parse(&text, true) {
                    self.pages.insert(
                        Key {
                            platform,
                            name: name.to_ascii_lowercase(),
                        },
                        page,
                    );
                }
            }
        }
        Ok(())
    }

    pub(super) fn search(&self, text: &str, preferred: Platform) -> Vec<(&Key, &Page)> {
        let text = query(text);
        if text.is_empty() {
            return Vec::new();
        }
        let mut matches: Vec<_> = self
            .pages
            .iter()
            .filter_map(|(key, page)| {
                let rank = if key.name == text {
                    0
                } else if key.name.starts_with(&text) {
                    1
                } else if key.name.contains(&text) {
                    2
                } else {
                    return None;
                };
                let platform = if key.platform == preferred {
                    0
                } else if key.platform == COMMON {
                    1
                } else {
                    2
                };
                Some((
                    (rank, platform, key.name.as_str(), key.platform),
                    (key, page),
                ))
            })
            .collect();
        matches.sort_unstable_by_key(|&(rank, _)| rank);
        let mut names = BTreeSet::new();
        matches
            .into_iter()
            .filter_map(|(_, (key, page))| names.insert(key.name.as_str()).then_some((key, page)))
            .take(18)
            .collect()
    }
}

fn read_page(file: impl Read) -> Result<String> {
    let mut text = String::new();
    file.take(MAX_PAGE as u64 + 1).read_to_string(&mut text)?;
    ensure!(
        text.len() <= MAX_PAGE,
        "Command reference page is too large"
    );
    Ok(text)
}

#[derive(Debug)]
pub struct Loaded {
    pub(super) catalog: Catalog,
    pub(super) status: String,
}

fn directory() -> Result<PathBuf> {
    Ok(w11term::keys::data_dir()?.join("command-reference"))
}

pub(super) fn load() -> Result<Loaded> {
    let directory = directory()?;
    let (catalog, status) = match File::open(directory.join("tldr-pages.en.zip")) {
        Ok(file) => {
            let mut bytes = Vec::new();
            match file
                .take(MAX_ARCHIVE as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(anyhow::Error::from)
                .and_then(|_| Catalog::archive(&bytes))
            {
                Ok(catalog) => (catalog, "Cached TLDR pages".to_owned()),
                Err(_) => (
                    Catalog::archive(BUNDLED)?,
                    "Using bundled TLDR pages because the local cache could not be read".to_owned(),
                ),
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (
            Catalog::archive(BUNDLED)?,
            "Bundled TLDR pages · v2.3".to_owned(),
        ),
        Err(_) => (
            Catalog::archive(BUNDLED)?,
            "Using bundled TLDR pages because the local cache could not be opened".to_owned(),
        ),
    };
    Ok(with_custom(catalog, status, &directory))
}

pub(super) fn update(bytes: Vec<u8>) -> Result<Loaded> {
    update_at(&directory()?, &bytes)
}

fn update_at(directory: &Path, bytes: &[u8]) -> Result<Loaded> {
    let catalog = Catalog::archive(bytes)?;
    fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(directory.join("tldr-pages.en.zip"))
        .context("Saving the TLDR cache")?;
    Ok(with_custom(
        catalog,
        "Updated TLDR pages".to_owned(),
        directory,
    ))
}

fn with_custom(mut catalog: Catalog, mut status: String, directory: &Path) -> Loaded {
    if let Err(err) = catalog.custom_pages(&directory.join("pages")) {
        status = format!("{status}. Reading custom pages: {err:#}");
    }
    Loaded { catalog, status }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_pages_cover_platform_common_and_cross_platform_lookup() {
        let catalog = Catalog::archive(BUNDLED).unwrap();
        for (text, preferred, expected, title) in [
            ("IP", LINUX, LINUX, "ip"),
            ("get-childitem", WINDOWS, WINDOWS, "Get-ChildItem"),
            ("get-childitem", LINUX, WINDOWS, "Get-ChildItem"),
            ("git checkout", WINDOWS, COMMON, "git checkout"),
        ] {
            let results = catalog.search(text, preferred);
            let (key, page) = results[0];
            assert_eq!(key.platform, expected);
            assert_eq!(page.title, title);
            assert!(!page.examples.is_empty());
            assert!(page.link.is_some());
        }
    }

    #[test]
    fn an_invalid_refresh_preserves_the_previous_archive() {
        let directory = tempfile::tempdir().unwrap();
        update_at(directory.path(), BUNDLED).unwrap();
        assert!(update_at(directory.path(), b"invalid ZIP").is_err());
        let cached = fs::read(directory.path().join("tldr-pages.en.zip")).unwrap();
        assert_eq!(cached, BUNDLED);
    }

    #[test]
    fn local_pages_override_the_matching_platform_and_preserve_template_text() {
        let local = tempfile::tempdir().unwrap();
        let linux = local.path().join("linux");
        fs::create_dir(&linux).unwrap();
        fs::write(
            linux.join("rg.md"),
            "# local rg\n\n> Internal page.\n\n- Search:\n\n`rg {{pattern}} \"{{path}}\"`\n",
        )
        .unwrap();
        let mut catalog = Catalog::archive(BUNDLED).unwrap();
        catalog.custom_pages(local.path()).unwrap();
        let results = catalog.search("rg", LINUX);
        assert_eq!(results[0].1.title, "local rg");
        assert!(results[0].1.custom);
        assert_eq!(
            results[0].1.examples[0].command,
            "rg {{pattern}} \"{{path}}\""
        );
        assert!(!catalog.search("rg", WINDOWS)[0].1.custom);
        for text in ["ls\r", "ls\n", "ls\x1b[D", "ls\t"] {
            assert!(!insertable(text));
        }
        assert!(Catalog::archive(b"invalid ZIP").is_err());
    }
}
