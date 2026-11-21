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
//!   `accessurl=<scheme>://<host>:<port>/v1` (the SOVD API base — the parent
//!   of the `version-info` resource);
//! - SRV port = the SOVD server's actual bind port (a parameter, never
//!   hardcoded).
//!
//! The server is advertised whether it is provisioned or not: the `accessurl`
//! scheme ([`Scheme`]) tells the client which transport to dial — `http` before
//! enrolment, `https` once a TLS leaf is installed.
//!
//! The two identity roles are resolved separately from the [`Identity`]
//! sources the caller passes (see [`SovdAdvertiser::new`]):
//! - instance / hostname — what TLS clients verify — come from the TLS leaf
//!   (see [`instance_from_cert_der`]) when there is one, else the configured
//!   server identification, else the OS hostname. TLS truth comes first: the
//!   tower issues role-prefixed SANs (e.g. `cvc-host-<hash>`) that a device
//!   cannot know before enrolment, so the hostname may change at the
//!   http→https switch, but on https it always matches the certificate;
//! - TXT `identification` — the stable id of the SOVD server — is the
//!   configured identification (verbatim) when set, else the instance, so it
//!   does not change across enrolment.
//!
//! ## Usage
//! ```no_run
//! # fn leaf_der() -> Vec<u8> { Vec::new() }
//! use sovd_mdns::{Identity, Scheme, SovdAdvertiser};
//! // Hostname from the leaf (else `cvc`, else the OS hostname); identification `cvc`:
//! let der = leaf_der();
//! let ids = [Identity::Configured("cvc"), Identity::Leaf(&der), Identity::Hostname];
//! if let Some(adv) = SovdAdvertiser::new(&ids, Scheme::Https, "0.0.0.0:4000") {
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
pub const ACCESS_URL_PATH: &str = "/v1";

/// The transport the SOVD server is reachable on — the `accessurl` scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    /// Plain HTTP (unprovisioned / dev, or TLS terminated by a fronting proxy).
    Http,
    /// HTTPS with the provisioned TLS leaf.
    Https,
}

impl fmt::Display for Scheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Scheme::Http => "http",
            Scheme::Https => "https",
        })
    }
}

/// A source for the DNS-SD instance id and TXT `identification`. See
/// [`SovdAdvertiser::new`] for how a list of these is resolved.
#[derive(Debug, Clone)]
pub enum Identity<'a> {
    /// Configured server identification (the device id the tower puts in the
    /// leaf SAN), e.g. `cvc`.
    Configured(&'a str),
    /// Derive from the TLS leaf (DER): `<id>.local` SAN, else CN (see
    /// [`instance_from_cert_der`]).
    Leaf(&'a [u8]),
    /// The OS hostname (`/proc/sys/kernel/hostname`, else `/etc/hostname`).
    Hostname,
}

impl Identity<'_> {
    /// The sanitised instance this source yields, if any.
    fn instance(&self) -> Option<String> {
        let raw = match self {
            Identity::Configured(id) => Some(id.to_string()),
            Identity::Leaf(der) => instance_from_cert_der(der),
            Identity::Hostname => os_hostname(),
        }?;
        dns_label(&raw)
    }
}

/// A request to advertise one SOVD server over mDNS / DNS-SD.
///
/// Build directly, or via [`SovdAdvertiser::new`], which resolves the instance
/// and identification from a list of [`Identity`] sources and derives the
/// conformant fields from them, the transport [`Scheme`] and the server's bind
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
    /// `https://cvc-host-rig1.local:4000/v1`.
    pub accessurl: String,
}

impl SovdAdvertiser {
    /// Register the service. The returned [`AdvertiserGuard`] unregisters (sends
    /// DNS-SD goodbye packets) and stops the background daemon when dropped —
    /// hold it for the SOVD server's lifetime.
    pub fn start(self) -> Result<AdvertiserGuard, AdvertiseError> {
        advertiser::start(self)
    }

