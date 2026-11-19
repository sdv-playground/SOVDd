//! Hand-rolled, pure-Rust mDNS / DNS-SD responder (RFC 6762 + RFC 6763).
//!
//! This is the SOVD advertiser backend on QNX (`target_os = "nto"`), where the
//! mature [`mdns-sd`](https://crates.io/crates/mdns-sd) stack can't be used: its
//! event loop (`mio`), interface enumeration (`if-addrs`) and `socket-pktinfo`
//! have no `nto` support. Everything here is built on two crates that *do*
//! cross-compile to `aarch64-unknown-nto-qnx710`:
//!
//! - [`simple-dns`] — a pure-Rust DNS wire codec (its only dep is `bitflags`);
//! - [`socket2`] — a thin libc UDP-socket wrapper (its only dep is `libc`, the
//!   same `0.6` line the supernova QNX cross-build already carries).
//!
//! It serves every up, multicast-capable IPv4 interface ([`interfaces`],
//! `getifaddrs`): a host whose guests sit on their own point-to-point links (the
//! CVC: the LAN, plus one /30 per VM) must be found on each of them, at that
//! link's address. Each interface gets its own `A` record, announcements go out
//! of each interface, and a query is answered with the address of the interface
//! whose subnet holds its sender. Where no interface can be listed, the egress
//! address toward the mDNS group serves alone (see [`local_ipv4`]).
//!
//! The module is compiled on *every* target (so it is testable on Linux) but is
//! only *selected* as the live backend on `nto` — see `src/lib.rs`. It is
//! `#[doc(hidden)] pub` so cross-target tests/examples can drive it directly;
//! the stable entry point remains [`SovdAdvertiser::start`](crate::SovdAdvertiser::start).
//!
//! ## What it does (minimal but correct)
//! - On start: send two unsolicited announcements ~1s apart (RFC 6762 §8.3),
//!   each carrying the PTR + SRV + TXT + A record set.
//! - Then serve: for a query naming our service (`_sovd._tcp.local`) reply with
//!   the PTR answer plus SRV/TXT/A as additionals (RFC 6763 §12.1); for a
//!   SRV/TXT/A query on our instance reply with those records. Replies go
//!   unicast when the question sets the QU bit (RFC 6762 §5.4), else multicast.
//! - On guard drop: send a goodbye (the records at TTL 0, RFC 6762 §10.1) and
//!   join the worker thread.
//!
//! ## Corners intentionally cut (not required for a conformant advertiser)
//! - No probing/conflict resolution (RFC 6762 §8.1): the instance name is the
//!   cert-unique device id, so a clash on-link is not expected.
//! - No known-answer suppression (RFC 6762 §7.1) and no response delay/dedup.
//! - The interface a query arrived on is inferred from its sender's subnet, not
//!   read from the packet (`IP_PKTINFO` has no `nto` path).

use crate::{AdvertiseError, SovdAdvertiser};
use simple_dns::rdata::{RData, A, PTR, SRV, TXT};
use simple_dns::{Name, Packet, PacketFlag, ResourceRecord, CLASS, QTYPE, TYPE};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::mem::MaybeUninit;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Standard mDNS UDP port (RFC 6762 §2).
const MDNS_PORT: u16 = 5353;
/// The IPv4 link-local mDNS multicast group (RFC 6762 §3).
const MDNS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
/// TTL for the host's own (unique) records — A, SRV (RFC 6762 §10).
const TTL_HOST: u32 = 120;
/// TTL for shared / descriptive records — PTR, TXT (RFC 6762 §10).
const TTL_SHARED: u32 = 4500;
/// Read-timeout granularity for the serve loop, so it can observe the stop flag
/// (and emit the goodbye) promptly on drop.
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Receive buffer — one link MTU comfortably holds an mDNS query.
const RX_BUF: usize = 1500;
/// How often the interface list is read again: a VM's link comes up only when
/// the VM starts, after the SOVD server.
const RESCAN_INTERVAL: Duration = Duration::from_secs(10);

/// The conformant SOVD record set, plus the lower-cased names we match queries
/// against (DNS names compare case-insensitively; `simple-dns`'s own `Name`
/// equality is case-sensitive, so we normalise here).
struct Records {
    /// `_sovd._tcp.local` — the DNS-SD service type (no trailing dot).
    service: String,
    /// `<instance>._sovd._tcp.local` — the service instance's fullname.
    instance_fqdn: String,
    /// `<instance>.local` — the SRV target / A-record host.
    host: String,
    /// `identification=<id>` TXT entry.
    txt_identification: String,
    /// `accessurl=https://<host>:<port>/vehicle` TXT entry.
    txt_accessurl: String,
    /// SRV port = the SOVD server's bind port.
    port: u16,
    /// The A-record address (this host's egress IPv4).
    addr: Ipv4Addr,
    service_lc: String,
    instance_lc: String,
    host_lc: String,
}

