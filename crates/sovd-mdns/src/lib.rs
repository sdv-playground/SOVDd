//! Conformant mDNS / DNS-SD advertiser for the SOVD server (ISO 17978-3 §5.11).
//!
//! Advertises the host SOVD endpoint so a SOVD client can discover it on the
//! local link via DNS-SD, closing ISO 17978-3 conformance items C-010 / C-011.
//!
//! ## Advertised shape (ISO 17978-3 §5.11)
//! - service type `_sovd._tcp` in domain `local.` (see [`SERVICE_TYPE`]);
//! - instance name = the SOVD-server id — we use the role-prefixed device id,
//!   e.g. `cvc-host-rig1`;
//! - hostname = `<instance>.local`. This MUST equal the `tls-identity` leaf's
//!   `.local` `subjectAltName`, so a discovery client that dials
//!   `<instance>.local` verifies the server certificate with ordinary hostname
//!   checking;
//! - TXT records `identification=<unique id>` and
//!   `accessurl=https://<host>:<port>/vehicle` (the SOVD API base — the parent
//!   of the `version-info` resource);
//! - SRV port = the SOVD server's actual bind port (a parameter, never
//!   hardcoded).
//!
//! The instance id is derived from the TLS leaf certificate (see
//! [`instance_from_cert_der`]) so the advertised hostname is guaranteed to match
//! what TLS clients verify. An unprovisioned device (no leaf) is not a
//! discoverable SOVD server and must not advertise — the integration only calls
//! the advertiser on the provisioned HTTPS path.
//!
//! ## Usage
//! ```no_run
//! # fn leaf_der() -> Vec<u8> { Vec::new() }
//! // On the provisioned HTTPS path, with the TLS leaf DER and the bind addr:
//! if let Some(adv) =
//!     sovd_mdns::SovdAdvertiser::from_leaf_and_bind(&leaf_der(), "0.0.0.0:4000")
//! {
//!     match adv.start() {
//!         Ok(guard) => { /* hold `guard` for the server's lifetime */ }
//!         Err(e) => tracing::warn!(error = %e, "SOVD mDNS advertise failed"),
//!     }
//! }
//! ```
//!
//! ## Portability
//! There are two wire backends behind one identical API:
//! - off-QNX, [`mdns-sd`](https://crates.io/crates/mdns-sd) — a mature pure-Rust
//!   DNS-SD stack ([`advertiser_real`]);
//! - on QNX (`target_os = "nto"`), a hand-rolled pure-Rust responder
//!   ([`responder`]) built on `simple-dns` (DNS wire codec) + `socket2` (UDP).
//!
//! The split exists because `mdns-sd`'s transitive deps (`mio`, `if-addrs`,
//! `socket-pktinfo`) have no `nto` support, whereas `simple-dns` (only dep:
//! `bitflags`) and `socket2` (only dep: `libc`, the line the supernova QNX
//! cross-build already carries) both cross-compile to
//! `aarch64-unknown-nto-qnx710`. The [`responder`] is compiled on every target
//! (so it stays testable on Linux) but is only *selected* as the live backend on
//! `nto`. The cert parse ([`instance_from_cert_der`]) is target-independent.

use std::fmt;

// Hand-rolled pure-Rust DNS-SD responder. The live backend on QNX (`nto`);
// elsewhere it is still compiled and exercised by tests/examples (the public
// API uses `mdns-sd` off-`nto`). `#[doc(hidden)] pub` is a deliberate test seam
// — the stable entry point is `SovdAdvertiser::start`.
#[doc(hidden)]
pub mod responder;

#[cfg_attr(not(target_os = "nto"), path = "advertiser_real.rs")]
#[cfg_attr(target_os = "nto", path = "advertiser_nto.rs")]
mod advertiser;

pub use advertiser::AdvertiserGuard;

/// DNS-SD service type for SOVD servers (ISO 17978-3 §5.11.2), fully qualified
/// with the mDNS domain. The registered service's fullname is
/// `<instance>._sovd._tcp.local.`.
pub const SERVICE_TYPE: &str = "_sovd._tcp.local.";

/// The SOVD API base path appended to the host in the `accessurl` TXT record
/// (ISO 17978-3 §5.11.4 — the parent of the `version-info` resource).
pub const ACCESS_URL_PATH: &str = "/vehicle";

