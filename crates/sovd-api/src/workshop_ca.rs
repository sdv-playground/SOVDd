//! Workshop-CA token validation — offline delegated-cert (`x5c`) JWTs.
//!
//! For the offline/workshop case (Phase 2, `tasks/sovdd-token-minter.md`) the
//! device can't fetch a JWKS. Instead the workshop's minter signs the JWT with
//! a leaf key whose cert chains to an OEM **Workshop CA**, and ships that chain
//! in the JWT `x5c` header. This validator:
//!
//!   1. validates the `x5c` chain up to a **pinned** CA (intermediates allowed,
//!      validity windows, `basicConstraints` CA:TRUE on issuers),
//!   2. verifies the JWT (ES256) signature with the leaf cert's key,
//!   3. checks `exp`/`nbf` and `aud == this device id` (the replay guard).
//!
//! The vehicle talks to neither the minter nor the CA. Scope authorization
//! (`component:<id>`) is then handled by the existing middleware via the
//! returned [`ClientContext`]-shaped `(subject, scopes)`.
//!
//! A controlled mini path-validator (RustCrypto `x509-cert` + `p256`), not a
//! TLS-shaped `webpki` engine. Fleet-constrained delegation is slice 2.

use base64::Engine;
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use serde::Deserialize;
use x509_cert::der::{Decode, Encode};
use x509_cert::ext::pkix::BasicConstraints;
use x509_cert::name::Name;
use x509_cert::Certificate;

const BASIC_CONSTRAINTS_OID: &str = "2.5.29.19";

/// Upper bound on the x5c chain length — caps the cert-parse + ECDSA-verify
/// work an attacker can force with a bloated header (bounded DoS).
const MAX_X5C_LEN: usize = 8;

/// Validates workshop tokens against a pinned set of CA roots + this device's id.
pub struct WorkshopCaValidator {
    roots: Vec<Certificate>,
    /// This device's id — the expected `aud` (a token minted for another
    /// vehicle is rejected).
    device_id: String,
}

/// Claims this validator reads. `exp`/`nbf`/`aud` are enforced here; `sub` and
/// the scope claim are extracted for authorization.
#[derive(Deserialize)]
struct WsClaims {
    sub: String,
    aud: String,
    exp: i64,
    #[serde(default)]
    nbf: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
}

impl WsClaims {
    fn into_scopes(self) -> Vec<String> {
        if let Some(arr) = self.scopes {
            arr
        } else if let Some(s) = self.scope {
            s.split_whitespace().map(str::to_string).collect()
        } else {
            Vec::new()
        }
    }
}

impl WorkshopCaValidator {
    /// Build from a PEM bundle of one or more pinned CA certs + this device id.
    pub fn from_pem(ca_pem: &str, device_id: &str) -> Result<Self, String> {
        let roots = Certificate::load_pem_chain(ca_pem.as_bytes())
            .map_err(|e| format!("parse workshop CA PEM: {e}"))?;
        if roots.is_empty() {
            return Err("workshop CA bundle contained no certificates".to_string());
        }
        Ok(Self {
            roots,
            device_id: device_id.to_string(),
        })
    }

    /// Validate a bearer JWT. Returns `(subject, scopes)` on success.
    pub fn validate(&self, jwt: &str) -> Result<(String, Vec<String>), String> {
        self.validate_at(jwt, chrono::Utc::now().timestamp())
    }

    fn validate_at(&self, jwt: &str, now: i64) -> Result<(String, Vec<String>), String> {
        // 1. Parse the x5c chain from the JWT header.
        let header =
            jsonwebtoken::decode_header(jwt).map_err(|e| format!("invalid JWT header: {e}"))?;
        // Pin the algorithm up front (defence-in-depth: the leaf-key verify is
        // hardcoded ES256, but reject other algs early and explicitly).
        if header.alg != jsonwebtoken::Algorithm::ES256 {
            return Err(format!(
                "unexpected JWT alg {:?} (expected ES256)",
                header.alg
            ));
        }
        let x5c = header.x5c.ok_or("workshop token is missing its x5c chain")?;
        // Bound the work an attacker can force (cert parses + ECDSA verifies).
        if x5c.len() > MAX_X5C_LEN {
            return Err(format!(
                "x5c chain too long ({} certs > max {MAX_X5C_LEN})",
                x5c.len()
            ));
        }
        let std_b64 = base64::engine::general_purpose::STANDARD;
        let mut chain = Vec::with_capacity(x5c.len());
        for entry in &x5c {
            let der = std_b64
                .decode(entry)
                .map_err(|e| format!("x5c entry is not valid base64: {e}"))?;
            chain.push(
                Certificate::from_der(&der).map_err(|e| format!("x5c cert parse failed: {e}"))?,
            );
        }

        // 2. Validate the chain up to a pinned CA; get the leaf.
        let leaf = self.verify_chain(&chain, now)?;

        // 3. Verify the JWT signature with the leaf key, + claims.
        verify_jws_es256(jwt, leaf, now, &self.device_id)
    }

