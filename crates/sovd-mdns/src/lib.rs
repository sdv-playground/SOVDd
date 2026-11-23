//! Conformant mDNS / DNS-SD advertiser for the SOVD server (ISO 17978-3 §5.11).
//!
//! Advertises the host SOVD endpoint so a SOVD client can discover it on the
//! local link via DNS-SD, closing ISO 17978-3 conformance items C-010 / C-011.
//!
//! ## Advertised shape (ISO 17978-3 §5.11)
//! - service type `_sovd._tcp` in domain `local.` (see [`SERVICE_TYPE`]);
//! - instance name = the SOVD-server id — we use the role-prefixed device id,
//!   e.g. `hpc-host-rig1`;
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
//!   tower issues role-prefixed SANs (e.g. `hpc-host-<hash>`) that a device
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
//! // Hostname from the leaf (else `hpc`, else the OS hostname); identification `hpc`:
//! let der = leaf_der();
//! let ids = [Identity::Configured("hpc"), Identity::Leaf(&der), Identity::Hostname];
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
//! `bitflags`) and `socket2` (only dep: `libc`, the line the node manager's QNX
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
    /// leaf SAN), e.g. `hpc`.
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
    /// DNS-SD instance name == the SOVD-server id, e.g. `hpc-host-rig1`. The
    /// advertised hostname is `<instance>.local`.
    pub instance: String,
    /// SRV port — the SOVD server's actual bind port.
    pub port: u16,
    /// TXT `identification` — a unique id of the SOVD server (e.g. the device
    /// id or VIN).
    pub identification: String,
    /// TXT `accessurl` — the SOVD API base, e.g.
    /// `https://hpc-host-rig1.local:4000/v1`.
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
/// Prefers the first `<id>.local` `subjectAltName` dNSName (see
/// [`local_host_label`]; returned lowercased): that is exactly the hostname a
/// discovery client dials and verifies against the certificate. Falls back to
/// the subject CommonName (the identity-tower CA sets both `CN=<device_id>` and
/// the `<device_id>.local` SAN). Returns `None` if neither is present /
/// parseable.
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
                if let Some(id) = local_host_label(dns.as_str()) {
                    return Some(id.to_ascii_lowercase());
                }
            }
        }
    }

    // 2) Fall back to the subject CommonName.
    let cn = cert
        .tbs_certificate
        .subject
        .0
        .iter()
        .flat_map(|rdn| rdn.0.iter())
        .find(|atv| atv.oid == COMMON_NAME)
        .and_then(|atv| std::str::from_utf8(atv.value.value()).ok())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    tracing::info!(
        cn = ?cn,
        "SOVD mDNS: TLS leaf has no `<label>.local` dNSName SAN (service names, wildcards, \
         multi-label and non-.local names are skipped); using the subject CN, else the next \
         identity source"
    );
    cn
}

