use anyhow::{Context, Result, ensure};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

pub const MAX_PATTERNS: usize = 16;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Definition {
    name: String,
    regex: String,
}

#[derive(Clone, Debug)]
pub struct Pattern {
    pub name: String,
    pub regex: Regex,
}

/// Compiled only when preferences are loaded or a rule is edited.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(try_from = "Vec<Definition>", into = "Vec<Definition>")]
pub struct Patterns(Vec<Pattern>);

impl Patterns {
    pub fn rules(&self) -> &[Pattern] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn save(&mut self, index: Option<usize>, name: &str, expression: &str) -> Result<()> {
        let name = name.trim();
        ensure!(
            !name.is_empty() && name.len() <= 64 && !name.chars().any(char::is_control),
            "Enter a pattern name of 1–64 bytes without control characters."
        );
        ensure!(
            !expression.is_empty() && expression.len() <= 512,
            "Enter a regular expression of 1–512 bytes."
        );
        ensure!(
            index.is_some_and(|i| i < self.0.len())
                || (index.is_none() && self.0.len() < MAX_PATTERNS),
            "Up to {MAX_PATTERNS} hint patterns can be saved."
        );
        for (_, other) in self.0.iter().enumerate().filter(|(i, _)| Some(*i) != index) {
            ensure!(
                other.name.to_lowercase() != name.to_lowercase(),
                "A pattern named {} already exists.",
                other.name
            );
            ensure!(
                other.regex.as_str() != expression,
                "This expression is already used by {}.",
                other.name
            );
        }
        let syntax = regex_syntax::Parser::new()
            .parse(expression)
            .context("Invalid regular expression")?;
        ensure!(
            syntax.properties().minimum_len().is_some_and(|len| len > 0),
            "The expression must match at least one character every time."
        );
        let regex = RegexBuilder::new(expression)
            .size_limit(1024 * 1024)
            .dfa_size_limit(256 * 1024)
            .build()
            .context("Compiling hint pattern")?;
        let rule = Pattern {
            name: name.into(),
            regex,
        };
        if let Some(i) = index {
            self.0[i] = rule;
        } else {
            self.0.push(rule);
        }
        Ok(())
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.0.len() {
            self.0.remove(index);
        }
    }
}

impl TryFrom<Vec<Definition>> for Patterns {
    type Error = anyhow::Error;
    fn try_from(definitions: Vec<Definition>) -> Result<Self> {
        let mut rules = Self::default();
        for rule in definitions {
            rules.save(None, &rule.name, &rule.regex)?;
        }
        Ok(rules)
    }
}

impl From<Patterns> for Vec<Definition> {
    fn from(rules: Patterns) -> Self {
        rules
            .0
            .into_iter()
            .map(|rule| Definition {
                name: rule.name,
                regex: rule.regex.as_str().into(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_edits_keep_the_previous_rule() {
        let mut p = Patterns::default();
        p.save(None, "Build ID", r"b-[0-9]+").unwrap();
        for invalid in ["", "(", "a*", "(x|)", r"\b"] {
            assert!(p.save(Some(0), "renamed", invalid).is_err());
        }
        assert!(p.save(None, "BUILD ID", "x").is_err());
        assert!(p.save(None, "other", r"b-[0-9]+").is_err());
        assert!(p.save(None, "a\nb", "x").is_err());
        assert!(p.save(None, "long", &"x".repeat(513)).is_err());
        assert_eq!(p.rules()[0].name, "Build ID");
        assert_eq!(p.rules()[0].regex.as_str(), r"b-[0-9]+");
        assert!(serde_json::from_str::<Patterns>(r#"[{"name":"bad","regex":"^"}]"#).is_err());
    }

    #[test]
    fn edit_remove_and_serde_preserve_priority() {
        let mut p = Patterns::default();
        for i in 0..MAX_PATTERNS {
            p.save(None, &format!("rule {i}"), &format!("id-{i}"))
                .unwrap();
        }
        assert!(p.save(None, "overflow", "one more").is_err());
        p.save(Some(0), "first", "ticket").unwrap();
        p.remove(1);
        let p: Patterns = serde_json::from_value(serde_json::to_value(p).unwrap()).unwrap();
        assert_eq!(p.rules().len(), MAX_PATTERNS - 1);
        assert_eq!(
            (&*p.rules()[0].name, p.rules()[0].regex.as_str()),
            ("first", "ticket")
        );
        assert_eq!(p.rules()[1].name, "rule 2");
    }
}