impl Records {
    fn new(adv: &SovdAdvertiser, addr: Ipv4Addr) -> Self {
        // `SERVICE_TYPE` is fully qualified with a trailing dot; the wire labels
        // carry no trailing dot, so strip it for our owned copy.
        let service = crate::SERVICE_TYPE.trim_end_matches('.').to_string();
        let instance_fqdn = format!("{}.{service}", adv.instance);
        let host = format!("{}.local", adv.instance);
        Records {
            txt_identification: format!("identification={}", adv.identification),
            txt_accessurl: format!("accessurl={}", adv.accessurl),
            port: adv.port,
            addr,
            service_lc: service.to_ascii_lowercase(),
            instance_lc: instance_fqdn.to_ascii_lowercase(),
            host_lc: host.to_ascii_lowercase(),
            service,
            instance_fqdn,
            host,
        }
    }
}

/// The four packet shapes we emit, each a complete response (QR=1, AA=1, ID 0 —
/// RFC 6762 §18.1, so one encoding serves announcements and query replies):
/// - [`Announce`](Shape::Announce): all four records as answers (unsolicited).
/// - [`PtrReply`](Shape::PtrReply): PTR answer + SRV/TXT/A additionals — the
///   reply to a service `PTR` query (RFC 6763 §12.1).
/// - [`InstanceReply`](Shape::InstanceReply): SRV/TXT/A answers — the reply to a
///   `SRV`/`TXT`/`A` query on our instance/host.
/// - [`Goodbye`](Shape::Goodbye): all four records at TTL 0 (RFC 6762 §10.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Announce,
    PtrReply,
    InstanceReply,
    Goodbye,
}

/// Encode one [`Shape`] to mDNS wire bytes. Unique host records (SRV/TXT/A) set
/// the cache-flush bit (RFC 6762 §10.2); the shared PTR does not. A goodbye
/// drops every TTL to 0 and clears cache-flush.
fn encode(r: &Records, shape: Shape) -> Result<Vec<u8>, String> {
    let goodbye = shape == Shape::Goodbye;
    let mut pkt = Packet::new_reply(0);
    pkt.set_flags(PacketFlag::RESPONSE | PacketFlag::AUTHORITATIVE_ANSWER);

    let service = Name::new_unchecked(&r.service);
    let instance = Name::new_unchecked(&r.instance_fqdn);
    let host = Name::new_unchecked(&r.host);
    let ttl_shared = if goodbye { 0 } else { TTL_SHARED };
    let ttl_host = if goodbye { 0 } else { TTL_HOST };
    let flush = !goodbye;

    // Build the four records once; place them into sections per shape below.
    let ptr = ResourceRecord::new(
        service,
        CLASS::IN,
        ttl_shared,
        RData::PTR(PTR(instance.clone())),
    );
    let srv = ResourceRecord::new(
        instance.clone(),
        CLASS::IN,
        ttl_host,
        RData::SRV(SRV {
            priority: 0,
            weight: 0,
            port: r.port,
            target: host.clone(),
        }),
    )
    .with_cache_flush(flush);
    let mut txt_rdata = TXT::new();
    txt_rdata
        .add_string(&r.txt_identification)
        .map_err(|e| format!("TXT identification: {e}"))?;
    txt_rdata
        .add_string(&r.txt_accessurl)
        .map_err(|e| format!("TXT accessurl: {e}"))?;
    let txt = ResourceRecord::new(instance, CLASS::IN, ttl_shared, RData::TXT(txt_rdata))
        .with_cache_flush(flush);
    let a = ResourceRecord::new(host, CLASS::IN, ttl_host, RData::A(A::from(r.addr)))
        .with_cache_flush(flush);

    match shape {
        Shape::Announce | Shape::Goodbye => {
            pkt.answers = vec![ptr, srv, txt, a];
        }
        Shape::PtrReply => {
            // PTR answers the service query; SRV/TXT/A ride along as additionals
            // so the client resolves in one round trip (RFC 6763 §12.1).
            pkt.answers = vec![ptr];
            pkt.additional_records = vec![srv, txt, a];
        }
        Shape::InstanceReply => {
            pkt.answers = vec![srv, txt, a];
        }
    }

    pkt.build_bytes_vec_compressed()
        .map_err(|e| format!("encode mDNS response: {e}"))
}