/// `<label>.local` (ASCII case-insensitive) → `<label>`, when `<label>` is one
/// plain host label: non-empty, no `.`, no `*`, not `_`-prefixed. DNS-SD service
/// names (`<id>._sovd._tcp.local`), wildcards (`*.local`), multi-label hosts and
/// names outside `.local` yield `None`.
fn local_host_label(name: &str) -> Option<&str> {
    let split = name.len().checked_sub(".local".len())?;
    let (label, suffix) = (name.get(..split)?, name.get(split..)?);
    (suffix.eq_ignore_ascii_case(".local")
        && !label.is_empty()
        && !label.starts_with('_')
        && !label.contains(['.', '*']))
    .then_some(label)
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

    // A leaf mirroring the identity-tower CA shape: CN=hpc-host-rig1 with SAN
    // dNSNames [hpc-host-rig1, hpc-host-rig1.local] (see
    // sumo-provision/crates/identity-tower/src/ca.rs).
    const LEAF_DER: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/leaf-hpc-host-rig1.der"
    ));

    #[test]
    fn instance_parsed_from_local_san() {
        // The `.local` SAN is preferred and the `.local` suffix is stripped.
        assert_eq!(
            instance_from_cert_der(LEAF_DER).as_deref(),
            Some("hpc-host-rig1")
        );
    }

    // Leaves with no CN (subject `O=sovd-mdns test`), so the instance can only
    // come from a SAN. Generated with OpenSSL 3.5 as
    //   openssl req -x509 -newkey ed25519 -nodes -keyout /dev/null \
    //     -subj "/O=sovd-mdns test" -days 36500 \
    //     -addext "subjectAltName=<SANs>" -outform DER -out <file>.der
    // with <SANs> as noted on each fixture.
    macro_rules! fixture {
        ($file:literal) => {
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/",
                $file
            ))
        };
    }
    // DNS:hpc-host-4c6ff3d8a60d6655._sovd._tcp.local,DNS:hpc-host-4c6ff3d8a60d6655.local
    const SERVICE_SAN_FIRST_DER: &[u8] = fixture!("leaf-service-san-first.der");
    // DNS:hpc-host-4c6ff3d8a60d6655._sovd._tcp.local
    const SERVICE_SAN_ONLY_DER: &[u8] = fixture!("leaf-service-san-only.der");
    // DNS:*.local
    const WILDCARD_ONLY_DER: &[u8] = fixture!("leaf-wildcard-only.der");
    // DNS:HPC-HOST-4C6FF3D8A60D6655.LOCAL
    const UPPERCASE_SAN_DER: &[u8] = fixture!("leaf-uppercase-san.der");
    // IP:192.0.2.1,DNS:host.example.com,DNS:hpc.rig1.local,DNS:hpc-host-4c6ff3d8a60d6655.local
    const IP_AND_FOREIGN_SANS_DER: &[u8] = fixture!("leaf-ip-and-foreign-sans.der");

    #[test]
    fn service_name_san_is_skipped_for_the_host_san() {
        assert_eq!(
            instance_from_cert_der(SERVICE_SAN_FIRST_DER).as_deref(),
            Some("hpc-host-4c6ff3d8a60d6655")
        );
    }

    #[test]
    fn wildcard_only_leaf_yields_no_instance() {
        assert_eq!(instance_from_cert_der(WILDCARD_ONLY_DER), None);
    }

    #[test]
    fn service_name_only_leaf_yields_no_instance() {
        assert_eq!(instance_from_cert_der(SERVICE_SAN_ONLY_DER), None);
    }

    #[test]
    fn uppercase_local_san_is_lowercased() {
        assert_eq!(
            instance_from_cert_der(UPPERCASE_SAN_DER).as_deref(),
            Some("hpc-host-4c6ff3d8a60d6655")
        );
    }

    #[test]
    fn ip_foreign_and_multi_label_sans_are_skipped() {
        assert_eq!(
            instance_from_cert_der(IP_AND_FOREIGN_SANS_DER).as_deref(),
            Some("hpc-host-4c6ff3d8a60d6655")
        );
    }

    #[test]
    fn local_host_label_accepts_only_single_label_local_hosts() {
        for (name, label) in [
            ("hpc-host-rig1.local", Some("hpc-host-rig1")),
            ("ID.LOCAL", Some("ID")),
            ("hpc-host-rig1", None),
            ("x._sovd._tcp.local", None),
            ("_x.local", None),
            ("*.local", None),
            ("a.b.local", None),
            ("host.example.com", None),
            ("hpc.local.", None),
            (".local", None),
            ("local", None),
            ("", None),
        ] {
            assert_eq!(local_host_label(name), label, "{name:?}");
        }
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
        assert_eq!(adv.instance, "hpc-host-rig1");
        assert_eq!(adv.port, 4000);
        assert_eq!(adv.identification, "hpc-host-rig1");
        assert_eq!(adv.accessurl, "https://hpc-host-rig1.local:4000/v1");
    }

    #[test]
    fn from_leaf_and_bind_honours_the_actual_port() {
        // The SRV port follows the bind addr — never a hardcoded default.
        let adv = SovdAdvertiser::from_leaf_and_bind(LEAF_DER, "127.0.0.1:8443").unwrap();
        assert_eq!(adv.port, 8443);
        assert_eq!(adv.accessurl, "https://hpc-host-rig1.local:8443/v1");
    }

    #[test]
    fn unprovisioned_or_portless_does_not_advertise() {
        assert!(SovdAdvertiser::from_leaf_and_bind(b"junk", "0.0.0.0:4000").is_none());
        assert!(SovdAdvertiser::from_leaf_and_bind(LEAF_DER, "0.0.0.0").is_none());
    }

    #[test]
    fn new_sets_the_accessurl_scheme_from_the_transport() {
        let ids = [Identity::Configured("hpc")];
        let http = SovdAdvertiser::new(&ids, Scheme::Http, "0.0.0.0:9080").unwrap();
        assert_eq!(http.accessurl, "http://hpc.local:9080/v1");
        let https = SovdAdvertiser::new(&ids, Scheme::Https, "0.0.0.0:4000").unwrap();
        assert_eq!(https.accessurl, "https://hpc.local:4000/v1");
        assert_eq!(https.instance, "hpc");
        assert_eq!(https.identification, "hpc");
    }

    fn adv(ids: &[Identity<'_>]) -> Option<SovdAdvertiser> {
        SovdAdvertiser::new(ids, Scheme::Https, "0.0.0.0:4000")
    }

    #[test]
    fn configured_and_leaf_hostname_from_leaf_identification_from_config() {
        // Whatever the list order, the hostname follows the TLS leaf.
        let ids = [
            Identity::Configured("hpc"),
            Identity::Leaf(LEAF_DER),
            Identity::Hostname,
        ];
        let a = adv(&ids).unwrap();
        assert_eq!(a.instance, "hpc-host-rig1");
        assert_eq!(a.identification, "hpc");
        assert_eq!(a.accessurl, "https://hpc-host-rig1.local:4000/v1");
    }

    #[test]
    fn leaf_without_a_usable_san_falls_through_to_configured() {
        for der in [WILDCARD_ONLY_DER, SERVICE_SAN_ONLY_DER] {
            let a = adv(&[Identity::Leaf(der), Identity::Configured("x")]).unwrap();
            assert_eq!(a.instance, "x");
            assert_eq!(a.identification, "x");
            assert_eq!(a.accessurl, "https://x.local:4000/v1");
        }
        // A usable SAN behind a service-name SAN still wins over (and differs
        // from) the configured id: the "leaf != configured" path.
        let a = adv(&[
            Identity::Leaf(SERVICE_SAN_FIRST_DER),
            Identity::Configured("x"),
        ])
        .unwrap();
        assert_eq!(a.instance, "hpc-host-4c6ff3d8a60d6655");
        assert_eq!(a.identification, "x");
        // No configured id: on to the OS hostname.
        let expected = os_hostname().as_deref().and_then(dns_label);
        let a = adv(&[Identity::Leaf(WILDCARD_ONLY_DER), Identity::Hostname]);
        assert_eq!(a.map(|a| a.instance), expected);
    }

    #[test]
    fn leaf_only() {
        let a = adv(&[Identity::Leaf(LEAF_DER), Identity::Hostname]).unwrap();
        assert_eq!(a.instance, "hpc-host-rig1");
        assert_eq!(a.identification, "hpc-host-rig1");
    }

    #[test]
    fn configured_only() {
        // Unenrolled: no (usable) leaf, so the configured id is the hostname.
        let a = adv(&[
            Identity::Configured("HPC_1"),
            Identity::Leaf(b"junk"),
            Identity::Hostname,
        ])
        .unwrap();
        assert_eq!(a.instance, "hpc-1");
        assert_eq!(a.identification, "HPC_1", "identification is verbatim");
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
        assert_eq!(dns_label("HPC").as_deref(), Some("hpc"));
        assert_eq!(dns_label("hpc_host.rig1").as_deref(), Some("hpc-host-rig1"));
        assert_eq!(dns_label("-_hpc_-").as_deref(), Some("hpc"));
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
            SovdAdvertiser::new(&[Identity::Configured("hpc")], Scheme::Http, "0.0.0.0").is_none()
        );
    }

    #[test]
    fn port_from_bind_parses_v4_v6_and_bare() {
        assert_eq!(port_from_bind("0.0.0.0:4000"), Some(4000));
        assert_eq!(port_from_bind("[::]:4000"), Some(4000));
        assert_eq!(port_from_bind("0.0.0.0"), None);
    }
}
