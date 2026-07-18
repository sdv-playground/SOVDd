//! Transparent server-side SecurityAccess (UDS 0x27) key derivation.
//!
//! When a diagnostic flow needs SecurityAccess on an ECU, SOVDd can perform
//! the seed/key dance itself — transparently, on demand — instead of requiring
//! the client to drive `modes/security`. The key-derivation algorithm is
//! pluggable per-ECU behind the [`UnlockProvider`] trait so a real deployment
//! can swap in a vendor- or HSM-backed provider without touching the call
//! sites; the seam is the trait, and construction from config is a match on an
//! algorithm string ([`provider_from_config`]).
//!
//! The bundled dev/simulation implementation is [`XorUnlock`], byte-for-byte
//! the sim gate in `example-ecu` (`handle_security_access`) and the XOR
//! algorithm the retired SOVD-security-helper used.

use std::sync::Arc;

use crate::config::UnlockConfig;

/// Algorithm identifier for the XOR simulation gate ([`XorUnlock`]).
pub const ALGORITHM_XOR: &str = "xor";

/// Error raised while constructing an [`UnlockProvider`] from config or while
/// computing a SecurityAccess key.
#[derive(Debug, thiserror::Error)]
pub enum UnlockError {
    /// The configured secret was empty (or decoded to zero bytes). An empty
    /// secret has no defined `% len` and could never match a real gate.
    #[error("unlock secret must not be empty")]
    EmptySecret,

    /// `secret_hex` was not valid hexadecimal.
    #[error("invalid unlock secret hex: {0}")]
    InvalidSecretHex(String),

    /// The configured `algorithm` string is not recognised.
    #[error("unknown unlock algorithm: {0}")]
    UnknownAlgorithm(String),

    /// Key computation failed (e.g. a malformed seed for the algorithm).
    #[error("unlock key computation failed: {0}")]
    Compute(String),
}

/// Pluggable per-ECU SecurityAccess key derivation.
///
/// Given the security `level` and the ECU-provided `seed`, return the key
/// bytes to send back in the UDS 0x27 sendKey step. One provider is held for
/// the life of the backend and shared across tasks, so implementations must be
/// `Send + Sync`.
pub trait UnlockProvider: Send + Sync {
    /// Compute the SecurityAccess key for `seed` at security `level`.
    fn compute_key(&self, level: u8, seed: &[u8]) -> Result<Vec<u8>, UnlockError>;
}

/// Simulation/dev unlock: `key[i] = seed[i] ^ secret[i % secret.len()]`.
///
/// This mirrors, byte-for-byte, the gate implemented by `example-ecu`
/// (`crates/example-ecu/src/parameters.rs`, `handle_security_access`). It is
/// intended for simulation only — production ECUs plug in a vendor/HSM
/// provider via [`UnlockProvider`].
pub struct XorUnlock {
    secret: Vec<u8>,
}

impl XorUnlock {
    /// Build an XOR unlock from raw secret bytes. Rejects an empty secret.
    pub fn new(secret: Vec<u8>) -> Result<Self, UnlockError> {
        if secret.is_empty() {
            return Err(UnlockError::EmptySecret);
        }
        Ok(Self { secret })
    }

    /// Build an XOR unlock from a hex-encoded secret (e.g. `"ff"`,
    /// `"deadbeef"`). Rejects invalid hex and an empty secret.
    pub fn from_hex(secret_hex: &str) -> Result<Self, UnlockError> {
        let secret =
            hex::decode(secret_hex).map_err(|e| UnlockError::InvalidSecretHex(e.to_string()))?;
        Self::new(secret)
    }
}

impl UnlockProvider for XorUnlock {
    fn compute_key(&self, _level: u8, seed: &[u8]) -> Result<Vec<u8>, UnlockError> {
        // `secret` is guaranteed non-empty by construction, so `% len` is safe.
        Ok(seed
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ self.secret[i % self.secret.len()])
            .collect())
    }
}