/// The precomputed wire bytes for each [`Shape`] (mDNS replies don't echo the
/// query ID, so they can be built once at start).
struct Wire {
    announce: Vec<u8>,
    ptr_reply: Vec<u8>,
    instance_reply: Vec<u8>,
    goodbye: Vec<u8>,
}

impl Wire {
    fn build(r: &Records) -> Result<Self, String> {
        Ok(Wire {
            announce: encode(r, Shape::Announce)?,
            ptr_reply: encode(r, Shape::PtrReply)?,
            instance_reply: encode(r, Shape::InstanceReply)?,
            goodbye: encode(r, Shape::Goodbye)?,
        })
    }

    fn reply(&self, shape: Shape) -> &[u8] {
        match shape {
            Shape::PtrReply => &self.ptr_reply,
            Shape::InstanceReply => &self.instance_reply,
            Shape::Announce => &self.announce,
            Shape::Goodbye => &self.goodbye,
        }
    }
}

/// Inspect an incoming datagram. Returns the reply [`Shape`] plus whether a
/// unicast response was requested (QU bit) when the datagram is a query naming
/// our service or instance; `None` when it is irrelevant or itself a response
/// (RFC 6762 §6 — never answer responses).
fn query_match(datagram: &[u8], r: &Records) -> Option<(Shape, bool)> {
    let pkt = Packet::parse(datagram).ok()?;
    if pkt.has_flags(PacketFlag::RESPONSE) {
        return None; // a response/announcement, not a question for us
    }
    for q in &pkt.questions {
        let qname = q.qname.to_string().to_ascii_lowercase();
        let shape = match q.qtype {
            QTYPE::ANY if qname == r.service_lc => Some(Shape::PtrReply),
            QTYPE::ANY if qname == r.instance_lc || qname == r.host_lc => {
                Some(Shape::InstanceReply)
            }
            QTYPE::TYPE(TYPE::PTR) => (qname == r.service_lc).then_some(Shape::PtrReply),
            QTYPE::TYPE(TYPE::SRV) | QTYPE::TYPE(TYPE::TXT) => {
                (qname == r.instance_lc).then_some(Shape::InstanceReply)
            }
            QTYPE::TYPE(TYPE::A) => (qname == r.host_lc).then_some(Shape::InstanceReply),
            _ => None,
        };
        if let Some(shape) = shape {
            return Some((shape, q.unicast_response));
        }
    }
    None
}

/// One IPv4 interface the responder serves: its address, which its `A` record
/// carries, and its netmask, which decides the senders it answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Iface {
    addr: Ipv4Addr,
    mask: Ipv4Addr,
}

impl Iface {
    /// Whether `ip` is on this interface's subnet.
    fn contains(&self, ip: Ipv4Addr) -> bool {
        let mask = u32::from(self.mask);
        u32::from(ip) & mask == u32::from(self.addr) & mask
    }
}

/// The interfaces to serve: every up, multicast-capable, non-loopback IPv4
/// interface, or, when none can be listed, the egress address alone.
fn interfaces() -> Vec<Iface> {
    let listed = list_interfaces().unwrap_or_default();
    if listed.is_empty() {
        return vec![Iface {
            addr: local_ipv4(),
            mask: Ipv4Addr::UNSPECIFIED,
        }];
    }
    listed
}

/// `getifaddrs`, filtered to up, multicast-capable, non-loopback IPv4
/// interfaces, one entry per address. `None` when the call fails.
#[cfg(unix)]
fn list_interfaces() -> Option<Vec<Iface>> {
    let flag = |flags: u32, f: libc::c_int| flags & f as u32 != 0;
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list freed by freeifaddrs below;
    // every pointer read is checked for null first.
    unsafe {
        if libc::getifaddrs(&mut head) != 0 {
            return None;
        }
        let mut out: Vec<Iface> = Vec::new();
        let mut cur = head;
        while !cur.is_null() {
            let ifa = &*cur;
            cur = ifa.ifa_next;
            let flags = ifa.ifa_flags as u32;
            if !flag(flags, libc::IFF_UP)
                || flag(flags, libc::IFF_LOOPBACK)
                || !flag(flags, libc::IFF_MULTICAST)
                || ifa.ifa_addr.is_null()
                || i32::from((*ifa.ifa_addr).sa_family) != libc::AF_INET
            {
                continue;
            }
            let addr = sockaddr_ipv4(ifa.ifa_addr);
            let mask = if ifa.ifa_netmask.is_null() {
                Ipv4Addr::UNSPECIFIED
            } else {
                sockaddr_ipv4(ifa.ifa_netmask)
            };
            if !out.iter().any(|i| i.addr == addr) {
                out.push(Iface { addr, mask });
            }
        }
        libc::freeifaddrs(head);
        Some(out)
    }
}

