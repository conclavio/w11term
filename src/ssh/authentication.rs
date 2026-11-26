//! Public evidence of a completed SSH login. Authentication secrets never enter this type.

use russh::keys::{Certificate, HashAlg};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SshKeySource {
    File,
    Agent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdditionalSshAuthentication {
    Password,
    KeyboardInteractive,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "method", rename_all = "kebab-case")]
pub enum VerifiedSshAuthentication {
    None,
    Password,
    KeyboardInteractive,
    PublicKey {
        source: SshKeySource,
        key_fingerprint: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        certificate_authority_fingerprint: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        additional_methods: Vec<AdditionalSshAuthentication>,
    },
}

impl VerifiedSshAuthentication {
    pub(super) fn certificate_authority(certificate: &Certificate) -> String {
        certificate
            .signature_key()
            .fingerprint(HashAlg::Sha256)
            .to_string()
    }
}
