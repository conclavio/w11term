use super::{RosterStatus, Target};
use regex::{RegexSet, RegexSetBuilder};
use std::{fmt::Write, sync::Arc};

const MAX_PATTERN_BYTES: usize = 4096;
pub(super) const PROFILE_PAGE_SIZE: usize = 24;

#[derive(Clone, Default)]
pub(super) struct Filter {
    query: String,
    wildcards: bool,
    matcher: Matcher,
    status: RosterStatus,
}

#[derive(Clone, Default)]
enum Matcher {
    #[default]
    All,
    Text(Vec<String>),
    Wildcards(Arc<Wildcards>),
    Invalid(&'static str),
}

struct Wildcards {
    included: RegexSet,
    excluded: RegexSet,
}

#[derive(Clone, Default)]
pub(super) struct Names {
    pub(super) label: String,
    pub(super) host: String,
    address: String,
}

impl Names {
    pub(super) fn new(label: &str, host: &str, user: &str, port: u16) -> Self {
        let address = super::address::display(user, host, port);
        let host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        Self {
            label: label.into(),
            host: host.into(),
            address,
        }
    }

    pub(super) fn display(&self, pane: u64) -> String {
        format!("{} · {} · Pane {pane}", self.label, self.address)
    }

    pub(super) fn address(&self) -> &str {
        &self.address
    }

    fn values(&self) -> [&str; 4] {
        let (login, _) = self.address.rsplit_once(':').unwrap_or_default();
        [&self.label, &self.host, login, &self.address]
    }
}

impl Filter {
    pub(super) fn query(&self) -> &str {
        &self.query
    }

    pub(super) fn wildcards(&self) -> bool {
        self.wildcards
    }

    pub(super) fn status(&self) -> RosterStatus {
        self.status
    }

    pub(super) fn set_status(&mut self, status: RosterStatus) {
        self.status = status;
    }

    pub(super) fn set_query(&mut self, query: String) {
        if self.query != query {
            self.query = query;
            self.refresh();
        }
    }

    pub(super) fn set_wildcards(&mut self, wildcards: bool) {
        if self.wildcards != wildcards {
            self.wildcards = wildcards;
            self.refresh();
        }
    }

    pub(super) fn clear(&mut self) {
        self.query.clear();
        self.matcher = Matcher::All;
        self.status = RosterStatus::All;
    }

    fn refresh(&mut self) {
        let query = self.query.trim();
        self.matcher = if query.is_empty() {
            Matcher::All
        } else if self.wildcards {
            parse_patterns(query).unwrap_or_else(Matcher::Invalid)
        } else {
            Matcher::Text(query.split_whitespace().map(str::to_lowercase).collect())
        };
    }

    pub(super) fn error(&self) -> Option<&'static str> {
        match self.matcher {
            Matcher::Invalid(message) => Some(message),
            _ => None,
        }
    }

    pub(super) fn matches(&self, target: &Target) -> bool {
        self.matches_state(target) && self.matches_query(target)
    }

    pub(super) fn matches_state(&self, target: &Target) -> bool {
        self.status == RosterStatus::All || self.status == target.state.status()
    }

    pub(super) fn matches_query(&self, target: &Target) -> bool {
        match &self.matcher {
            Matcher::All => true,
            Matcher::Invalid(_) => false,
            Matcher::Text(terms) => matches_text(
                terms,
                &target.name,
                target.profile.as_deref().unwrap_or_default(),
            ),
            Matcher::Wildcards(wildcards) => {
                let mut values = target
                    .names
                    .values()
                    .into_iter()
                    .chain(target.profile.as_deref());
                values
                    .clone()
                    .any(|value| wildcards.included.is_match(value))
                    && !values.any(|value| wildcards.excluded.is_match(value))
            }
        }
    }

    pub(super) fn matches_profile(&self, name: &str, address: Option<&str>) -> bool {
        match &self.matcher {
            Matcher::All => true,
            Matcher::Text(terms) => matches_text(terms, name, address.unwrap_or_default()),
            _ => false,
        }
    }
}

