use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};
use w11term::saved_reference_pages::valid_page_name as valid_name;

const BUNDLED: &[u8] = include_bytes!("../../../assets/command-reference/tldr-pages.en.zip");
pub(super) const MAX_ARCHIVE: usize = 16 * 1024 * 1024;
pub(super) const MAX_PAGE: usize = 64 * 1024;
const MAX_CONTENT: usize = 32 * 1024 * 1024;
const MAX_PAGES: usize = 20_000;
const MAX_RELATED: usize = 12;
const TOOL_PAGES: &[(&str, &str)] = &[
    (
        "diskwatch",
        include_str!("../../../assets/command-reference/linux/diskwatch.md"),
    ),
    (
        "mdd",
        include_str!("../../../assets/command-reference/linux/mdd.md"),
    ),
    (
        "netwatch",
        include_str!("../../../assets/command-reference/linux/netwatch.md"),
    ),
];

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
pub struct Key {
    pub(super) platform: Platform,
    pub(super) name: String,
}

#[derive(Debug)]
pub(super) struct Example {
    pub description: String,
    pub command: String,
}

#[derive(Debug)]
pub(super) enum PageSource {
    Tldr,
    BuiltIn,
    Custom,
}

impl PageSource {
    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::Tldr => "tldr-pages (CC BY 4.0)",
            Self::BuiltIn => "w11term manual",
            Self::Custom => "Custom page",
        }
    }
}

#[derive(Debug)]
pub(super) struct Page {
    pub title: String,
    pub description: String,
    pub link: Option<String>,
    pub related: Vec<String>,
    pub examples: Vec<Example>,
    pub source: PageSource,
    search_words: Vec<Box<str>>,
}

impl Page {
    pub(super) fn parse(text: &str, source: PageSource) -> Option<Self> {
        let mut lines = text.trim_start_matches('\u{feff}').lines().map(str::trim);
        let title = lines.find(|line| !line.is_empty())?.strip_prefix("# ")?;
        let mut description = Vec::new();
        let mut link = None;
        let mut related = Vec::new();
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
                } else if let Some(names) = related_names(quote)
                    && related.len() + names.len() <= MAX_RELATED
                {
                    related.extend(names);
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
        let search_text = std::iter::once(title)
            .chain(description.iter().copied())
            .chain(related.iter().map(String::as_str))
            .chain(examples.iter().map(|example| example.description.as_str()))
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        let mut search_words: Vec<Box<str>> = keywords(&search_text).map(Box::from).collect();
        search_words.sort_unstable();
        search_words.dedup();
        Some(Self {
            title: title.to_owned(),
            description: description.join("\n"),
            link,
            related,
            examples,
            source,
            search_words,
        })
    }
}

fn related_names(quote: &str) -> Option<Vec<String>> {
    let list = quote.strip_prefix("See also: ")?.trim_end_matches('.');
    let mut pieces = list.split('`');
    if pieces.next()? != "" {
        return None;
    }
    let mut names = Vec::new();
    loop {
        let label = pieces.next()?;
        if names.len() == MAX_RELATED || !valid_name(&query(label)) {
            return None;
        }
        names.push(label.to_owned());
        match pieces.next()?.trim() {
            "" if pieces.next().is_none() => return Some(names),
            "," | "and" | ", and" => {}
            _ => return None,
        }
    }
}