/// A request to advertise one SOVD server over mDNS / DNS-SD.
///
/// Build directly, or via [`SovdAdvertiser::from_leaf_and_bind`], which derives
/// the conformant fields from the TLS leaf certificate plus the server's bind
/// address.
#[derive(Debug, Clone)]
pub struct SovdAdvertiser {
    /// DNS-SD instance name == the SOVD-server id, e.g. `cvc-host-rig1`. The
    /// advertised hostname is `<instance>.local`.
    pub instance: String,
    /// SRV port — the SOVD server's actual bind port.
    pub port: u16,
    /// TXT `identification` — a unique id of the SOVD server (e.g. the device
    /// id or VIN).
    pub identification: String,
    /// TXT `accessurl` — the SOVD API base, e.g.
    /// `https://cvc-host-rig1.local:4000/vehicle`.
    pub accessurl: String,
}

impl SovdAdvertiser {
    /// Register the service. The returned [`AdvertiserGuard`] unregisters (sends
    /// DNS-SD goodbye packets) and stops the background daemon when dropped —
    /// hold it for the SOVD server's lifetime.
    pub fn start(self) -> Result<AdvertiserGuard, AdvertiseError> {
        advertiser::start(self)
    }

    /// Build the conformant advertiser from the TLS leaf certificate (DER) and
    /// the SOVD server's bind address (e.g. `"0.0.0.0:4000"`).
    ///
    /// The instance / hostname come from the certificate (see
    /// [`instance_from_cert_der`]) so the advertised `<instance>.local` matches
    /// the leaf SAN that TLS clients verify. `identification` defaults to the
    /// device id; `accessurl` is `https://<instance>.local:<port>/vehicle`.
    ///
    /// Returns `None` when the certificate carries no parseable instance id (an
    /// unprovisioned / non-conformant leaf) or the bind address has no port —
    /// the caller must then not advertise.
    pub fn from_leaf_and_bind(leaf_der: &[u8], bind: &str) -> Option<SovdAdvertiser> {
        let instance = instance_from_cert_der(leaf_der)?;
        let port = port_from_bind(bind)?;
        let host = format!("{instance}.local");
        Some(SovdAdvertiser {
            // ISO 17978-3 §5.11.4 wants a *unique id of the SOVD server* here,
            // "such as the VIN". We use the device id (the cert identity); a
            // vehicle-level VIN would be equally conformant once one exists.
            identification: instance.clone(),
            accessurl: format!("https://{host}:{port}{ACCESS_URL_PATH}"),
            instance,
            port,
        })
    }
}

/// Derive the DNS-SD instance id from a TLS leaf certificate (DER).
///
/// The host label of the first `subjectAltName` dNSName of a shape the minters
/// name devices with — `<id>`, `<id>.local` or `<id>._sovd._tcp.local`, `<id>`
/// one DNS label of letters, digits and `-` — the hostname a discovery client
/// dials and verifies against the certificate. drive-security lists
/// `<id>._sovd._tcp.local` first, the identity-tower CA a bare `<id>`. Other
/// shapes (wildcards, other domains) never name the node. Falls back to the
/// subject CommonName if it is such a label. Returns `None` otherwise.
pub fn instance_from_cert_der(der: &[u8]) -> Option<String> {
    use const_oid::db::rfc4519::COMMON_NAME;
    use x509_cert::der::Decode;
    use x509_cert::ext::pkix::{name::GeneralName, SubjectAltName};
    use x509_cert::Certificate;

    let cert = Certificate::from_der(der).ok()?;

    // 1) The host label of the first device-shaped SAN dNSName (what TLS clients verify).
    if let Ok(Some((_critical, san))) = cert.tbs_certificate.get::<SubjectAltName>() {
        for name in san.0.iter() {
            if let GeneralName::DnsName(dns) = name {
                if let Some(label) = device_label(dns.as_str()) {
                    return Some(label.to_string());
                }
            }
        }
    }

    // 2) Fall back to the subject CommonName.
    cert.tbs_certificate
        .subject
        .0
        .iter()
        .flat_map(|rdn| rdn.0.iter())
        .find(|atv| atv.oid == COMMON_NAME)
        .and_then(|atv| std::str::from_utf8(atv.value.value()).ok())
        .filter(|s| is_dns_label(s))
        .map(|s| s.to_string())
}

/// `<id>`, `<id>.local` or `<id>._sovd._tcp.local` → `<id>`, if `<id>` is a DNS label.
fn device_label(name: &str) -> Option<&str> {
    let (label, suffix) = name.split_at(name.find('.').unwrap_or(name.len()));
    (is_dns_label(label) && matches!(suffix, "" | ".local" | "._sovd._tcp.local")).then_some(label)
}