#[cfg(not(unix))]
fn list_interfaces() -> Option<Vec<Iface>> {
    None
}

/// The IPv4 address in an `AF_INET` sockaddr.
///
/// # Safety
/// `sa` must point to a valid `sockaddr_in`.
#[cfg(unix)]
unsafe fn sockaddr_ipv4(sa: *const libc::sockaddr) -> Ipv4Addr {
    let sin = &*(sa as *const libc::sockaddr_in);
    Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr))
}

/// Resolve this host's egress IPv4 without interface enumeration: connect a
/// throwaway UDP socket toward a destination and read the local address the
/// kernel bound (the "connected-UDP getsockname" trick). We aim at the mDNS
/// group first (yields the multicast egress interface), then a TEST-NET-1
/// unicast address (RFC 5737), and finally fall back to loopback.
fn local_ipv4() -> Ipv4Addr {
    fn egress_toward(dst: (Ipv4Addr, u16)) -> Option<Ipv4Addr> {
        let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
        probe.connect(dst).ok()?; // no datagram is sent; this only sets the route
        match probe.local_addr().ok()? {
            SocketAddr::V4(a) if !a.ip().is_unspecified() => Some(*a.ip()),
            _ => None,
        }
    }
    egress_toward((MDNS_GROUP, MDNS_PORT))
        .or_else(|| egress_toward((Ipv4Addr::new(192, 0, 2, 1), 9)))
        .unwrap_or(Ipv4Addr::LOCALHOST)
}

/// Open and configure the mDNS UDP socket: reuse the well-known port (so the
/// responder can co-exist with a host mDNS daemon during testing), bind
/// `0.0.0.0:5353` and set the mDNS multicast options. The group is joined per
/// interface as each is served ([`Served::join`]).
fn open_socket() -> std::io::Result<Socket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    // SO_REUSEPORT lets the responder share :5353 with another mDNS stack (e.g.
    // avahi) while testing on Linux. It is not needed on the device (nothing
    // else holds :5353 there) and `socket2`'s method is gated behind its `all`
    // feature, which we only enable off-`nto` — so skip it on QNX.
    #[cfg(all(unix, not(target_os = "nto")))]
    let _ = sock.set_reuse_port(true);
    sock.bind(&SockAddr::from(SocketAddr::from((
        Ipv4Addr::UNSPECIFIED,
        MDNS_PORT,
    ))))?;
    // Loop our own multicast back so co-resident stacks on this host see us.
    let _ = sock.set_multicast_loop_v4(true);
    let _ = sock.set_multicast_ttl_v4(255); // RFC 6762 §11: mDNS uses IP TTL 255
    sock.set_read_timeout(Some(POLL_INTERVAL))?;
    Ok(sock)
}

/// Sleep up to `dur`, waking early if `stop` is set, so an announcement pause
/// can't delay teardown by a full second.
fn nap(dur: Duration, stop: &AtomicBool) {
    let step = Duration::from_millis(50);
    let mut left = dur;
    while left > Duration::ZERO && !stop.load(Ordering::Relaxed) {
        let s = step.min(left);
        std::thread::sleep(s);
        left = left.saturating_sub(s);
    }
}

/// One served interface and its encoded record set.
struct Served {
    iface: Iface,
    wire: Wire,
}

impl Served {
    /// Join the mDNS group on this interface. A failure is logged and the
    /// interface still answers the queries the socket receives.
    fn join(&self, sock: &Socket) {
        let joined = if self.iface.mask.is_unspecified() {
            sock.join_multicast_v4(&MDNS_GROUP, &Ipv4Addr::UNSPECIFIED)
        } else {
            sock.join_multicast_v4(&MDNS_GROUP, &self.iface.addr)
        };
        if let Err(e) = joined {
            tracing::debug!(address = %self.iface.addr, error = %e, "mDNS group join failed");
        }
    }