    /// Validate `chain` = `[leaf, intermediate…]` up to a pinned root CA.
    /// Returns the leaf certificate on success.
    fn verify_chain<'a>(
        &self,
        chain: &'a [Certificate],
        now: i64,
    ) -> Result<&'a Certificate, String> {
        if chain.is_empty() {
            return Err("empty x5c chain".to_string());
        }
        // Defence-in-depth follow-up (low risk for a controlled PKI): we check
        // basicConstraints CA:TRUE on issuers but not keyUsage(keyCertSign)/EKU
        // on issuers, leaf keyUsage(digitalSignature), or pathLenConstraint.
        for i in 0..chain.len() {
            let subject = &chain[i];
            check_validity(subject, now)?;
            if i + 1 < chain.len() {
                // Signed by the next cert in the chain (an intermediate CA).
                let issuer = &chain[i + 1];
                if !is_ca(issuer) {
                    return Err("x5c intermediate is not a CA (basicConstraints)".to_string());
                }
                verify_signed_by(subject, issuer)?;
            } else {
                // Topmost x5c cert must be issued by one of the pinned roots.
                let ok = self.roots.iter().any(|root| {
                    is_ca(root)
                        && names_match(&subject.tbs_certificate.issuer, &root.tbs_certificate.subject)
                        && check_validity(root, now).is_ok()
                        && verify_signed_by(subject, root).is_ok()
                });
                if !ok {
                    return Err(
                        "x5c chain does not terminate at the pinned workshop CA".to_string()
                    );
                }
            }
        }
        Ok(&chain[0])
    }
}

/// Verify the leaf-signed ES256 JWS and check `exp`/`nbf`/`aud`.
fn verify_jws_es256(
    jwt: &str,
    leaf: &Certificate,
    now: i64,
    expected_aud: &str,
) -> Result<(String, Vec<String>), String> {
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() != 3 {
        return Err("malformed JWT (expected 3 segments)".to_string());
    }
    let url = base64::engine::general_purpose::URL_SAFE_NO_PAD;

    // Signature: JWS ES256 is raw r||s (64 bytes), not DER.
    let sig_bytes = url
        .decode(parts[2])
        .map_err(|e| format!("JWT signature base64: {e}"))?;
    let sig = Signature::from_slice(&sig_bytes).map_err(|e| format!("JWT signature: {e}"))?;

    let vk = leaf_verifying_key(leaf)?;
    let signing_input = format!("{}.{}", parts[0], parts[1]);
    vk.verify(signing_input.as_bytes(), &sig)
        .map_err(|_| "JWT signature is invalid".to_string())?;

    // Claims.
    let payload = url
        .decode(parts[1])
        .map_err(|e| format!("JWT payload base64: {e}"))?;
    let claims: WsClaims =
        serde_json::from_slice(&payload).map_err(|e| format!("JWT claims: {e}"))?;
    if claims.exp <= now {
        return Err("token expired".to_string());
    }
    if let Some(nbf) = claims.nbf {
        if nbf > now {
            return Err("token not yet valid (nbf)".to_string());
        }
    }
    if claims.aud != expected_aud {
        return Err(format!(
            "token audience '{}' does not match this device",
            claims.aud
        ));
    }
    let sub = claims.sub.clone();
    Ok((sub, claims.into_scopes()))
}

/// Extract a P-256 verifying key from a certificate's SubjectPublicKeyInfo.
fn leaf_verifying_key(cert: &Certificate) -> Result<VerifyingKey, String> {
    let point = cert
        .tbs_certificate
        .subject_public_key_info
        .subject_public_key
        .as_bytes()
        .ok_or("certificate public key is not octet-aligned")?;
    VerifyingKey::from_sec1_bytes(point).map_err(|e| format!("certificate public key: {e}"))
}

