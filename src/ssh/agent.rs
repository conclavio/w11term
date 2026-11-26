use anyhow::{Context, Result};
use russh::{
    AgentAuthError, Signer,
    keys::{
        Certificate, HashAlg,
        agent::{AgentIdentity, client::AgentClient},
    },
};
use std::time::Duration;
use tokio::net::windows::named_pipe::NamedPipeClient;

use crate::options::AgentFingerprint;

pub(super) type WindowsAgent = AgentClient<NamedPipeClient>;

pub(super) async fn connect() -> Result<WindowsAgent> {
    let pipe =
        std::env::var_os("SSH_AUTH_SOCK").unwrap_or_else(|| r"\\.\pipe\openssh-ssh-agent".into());
    AgentClient::connect_named_pipe(pipe)
        .await
        .context("cannot connect to Windows SSH agent")
}

pub(super) struct CertificateSigner<'a> {
    agent: &'a mut WindowsAgent,
    identity: &'a AgentIdentity,
}

impl<'a> CertificateSigner<'a> {
    pub(super) fn matching(
        agent: &'a mut WindowsAgent,
        identities: &'a [AgentIdentity],
        desired: &Certificate,
    ) -> Option<Self> {
        // If the agent has the exact certificate, honor any constraints on that
        // entry. Agents holding only the plain key sign the same SSH payload
        // using its public key blob while the server receives the certificate.
        let identity = identities.iter().find(|identity| {
            matches!(identity, AgentIdentity::Certificate { certificate, .. } if certificate == desired)
        }).or_else(|| {
            identities.iter().find(|identity| {
                matches!(identity, AgentIdentity::PublicKey { key, .. } if key.key_data() == desired.public_key())
            })
        })?;
        Some(Self { agent, identity })
    }
}

impl Signer for CertificateSigner<'_> {
    type Error = AgentAuthError;

    async fn auth_sign(
        &mut self,
        _: &AgentIdentity,
        hash_alg: Option<HashAlg>,
        data: Vec<u8>,
    ) -> Result<Vec<u8>, Self::Error> {
        Ok(self
            .agent
            .sign_request(self.identity, hash_alg, data)
            .await?)
    }
}

#[derive(Debug)]
pub struct Key {
    pub fingerprint: AgentFingerprint,
    pub algorithm: String,
    pub comment: String,
}

/// Read the agent's public key inventory for a user who requested it.
pub async fn list_keys() -> Result<Vec<Key>> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut agent = connect().await?;
        let mut keys = agent
            .request_identities()
            .await
            .context("could not list Windows SSH agent keys")?
            .into_iter()
            .filter_map(|identity| match identity {
                AgentIdentity::PublicKey { key, comment } => Some(Key {
                    fingerprint: AgentFingerprint::for_key(&key),
                    algorithm: key.algorithm().to_string(),
                    comment: crate::prompts::sanitize_single_line(&comment, 80),
                }),
                AgentIdentity::Certificate { .. } => None,
            })
            .collect::<Vec<_>>();
        keys.sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
        keys.dedup_by(|a, b| a.fingerprint == b.fingerprint);
        Ok(keys)
    })
    .await
    .context("Windows SSH agent did not respond")?
}