    /// Multicast `bytes` out of this interface.
    fn multicast(&self, sock: &Socket, group: &SockAddr, bytes: &[u8], what: &str) {
        if !self.iface.mask.is_unspecified() {
            let _ = sock.set_multicast_if_v4(&self.iface.addr);
        }
        if let Err(e) = sock.send_to(bytes, group) {
            tracing::debug!(address = %self.iface.addr, error = %e, "mDNS {what} send failed");
        }
    }
}

/// Bring `served` in line with the host's interfaces now: serve the new ones
/// (join, record set) and drop the gone. Returns the newly served.
fn rescan(sock: &Socket, adv: &SovdAdvertiser, served: &mut Vec<Served>) -> Vec<usize> {
    let now = interfaces();
    served.retain(|s| now.contains(&s.iface));
    let mut added = Vec::new();
    for iface in now {
        if served.iter().any(|s| s.iface == iface) {
            continue;
        }
        match Wire::build(&Records::new(adv, iface.addr)) {
            Ok(wire) => {
                let s = Served { iface, wire };
                s.join(sock);
                tracing::info!(address = %iface.addr, "SOVD mDNS: serving interface");
                served.push(s);
                added.push(served.len() - 1);
            }
            Err(e) => {
                tracing::warn!(address = %iface.addr, error = %e, "mDNS records not encodable")
            }
        }
    }
    added
}

/// The served interface a query from `from` is answered on: the one whose
/// subnet holds the sender, else the first.
fn answering<'a>(served: &'a [Served], from: &SockAddr) -> Option<&'a Served> {
    let sender = from.as_socket_ipv4().map(|a| *a.ip());
    sender
        .and_then(|ip| served.iter().find(|s| s.iface.contains(ip)))
        .or_else(|| served.first())
}

/// The worker thread: announce on every interface, serve queries, rescan the
/// interfaces now and then, and say goodbye on stop.
fn serve(sock: Socket, adv: SovdAdvertiser, records: Records, stop: Arc<AtomicBool>) {
    let group = SockAddr::from(SocketAddr::V4(SocketAddrV4::new(MDNS_GROUP, MDNS_PORT)));
    let mut served: Vec<Served> = Vec::new();
    let mut next_scan = Instant::now();
    let mut buf = [MaybeUninit::<u8>::uninit(); RX_BUF];

    while !stop.load(Ordering::Relaxed) {
        if Instant::now() >= next_scan {
            next_scan = Instant::now() + RESCAN_INTERVAL;
            let added = rescan(&sock, &adv, &mut served);
            // Unsolicited announcements on each new interface: at least two,
            // ~1s apart (RFC 6762 §8.3).
            if !added.is_empty() {
                for i in 0..2 {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    for &n in &added {
                        served[n].multicast(
                            &sock,
                            &group,
                            &served[n].wire.announce,
                            "announcement",
                        );
                    }
                    if i == 0 {
                        nap(Duration::from_secs(1), &stop);
                    }
                }
            }
        }
        // The read timeout bounds each iteration, so the stop flag (and hence
        // the goodbye) is observed within `POLL_INTERVAL`.
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                // SAFETY: `recv_from` initialised the first `n` bytes.
                let datagram = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, n) };
                let Some((shape, unicast)) = query_match(datagram, &records) else {
                    continue;
                };
                let Some(s) = answering(&served, &from) else {
                    continue;
                };
                if unicast {
                    if let Err(e) = sock.send_to(s.wire.reply(shape), &from) {
                        tracing::debug!(error = %e, "mDNS response send failed");
                    }
                } else {
                    s.multicast(&sock, &group, s.wire.reply(shape), "response");
                }
            }
            // Read timeout (the common case) or a transient error: just re-check
            // the stop flag and loop. The timeout paces this; it does not spin.
            Err(_) => continue,
        }
    }

    // Goodbye on every interface: withdraw our records (TTL 0) so caches drop us
    // promptly.
    for s in &served {
        s.multicast(&sock, &group, &s.wire.goodbye, "goodbye");
    }
}