/// Verify `subject` was issued (name + signature) by `issuer`.
fn verify_signed_by(subject: &Certificate, issuer: &Certificate) -> Result<(), String> {
    if !names_match(
        &subject.tbs_certificate.issuer,
        &issuer.tbs_certificate.subject,
    ) {
        return Err("certificate issuer/subject name mismatch".to_string());
    }
    let vk = leaf_verifying_key(issuer)?;
    let tbs = subject
        .tbs_certificate
        .to_der()
        .map_err(|e| format!("re-encode TBSCertificate: {e}"))?;
    let sig_der = subject
        .signature
        .as_bytes()
        .ok_or("certificate signature is not octet-aligned")?;
    let sig = Signature::from_der(sig_der).map_err(|e| format!("certificate signature: {e}"))?;
    vk.verify(&tbs, &sig)
        .map_err(|_| "certificate signature is invalid".to_string())
}

/// Now must fall within the certificate's validity window.
fn check_validity(cert: &Certificate, now: i64) -> Result<(), String> {
    let nb = cert
        .tbs_certificate
        .validity
        .not_before
        .to_unix_duration()
        .as_secs() as i64;
    let na = cert
        .tbs_certificate
        .validity
        .not_after
        .to_unix_duration()
        .as_secs() as i64;
    if now < nb {
        return Err("certificate not yet valid".to_string());
    }
    if now > na {
        return Err("certificate expired".to_string());
    }
    Ok(())
}

/// True iff the cert asserts `basicConstraints` CA:TRUE.
fn is_ca(cert: &Certificate) -> bool {
    let Some(exts) = &cert.tbs_certificate.extensions else {
        return false;
    };
    for ext in exts {
        if ext.extn_id.to_string() == BASIC_CONSTRAINTS_OID {
            if let Ok(bc) = BasicConstraints::from_der(ext.extn_value.as_bytes()) {
                return bc.ca;
            }
        }
    }
    false
}

