//! Expand global Include directives in order. Each file has its own Host/Match
//! scope; options and aliases from all files feed the same first-value resolver.
use super::*;
use regex::Regex;
use std::io::Read;
mod local;
#[cfg(test)]
mod tests;

const MAX_BYTES: usize = 1024 * 1024;
const MAX_LINES: usize = 16_384;
const MAX_FILES: usize = 64;
const MAX_DEPTH: usize = 8;
const MAX_PATTERNS: usize = 1024;
const MAX_DIRECTORY_ENTRIES: usize = 16_384;

struct Reader<'a> {
    home: &'a Path,
    parsed: Parsed,
    bytes: usize,
    lines: usize,
    files: usize,
    patterns: usize,
    entries: usize,
    active: Vec<((u32, u64), PathBuf)>,
}

pub(super) fn load(path: &Path, home: &Path, user: &str) -> Result<ImportReport> {
    // Preserve the explicit picker grant, including network shares and links.
    // Only automatically discovered includes go through the local-only opener.
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    ensure!(file.metadata()?.is_file(), "selected config must be a file");
    let mut reader = Reader::new(home);
    reader.read(path, file)?;
    Ok(reader.parsed.report(home, user))
}

#[cfg(test)]
pub(super) fn parse_test(text: &str, home: &Path, user: &str) -> Result<ImportReport> {
    let mut reader = Reader::new(home);
    reader.source(Path::new("config").into(), text)?;
    Ok(reader.parsed.report(home, user))
}

impl<'a> Reader<'a> {
    fn new(home: &'a Path) -> Self {
        Self {
            home,
            parsed: Parsed::default(),
            bytes: 0,
            lines: 0,
            files: 0,
            patterns: 0,
            entries: 0,
            active: Vec::new(),
        }
    }

    fn file(&mut self, path: &Path, required: bool) -> Result<()> {
        let Some(file) =
            local::open(path, false).with_context(|| format!("opening {}", path.display()))?
        else {
            ensure!(!required, "config file does not exist: {}", path.display());
            return Ok(());
        };
        self.read(path, file)
    }

    fn read(&mut self, path: &Path, file: std::fs::File) -> Result<()> {
        ensure!(self.files < MAX_FILES, "config exceeds {MAX_FILES} files");
        ensure!(
            self.active.len() <= MAX_DEPTH,
            "config exceeds Include depth {MAX_DEPTH}"
        );
        let id = local::file_id(&file)?;
        ensure!(
            !self
                .active
                .iter()
                .any(|(prior, name)| (id.1 != 0 && prior == &id) || name == path),
            "recursive Include of {}",
            path.display()
        );
        self.files += 1;
        let mut text = String::new();
        file.take((MAX_BYTES - self.bytes + 1) as u64)
            .read_to_string(&mut text)
            .with_context(|| format!("reading UTF-8 config {}", path.display()))?;
        self.bytes += text.len();
        ensure!(
            self.bytes <= MAX_BYTES,
            "SSH config exceeds 1 MiB across files"
        );
        self.active.push((id, path.to_owned()));
        let result = self.source(path.into(), &text);
        self.active.pop();
        result
    }

    fn source(&mut self, path: Arc<Path>, text: &str) -> Result<()> {
        let mut section = Section::global();
        for (i, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
            let here = Location {
                path: path.clone(),
                line: i + 1,
            };
            self.lines += 1;
            ensure!(
                self.lines <= MAX_LINES,
                "{here}: SSH config exceeds {MAX_LINES} lines across files"
            );
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let split = line
                .find(|c: char| c.is_whitespace() || c == '=')
                .unwrap_or(line.len());
            let key = line[..split].to_ascii_lowercase();
            let value = line[split..].trim_start();
            let args = words(value.strip_prefix('=').unwrap_or(value))
                .with_context(|| here.to_string())?;
            ensure!(
                !key.is_empty()
                    && key.len() <= 128
                    && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && !args.is_empty(),
                "{here}: invalid directive or missing argument"
            );
            match key.as_str() {
                "host" => {
                    for alias in &args {
                        if !alias.starts_with('!') && !alias.contains(['*', '?']) {
                            valid_host(alias)
                                .with_context(|| format!("{here}: invalid Host alias"))?;
                            self.parsed.aliases.insert(alias.clone());
                            ensure!(
                                self.parsed.aliases.len() <= 1024,
                                "{here}: config exceeds 1024 explicit host aliases"
                            );
                        }
                    }
                    self.parsed.section(section);
                    section = Section {
                        scope: Scope::Hosts(args),
                        directives: Vec::new(),
                    };
                }
                "match" => {
                    self.parsed.section(section);
                    section = Section {
                        scope: Scope::Conditional(here, args),
                        directives: Vec::new(),
                    };
                }
                "include" => {
                    if matches!(section.scope, Scope::Global) {
                        self.patterns += args.len();
                        ensure!(
                            self.patterns <= MAX_PATTERNS,
                            "{here}: config exceeds {MAX_PATTERNS} Include patterns"
                        );
                        match args
                            .iter()
                            .map(|arg| pattern(arg, self.home))
                            .collect::<Result<Vec<_>>>()
                        {
                            Ok(patterns) => {
                                self.parsed
                                    .section(std::mem::replace(&mut section, Section::global()));
                                for pattern in patterns {
                                    self.include(&pattern).with_context(|| {
                                        format!("{here}: Include {}", pattern.display())
                                    })?;
                                }
                                continue;
                            }
                            Err(error) => {
                                self.parsed
                                    .warnings
                                    .insert(format!("{here}: unsupported Include: {error}"));
                            }
                        }
                    } else {
                        self.parsed.warnings.insert(format!(
                            "{here}: Host- and Match-scoped Include directives are unsupported."
                        ));
                    }
                    section.directives.push(Directive {
                        line: here,
                        key,
                        args,
                    });
                }
                _ => section.directives.push(Directive {
                    line: here,
                    key,
                    args,
                }),
            }
        }
        self.parsed.section(section);
        Ok(())
    }