/// Guard returned by [`start`]. On drop it signals the worker to stop, which
/// emits the DNS-SD goodbye and then exits; the join blocks until that is done.
pub struct ResponderGuard {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for ResponderGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Start advertising `adv` over mDNS / DNS-SD. The returned [`ResponderGuard`]
/// must be held for the SOVD server's lifetime; dropping it sends the goodbye
/// and stops the worker thread.
pub fn start(adv: SovdAdvertiser) -> Result<ResponderGuard, AdvertiseError> {
    let sock = open_socket().map_err(|e| AdvertiseError(format!("mDNS socket: {e}")))?;
    // The query-matching names do not depend on the address; each served
    // interface encodes its own record set (`rescan`).
    let records = Records::new(&adv, Ipv4Addr::UNSPECIFIED);
    Wire::build(&records).map_err(AdvertiseError)?;

    let fullname = records.instance_fqdn.clone();
    let (port, accessurl) = (adv.port, adv.accessurl.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let stop_worker = Arc::clone(&stop);
    let handle = std::thread::Builder::new()
        .name("sovd-mdns".into())
        .spawn(move || serve(sock, adv, records, stop_worker))
        .map_err(|e| AdvertiseError(format!("spawn mDNS worker: {e}")))?;

    tracing::info!(
        service = %fullname,
        port,
        accessurl = %accessurl,
        "SOVD mDNS/DNS-SD responder started (pure-Rust backend, every IPv4 interface)"
    );
    Ok(ResponderGuard {
        stop,
        handle: Some(handle),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use simple_dns::{Question, QCLASS};

    fn sample() -> SovdAdvertiser {
        SovdAdvertiser {
            instance: "cvc-host-deadbeef".into(),
            port: 4000,
            identification: "cvc-host-deadbeef".into(),
            accessurl: "https://cvc-host-deadbeef.local:4000/vehicle".into(),
        }
    }

    fn records() -> Records {
        Records::new(&sample(), Ipv4Addr::new(10, 1, 2, 3))
    }

    fn served(addr: [u8; 4], mask: [u8; 4]) -> Served {
        let iface = Iface {
            addr: Ipv4Addr::from(addr),
            mask: Ipv4Addr::from(mask),
        };
        Served {
            iface,
            wire: Wire::build(&Records::new(&sample(), iface.addr)).unwrap(),
        }
    }

    fn from(ip: [u8; 4]) -> SockAddr {
        SockAddr::from(SocketAddr::from((Ipv4Addr::from(ip), MDNS_PORT)))
    }

    /// The `A` record a reply on `s` carries.
    fn a_record(s: &Served) -> Ipv4Addr {
        let pkt = Packet::parse(s.wire.reply(Shape::InstanceReply)).unwrap();
        pkt.answers
            .iter()
            .find_map(|rr| match &rr.rdata {
                RData::A(a) => Some(Ipv4Addr::from(a.address)),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn a_query_is_answered_with_the_address_of_its_senders_link() {
        // The CVC host: the LAN, and a /30 to vm1.
        let links = [
            served([192, 168, 1, 20], [255, 255, 255, 0]),
            served([10, 0, 101, 1], [255, 255, 255, 252]),
        ];
        let vm1 = answering(&links, &from([10, 0, 101, 2])).unwrap();
        assert_eq!(a_record(vm1), Ipv4Addr::new(10, 0, 101, 1));
        let lan = answering(&links, &from([192, 168, 1, 77])).unwrap();
        assert_eq!(a_record(lan), Ipv4Addr::new(192, 168, 1, 20));
        // A sender on no served subnet gets the first interface's address.
        let other = answering(&links, &from([172, 16, 0, 9])).unwrap();
        assert_eq!(a_record(other), Ipv4Addr::new(192, 168, 1, 20));
        assert!(answering(&[], &from([10, 0, 101, 2])).is_none());
    }

    #[test]
    fn an_interface_holds_the_addresses_of_its_subnet() {
        let vp1 = Iface {
            addr: Ipv4Addr::new(10, 0, 101, 1),
            mask: Ipv4Addr::new(255, 255, 255, 252),
        };
        assert!(vp1.contains(Ipv4Addr::new(10, 0, 101, 2)));
        assert!(!vp1.contains(Ipv4Addr::new(10, 0, 101, 5)));
        // The egress fallback (no mask) holds every sender.
        let fallback = Iface {
            addr: Ipv4Addr::new(192, 168, 1, 20),
            mask: Ipv4Addr::UNSPECIFIED,
        };
        assert!(fallback.contains(Ipv4Addr::new(10, 0, 101, 2)));
    }

    #[test]
    fn the_served_interfaces_are_never_empty_and_never_loopback() {
        let ifaces = interfaces();
        assert!(!ifaces.is_empty());
        assert!(ifaces
            .iter()
            .all(|i| !i.addr.is_loopback() || i.mask.is_unspecified()));
    }

    // ---- Deterministic wire-correctness tests (no networking) ----

    #[test]
    fn announcement_carries_the_conformant_record_set() {
        let bytes = encode(&records(), Shape::Announce).unwrap();
        let pkt = Packet::parse(&bytes).unwrap();
        assert!(pkt.has_flags(PacketFlag::RESPONSE));
        assert!(pkt.has_flags(PacketFlag::AUTHORITATIVE_ANSWER));
        assert!(
            pkt.additional_records.is_empty(),
            "announcement: all in answers"
        );

        let mut saw_ptr = false;
        let mut saw_srv = false;
        let mut saw_txt = false;
        let mut saw_a = false;
        for rr in &pkt.answers {
            match &rr.rdata {
                RData::PTR(ptr) => {
                    saw_ptr = true;
                    assert_eq!(rr.name.to_string(), "_sovd._tcp.local");
                    assert_eq!(ptr.0.to_string(), "cvc-host-deadbeef._sovd._tcp.local");
                    assert_eq!(rr.ttl, TTL_SHARED);
                    assert!(!rr.cache_flush, "shared PTR must not set cache-flush");
                }
                RData::SRV(srv) => {
                    saw_srv = true;
                    assert_eq!(rr.name.to_string(), "cvc-host-deadbeef._sovd._tcp.local");
                    assert_eq!(srv.port, 4000);
                    assert_eq!(srv.target.to_string(), "cvc-host-deadbeef.local");
                    assert_eq!(rr.ttl, TTL_HOST);
                    assert!(rr.cache_flush, "unique SRV should set cache-flush");
                }
                RData::TXT(txt) => {
                    saw_txt = true;
                    let attrs = txt.attributes();
                    assert_eq!(
                        attrs.get("identification"),
                        Some(&Some("cvc-host-deadbeef".to_string()))
                    );
                    assert_eq!(
                        attrs.get("accessurl"),
                        Some(&Some(
                            "https://cvc-host-deadbeef.local:4000/vehicle".to_string()
                        ))
                    );
                    assert!(rr.cache_flush, "unique TXT should set cache-flush");
                }
                RData::A(a) => {
                    saw_a = true;
                    assert_eq!(rr.name.to_string(), "cvc-host-deadbeef.local");
                    assert_eq!(Ipv4Addr::from(a.address), Ipv4Addr::new(10, 1, 2, 3));
                    assert_eq!(rr.ttl, TTL_HOST);
                }
                other => panic!("unexpected record: {other:?}"),
            }
        }
        assert!(
            saw_ptr && saw_srv && saw_txt && saw_a,
            "all four records present"
        );
    }

    #[test]
    fn ptr_reply_splits_answer_from_additionals() {
        // RFC 6763 §12.1: PTR in the answer section, SRV/TXT/A as additionals.
        let pkt_bytes = encode(&records(), Shape::PtrReply).unwrap();
        let pkt = Packet::parse(&pkt_bytes).unwrap();
        assert_eq!(pkt.answers.len(), 1);
        assert!(matches!(pkt.answers[0].rdata, RData::PTR(_)));
        assert_eq!(pkt.additional_records.len(), 3);
        let mut kinds: Vec<&str> = pkt
            .additional_records
            .iter()
            .map(|rr| match rr.rdata {
                RData::SRV(_) => "SRV",
                RData::TXT(_) => "TXT",
                RData::A(_) => "A",
                _ => "?",
            })
            .collect();
        kinds.sort_unstable();
        assert_eq!(kinds, ["A", "SRV", "TXT"]);
    }

    #[test]
    fn goodbye_zeroes_every_ttl() {
        let bytes = encode(&records(), Shape::Goodbye).unwrap();
        let pkt = Packet::parse(&bytes).unwrap();
        assert_eq!(pkt.answers.len(), 4);
        for rr in &pkt.answers {
            assert_eq!(rr.ttl, 0, "goodbye record {:?} must have TTL 0", rr.rdata);
        }
    }

    #[test]
    fn matches_service_and_instance_queries_only() {
        let r = records();

        let mut ptr_q = Packet::new_query(0);
        ptr_q.questions.push(Question::new(
            Name::new_unchecked("_sovd._tcp.local"),
            QTYPE::TYPE(TYPE::PTR),
            QCLASS::CLASS(CLASS::IN),
            false,
        ));
        assert_eq!(
            query_match(&ptr_q.build_bytes_vec().unwrap(), &r),
            Some((Shape::PtrReply, false))
        );

        // QU (unicast-response) bit set on an SRV query for the instance, and a
        // mixed-case name (DNS names are case-insensitive).
        let mut srv_q = Packet::new_query(0);
        srv_q.questions.push(Question::new(
            Name::new_unchecked("CVC-HOST-DEADBEEF._sovd._tcp.local"),
            QTYPE::TYPE(TYPE::SRV),
            QCLASS::CLASS(CLASS::IN),
            true,
        ));
        assert_eq!(
            query_match(&srv_q.build_bytes_vec().unwrap(), &r),
            Some((Shape::InstanceReply, true))
        );

        // Unrelated service: ignored.
        let mut other_q = Packet::new_query(0);
        other_q.questions.push(Question::new(
            Name::new_unchecked("_http._tcp.local"),
            QTYPE::TYPE(TYPE::PTR),
            QCLASS::CLASS(CLASS::IN),
            false,
        ));
        assert_eq!(query_match(&other_q.build_bytes_vec().unwrap(), &r), None);

        // A response (not a query) is never answered.
        let resp = encode(&r, Shape::Announce).unwrap();
        assert_eq!(query_match(&resp, &r), None);
    }

    // ---- Live round-trip over real UDP multicast ----
    //
    // Ignored by default: it needs working IPv4 multicast loopback on :5353 and
    // (where present) co-existence with a host mDNS daemon, which isn't a given
    // in every CI sandbox. Run explicitly:
    //     cargo test -p sovd-mdns -- --ignored live_query_gets_a_unicast_reply
    #[test]
    #[ignore = "requires UDP multicast on :5353; run with --ignored"]
    fn live_query_gets_a_unicast_reply() {
        // Start the real responder.
        let _guard = start(sample()).expect("responder starts");
        // Give the worker a moment to bind/join before we query.
        std::thread::sleep(Duration::from_millis(300));

        // In-process client on an ephemeral port. We set the QU bit so the
        // responder unicasts the reply straight back to us (no need to join the
        // group or share :5353).
        let client = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("client bind");
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let _ = client.set_multicast_loop_v4(true);

        let mut q = Packet::new_query(0x1234);
        q.questions.push(Question::new(
            Name::new_unchecked("_sovd._tcp.local"),
            QTYPE::TYPE(TYPE::PTR),
            QCLASS::CLASS(CLASS::IN),
            true, // QU: request a unicast response
        ));
        let query = q.build_bytes_vec().unwrap();
        client
            .send_to(&query, (MDNS_GROUP, MDNS_PORT))
            .expect("send query");

        // Read replies until we see our instance (skip unrelated mDNS chatter).
        let mut buf = [0u8; RX_BUF];
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "no reply for _sovd._tcp.local"
            );
            let (n, _from) = match client.recv_from(&mut buf) {
                Ok(v) => v,
                Err(_) => panic!("timed out waiting for the mDNS reply"),
            };
            let pkt = match Packet::parse(&buf[..n]) {
                Ok(p) => p,
                Err(_) => continue,
            };
            if !pkt.has_flags(PacketFlag::RESPONSE) {
                continue;
            }
            let mut port = None;
            let mut host = None;
            let mut ident = None;
            let mut url = None;
            let mut a_addr = None;
            let mut ptr_target = None;
            for rr in pkt.answers.iter().chain(pkt.additional_records.iter()) {
                match &rr.rdata {
                    RData::PTR(p) => ptr_target = Some(p.0.to_string()),
                    RData::SRV(s) => {
                        port = Some(s.port);
                        host = Some(s.target.to_string());
                    }
                    RData::TXT(t) => {
                        let a = t.attributes();
                        ident = a.get("identification").cloned().flatten();
                        url = a.get("accessurl").cloned().flatten();
                    }
                    RData::A(a) => a_addr = Some(Ipv4Addr::from(a.address)),
                    _ => {}
                }
            }
            if ptr_target.as_deref() == Some("cvc-host-deadbeef._sovd._tcp.local") {
                assert_eq!(port, Some(4000), "SRV port");
                assert_eq!(
                    host.as_deref(),
                    Some("cvc-host-deadbeef.local"),
                    "SRV target"
                );
                assert_eq!(
                    ident.as_deref(),
                    Some("cvc-host-deadbeef"),
                    "TXT identification"
                );
                assert_eq!(
                    url.as_deref(),
                    Some("https://cvc-host-deadbeef.local:4000/vehicle"),
                    "TXT accessurl"
                );
                assert!(a_addr.is_some(), "A record present");
                return; // success
            }
        }
    }
}