fn names_match(a: &Name, b: &Name) -> bool {
    match (a.to_der(), b.to_der()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Throwaway test PKI (scripts/gen-workshop-pki.sh) — TEST FIXTURES ONLY.
    const CA_CRT: &str = "-----BEGIN CERTIFICATE-----
MIIBmDCCAT+gAwIBAgIUR/uEFGfa9NGFkbrEUkS+yhmZka8wCgYIKoZIzj0EAwIw
GjEYMBYGA1UEAwwPT0VNLVdvcmtzaG9wLUNBMB4XDTI2MDYwMzE0MzEwM1oXDTM2
MDUzMTE0MzEwM1owGjEYMBYGA1UEAwwPT0VNLVdvcmtzaG9wLUNBMFkwEwYHKoZI
zj0CAQYIKoZIzj0DAQcDQgAEAKjEU7eMdLg6G3Z8NC7GNhtD4ay4PEAx++UgFEVj
hVMM804c2F8m3F28HEAukT8xLCCEuMzQWbqlwIRcHw6k8aNjMGEwHQYDVR0OBBYE
FDO7tH5nczIUeFDzRYUrB8o8OGVsMB8GA1UdIwQYMBaAFDO7tH5nczIUeFDzRYUr
B8o8OGVsMA8GA1UdEwEB/wQFMAMBAf8wDgYDVR0PAQH/BAQDAgEGMAoGCCqGSM49
BAMCA0cAMEQCIDVoAfMQTozw/RSGz7AOgMyIH+s3rTmw1qKIeF5gRmujAiADskIj
ArkieB9O4IfD4xdyhsCXuSRAe4Szf42pVIbfLw==
-----END CERTIFICATE-----
";
    const INT_CRT: &str = "-----BEGIN CERTIFICATE-----
MIIBmTCCAT+gAwIBAgIUA07s6iSRDhI4refSVvo8NnJwrfUwCgYIKoZIzj0EAwIw
GjEYMBYGA1UEAwwPT0VNLVdvcmtzaG9wLUNBMB4XDTI2MDYwMzE0MzEwNFoXDTMx
MDYwMjE0MzEwNFowGjEYMBYGA1UEAwwPUmVnaW9uLUVVLVN1YkNBMFkwEwYHKoZI
zj0CAQYIKoZIzj0DAQcDQgAEw5NWUViXwxeO1NEuiZMQJxTayZxMkBFR7ZwAk4x3
AJb8nFEopboFGtr4VD2/4NO9CGyY6gg4fBfGsx62Q5nbcKNjMGEwDwYDVR0TAQH/
BAUwAwEB/zAOBgNVHQ8BAf8EBAMCAQYwHQYDVR0OBBYEFLWth65ht3G/qdApRdls
VQOjGIfDMB8GA1UdIwQYMBaAFDO7tH5nczIUeFDzRYUrB8o8OGVsMAoGCCqGSM49
BAMCA0gAMEUCIQDqqoJLopLrgj50KszzJinNN2ExYEvDFTQaMxu18WovTgIgE5T0
QKOsCi7I7QyUBCUbBKZYmS2yjJnuk7RO40aKwq0=
-----END CERTIFICATE-----
";
    const LEAF_CRT: &str = "-----BEGIN CERTIFICATE-----
MIIBlDCCATugAwIBAgIUfVbqOs0W/+MMymiqpYwV+bNgLYgwCgYIKoZIzj0EAwIw
GjEYMBYGA1UEAwwPUmVnaW9uLUVVLVN1YkNBMB4XDTI2MDYwMzE0MzEwNFoXDTI3
MDYwMzE0MzEwNFowGTEXMBUGA1UEAwwOV29ya3Nob3AtQmF5LTcwWTATBgcqhkjO
PQIBBggqhkjOPQMBBwNCAAR49pTZHSd+ggE7+KJOuWYW2OfSOLyLcAwP8JERhQ6j
pQRX5N3dx6ydnCpWxjqrU2afQhNDj1tN7V/GaL9j9f3po2AwXjAMBgNVHRMBAf8E
AjAAMA4GA1UdDwEB/wQEAwIHgDAdBgNVHQ4EFgQUZjZdhdHkZB4D58vvS0AQMKt+
W38wHwYDVR0jBBgwFoAUta2HrmG3cb+p0ClF2WxVA6MYh8MwCgYIKoZIzj0EAwID
RwAwRAIgZfMsu0h0kvWWaSL5yfXAx9L7WKZdm0j1AlY9i3/emP8CIEwXr76+Iz9Y
6J+wSkgfsnmUGQdz0v+68CgW9dTFvLpH
-----END CERTIFICATE-----
";
    const LEAF_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg0f0shY0eYUdamL01
lY+KDWz0y9nKYHs7KwplnY+T752hRANCAAR49pTZHSd+ggE7+KJOuWYW2OfSOLyL
cAwP8JERhQ6jpQRX5N3dx6ydnCpWxjqrU2afQhNDj1tN7V/GaL9j9f3p
-----END PRIVATE KEY-----
";
    const ROGUE_CRT: &str = "-----BEGIN CERTIFICATE-----
MIIBgDCCASagAwIBAgIUcNz5vpW+1u03/U9yO03hbwth1AQwCgYIKoZIzj0EAwIw
FzEVMBMGA1UEAwwMUm9ndWUtTWludGVyMB4XDTI2MDYwMzE0MzEwNFoXDTI3MDYw
MzE0MzEwNFowFzEVMBMGA1UEAwwMUm9ndWUtTWludGVyMFkwEwYHKoZIzj0CAQYI
KoZIzj0DAQcDQgAEn0cSgVUKXPcDYMa80D2yGtTWwLHclZCGWZoAQpnGvhWKTAaT
n3a5IxLp4M5O4sRPUfcpBSmdSYEAHsleLPCwWaNQME4wHQYDVR0OBBYEFPZ8wEEI
c8G2OU3zUy4kmhtklliiMB8GA1UdIwQYMBaAFPZ8wEEIc8G2OU3zUy4kmhtkllii
MAwGA1UdEwEB/wQCMAAwCgYIKoZIzj0EAwIDSAAwRQIhAPgntdVkuCNLxeG1+40n
h0wCUhMOImkq1nNzFEPNXfWrAiAAxreze5OMqvGPAW8Wc21iYRMs0NjrJyIDR405
RD3yZQ==
-----END CERTIFICATE-----
";
    const ROGUE_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgSlKD+FLcdCcqOAwR
DtlpPmTYwcMYAeJ1uZER8vQ+YwqhRANCAASfRxKBVQpc9wNgxrzQPbIa1NbAsdyV
kIZZmgBCmca+FYpMBpOfdrkjEungzk7ixE9R9ykFKZ1JgQAeyV4s8LBZ
-----END PRIVATE KEY-----
";

    fn x5c(pems: &[&str]) -> Vec<String> {
        let std_b64 = base64::engine::general_purpose::STANDARD;
        pems.iter()
            .map(|p| {
                let der = Certificate::load_pem_chain(p.as_bytes()).unwrap()[0]
                    .to_der()
                    .unwrap();
                std_b64.encode(der)
            })
            .collect()
    }

    /// Mint a test JWT exactly as the minter does (ES256 + x5c header).
    fn mint(key_pem: &str, x5c_chain: Vec<String>, aud: &str, scope: &str, ttl: i64) -> String {
        use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
        #[derive(serde::Serialize)]
        struct C<'a> {
            sub: &'a str,
            aud: &'a str,
            exp: i64,
            scope: &'a str,
        }
        let now = chrono::Utc::now().timestamp();
        let claims = C {
            sub: "tech-1",
            aud,
            exp: now + ttl,
            scope,
        };
        let mut header = Header::new(Algorithm::ES256);
        if !x5c_chain.is_empty() {
            header.x5c = Some(x5c_chain);
        }
        encode(
            &header,
            &claims,
            &EncodingKey::from_ec_pem(key_pem.as_bytes()).unwrap(),
        )
        .unwrap()
    }

    fn validator() -> WorkshopCaValidator {
        WorkshopCaValidator::from_pem(CA_CRT, "vin:1HGBH41JXMN109186").unwrap()
    }

    #[test]
    fn valid_chain_via_intermediate_accepts() {
        let token = mint(
            LEAF_KEY,
            x5c(&[LEAF_CRT, INT_CRT]),
            "vin:1HGBH41JXMN109186",
            "component:engine_ecu component:trans",
            300,
        );
        let (sub, scopes) = validator().validate(&token).expect("valid workshop token");
        assert_eq!(sub, "tech-1");
        assert!(scopes.contains(&"component:engine_ecu".to_string()));
    }

    #[test]
    fn rogue_chain_rejected() {
        // Signed by a self-signed key that does NOT chain to the pinned CA.
        let token = mint(
            ROGUE_KEY,
            x5c(&[ROGUE_CRT]),
            "vin:1HGBH41JXMN109186",
            "component:engine_ecu",
            300,
        );
        let err = validator().validate(&token).unwrap_err();
        assert!(err.contains("pinned workshop CA"), "got: {err}");
    }

    #[test]
    fn wrong_device_audience_rejected() {
        let token = mint(
            LEAF_KEY,
            x5c(&[LEAF_CRT, INT_CRT]),
            "vin:SOME-OTHER-CAR",
            "component:engine_ecu",
            300,
        );
        let err = validator().validate(&token).unwrap_err();
        assert!(err.contains("audience"), "got: {err}");
    }

    #[test]
    fn expired_token_rejected() {
        let token = mint(
            LEAF_KEY,
            x5c(&[LEAF_CRT, INT_CRT]),
            "vin:1HGBH41JXMN109186",
            "component:engine_ecu",
            -10, // already expired
        );
        let err = validator().validate(&token).unwrap_err();
        assert!(err.contains("expired"), "got: {err}");
    }

    #[test]
    fn expired_leaf_cert_rejected() {
        // Validate far in the future, past the leaf cert's 1-year validity.
        let token = mint(
            LEAF_KEY,
            x5c(&[LEAF_CRT, INT_CRT]),
            "vin:1HGBH41JXMN109186",
            "component:engine_ecu",
            300,
        );
        let future = chrono::Utc::now().timestamp() + 400 * 86_400;
        let err = validator().validate_at(&token, future).unwrap_err();
        assert!(err.contains("expired"), "got: {err}");
    }

    #[test]
    fn missing_x5c_rejected() {
        let token = mint(
            LEAF_KEY,
            Vec::new(), // no x5c header
            "vin:1HGBH41JXMN109186",
            "component:engine_ecu",
            300,
        );
        let err = validator().validate(&token).unwrap_err();
        assert!(err.contains("x5c"), "got: {err}");
    }

    #[test]
    fn overlong_x5c_rejected() {
        let one = x5c(&[LEAF_CRT]).remove(0);
        let bloated = vec![one; MAX_X5C_LEN + 1];
        let token = mint(
            LEAF_KEY,
            bloated,
            "vin:1HGBH41JXMN109186",
            "component:engine_ecu",
            300,
        );
        let err = validator().validate(&token).unwrap_err();
        assert!(err.contains("too long"), "got: {err}");
    }

    #[test]
    fn non_es256_alg_rejected() {
        use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
        #[derive(serde::Serialize)]
        struct C {
            sub: &'static str,
            aud: &'static str,
            exp: i64,
        }
        let mut header = Header::new(Algorithm::HS256);
        header.x5c = Some(x5c(&[LEAF_CRT, INT_CRT]));
        let claims = C {
            sub: "x",
            aud: "vin:1HGBH41JXMN109186",
            exp: chrono::Utc::now().timestamp() + 300,
        };
        let token = encode(&header, &claims, &EncodingKey::from_secret(b"secret")).unwrap();
        let err = validator().validate(&token).unwrap_err();
        assert!(err.contains("alg") || err.contains("ES256"), "got: {err}");
    }
}
