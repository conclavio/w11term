use anyhow::{Context as _, Result, ensure};
use regex::{Regex, RegexBuilder};
use std::sync::Arc;

/// The complete visible prompt prefix for a direct SSH profile.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct GroupPromptPattern(Arc<CompiledPattern>);

#[derive(Debug)]
struct CompiledPattern {
    source: String,
    regex: Regex,
}

impl GroupPromptPattern {
    pub fn as_str(&self) -> &str {
        &self.0.source
    }

    pub(crate) fn matches(&self, text: &str) -> bool {
        self.0
            .regex
            .find(text)
            .is_some_and(|m| m.start() == 0 && m.end() == text.len())
    }
}

impl PartialEq for GroupPromptPattern {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl TryFrom<String> for GroupPromptPattern {
    type Error = anyhow::Error;

    fn try_from(source: String) -> Result<Self> {
        ensure!(
            source.len() <= 256 && !source.chars().any(char::is_control),
            "Use a group prompt pattern of up to 256 bytes on one line"
        );
        let regex = RegexBuilder::new(&format!(r"\A(?:{source})\z"))
            .size_limit(512 * 1024)
            .build()
            .context("Invalid group prompt regex")?;
        ensure!(
            !regex.is_match(""),
            "A group prompt pattern must match visible text"
        );
        Ok(Self(Arc::new(CompiledPattern { source, regex })))
    }
}

impl From<GroupPromptPattern> for String {
    fn from(pattern: GroupPromptPattern) -> Self {
        pattern.as_str().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_prompt_is_a_bounded_full_line_regex_stored_as_a_string() {
        let pattern: GroupPromptPattern = serde_json::from_str(r#""rack-[12]>>""#).unwrap();
        assert_eq!(serde_json::to_string(&pattern).unwrap(), r#""rack-[12]>>""#);
        assert!(pattern.matches("rack-2>>"));
        for text in ["Log: rack-1>>", "rack-1>> payload", "rack-3>>", ""] {
            assert!(!pattern.matches(text), "matched {text:?}");
        }
        for pattern in ["[", ".*", "two\nlines", &"x".repeat(257)] {
            assert!(GroupPromptPattern::try_from(pattern.to_owned()).is_err());
        }
    }
}
