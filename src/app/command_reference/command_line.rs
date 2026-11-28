use super::catalog::{Catalog, Key, Platform, WINDOWS};

pub(in crate::app) struct Lookup {
    platform: Platform,
    names: Vec<String>,
}

impl Lookup {
    pub(super) fn new(line: &str, shell_platform: Platform, platform: Platform) -> Option<Self> {
        let (executable, mut tail) = command_head(line, shell_platform)?;
        let mut names = vec![executable];
        let mut prefix = names[0].clone();
        while let Some((part, remaining)) = word(tail) {
            if !component(part) || prefix.len() + 1 + part.len() > 128 {
                break;
            }
            prefix.push('-');
            prefix.push_str(&part.to_ascii_lowercase());
            names.push(prefix.clone());
            tail = remaining;
        }
        Some(Self { platform, names })
    }

    pub(super) fn executable(&self) -> &str {
        &self.names[0]
    }

    pub(super) fn platform(&self) -> Platform {
        self.platform
    }

    pub(super) fn resolve(&self, catalog: &Catalog) -> Option<Key> {
        self.names
            .iter()
            .rev()
            .find_map(|name| catalog.exact(name, self.platform))
    }
}

fn horizontal_space(text: &str) -> &str {
    text.trim_start_matches([' ', '\t'])
}

fn separator(c: char) -> bool {
    c.is_whitespace() || c.is_control() || matches!(c, '|' | ';' | '&' | '<' | '>' | '(' | ')')
}

// Read a literal word from the first command only. The recorded input is never
// evaluated. Adjacent quoted and unquoted fragments need fuller shell parsing.
fn word(input: &str) -> Option<(&str, &str)> {
    let input = horizontal_space(input);
    match input.chars().next()? {
        quote @ ('"' | '\'') => {
            let body = &input[1..];
            let close = body.find(quote)?;
            let rest = &body[close + 1..];
            if rest.chars().next().is_some_and(|c| !separator(c)) {
                return None;
            }
            Some((&body[..close], rest))
        }
        '#' => None,
        _ => {
            let stop = input
                .find(|c| separator(c) || matches!(c, '\'' | '"'))
                .unwrap_or(input.len());
            let (value, rest) = input.split_at(stop);
            if value.is_empty() || rest.starts_with(['\'', '"']) {
                return None;
            }
            Some((value, rest))
        }
    }
}

fn component(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !matches!(name, "." | "..")
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
}

fn executable(token: &str, platform: Platform) -> Option<String> {
    if token.contains(['$', '\u{60}', '\r', '\n']) {
        return None;
    }
    let mut name = if platform == WINDOWS {
        token.rsplit(['/', '\\']).next()?
    } else {
        token.rsplit('/').next()?
    }
    .to_ascii_lowercase();
    if platform == WINDOWS
        && let Some((stem, suffix)) = name.rsplit_once('.')
        && matches!(suffix, "exe" | "com" | "cmd" | "bat" | "ps1")
    {
        name.truncate(stem.len());
    }
    component(&name).then_some(name)
}

fn assignment(input: &str) -> Option<&str> {
    let input = horizontal_space(input);
    if !input
        .bytes()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
    {
        return None;
    }
    let end = input.find(|c: char| !c.is_ascii_alphanumeric() && c != '_')?;
    let value = input[end..].strip_prefix('=')?;
    if value.chars().next().is_none_or(separator) {
        return Some(value);
    }
    word(value).map(|(_, tail)| tail)
}

fn wrapper_tail<'a>(wrapper: &str, mut input: &'a str) -> &'a str {
    while let Some((flag, tail)) = word(input) {
        match (wrapper, flag) {
            (_, "--") => return tail,
            ("sudo", "-n" | "-H" | "-E" | "--non-interactive")
            | ("env", "-i" | "--ignore-environment") => {
                input = tail;
            }
            ("sudo" | "doas", "-u")
            | ("sudo", "-g" | "--user" | "--group")
            | ("env", "-u" | "--unset") => {
                let Some((_, remaining)) = word(tail) else {
                    return input;
                };
                input = remaining;
            }
            _ => break,
        }
    }
    input
}

fn command_head(input: &str, platform: Platform) -> Option<(String, &str)> {
    let mut input = horizontal_space(input);
    if platform == WINDOWS
        && let Some(tail) = input.strip_prefix('&')
    {
        input = tail;
    }
    let mut wrapper = None;
    loop {
        if platform != WINDOWS
            && let Some(tail) = assignment(input)
        {
            input = tail;
            continue;
        }
        let Some((token, tail)) = word(input) else {
            return wrapper;
        };
        let Some(name) = executable(token, platform) else {
            return wrapper;
        };
        if platform == WINDOWS
            || !matches!(name.as_str(), "sudo" | "doas" | "env" | "command" | "nohup")
        {
            return Some((name, tail));
        }
        input = wrapper_tail(&name, tail);
        wrapper = Some((name, tail));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::command_reference::LINUX;

    #[test]
    fn extracts_literal_linux_prefixes_and_stops_at_shell_punctuation() {
        let lookup = Lookup::new(
            r#"LANG="C UTF-8" sudo -n -u operator env -i MODE=plain "/usr/bin/git" log -5 | sed argument"#,
            LINUX,
            WINDOWS,
        )
        .unwrap();
        assert_eq!(lookup.names, ["git", "git-log"]);
        assert_eq!(lookup.platform(), WINDOWS);
        let lookup = Lookup::new("git log; curl example.com", LINUX, LINUX).unwrap();
        assert_eq!(lookup.names, ["git", "git-log"]);
        let lookup = Lookup::new("sudo --unknown command", LINUX, LINUX).unwrap();
        assert_eq!(lookup.names, ["sudo"]);
    }

    #[test]
    fn handles_windows_call_operator_and_literal_paths() {
        let lookup = Lookup::new(
            r#"& "C:\Program Files\Git\bin\git.EXE" worktree list"#,
            WINDOWS,
            WINDOWS,
        )
        .unwrap();
        assert_eq!(lookup.names, ["git", "git-worktree", "git-worktree-list"]);
        let lookup =
            Lookup::new("Get-ChildItem -LiteralPath 'C:\\report'", WINDOWS, WINDOWS).unwrap();
        assert_eq!(lookup.names, ["get-childitem"]);
        assert!(Lookup::new("& $tool script", WINDOWS, WINDOWS).is_none());
        assert!(Lookup::new("\"unterminated", WINDOWS, WINDOWS).is_none());
        assert!(Lookup::new("git'log' arg", LINUX, LINUX).is_none());
    }
}