fn keywords(text: &str) -> impl Iterator<Item = &str> {
    text.split(|ch: char| !ch.is_alphanumeric()).filter(|word| {
        !matches!(
            *word,
            "" | "a"
                | "an"
                | "and"
                | "by"
                | "do"
                | "for"
                | "from"
                | "how"
                | "i"
                | "in"
                | "is"
                | "of"
                | "on"
                | "or"
                | "the"
                | "to"
                | "with"
        )
    })
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

#[derive(Debug)]
pub(super) struct Catalog {
    pages: BTreeMap<Key, Page>,
}

impl Catalog {
    pub(super) fn page(&self, key: &Key) -> Option<&Page> {
        self.pages.get(key)
    }

    pub(super) fn set_private_page(&mut self, key: Key, page: Page) {
        self.pages.insert(key, page);
    }

    pub(super) fn exact(&self, text: &str, preferred: Platform) -> Option<Key> {
        let name = query(text);
        [preferred, COMMON]
            .into_iter()
            .map(|platform| Key {
                platform,
                name: name.clone(),
            })
            .find(|key| self.pages.contains_key(key))
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
            let Some(page) = Page::parse(&text, PageSource::Tldr) else {
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
                if let Some(page) = Page::parse(&text, PageSource::Custom) {
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

    fn add_tool_pages(&mut self) {
        for &(name, text) in TOOL_PAGES {
            if self.exact(name, LINUX).is_none() {
                let page =
                    Page::parse(text, PageSource::BuiltIn).expect("valid embedded command page");
                self.pages.insert(
                    Key {
                        platform: LINUX,
                        name: name.to_owned(),
                    },
                    page,
                );
            }
        }
    }

    pub(super) fn search(&self, text: &str, preferred: Platform) -> Vec<(&Key, &Page)> {
        let task = text.to_lowercase();
        let mut terms: Vec<_> = keywords(&task).collect();
        terms.sort_unstable();
        terms.dedup();
        let search_content = terms.iter().any(|term| term.chars().count() >= 3);
        let text = query(text);
        if text.is_empty() {
            return Vec::new();
        }
        let mut matches: Vec<_> = self
            .pages
            .iter()
            .filter_map(|(key, page)| {
                let platform = if key.platform == preferred {
                    0
                } else if key.platform == COMMON {
                    1
                } else {
                    2
                };
                let rank = if key.name == text {
                    0
                } else if key.name.starts_with(&text) || (text == "mddog" && key.name == "mdd") {
                    1
                } else if key.name.contains(&text) {
                    2
                } else if platform < 2
                    && search_content
                    && terms.iter().all(|term| {
                        page.search_words
                            .binary_search_by(|word| word.as_ref().cmp(term))
                            .is_ok()
                    })
                {
                    3
                } else {
                    return None;
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

pub(super) fn read_page(file: impl Read) -> Result<String> {
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

pub(super) fn directory() -> Result<PathBuf> {
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
    Ok(with_local_pages(catalog, status, &directory))
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
    Ok(with_local_pages(
        catalog,
        "Updated TLDR pages".to_owned(),
        directory,
    ))
}

fn with_local_pages(mut catalog: Catalog, mut status: String, directory: &Path) -> Loaded {
    if let Err(err) = catalog.custom_pages(&directory.join("pages")) {
        status = format!("{status}. Reading custom pages: {err:#}");
    }
    catalog.add_tool_pages();
    Loaded { catalog, status }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_standalone_related_command_lists() {
        let page = Page::parse(
            concat!(
                "# Cargo guide\n> Describe a shipping task.\n",
                "> See also: `port inventory`, `ospp.vbs`, and `carrier-route`.\n",
                "> See also: `cargo` when preparing a new crate.\n",
                "> See also: `https://example.invalid/path`.",
            ),
            PageSource::Custom,
        )
        .unwrap();
        assert_eq!(
            page.related,
            ["port inventory", "ospp.vbs", "carrier-route"]
        );
        assert_eq!(
            page.description,
            "Describe a shipping task.\nSee also: `cargo` when preparing a new crate.\nSee also: `https://example.invalid/path`."
        );
        assert!(page.search_words.iter().any(|word| &**word == "inventory"));
    }

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
    fn installed_tool_lookup_uses_an_exact_platform_or_common_page() {
        let catalog = Catalog::archive(BUNDLED).unwrap();
        assert_eq!(catalog.exact("ip", LINUX).unwrap().platform, LINUX);
        assert_eq!(catalog.exact("rg", LINUX).unwrap().platform, COMMON);
        assert!(catalog.exact("get-childitem", LINUX).is_none());
        assert!(catalog.exact("git check", LINUX).is_none());
    }

    #[test]
    fn task_search_ranks_names_first_and_uses_the_selected_platform() {
        let mut catalog = Catalog {
            pages: BTreeMap::new(),
        };
        for (platform, name, markdown) in [
            (WINDOWS, "meter", "# meter\n> Collect timings."),
            (
                WINDOWS,
                "clock",
                "# clock\n> Measure time.\n- Use a meter:\n`clock --hiddenflag`",
            ),
            (COMMON, "timer", "# timer\n> Measure time with a meter."),
            (
                LINUX,
                "time",
                "# time\n> Measure command time with a meter.",
            ),
        ] {
            catalog.set_private_page(
                Key {
                    platform,
                    name: name.into(),
                },
                Page::parse(markdown, PageSource::Custom).unwrap(),
            );
        }
        let matches = |text, platform| {
            catalog
                .search(text, platform)
                .into_iter()
                .map(|(key, _)| key.name.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(matches("meter", WINDOWS), ["meter", "clock", "timer"]);
        assert_eq!(
            matches("How do I MEASURE time with a meter?", WINDOWS),
            ["clock", "timer"]
        );
        assert_eq!(matches("measure time", LINUX), ["time", "timer"]);
        assert_eq!(matches("time", WINDOWS), ["time", "timer", "clock"]);
        assert!(matches("hiddenflag", WINDOWS).is_empty());
        assert!(matches("with the", WINDOWS).is_empty());
    }

    #[test]
    fn every_builtin_tool_has_an_exact_offline_reference() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = with_local_pages(
            Catalog::archive(BUNDLED).unwrap(),
            String::new(),
            directory.path(),
        )
        .catalog;
        for name in w11term::sftp::builtin_tool_names() {
            let key = catalog
                .exact(name, LINUX)
                .unwrap_or_else(|| panic!("missing page for {name}"));
            let page = catalog.page(&key).unwrap();
            assert!(!page.examples.is_empty(), "{name}");
            assert!(page.link.is_some(), "{name}");
        }
        let results = catalog.search("mddog", LINUX);
        assert_eq!(results[0].0.name, "mdd");
        assert!(matches!(results[0].1.source, PageSource::BuiltIn));
    }

    #[test]
    fn refreshed_and_private_pages_take_precedence_over_supplements() {
        let directory = tempfile::tempdir().unwrap();
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (path, title) in [
            ("common/netwatch.md", "Upstream NetWatch"),
            ("linux/ip.md", "ip"),
            ("windows/dir.md", "dir"),
        ] {
            zip.start_file(path, zip::write::SimpleFileOptions::default())
                .unwrap();
            writeln!(zip, "# {title}").unwrap();
        }
        let upstream = zip.finish().unwrap().into_inner();
        let common = directory.path().join("pages").join("common");
        fs::create_dir_all(&common).unwrap();
        fs::write(common.join("diskwatch.md"), "# Team disk notes").unwrap();
        let updated = update_at(directory.path(), &upstream).unwrap();
        let lookup = |name: &str| {
            updated
                .catalog
                .page(&updated.catalog.exact(name, LINUX).unwrap())
                .unwrap()
        };
        assert!(matches!(lookup("netwatch").source, PageSource::Tldr));
        assert_eq!(lookup("netwatch").title, "Upstream NetWatch");
        assert!(matches!(lookup("diskwatch").source, PageSource::Custom));
        assert_eq!(lookup("diskwatch").title, "Team disk notes");
        assert!(matches!(lookup("mdd").source, PageSource::BuiltIn));
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
        assert!(matches!(results[0].1.source, PageSource::Custom));
        assert_eq!(
            results[0].1.examples[0].command,
            "rg {{pattern}} \"{{path}}\""
        );
        assert!(matches!(
            catalog.search("rg", WINDOWS)[0].1.source,
            PageSource::Tldr
        ));
        for text in ["ls\r", "ls\n", "ls\x1b[D", "ls\t"] {
            assert!(!insertable(text));
        }
        assert!(Catalog::archive(b"invalid ZIP").is_err());
    }
}