    /// Build the conformant advertiser from an ordered list of identity
    /// sources, the transport scheme and the SOVD server's bind address (e.g.
    /// `"0.0.0.0:4000"`).
    ///
    /// Instance / `<instance>.local` hostname: the first `Leaf` that resolves,
    /// else the first `Configured`, else the first `Hostname` — whatever order
    /// the list is in. Each candidate is sanitised to a DNS label (lowercase
    /// `[a-z0-9-]`, no leading/trailing `-`, ≤63 bytes — other characters are
    /// replaced by `-`); one that sanitises to nothing is skipped.
    /// `identification` is the first non-empty `Configured` verbatim, else the
    /// instance. `accessurl` is `<scheme>://<instance>.local:<port>/v1`.
    ///
    /// Returns `None` when no source yields an instance or the bind address has
    /// no port — the caller must then not advertise.
    pub fn new(identities: &[Identity<'_>], scheme: Scheme, bind: &str) -> Option<SovdAdvertiser> {
        let first = |want: fn(&Identity<'_>) -> bool| {
            identities
                .iter()
                .filter(|i| want(i))
                .find_map(Identity::instance)
        };
        let leaf = first(|i| matches!(i, Identity::Leaf(_)));
        let configured = first(|i| matches!(i, Identity::Configured(_)));
        if let (Some(leaf), Some(configured)) = (&leaf, &configured) {
            if leaf != configured {
                tracing::info!(
                    leaf = %leaf,
                    configured = %configured,
                    "SOVD mDNS: TLS leaf identity differs from configured identification; \
                     hostname follows the leaf"
                );
            }
        }
        let instance = leaf
            .or(configured)
            .or_else(|| first(|i| matches!(i, Identity::Hostname)))?;
        let port = port_from_bind(bind)?;
        let host = format!("{instance}.local");
        // ISO 17978-3 §5.11.4 wants a *unique id of the SOVD server* here,
        // "such as the VIN": the configured identification when set (stable
        // across enrolment), else the device id the hostname carries.
        let identification = identities
            .iter()
            .find_map(|i| match i {
                Identity::Configured(id) if !id.is_empty() => Some(id.to_string()),
                _ => None,
            })
            .unwrap_or_else(|| instance.clone());
        Some(SovdAdvertiser {
            identification,
            accessurl: format!("{scheme}://{host}:{port}{ACCESS_URL_PATH}"),
            instance,
            port,
        })
    }

    /// Build the HTTPS advertiser from the TLS leaf certificate (DER) alone —
    /// `new(&[Identity::Leaf(leaf_der)], Scheme::Https, bind)`.
    pub fn from_leaf_and_bind(leaf_der: &[u8], bind: &str) -> Option<SovdAdvertiser> {
        Self::new(&[Identity::Leaf(leaf_der)], Scheme::Https, bind)
    }
}

/// The OS hostname, trimmed: `/proc/sys/kernel/hostname`, else `/etc/hostname`.
fn os_hostname() -> Option<String> {
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty())
}

/// Sanitise `raw` into a DNS label: lowercase, every character outside
/// `[a-z0-9-]` replaced by `-`, leading/trailing `-` trimmed, at most 63 bytes.
/// `None` if nothing is left.
fn dns_label(raw: &str) -> Option<String> {
    let mapped: String = raw
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9' | '-') => c,
            _ => '-',
        })
        .collect();
    let trimmed = mapped.trim_matches('-');
    // All ASCII now, so byte truncation is char-safe.
    let label = trimmed[..trimmed.len().min(63)].trim_end_matches('-');
    (!label.is_empty()).then(|| label.to_string())
}