    fn include(&mut self, path: &Path) -> Result<()> {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .context("missing Include filename")?;
        if !name.contains(['*', '?']) {
            return self.file(path, false);
        }
        let parent = path.parent().context("missing Include directory")?;
        let Some(dir) = local::open(parent, true)? else {
            return Ok(());
        };
        let mut pattern = String::from(r"\A");
        for ch in name.chars() {
            match ch {
                '*' => pattern.push_str(".*"),
                '?' => pattern.push('.'),
                _ => pattern.push_str(&regex::escape(&ch.to_string())),
            }
        }
        pattern.push_str(r"\z");
        let matcher = Regex::new(&pattern)?;
        let mut matches = Vec::new();
        local::names(&dir, |units| {
            self.entries += 1;
            ensure!(
                self.entries <= MAX_DIRECTORY_ENTRIES,
                "config exceeds {MAX_DIRECTORY_ENTRIES} directory entries scanned"
            );
            let found =
                String::from_utf16(units).context("non-Unicode name in Include directory")?;
            if (!found.starts_with('.') || name.starts_with('.')) && matcher.is_match(&found) {
                component(&found, false)?;
                ensure!(
                    self.files + matches.len() < MAX_FILES,
                    "config exceeds {MAX_FILES} files"
                );
                matches.push(found);
            }
            Ok(())
        })?;
        matches.sort_unstable();
        for name in matches {
            self.file(&parent.join(name), true)?;
        }
        Ok(())
    }
}

fn pattern(raw: &str, home: &Path) -> Result<PathBuf> {
    ensure!(
        !raw.contains(['%', '$', '[', ']', '{', '}']),
        "tokens, environment expansion, and bracket/brace patterns are unsupported"
    );
    if let Some(relative) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        ensure!(
            !relative.starts_with(['\\', '/']) && !relative.contains(':'),
            "invalid home-relative Include"
        );
        absolute_pattern(relative, home)
    } else {
        ensure!(
            !raw.starts_with('~'),
            "named-user home expansion is unsupported"
        );
        absolute_pattern(raw, &home.join(".ssh"))
    }
}

fn absolute_pattern(raw: &str, base: &Path) -> Result<PathBuf> {
    ensure!(
        !raw.is_empty() && !raw.ends_with(['\\', '/']),
        "expected a config filename"
    );
    let drive_rooted =
        |s: &str| matches!(s.as_bytes(),[l,b':',b'\\'|b'/',..] if l.is_ascii_alphabetic());
    let full = if drive_rooted(raw) {
        raw.to_owned()
    } else {
        ensure!(
            !raw.starts_with(['\\', '/']) && !raw.contains(':'),
            "expected a local drive path; UNC, device and drive-relative paths are unsupported"
        );
        format!(
            "{}\\{}",
            base.to_str().context("non-Unicode base directory")?,
            raw
        )
    };
    ensure!(
        drive_rooted(&full) && full.encode_utf16().count() <= 32767,
        "expected a local drive-letter path"
    );
    let mut result = PathBuf::from(format!("{}:\\", &full[..1]));
    let mut parts = full[3..].split(['\\', '/']).peekable();
    while let Some(part) = parts.next() {
        match part {
            "" | "." => {}
            ".." => {
                result.pop();
            }
            _ => {
                component(part, parts.peek().is_none())?;
                result.push(part);
            }
        }
    }
    ensure!(result.file_name().is_some(), "expected a config filename");
    Ok(result)
}

fn component(name: &str, allow_glob: bool) -> Result<()> {
    ensure!(
        !name.is_empty()
            && !name.ends_with(['.', ' '])
            && name.encode_utf16().count() <= 255
            && !name
                .chars()
                .any(|c| c.is_control() || r#"<>:"|\/"#.contains(c))
            && (allow_glob || !name.contains(['*', '?'])),
        "unsupported path component (globs are supported only in filenames)"
    );
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON" | "NUL" | "AUX" | "PRN" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) || stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|digit| {
            matches!(
                digit,
                "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        });
    ensure!(!reserved, "device names are unsupported");
    Ok(())
}