/// One DNS label: 1–63 ASCII letters, digits and `-`.
fn is_dns_label(s: &str) -> bool {
    (1..=63).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Parse the port from a bind address like `"0.0.0.0:4000"` or `"[::]:4000"`.
fn port_from_bind(bind: &str) -> Option<u16> {
    if let Ok(addr) = bind.parse::<std::net::SocketAddr>() {
        return Some(addr.port());
    }
    bind.rsplit(':').next()?.parse().ok()
}

/// Error starting the advertiser.
#[derive(Debug)]
pub struct AdvertiseError(pub String);

impl fmt::Display for AdvertiseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SOVD mDNS advertise: {}", self.0)
    }
}

impl std::error::Error for AdvertiseError {}

#[cfg(test)]
mod tests {
    use super::*;

    // A leaf mirroring the identity-tower CA shape: CN=cvc-host-rig1 with SAN
    // dNSNames [cvc-host-rig1, cvc-host-rig1.local] (see
    // sumo-provision/crates/identity-tower/src/ca.rs).
    const LEAF_DER: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/leaf-cvc-host-rig1.der"
    ));

    // A drive-security-shaped leaf: CN=4c6ff3d8a60d6655 with SAN dNSNames
    // [4c6ff3d8a60d6655._sovd._tcp.local, 4c6ff3d8a60d6655.local], service SAN first.
    const SERVICE_SAN_FIRST_DER: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/leaf-drive-security-service-san-first.der"
    ));

    #[test]
    fn instance_is_the_host_label_of_a_service_instance_san() {
        assert_eq!(
            instance_from_cert_der(SERVICE_SAN_FIRST_DER).as_deref(),
            Some("4c6ff3d8a60d6655")
        );
    }

    #[test]
    fn only_device_shaped_names_name_the_node() {
        for (name, label) in [
            ("cvc-host-rig1", Some("cvc-host-rig1")),
            ("cvc-host-rig1.local", Some("cvc-host-rig1")),
            (
                "4c6ff3d8a60d6655._sovd._tcp.local",
                Some("4c6ff3d8a60d6655"),
            ),
            ("*.local", None),
            ("cvc.example.com", None),
            ("cvc.local.", None),
            (".local", None),
            ("", None),
            ("under_score.local", None),
            (&"a".repeat(64), None),
        ] {
            assert_eq!(device_label(name), label, "{name:?}");
        }
        assert!(is_dns_label(&"a".repeat(63)));
    }

    #[test]
    fn instance_parsed_from_local_san() {
        // The first SAN's host label.
        assert_eq!(
            instance_from_cert_der(LEAF_DER).as_deref(),
            Some("cvc-host-rig1")
        );
    }

    #[test]
    fn garbage_der_yields_no_instance() {
        assert_eq!(instance_from_cert_der(b"not a certificate"), None);
        assert_eq!(instance_from_cert_der(&[]), None);
    }

    #[test]
    fn from_leaf_and_bind_builds_conformant_fields() {
        let adv = SovdAdvertiser::from_leaf_and_bind(LEAF_DER, "0.0.0.0:4000")
            .expect("leaf has a parseable instance");
        assert_eq!(adv.instance, "cvc-host-rig1");
        assert_eq!(adv.port, 4000);
        assert_eq!(adv.identification, "cvc-host-rig1");
        assert_eq!(adv.accessurl, "https://cvc-host-rig1.local:4000/vehicle");
    }

    #[test]
    fn from_leaf_and_bind_honours_the_actual_port() {
        // The SRV port follows the bind addr — never a hardcoded default.
        let adv = SovdAdvertiser::from_leaf_and_bind(LEAF_DER, "127.0.0.1:8443").unwrap();
        assert_eq!(adv.port, 8443);
        assert_eq!(adv.accessurl, "https://cvc-host-rig1.local:8443/vehicle");
    }

    #[test]
    fn unprovisioned_or_portless_does_not_advertise() {
        assert!(SovdAdvertiser::from_leaf_and_bind(b"junk", "0.0.0.0:4000").is_none());
        assert!(SovdAdvertiser::from_leaf_and_bind(LEAF_DER, "0.0.0.0").is_none());
    }

    #[test]
    fn port_from_bind_parses_v4_v6_and_bare() {
        assert_eq!(port_from_bind("0.0.0.0:4000"), Some(4000));
        assert_eq!(port_from_bind("[::]:4000"), Some(4000));
        assert_eq!(port_from_bind("0.0.0.0"), None);
    }
}