fn matches_text(terms: &[String], name: &str, address: &str) -> bool {
    let name = name.to_lowercase();
    let address = address.to_lowercase();
    terms
        .iter()
        .all(|term| name.contains(term) || address.contains(term))
}

fn parse_patterns(query: &str) -> Result<Matcher, &'static str> {
    if query.len() > MAX_PATTERN_BYTES {
        return Err("Wildcard filters can use up to 4096 bytes.");
    }
    let mut included = Vec::new();
    let mut excluded = Vec::new();
    for entry in entries(query) {
        let entry = entry.trim();
        let (group, pattern) = match entry.strip_prefix('!') {
            Some(pattern) => (&mut excluded, pattern.trim_start()),
            None => (&mut included, entry),
        };
        if pattern.is_empty() {
            return Err("Enter a pattern for every comma-separated entry.");
        }
        group.push(translate(pattern)?);
    }
    if included.is_empty() {
        return Err("Include a host pattern, or use * before exclusions.");
    }
    Ok(Matcher::Wildcards(Arc::new(Wildcards {
        included: compile(included)?,
        excluded: compile(excluded)?,
    })))
}

fn entries(query: &str) -> Vec<&str> {
    let mut start = 0;
    let mut in_class = false;
    let mut escaped = false;
    let mut entries = Vec::new();
    for (index, ch) in query.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '[' => in_class = true,
            ']' => in_class = false,
            ',' if !in_class => {
                entries.push(&query[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    entries.push(&query[start..]);
    entries
}

fn compile(patterns: Vec<String>) -> Result<RegexSet, &'static str> {
    RegexSetBuilder::new(patterns)
        .case_insensitive(true)
        .dot_matches_new_line(true)
        .size_limit(1 << 20)
        .build()
        .map_err(|_| "Check wildcard character ranges and pattern length.")
}

fn translate(pattern: &str) -> Result<String, &'static str> {
    let mut expression = String::from(r"\A");
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' => expression.push_str(".*"),
            '?' => expression.push('.'),
            '[' => character_class(&mut chars, &mut expression)?,
            '\\' => literal(
                &mut expression,
                chars.next().ok_or("Finish the escaped character.")?,
            ),
            ch => literal(&mut expression, ch),
        }
    }
    expression.push_str(r"\z");
    Ok(expression)
}

fn literal(expression: &mut String, ch: char) {
    if regex_syntax::is_meta_character(ch) {
        expression.push('\\');
    }
    expression.push(ch);
}