/// Construct an [`UnlockProvider`] from an [`UnlockConfig`]. The `algorithm`
/// string selects the implementation; this match is the single place new
/// algorithms (vendor/HSM) are wired in.
pub fn provider_from_config(config: &UnlockConfig) -> Result<Arc<dyn UnlockProvider>, UnlockError> {
    match config.algorithm.as_str() {
        ALGORITHM_XOR => Ok(Arc::new(XorUnlock::from_hex(&config.secret_hex)?)),
        other => Err(UnlockError::UnknownAlgorithm(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference XOR key exactly as `example-ecu`'s `handle_security_access`
    /// computes it: `expected[i] = seed[i] ^ secret[i % secret.len()]`.
    /// The provider MUST reproduce this shape or the sim gate rejects the key.
    fn example_ecu_expected_key(secret: &[u8], seed: &[u8]) -> Vec<u8> {
        seed.iter()
            .enumerate()
            .map(|(i, b)| b ^ secret[i % secret.len()])
            .collect()
    }

    #[test]
    fn xor_known_vector_single_byte_secret() {
        // Default example-ecu secret is 0xFF.
        let unlock = XorUnlock::new(vec![0xFF]).unwrap();
        let seed = [0x01, 0x02, 0x03, 0x04];
        let key = unlock.compute_key(1, &seed).unwrap();
        assert_eq!(key, vec![0xFE, 0xFD, 0xFC, 0xFB]);
    }

    #[test]
    fn xor_known_vector_multi_byte_secret_wraps() {
        // Multi-byte secret must wrap with `i % len`.
        let secret = vec![0xAA, 0xBB, 0xCC];
        let unlock = XorUnlock::new(secret.clone()).unwrap();
        let seed = [0x00, 0x00, 0x00, 0x00, 0x00];
        let key = unlock.compute_key(1, &seed).unwrap();
        // 0x00 ^ secret[i % 3]
        assert_eq!(key, vec![0xAA, 0xBB, 0xCC, 0xAA, 0xBB]);
    }

    /// Cross-check: the provider output equals the `example-ecu` gate formula
    /// for several secret/seed shapes (single-byte, multi-byte wrapping,
    /// odd seed length). This is the contract that lets the sim gate accept
    /// the server-computed key.
    #[test]
    fn xor_matches_example_ecu_gate_formula() {
        let cases: &[(&[u8], &[u8])] = &[
            (&[0xFF], &[0x11, 0x22, 0x33, 0x44]),
            (&[0xDE, 0xAD, 0xBE, 0xEF], &[0x01, 0x02, 0x03, 0x04]),
            (&[0xAA, 0xBB, 0xCC], &[0x10, 0x20, 0x30, 0x40, 0x50]),
        ];
        for (secret, seed) in cases {
            let unlock = XorUnlock::new(secret.to_vec()).unwrap();
            let key = unlock.compute_key(1, seed).unwrap();
            assert_eq!(
                key,
                example_ecu_expected_key(secret, seed),
                "provider key must match the example-ecu gate for secret={secret:02X?} seed={seed:02X?}"
            );
        }
    }

    #[test]
    fn xor_rejects_empty_secret() {
        assert!(matches!(
            XorUnlock::new(vec![]),
            Err(UnlockError::EmptySecret)
        ));
        assert!(matches!(
            XorUnlock::from_hex(""),
            Err(UnlockError::EmptySecret)
        ));
    }

    #[test]
    fn xor_from_hex_rejects_invalid_hex() {
        assert!(matches!(
            XorUnlock::from_hex("zz"),
            Err(UnlockError::InvalidSecretHex(_))
        ));
    }

    #[test]
    fn from_hex_roundtrips_secret() {
        let unlock = XorUnlock::from_hex("ff").unwrap();
        let key = unlock.compute_key(1, &[0x0F, 0xF0]).unwrap();
        assert_eq!(key, vec![0xF0, 0x0F]);
    }

    #[test]
    fn provider_from_config_selects_xor() {
        let cfg = UnlockConfig {
            algorithm: "xor".to_string(),
            secret_hex: "ff".to_string(),
            level: None,
        };
        let provider = provider_from_config(&cfg).unwrap();
        let key = provider.compute_key(1, &[0x01, 0x02]).unwrap();
        assert_eq!(key, vec![0xFE, 0xFD]);
    }

    #[test]
    fn provider_from_config_rejects_unknown_algorithm() {
        let cfg = UnlockConfig {
            algorithm: "rsa-hsm".to_string(),
            secret_hex: "ff".to_string(),
            level: None,
        };
        assert!(matches!(
            provider_from_config(&cfg),
            Err(UnlockError::UnknownAlgorithm(_))
        ));
    }

    #[test]
    fn provider_from_config_rejects_empty_secret() {
        let cfg = UnlockConfig {
            algorithm: "xor".to_string(),
            secret_hex: "".to_string(),
            level: None,
        };
        assert!(matches!(
            provider_from_config(&cfg),
            Err(UnlockError::EmptySecret)
        ));
    }
}
