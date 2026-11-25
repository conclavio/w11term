use anyhow::{Context, Result, ensure};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    fs,
    path::{Path, PathBuf},
};

use crate::search::{MAX_QUERY_BYTES, compile_regex};

pub const MAX_RULES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub name: String,
    pub query: String,
    pub regex: bool,
    pub match_case: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(try_from = "Vec<Rule>")]
pub struct Rules(Vec<Rule>);

pub fn compile_query(query: &str, regex: bool, match_case: bool) -> Result<Regex, String> {
    if query.len() > MAX_QUERY_BYTES {
        return Err(format!("Search text exceeds {MAX_QUERY_BYTES} UTF-8 bytes"));
    }
    let expression = if regex {
        Cow::Borrowed(query)
    } else {
        Cow::Owned(regex::escape(query))
    };
    compile_regex(&expression, match_case)
}

fn single_line(text: &str) -> bool {
    !text
        .chars()
        .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
}

pub enum Edit {
    Save(Rule),
    Remove(Rule),
}

impl Rules {
    pub fn entries(&self) -> &[Rule] {
        &self.0
    }

    pub fn path() -> Result<PathBuf> {
        Ok(crate::keys::data_dir()?.join("output-alert-rules.json"))
    }

    pub fn load(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("Reading saved output rules"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).context("Opening saved output rules"),
        }
    }

    fn insert(&mut self, mut rule: Rule) -> Result<()> {
        ensure!(single_line(&rule.name), "Enter a single-line rule name.");
        rule.name = rule.name.trim().to_owned();
        ensure!(
            !rule.name.is_empty() && rule.name.chars().count() <= 80,
            "Enter a rule name of 1–80 characters."
        );
        ensure!(
            !rule.query.is_empty() && single_line(&rule.query),
            "Enter a single-line output search."
        );
        compile_query(&rule.query, rule.regex, rule.match_case).map_err(anyhow::Error::msg)?;
        let key = rule.name.to_lowercase();
        let index = self
            .0
            .binary_search_by(|current| current.name.to_lowercase().cmp(&key))
            .err()
            .with_context(|| format!("A rule named {} already exists.", rule.name))?;
        ensure!(
            self.0.len() < MAX_RULES,
            "Up to {MAX_RULES} output rules can be saved."
        );
        self.0.insert(index, rule);
        Ok(())
    }

    pub fn update(path: &Path, edit: Edit) -> Result<Self> {
        crate::stored_json::update(path, "output rules", || {
            let mut rules = Self::load(path)?;
            match edit {
                Edit::Save(rule) => rules.insert(rule)?,
                Edit::Remove(rule) => {
                    let index = rules.0.iter().position(|current| *current == rule)
                        .context("This output rule changed in another window. Reopen the saved rules to refresh.")?;
                    rules.0.remove(index);
                }
            }
            Ok(rules)
        })
    }
}

impl TryFrom<Vec<Rule>> for Rules {
    type Error = anyhow::Error;

    fn try_from(entries: Vec<Rule>) -> Result<Self> {
        let mut rules = Self::default();
        for entry in entries {
            rules.insert(entry)?;
        }
        Ok(rules)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_rules_roundtrip_the_exact_search_and_reject_conflicting_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("output-alert-rules.json");
        let rule = Rule {
            name: " Ready ".into(),
            query: "^Ready $".into(),
            regex: true,
            match_case: true,
        };
        let saved = Rules::update(&path, Edit::Save(rule.clone())).unwrap();
        let original = saved.entries()[0].clone();
        assert_eq!(original.name, "Ready");
        assert_eq!(original.query, rule.query);
        let mut conflict = rule.clone();
        conflict.name = "ready".into();
        assert!(Rules::update(&path, Edit::Save(conflict)).is_err());
        let mut invalid = rule;
        invalid.name = "Broken".into();
        invalid.query = "(".into();
        assert!(Rules::update(&path, Edit::Save(invalid)).is_err());
        assert_eq!(
            Rules::load(&path).unwrap().entries(),
            std::slice::from_ref(&original)
        );
        Rules::update(&path, Edit::Remove(original.clone())).unwrap();
        assert!(Rules::update(&path, Edit::Remove(original)).is_err());
        assert!(Rules::load(&path).unwrap().entries().is_empty());
    }
}