/// Derive the DNS-SD instance id from a TLS leaf certificate (DER).
///
/// Prefers the `<id>.local` `subjectAltName` dNSName (stripping the `.local`
/// suffix): that is exactly the hostname a discovery client dials and verifies
/// against the certificate. Falls back to the subject CommonName (the
/// identity-tower CA sets both `CN=<device_id>` and the `<device_id>.local`
/// SAN). Returns `None` if neither is present / parseable.
pub fn instance_from_cert_der(der: &[u8]) -> Option<String> {
    use const_oid::db::rfc4519::COMMON_NAME;
    use x509_cert::der::Decode;
    use x509_cert::ext::pkix::{name::GeneralName, SubjectAltName};
    use x509_cert::Certificate;

    let cert = Certificate::from_der(der).ok()?;

    // 1) Prefer the `<id>.local` SAN dNSName (what TLS clients verify).
    if let Ok(Some((_critical, san))) = cert.tbs_certificate.get::<SubjectAltName>() {
        for name in san.0.iter() {
            if let GeneralName::DnsName(dns) = name {
                if let Some(id) = dns.as_str().strip_suffix(".local") {
                    if !id.is_empty() {
                        return Some(id.to_string());
                    }
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
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
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

    #[test]
    fn instance_parsed_from_local_san() {
        // The `.local` SAN is preferred and the `.local` suffix is stripped.
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
        assert_eq!(adv.accessurl, "https://cvc-host-rig1.local:4000/v1");
    }

    #[test]
    fn from_leaf_and_bind_honours_the_actual_port() {
        // The SRV port follows the bind addr — never a hardcoded default.
        let adv = SovdAdvertiser::from_leaf_and_bind(LEAF_DER, "127.0.0.1:8443").unwrap();
        assert_eq!(adv.port, 8443);
        assert_eq!(adv.accessurl, "https://cvc-host-rig1.local:8443/v1");
    }

    #[test]
    fn unprovisioned_or_portless_does_not_advertise() {
        assert!(SovdAdvertiser::from_leaf_and_bind(b"junk", "0.0.0.0:4000").is_none());
        assert!(SovdAdvertiser::from_leaf_and_bind(LEAF_DER, "0.0.0.0").is_none());
    }

    #[test]
    fn new_sets_the_accessurl_scheme_from_the_transport() {
        let ids = [Identity::Configured("cvc")];
        let http = SovdAdvertiser::new(&ids, Scheme::Http, "0.0.0.0:9080").unwrap();
        assert_eq!(http.accessurl, "http://cvc.local:9080/v1");
        let https = SovdAdvertiser::new(&ids, Scheme::Https, "0.0.0.0:4000").unwrap();
        assert_eq!(https.accessurl, "https://cvc.local:4000/v1");
        assert_eq!(https.instance, "cvc");
        assert_eq!(https.identification, "cvc");
    }

    fn adv(ids: &[Identity<'_>]) -> Option<SovdAdvertiser> {
        SovdAdvertiser::new(ids, Scheme::Https, "0.0.0.0:4000")
    }

    #[test]
    fn configured_and_leaf_hostname_from_leaf_identification_from_config() {
        // Whatever the list order, the hostname follows the TLS leaf.
        let ids = [
            Identity::Configured("cvc"),
            Identity::Leaf(LEAF_DER),
            Identity::Hostname,
        ];
        let a = adv(&ids).unwrap();
        assert_eq!(a.instance, "cvc-host-rig1");
        assert_eq!(a.identification, "cvc");
        assert_eq!(a.accessurl, "https://cvc-host-rig1.local:4000/v1");
    }

    #[test]
    fn leaf_only() {
        let a = adv(&[Identity::Leaf(LEAF_DER), Identity::Hostname]).unwrap();
        assert_eq!(a.instance, "cvc-host-rig1");
        assert_eq!(a.identification, "cvc-host-rig1");
    }

    #[test]
    fn configured_only() {
        // Unenrolled: no (usable) leaf, so the configured id is the hostname.
        let a = adv(&[
            Identity::Configured("CVC_1"),
            Identity::Leaf(b"junk"),
            Identity::Hostname,
        ])
        .unwrap();
        assert_eq!(a.instance, "cvc-1");
        assert_eq!(a.identification, "CVC_1", "identification is verbatim");
    }

    #[test]
    fn hostname_only() {
        // An unusable configured id falls through to the OS hostname for the
        // instance, but is still the (verbatim) identification.
        let expected = os_hostname().as_deref().and_then(dns_label);
        let a = adv(&[Identity::Hostname]);
        assert_eq!(a.as_ref().map(|a| a.instance.clone()), expected);
        assert_eq!(a.map(|a| a.identification), expected);
        let a = adv(&[Identity::Configured("--"), Identity::Hostname]);
        assert_eq!(a.as_ref().map(|a| a.instance.clone()), expected);
    }

    #[test]
    fn dns_label_sanitises_or_rejects() {
        assert_eq!(dns_label("CVC").as_deref(), Some("cvc"));
        assert_eq!(dns_label("cvc_host.rig1").as_deref(), Some("cvc-host-rig1"));
        assert_eq!(dns_label("-_cvc_-").as_deref(), Some("cvc"));
        assert_eq!(dns_label(&"a".repeat(70)).map(|s| s.len()), Some(63));
        assert_eq!(dns_label(""), None);
        assert_eq!(dns_label("___"), None);
        assert!(
            SovdAdvertiser::new(&[Identity::Configured("")], Scheme::Http, "0.0.0.0:1").is_none()
        );
        assert!(SovdAdvertiser::new(&[], Scheme::Http, "0.0.0.0:1").is_none());
    }

    #[test]
    fn new_without_a_port_does_not_advertise() {
        assert!(
            SovdAdvertiser::new(&[Identity::Configured("cvc")], Scheme::Http, "0.0.0.0").is_none()
        );
    }

    #[test]
    fn port_from_bind_parses_v4_v6_and_bare() {
        assert_eq!(port_from_bind("0.0.0.0:4000"), Some(4000));
        assert_eq!(port_from_bind("[::]:4000"), Some(4000));
        assert_eq!(port_from_bind("0.0.0.0"), None);
    }
}
