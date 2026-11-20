//! A durable reference identifies one generation of a PTY on an authenticated host.
use crate::options::SshOptions;
use anyhow::{Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableReference {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub host_key: String,
    pub name: String,
    pub generation: String,
}

impl DurableReference {
    pub fn new(options: &SshOptions, host_key: &str) -> Self {
        Self {
            host: options.host.clone(),
            port: options.port,
            user: options.user.clone(),
            host_key: host_key.to_owned(),
            name: format!("w11term-{:032x}", rand::random::<u128>()),
            generation: format!("{:032x}", rand::random::<u128>()),
        }
    }

    pub fn validate(&self) -> Result<()> {
        fn token(value: &str) -> bool {
            value.len() == 32
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }
        ensure!(
            self.name.strip_prefix("w11term-").is_some_and(token) && token(&self.generation),
            "Invalid durable session name or generation"
        );
        ensure!(
            !self.user.is_empty() && !self.host.is_empty() && self.port != 0,
            "Incomplete durable SSH endpoint"
        );
        ensure!(
            self.host_key
                .strip_prefix("SHA256:")
                .and_then(|encoded| STANDARD_NO_PAD.decode(encoded).ok())
                .is_some_and(|bytes| bytes.len() == 32),
            "Invalid durable host-key fingerprint"
        );
        Ok(())
    }

    pub fn matches_endpoint(&self, options: &SshOptions) -> bool {
        self.host.eq_ignore_ascii_case(&options.host)
            && self.port == options.port
            && self.user == options.user
            && options
                .host_key
                .as_ref()
                .is_none_or(|pin| pin == &self.host_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_is_unique_and_bound_to_the_pinned_account() {
        let mut options: SshOptions =
            serde_json::from_str(r#"{"host":"host.example","user":"pilot","auth":"none"}"#)
                .unwrap();
        let pin = format!("SHA256:{}", STANDARD_NO_PAD.encode([3u8; 32]));
        let first = DurableReference::new(&options, &pin);
        assert!(first.validate().is_ok() && first.matches_endpoint(&options));
        assert_ne!(first, DurableReference::new(&options, &pin));
        options.user = "other".into();
        assert!(!first.matches_endpoint(&options));
        options.user = first.user.clone();
        options.host_key = Some(format!("SHA256:{}", STANDARD_NO_PAD.encode([4u8; 32])));
        assert!(!first.matches_endpoint(&options));
        let mut invalid = first;
        invalid.name.push('/');
        assert!(invalid.validate().is_err());
    }
}