fn character_class(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    expression: &mut String,
) -> Result<(), &'static str> {
    let negate = chars.next_if_eq(&'!').is_some();
    let mut members = Vec::new();
    loop {
        match chars.next() {
            None => return Err("Close the wildcard character set with ]."),
            Some(']') => break,
            Some('\\') => {
                members.push((chars.next().ok_or("Finish the escaped character.")?, false))
            }
            Some(ch) => members.push((ch, ch == '-')),
        }
    }
    if members.is_empty() {
        return Err("Enter at least one character inside a wildcard set.");
    }
    expression.push('[');
    if negate {
        expression.push('^');
    }
    for (index, &(ch, dash)) in members.iter().enumerate() {
        if dash && index > 0 && index + 1 < members.len() {
            if members[index - 1].1 || members[index + 1].1 {
                return Err("Escape literal hyphens inside a wildcard range.");
            }
            expression.push('-');
        } else {
            // Encode character values so regex-only set operators stay literal.
            write!(expression, "\\x{{{:x}}}", ch as u32).unwrap();
        }
    }
    expression.push(']');
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::host_groups::{HostGroup, TargetState};
    use std::collections::BTreeSet;

    fn target(pane: u64, label: &str, host: &str, port: u16, profile: &str) -> Target {
        let names = Names::new(label, host, "ops", port);
        Target {
            pane,
            name: names.display(pane),
            names,
            profile: Some(profile.into()),
            connected: false,
            state: TargetState::Offline,
        }
    }

    #[test]
    fn wildcard_scope_matches_whole_names_and_applies_exclusions_across_fields() {
        let targets = [
            target(1, "Primary", "web-01", 22, "Production"),
            target(2, "Secondary", "web-02", 22, "Staging-canary"),
            target(3, "DB 1", "2001:db8::7", 2200, "Database"),
            target(4, "Mirror", "old-web-01", 22, "Archive"),
            target(5, "cache [east]", "cache", 22, "OPS, cache"),
        ];
        let mut group = HostGroup {
            selected: (1..=5).collect(),
            ..Default::default()
        };
        group.filter.set_wildcards(true);
        for (query, expected) in [
            ("WEB-0[1-2], DB ?, !*-canary", vec![1, 3]),
            ("web-0[!2]", vec![1]),
            ("ops@web-?", vec![]),
            ("ops@web-01", vec![1]),
            ("2001:db8::*", vec![3]),
            (r"ops@\[2001:db8::*\]:22??", vec![3]),
            (r"cache \[east\]", vec![5]),
            (r"ops\, cache", vec![5]),
            ("ops[,] cache", vec![5]),
            ("*, !Archive", vec![1, 2, 3, 5]),
        ] {
            group.filter.set_query(query.into());
            assert_eq!(group.filter.error(), None, "{query}");
            assert_eq!(
                group.selected_shown(&targets),
                expected.into_iter().collect::<BTreeSet<_>>(),
                "{query}"
            );
        }
        assert_eq!(group.selected, BTreeSet::from([1, 2, 3, 4, 5]));
    }

    #[test]
    fn invalid_patterns_expose_no_targets_and_recovery_does_not_change_selection() {
        let targets = [target(1, "edge*", "edge1", 22, "edge")];
        let mut group = HostGroup {
            selected: BTreeSet::from([1]),
            ..Default::default()
        };
        group.filter.set_wildcards(true);
        for malformed in [
            "edge[", "edge[]", "[z-a]", "*,,edge", "!, *", "!edge*", "edge\\",
        ] {
            group.filter.set_query(malformed.into());
            assert!(group.filter.error().is_some(), "{malformed}");
            assert!(group.selected_shown(&targets).is_empty(), "{malformed}");
        }
        group.filter.set_query(r"edge\*".into());
        assert_eq!(group.filter.error(), None);
        assert_eq!(group.selected_shown(&targets), BTreeSet::from([1]));
        group.filter.set_query("edge*".into());
        group.filter.set_wildcards(false);
        assert_eq!(group.selected_shown(&targets), BTreeSet::from([1]));
    }

    #[test]
    fn live_state_intersects_name_filters_and_keeps_hidden_checks() {
        let mut targets = [
            target(1, "East", "web-01", 22, "Production East"),
            target(2, "West", "web-02", 22, "Production West"),
            target(3, "Cache", "cache-01", 22, "Cache"),
        ];
        targets[1].state = TargetState::Alternate;
        let mut group = HostGroup {
            selected: BTreeSet::from([1, 2, 3]),
            ..Default::default()
        };
        group.filter.set_query("production".into());
        group.filter.set_status(RosterStatus::Disconnected);
        assert_eq!(group.selected_shown(&targets), BTreeSet::from([1]));
        group.filter.set_status(RosterStatus::Alternate);
        assert_eq!(group.selected_shown(&targets), BTreeSet::from([2]));

        targets[1].state = TargetState::InUse;
        assert!(group.selected_shown(&targets).is_empty());
        group.filter.set_status(RosterStatus::Waiting);
        assert_eq!(group.selected_shown(&targets), BTreeSet::from([2]));

        group.filter.set_wildcards(true);
        group.filter.set_query("web-*, !*West".into());
        assert!(group.selected_shown(&targets).is_empty());
        group.filter.clear();
        assert_eq!(group.selected, BTreeSet::from([1, 2, 3]));
        assert_eq!(group.selected_shown(&targets), group.selected);
    }
}
