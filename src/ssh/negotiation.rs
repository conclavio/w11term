use russh::{
    Preferred,
    keys::{Algorithm, EcdsaCurve},
};
use std::borrow::Cow;

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

pub(super) fn preferred(trust_host_ca: bool) -> Preferred {
    Preferred {
        host_key_certificates: Cow::Borrowed(if trust_host_ca { CERTIFICATE_KEYS } else { &[] }),
        ..Preferred::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ca_endpoints_negotiate_host_certificates_and_keep_the_bare_key_order() {
        let defaults = Preferred::default();
        let plain = preferred(false);
        assert!(plain.host_key_certificates.is_empty());
        assert_eq!(plain.key, defaults.key);

        let ca = preferred(true);
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
