use russh::{
    Preferred,
    keys::{Algorithm, EcdsaCurve},
};
use std::borrow::Cow;

/// Public algorithm names from the first key exchange on one authenticated SSH transport.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct InitialSshAlgorithms {
    pub key_exchange: String,
    pub host_key: String,
    pub cipher: String,
    pub strict_key_exchange: bool,
}

impl InitialSshAlgorithms {
    pub(super) fn read(names: &russh::Names, certificate: bool) -> Self {
        Self {
            key_exchange: names.kex.as_ref().into(),
            host_key: if certificate {
                names.key.to_certificate_type()
            } else {
                names.key.to_string()
            },
            cipher: names.cipher.as_ref().into(),
            strict_key_exchange: names.strict_kex(),
        }
    }
}

pub(super) struct VerifiedHandshake {
    pub fingerprint: String,
    pub initial_algorithms: Option<InitialSshAlgorithms>,
}

// The pinned ssh-key build can verify Ed25519 and all three NIST ECDSA curves.
// russh's default plain list also contains RSA names without the RSA feature.
const CERTIFICATE_KEYS: &[Algorithm] = &[
    Algorithm::Ed25519,
    Algorithm::Ecdsa {
        curve: EcdsaCurve::NistP256,
    },
    Algorithm::Ecdsa {
        curve: EcdsaCurve::NistP384,
    },
    Algorithm::Ecdsa {
        curve: EcdsaCurve::NistP521,
    },
];

/// `recorded` lists the key algorithms known_hosts already holds for the host;
/// they are requested first, as OpenSSH does, keeping the default order otherwise.
pub(super) fn preferred(trust_host_ca: bool, recorded: &[Algorithm]) -> Preferred {
    let defaults = Preferred::default();
    let is_recorded = |offered: &Algorithm| {
        recorded.iter().any(|known| {
            known == offered
                || (matches!(known, Algorithm::Rsa { .. })
                    && matches!(offered, Algorithm::Rsa { .. }))
        })
    };
    let mut key = defaults.key.to_vec();
    // A stable sort keeps russh's order within each group.
    key.sort_by_key(|offered| !is_recorded(offered));
    Preferred {
        host_key_certificates: Cow::Borrowed(if trust_host_ca { CERTIFICATE_KEYS } else { &[] }),
        key: Cow::Owned(key),
        ..defaults
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn verified_host_certificate_keeps_the_negotiated_certificate_algorithm()
    -> anyhow::Result<()> {
        use crate::{options::SshOptions, prompts::Prompts};
        use russh::{
            keys::{PrivateKey, ssh_key::certificate},
            server,
        };
        use std::sync::Arc;

        struct Peer;
        impl server::Handler for Peer {
            type Error = anyhow::Error;
        }

        let host = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
        let ca = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
        let endpoint = "certificate-fixture.example";
        let mut cert = certificate::Builder::new_with_random_nonce(
            &mut rand::rng(),
            host.public_key().key_data().clone(),
            0,
            u64::MAX,
        )?;
        cert.cert_type(certificate::CertType::Host)?;
        cert.valid_principal(endpoint)?;
        let config = Arc::new(server::Config {
            keys: vec![host],
            certificates: vec![cert.sign(&ca)?],
            ..Default::default()
        });
        let state = tempfile::tempdir()?;
        let known_hosts = state.path().join("known_hosts");
        std::fs::write(
            &known_hosts,
            format!(
                "@cert-authority {endpoint} {}\n",
                ca.public_key().to_openssh()?
            ),
        )?;
        let prompts = Prompts::with_known_hosts(known_hosts);
        let options: SshOptions = serde_json::from_value(serde_json::json!({
            "host": endpoint, "user": "fixture", "auth": "none"
        }))?;
        let (client, server_stream) = tokio::io::duplex(65_536);
        let server =
            tokio::spawn(
                async move { server::run_stream(config, server_stream, Peer).await?.await },
            );
        let (handle, verified) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            super::super::transport::key_exchange(&options, Some(&prompts), None, client, None),
        )
        .await??;
        assert_eq!(
            verified.initial_algorithms.unwrap().host_key,
            "ssh-ed25519-cert-v01@openssh.com",
        );
        drop(handle);
        server.abort();
        Ok(())
    }

    #[test]
    fn recorded_host_key_algorithms_are_requested_first() {
        let defaults = Preferred::default();
        let ecdsa = Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP256,
        };
        let reordered = preferred(false, std::slice::from_ref(&ecdsa));
        assert_eq!(reordered.key.first(), Some(&ecdsa));
        assert_eq!(reordered.key.len(), defaults.key.len());
        let rest: Vec<_> = reordered.key.iter().filter(|key| **key != ecdsa).collect();
        let expected: Vec<_> = defaults.key.iter().filter(|key| **key != ecdsa).collect();
        assert_eq!(rest, expected);
    }

    #[test]
    fn only_ca_endpoints_negotiate_host_certificates_and_keep_the_bare_key_order() {
        let defaults = Preferred::default();
        let plain = preferred(false, &[]);
        assert!(plain.host_key_certificates.is_empty());
        assert_eq!(plain.key, defaults.key);

        let ca = preferred(true, &[]);
        assert_eq!(ca.key, defaults.key);
        let certificates: Vec<_> = ca
            .host_key_certificates
            .iter()
            .map(Algorithm::to_certificate_type)
            .collect();
        assert_eq!(
            certificates,
            [
                "ssh-ed25519-cert-v01@openssh.com",
                "ecdsa-sha2-nistp256-cert-v01@openssh.com",
                "ecdsa-sha2-nistp384-cert-v01@openssh.com",
                "ecdsa-sha2-nistp521-cert-v01@openssh.com",
            ]
        );
    }
}
