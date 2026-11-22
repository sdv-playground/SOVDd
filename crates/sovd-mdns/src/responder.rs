//! Hand-rolled, pure-Rust mDNS / DNS-SD responder (RFC 6762 + RFC 6763).
//!
//! This is the SOVD advertiser backend on QNX (`target_os = "nto"`), where the
//! mature [`mdns-sd`](https://crates.io/crates/mdns-sd) stack can't be used: its
//! event loop (`mio`), interface enumeration (`if-addrs`) and `socket-pktinfo`
//! have no `nto` support. Everything here is built on crates that *do*
//! cross-compile to `aarch64-unknown-nto-qnx710`:
//!
//! - [`simple-dns`] — a pure-Rust DNS wire codec (its only dep is `bitflags`);
//! - [`socket2`] — a thin libc UDP-socket wrapper (its only dep is `libc`, the
//!   same `0.6` line the supernova QNX cross-build already carries);
//! - [`libc`] itself, for `getifaddrs` / `if_nametoindex` only.
//!
//! The module is compiled on *every* target (so it is testable on Linux) but is
//! only *selected* as the live backend on `nto` — see `src/lib.rs`. It is
//! `#[doc(hidden)] pub` so cross-target tests/examples can drive it directly;
//! the stable entry point remains [`SovdAdvertiser::start`](crate::SovdAdvertiser::start).
//!
//! ## Every interface, IPv4 and IPv6
//! The responder serves every interface that is up, multicast-capable, not
//! loopback, and holds an IPv4 and/or IPv6 address ([`interfaces`], read with
//! `getifaddrs`): a host whose guests sit on their own point-to-point links (the
//! CVC: the LAN plus one link per VM) must be found on each of them, at that
//! link's address. Each interface gets its own record set — an `A` for its
//! IPv4 address, an `AAAA` per IPv6 address (link-local included) — which is
//! announced, answered and withdrawn on that interface over both families
//! (224.0.0.251 and ff02::fb). The interface list is read again every
//! [`RESCAN_INTERVAL`], since a VM's link only comes up when the VM starts.
//! Where no interface can be listed, the egress IPv4 toward the mDNS group
//! serves alone (see [`local_ipv4`]).
//!
//! ## Socket layout
//! - IPv4: one shared socket on `0.0.0.0:5353`, joined to the group on every
//!   interface, receives the multicast queries. Each interface *sends* through
//!   its own socket bound to `<its address>:5353` ([`v4_send_socket`]), which
//!   also receives the unicast queries sent to that address. The reason is
//!   QNX: there a socket bound to `0.0.0.0` gets each datagram's source address
//!   from a route lookup toward the destination, multicast with
//!   `IP_MULTICAST_IF` included, so while the default gateway does not answer
//!   ARP every such send fails with `EHOSTUNREACH` (seen on the CVC in ~20 s
//!   stretches). A socket bound to the interface address has a fixed source
//!   and consults no route. `IP_PKTINFO` has no `nto` path, so the interface a
//!   query arrived on is inferred from its sender's subnet.
//! - IPv6: one socket per interface on `[::]:5353` (`IPV6_V6ONLY`), joined to
//!   ff02::fb on that interface only (`IPV6_MULTICAST_ALL` off on Linux) and
//!   sending out of it (`IPV6_MULTICAST_IF`). A query is answered by the
//!   interface it arrived on: the sender's scope id when it has one, else the
//!   socket it came in on. Where the stack refuses a second `[::]:5353` bind
//!   (no `SO_REUSEPORT`, as on the `nto` build), one shared IPv6 socket joins
//!   the group on every interface and the scope id alone picks the interface.
//!   Whether QNX picks IPv6 source addresses without the route-lookup problem
//!   above is **unverified** — to be checked on the bench.
//!
//! ## What it does (minimal but correct)
//! - Per newly served interface: two unsolicited announcements ~1s apart
//!   (RFC 6762 §8.3), each carrying the PTR + SRV + TXT + A/AAAA record set. An
//!   interface whose addresses changed is served (and announced) afresh; the
//!   cache-flush bit replaces its old addresses in peers' caches.
//! - Then serve: for a query naming our service (`_sovd._tcp.local`) reply with
//!   the PTR answer plus SRV/TXT/A/AAAA as additionals (RFC 6763 §12.1); for a
//!   SRV/TXT/A/AAAA query on our instance/host reply with those records. Replies
//!   go unicast when the question sets the QU bit (RFC 6762 §5.4), else
//!   multicast on the query's family.
//! - On guard drop: send a goodbye (the records at TTL 0, RFC 6762 §10.1) out
//!   of every served interface and family, and join the worker thread.
//!
//! ## Corners intentionally cut (not required for a conformant advertiser)
//! - No probing/conflict resolution (RFC 6762 §8.1): the instance name is the
//!   cert-unique device id, so a clash on-link is not expected.
//! - No known-answer suppression (RFC 6762 §7.1) and no response delay/dedup.
//!   The IPv6 sockets are drained between the IPv4 reads, so an IPv6 reply can
//!   take up to [`POLL_INTERVAL`].
//! - One IPv4 address per interface (the first listed); secondary IPv4
//!   addresses are not advertised.
//! - An interface that vanishes gets no goodbye (it can no longer send one).

use crate::{AdvertiseError, SovdAdvertiser};
use simple_dns::rdata::{RData, A, AAAA, PTR, SRV, TXT};
use simple_dns::{Name, Packet, PacketFlag, ResourceRecord, CLASS, QTYPE, TYPE};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::cell::Cell;
use std::io;
use std::mem::MaybeUninit;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Standard mDNS UDP port (RFC 6762 §2).
const MDNS_PORT: u16 = 5353;
/// The IPv4 link-local mDNS multicast group (RFC 6762 §3).
const MDNS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
/// The IPv6 link-local mDNS multicast group (RFC 6762 §3).
const MDNS_GROUP_V6: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 0xfb);
/// TTL for the host's own (unique) records — A, AAAA, SRV (RFC 6762 §10).
const TTL_HOST: u32 = 120;
/// TTL for shared / descriptive records — PTR, TXT (RFC 6762 §10).
const TTL_SHARED: u32 = 4500;
/// Read-timeout granularity for the serve loop, so it can observe the stop flag
/// (and emit the goodbye) promptly on drop.
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// How often the interface list is read again: a VM's link comes up only when
/// the VM starts, after the SOVD server.
const RESCAN_INTERVAL: Duration = Duration::from_secs(10);
/// Receive buffer — one link MTU comfortably holds an mDNS query.
const RX_BUF: usize = 1500;

/// The conformant SOVD record set, plus the lower-cased names we match queries
/// against (DNS names compare case-insensitively; `simple-dns`'s own `Name`
/// equality is case-sensitive, so we normalise here).
struct Records {
    /// `_sovd._tcp.local` — the DNS-SD service type (no trailing dot).
    service: String,
    /// `<instance>._sovd._tcp.local` — the service instance's fullname.
    instance_fqdn: String,
    /// `<instance>.local` — the SRV target / A/AAAA-record host.
    host: String,
    /// `identification=<id>` TXT entry.
    txt_identification: String,
    /// `accessurl=<scheme>://<host>:<port>/v1` TXT entry.
    txt_accessurl: String,
    /// SRV port = the SOVD server's bind port.
    port: u16,
    /// The A-record address (the interface's IPv4), if it has one.
    v4: Option<Ipv4Addr>,
    /// The AAAA-record addresses (the interface's IPv6), possibly none.
    v6: Vec<Ipv6Addr>,
    service_lc: String,
    instance_lc: String,
    host_lc: String,
}

impl Records {
    fn new(adv: &SovdAdvertiser, v4: Option<Ipv4Addr>, v6: Vec<Ipv6Addr>) -> Self {
        // `SERVICE_TYPE` is fully qualified with a trailing dot; the wire labels
        // carry no trailing dot, so strip it for our owned copy.
        let service = crate::SERVICE_TYPE.trim_end_matches('.').to_string();
        let instance_fqdn = format!("{}.{service}", adv.instance);
        let host = format!("{}.local", adv.instance);
        Records {
            txt_identification: format!("identification={}", adv.identification),
            txt_accessurl: format!("accessurl={}", adv.accessurl),
            port: adv.port,
            v4,
            v6,
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
/// - [`Announce`](Shape::Announce): every record as answers (unsolicited).
/// - [`PtrReply`](Shape::PtrReply): PTR answer + SRV/TXT/A/AAAA additionals —
///   the reply to a service `PTR` query (RFC 6763 §12.1).
/// - [`InstanceReply`](Shape::InstanceReply): SRV/TXT/A/AAAA answers — the
///   reply to a `SRV`/`TXT`/`A`/`AAAA` query on our instance/host.
/// - [`Goodbye`](Shape::Goodbye): every record at TTL 0 (RFC 6762 §10.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Announce,
    PtrReply,
    InstanceReply,
    Goodbye,
}

/// Encode one [`Shape`] to mDNS wire bytes. Unique host records
/// (SRV/TXT/A/AAAA) set the cache-flush bit (RFC 6762 §10.2); the shared PTR
/// does not. A goodbye drops every TTL to 0 and clears cache-flush.
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

    // Build the records once; place them into sections per shape below.
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
    let address = |rdata| {
        ResourceRecord::new(host.clone(), CLASS::IN, ttl_host, rdata).with_cache_flush(flush)
    };
    let addresses =
        r.v4.map(|a| address(RData::A(A::from(a))))
            .into_iter()
            .chain(r.v6.iter().map(|&a| address(RData::AAAA(AAAA::from(a)))));

    match shape {
        Shape::Announce | Shape::Goodbye => {
            pkt.answers = vec![ptr, srv, txt];
            pkt.answers.extend(addresses);
        }
        Shape::PtrReply => {
            // PTR answers the service query; SRV/TXT/A/AAAA ride along as
            // additionals so the client resolves in one round trip (RFC 6763
            // §12.1).
            pkt.answers = vec![ptr];
            pkt.additional_records = vec![srv, txt];
            pkt.additional_records.extend(addresses);
        }
        Shape::InstanceReply => {
            pkt.answers = vec![srv, txt];
            pkt.answers.extend(addresses);
        }
    }

    pkt.build_bytes_vec_compressed()
        .map_err(|e| format!("encode mDNS response: {e}"))
}

/// The precomputed wire bytes for each [`Shape`] (mDNS replies don't echo the
/// query ID, so they can be built once per interface).
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
            QTYPE::TYPE(TYPE::A) | QTYPE::TYPE(TYPE::AAAA) => {
                (qname == r.host_lc).then_some(Shape::InstanceReply)
            }
            _ => None,
        };
        if let Some(shape) = shape {
            return Some((shape, q.unicast_response));
        }
    }
    None
}

/// Resolve this host's egress IPv4 without interface enumeration: connect a
/// throwaway UDP socket toward a destination and read the local address the
/// kernel bound (the "connected-UDP getsockname" trick). We aim at the mDNS
/// group first (yields the multicast egress interface), then a TEST-NET-1
/// unicast address (RFC 5737), and finally fall back to loopback. Used only
/// when no interface can be listed.
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

/// An interface's IPv4 address and netmask; the netmask decides which senders
/// the interface answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct V4 {
    addr: Ipv4Addr,
    mask: Ipv4Addr,
}

impl V4 {
    /// Whether `ip` is on this subnet (an unspecified mask holds every sender).
    fn contains(&self, ip: Ipv4Addr) -> bool {
        let mask = u32::from(self.mask);
        u32::from(ip) & mask == u32::from(self.addr) & mask
    }
}

/// One interface the responder serves.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Iface {
    /// The interface name; empty for the egress-address fallback.
    name: String,
    /// The interface index, i.e. the IPv6 scope id.
    index: u32,
    /// Its (first) IPv4 address, which the `A` record carries.
    v4: Option<V4>,
    /// Its IPv6 addresses, link-local included, which the `AAAA` records carry.
    v6: Vec<Ipv6Addr>,
}

impl Iface {
    /// The egress-address fallback ([`interfaces`]) has no address of its own
    /// to bind or join on.
    fn is_egress_fallback(&self) -> bool {
        self.name.is_empty()
    }
}

/// One `getifaddrs` entry (one address of one interface), reduced to what
/// [`select_interfaces`] looks at.
struct IfEntry {
    name: String,
    index: u32,
    up: bool,
    multicast: bool,
    loopback: bool,
    addr: Option<IpAddr>,
    /// The netmask, for an IPv4 entry.
    mask: Option<Ipv4Addr>,
}

/// Fold `getifaddrs` entries into the interfaces to serve: up,
/// multicast-capable, not loopback, with an IPv4 and/or IPv6 address.
fn select_interfaces(entries: impl IntoIterator<Item = IfEntry>) -> Vec<Iface> {
    let mut out: Vec<Iface> = Vec::new();
    for e in entries {
        let Some(addr) = e.addr.filter(|_| e.up && e.multicast && !e.loopback) else {
            continue;
        };
        let at = match out.iter().position(|i| i.name == e.name) {
            Some(at) => at,
            None => {
                out.push(Iface {
                    name: e.name,
                    index: e.index,
                    v4: None,
                    v6: Vec::new(),
                });
                out.len() - 1
            }
        };
        let iface = &mut out[at];
        match addr {
            IpAddr::V4(addr) if iface.v4.is_none() => {
                // No netmask listed: answer only senders at exactly this address.
                let mask = e.mask.unwrap_or(Ipv4Addr::BROADCAST);
                iface.v4 = Some(V4 { addr, mask });
            }
            IpAddr::V6(addr) if !iface.v6.contains(&addr) => iface.v6.push(addr),
            _ => {}
        }
    }
    out
}

/// The interfaces to serve ([`select_interfaces`]), or, when none can be
/// listed, the egress IPv4 alone.
fn interfaces() -> Vec<Iface> {
    let listed = select_interfaces(list_interfaces().unwrap_or_default());
    if !listed.is_empty() {
        return listed;
    }
    vec![Iface {
        name: String::new(),
        index: 0,
        v4: Some(V4 {
            addr: local_ipv4(),
            mask: Ipv4Addr::UNSPECIFIED,
        }),
        v6: Vec::new(),
    }]
}

/// Every `getifaddrs` entry; `None` when the call fails.
#[cfg(unix)]
fn list_interfaces() -> Option<Vec<IfEntry>> {
    let has = |flags: u32, f: libc::c_int| flags & f as u32 != 0;
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list freed by freeifaddrs below;
    // every pointer read is checked for null first.
    unsafe {
        if libc::getifaddrs(&mut head) != 0 {
            return None;
        }
        let mut out = Vec::new();
        let mut cur = head;
        while !cur.is_null() {
            let ifa = &*cur;
            cur = ifa.ifa_next;
            if ifa.ifa_name.is_null() {
                continue;
            }
            let flags = ifa.ifa_flags as u32;
            let addr = sockaddr_ip(ifa.ifa_addr);
            // The netmask's own family field is not reliably set on BSD-derived
            // stacks, so read it by the address's family.
            let mask = match addr {
                Some(IpAddr::V4(_)) if !ifa.ifa_netmask.is_null() => {
                    Some(sockaddr_in_ip(ifa.ifa_netmask))
                }
                _ => None,
            };
            out.push(IfEntry {
                name: std::ffi::CStr::from_ptr(ifa.ifa_name)
                    .to_string_lossy()
                    .into_owned(),
                index: libc::if_nametoindex(ifa.ifa_name),
                up: has(flags, libc::IFF_UP),
                multicast: has(flags, libc::IFF_MULTICAST),
                loopback: has(flags, libc::IFF_LOOPBACK),
                addr,
                mask,
            });
        }
        libc::freeifaddrs(head);
        Some(out)
    }
}

#[cfg(not(unix))]
fn list_interfaces() -> Option<Vec<IfEntry>> {
    None
}

/// The IP address in an `AF_INET` / `AF_INET6` sockaddr; `None` for a null
/// pointer or another family.
///
/// # Safety
/// `sa` must be null or point to a valid sockaddr of its family.
#[cfg(unix)]
unsafe fn sockaddr_ip(sa: *const libc::sockaddr) -> Option<IpAddr> {
    if sa.is_null() {
        return None;
    }
    match i32::from((*sa).sa_family) {
        libc::AF_INET => Some(IpAddr::V4(sockaddr_in_ip(sa))),
        libc::AF_INET6 => {
            let sin6 = &*(sa as *const libc::sockaddr_in6);
            Some(IpAddr::V6(Ipv6Addr::from(sin6.sin6_addr.s6_addr)))
        }
        _ => None,
    }
}

/// The IPv4 address in a `sockaddr_in`.
///
/// # Safety
/// `sa` must point to a valid `sockaddr_in`.
#[cfg(unix)]
unsafe fn sockaddr_in_ip(sa: *const libc::sockaddr) -> Ipv4Addr {
    let sin = &*(sa as *const libc::sockaddr_in);
    Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr))
}

/// `socket2` compiles `join_multicast_v6` / `leave_multicast_v6` out on QNX
/// (`target_os = "nto"`). The kernel has the options; go to libc there.
#[cfg(not(target_os = "nto"))]
fn join_multicast_v6(sock: &Socket, group: &Ipv6Addr, index: u32) -> io::Result<()> {
    sock.join_multicast_v6(group, index)
}

#[cfg(not(target_os = "nto"))]
fn leave_multicast_v6(sock: &Socket, group: &Ipv6Addr, index: u32) -> io::Result<()> {
    sock.leave_multicast_v6(group, index)
}

#[cfg(target_os = "nto")]
fn join_multicast_v6(sock: &Socket, group: &Ipv6Addr, index: u32) -> io::Result<()> {
    v6_group_opt(sock, libc::IPV6_JOIN_GROUP, group, index)
}

#[cfg(target_os = "nto")]
fn leave_multicast_v6(sock: &Socket, group: &Ipv6Addr, index: u32) -> io::Result<()> {
    v6_group_opt(sock, libc::IPV6_LEAVE_GROUP, group, index)
}

/// `setsockopt(IPPROTO_IPV6, IPV6_{JOIN,LEAVE}_GROUP, ipv6_mreq)`.
#[cfg(target_os = "nto")]
fn v6_group_opt(sock: &Socket, opt: libc::c_int, group: &Ipv6Addr, index: u32) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let mreq = libc::ipv6_mreq {
        ipv6mr_multiaddr: libc::in6_addr {
            s6_addr: group.octets(),
        },
        ipv6mr_interface: index as _,
    };
    // SAFETY: `mreq` is a fully initialised `ipv6_mreq` and the length passed is
    // its size; the fd is owned by `sock` for the duration of the call.
    let rc = unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::IPPROTO_IPV6,
            opt,
            &mreq as *const libc::ipv6_mreq as *const libc::c_void,
            std::mem::size_of::<libc::ipv6_mreq>() as libc::socklen_t,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// A UDP socket that shares :5353: `SO_REUSEADDR`, and `IPV6_V6ONLY` for IPv6.
fn mdns_socket(domain: Domain) -> io::Result<Socket> {
    let sock = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    if domain == Domain::IPV6 {
        sock.set_only_v6(true)?;
    }
    sock.set_reuse_address(true)?;
    // SO_REUSEPORT lets the responder share :5353 with another mDNS stack (e.g.
    // avahi) while testing on Linux, and lets the per-interface IPv6 sockets
    // share `[::]:5353`. `socket2`'s method is gated behind its `all` feature,
    // which we only enable off-`nto` — so skip it on QNX (where the IPv6 side
    // then falls back to one shared socket, see [`open_v6`]).
    #[cfg(all(unix, not(target_os = "nto")))]
    let _ = sock.set_reuse_port(true);
    Ok(sock)
}

/// The shared IPv4 socket: bound to `0.0.0.0:5353`, it receives the multicast
/// queries. The group is joined per interface as each is served
/// ([`Served::new`]).
fn shared_v4_socket() -> io::Result<Socket> {
    let sock = mdns_socket(Domain::IPV4)?;
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

/// The socket one interface sends IPv4 from: bound to its address on :5353
/// beside the shared socket, so the stack never picks the source by a route
/// lookup; multicast out of the interface at IP TTL 255; nonblocking, as the
/// serve loop only drains it.
fn v4_send_socket(addr: Ipv4Addr) -> io::Result<Socket> {
    let sock = mdns_socket(Domain::IPV4)?;
    sock.bind(&SockAddr::from(SocketAddr::from((addr, MDNS_PORT))))?;
    sock.set_multicast_if_v4(&addr)?;
    let _ = sock.set_multicast_loop_v4(true);
    let _ = sock.set_multicast_ttl_v4(255); // RFC 6762 §11
    sock.set_nonblocking(true)?;
    Ok(sock)
}

/// An IPv6 socket on `[::]:5353`. With `index`, it is joined to ff02::fb on
/// that interface only and sends out of it; without, it is the shared fallback
/// socket, joined per interface as each is served. Nonblocking, as the serve
/// loop only drains it.
fn v6_socket(index: Option<u32>) -> io::Result<Socket> {
    let sock = mdns_socket(Domain::IPV6)?;
    // Deliver only the groups joined on this socket, not every group joined on
    // the host (Linux's default), so the socket a query arrives on names its
    // interface. Older kernels lack the option; the scope id still decides.
    #[cfg(target_os = "linux")]
    let _ = sock.set_multicast_all_v6(false);
    sock.bind(&SockAddr::from(SocketAddr::from((
        Ipv6Addr::UNSPECIFIED,
        MDNS_PORT,
    ))))?;
    if let Some(index) = index {
        join_multicast_v6(&sock, &MDNS_GROUP_V6, index)?;
        sock.set_multicast_if_v6(index)?;
    }
    let _ = sock.set_multicast_loop_v6(true);
    let _ = sock.set_multicast_hops_v6(255); // RFC 6762 §11
    sock.set_nonblocking(true)?;
    Ok(sock)
}

/// How IPv6 is served.
enum V6 {
    /// `[::]:5353` could not be bound: IPv4 only.
    Off,
    /// One socket per interface ([`v6_socket`] with an index).
    PerInterface,
    /// The stack refuses a second `[::]:5353` bind: this one socket serves
    /// every interface.
    Shared(Socket),
}

/// Probe how IPv6 can be served: bind `[::]:5353` twice. Logs the fallback
/// once, here.
fn open_v6() -> V6 {
    let first = match v6_socket(None) {
        Ok(sock) => sock,
        Err(e) => {
            tracing::warn!(error = %e, "mDNS: cannot bind [::]:5353; IPv6 not served");
            return V6::Off;
        }
    };
    match v6_socket(None) {
        Ok(_) => V6::PerInterface,
        Err(e) => {
            tracing::info!(error = %e,
                "mDNS: no per-interface [::]:5353 sockets here; one shared IPv6 socket serves every interface");
            V6::Shared(first)
        }
    }
}

/// One served interface, its encoded record set and the sockets it sends from.
struct Served {
    iface: Iface,
    wire: Wire,
    /// Bound to the interface's IPv4 address ([`v4_send_socket`]); `None`
    /// without IPv4, for the egress fallback, or when the bind failed: the
    /// shared socket sends instead.
    v4: Option<Socket>,
    /// The interface's own IPv6 socket; `None` without IPv6 or when IPv6 is
    /// not served per interface.
    v6: Option<Socket>,
    /// Whether the last send failed, so a failing stretch logs once.
    failing: Cell<bool>,
}

impl Served {
    /// Serve `iface`: encode its record set, join the group on it and open its
    /// sockets. A failed join or bind is logged; the interface is still served
    /// through what works.
    fn new(
        iface: Iface,
        adv: &SovdAdvertiser,
        shared_v4: &Socket,
        v6: &V6,
    ) -> Result<Self, String> {
        let wire = Wire::build(&Records::new(
            adv,
            iface.v4.map(|v4| v4.addr),
            iface.v6.clone(),
        ))?;
        let mut own_v4 = None;
        if let Some(v4) = iface.v4 {
            let on = if iface.is_egress_fallback() {
                Ipv4Addr::UNSPECIFIED
            } else {
                v4.addr
            };
            if let Err(e) = shared_v4.join_multicast_v4(&MDNS_GROUP, &on) {
                tracing::debug!(address = %v4.addr, error = %e, "mDNS group join failed");
            }
            if !iface.is_egress_fallback() {
                own_v4 = v4_send_socket(v4.addr)
                    .map_err(|e| {
                        tracing::warn!(address = %v4.addr, error = %e,
                        "mDNS: cannot bind the interface's socket; sending through the shared one")
                    })
                    .ok();
            }
        }
        let mut own_v6 = None;
        if !iface.v6.is_empty() {
            match v6 {
                V6::Off => {}
                V6::PerInterface => {
                    own_v6 = v6_socket(Some(iface.index))
                        .map_err(|e| {
                            tracing::warn!(interface = %iface.name, error = %e,
                            "mDNS: cannot open the interface's IPv6 socket")
                        })
                        .ok();
                }
                V6::Shared(sock) => {
                    if let Err(e) = join_multicast_v6(sock, &MDNS_GROUP_V6, iface.index) {
                        tracing::debug!(interface = %iface.name, error = %e, "mDNS group join failed");
                    }
                }
            }
        }
        Ok(Served {
            iface,
            wire,
            v4: own_v4,
            v6: own_v6,
            failing: Cell::new(false),
        })
    }

    /// Undo the shared sockets' joins for this interface (it is no longer
    /// served). Errors are expected when it has vanished and are ignored.
    fn leave(&self, shared_v4: &Socket, v6: &V6) {
        if let Some(v4) = self.iface.v4.filter(|_| !self.iface.is_egress_fallback()) {
            let _ = shared_v4.leave_multicast_v4(&MDNS_GROUP, &v4.addr);
        }
        if let (V6::Shared(sock), false) = (v6, self.iface.v6.is_empty()) {
            let _ = leave_multicast_v6(sock, &MDNS_GROUP_V6, self.iface.index);
        }
    }

    /// The socket this interface sends IPv6 from, if IPv6 is served on it.
    fn v6_sock<'a>(&'a self, v6: &'a V6) -> Option<&'a Socket> {
        if self.iface.v6.is_empty() {
            return None;
        }
        match (&self.v6, v6) {
            (Some(own), _) => Some(own),
            (None, V6::Shared(shared)) => {
                let _ = shared.set_multicast_if_v6(self.iface.index);
                Some(shared)
            }
            _ => None,
        }
    }

    /// Multicast `bytes` out of this interface on every family it has.
    fn multicast(&self, shared_v4: &Socket, v6: &V6, bytes: &[u8], what: &str) {
        self.multicast_v4(shared_v4, bytes, what);
        self.multicast_v6(v6, bytes, what);
    }

    /// Multicast `bytes` to 224.0.0.251 out of this interface, if it has IPv4.
    fn multicast_v4(&self, shared: &Socket, bytes: &[u8], what: &str) {
        let Some(v4) = self.iface.v4 else {
            return;
        };
        let group = SockAddr::from(SocketAddrV4::new(MDNS_GROUP, MDNS_PORT));
        let sent = match &self.v4 {
            Some(own) => own.send_to(bytes, &group),
            None => {
                if !self.iface.is_egress_fallback() {
                    let _ = shared.set_multicast_if_v4(&v4.addr);
                }
                shared.send_to(bytes, &group)
            }
        };
        self.note(sent, what);
    }

    /// Multicast `bytes` to ff02::fb out of this interface, if it has IPv6.
    fn multicast_v6(&self, v6: &V6, bytes: &[u8], what: &str) {
        if let Some(sock) = self.v6_sock(v6) {
            let group = SocketAddrV6::new(MDNS_GROUP_V6, MDNS_PORT, 0, self.iface.index);
            self.note(sock.send_to(bytes, &SockAddr::from(group)), what);
        }
    }

    /// Unicast `bytes` to `to` (a QU reply) from this interface, over `to`'s
    /// family.
    fn unicast(&self, shared_v4: &Socket, v6: &V6, to: &SockAddr, bytes: &[u8]) {
        let sock = if to.as_socket_ipv6().is_some() {
            self.v6_sock(v6)
        } else {
            Some(self.v4.as_ref().unwrap_or(shared_v4))
        };
        if let Some(sock) = sock {
            self.note(sock.send_to(bytes, to), "unicast response");
        }
    }

    /// Log a send failure once per failing stretch, and the recovery.
    fn note(&self, sent: io::Result<usize>, what: &str) {
        let name = &self.iface.name;
        match sent {
            Ok(_) => {
                if self.failing.replace(false) {
                    tracing::info!(interface = %name, "mDNS sends work again");
                }
            }
            Err(e) if !self.failing.replace(true) => {
                tracing::warn!(interface = %name, error = %e, "mDNS {what} send failed")
            }
            Err(e) => tracing::debug!(interface = %name, error = %e, "mDNS {what} send failed"),
        }
    }
}

/// The served interface that answers a query from `from` which arrived on the
/// socket of `served[arrived]` (`None`: a shared socket). IPv4: the interface
/// whose subnet holds the sender; IPv6: the interface the sender's scope id
/// names. Else the arrival socket's interface, else the first serving the
/// query's family.
fn answering<'a>(
    served: &'a [Served],
    arrived: Option<usize>,
    from: &SockAddr,
) -> Option<&'a Served> {
    let (by_sender, v6) = match (from.as_socket_ipv4(), from.as_socket_ipv6()) {
        (Some(a), _) => (
            served
                .iter()
                .position(|s| s.iface.v4.is_some_and(|v4| v4.contains(*a.ip()))),
            false,
        ),
        (_, Some(a)) => (
            served
                .iter()
                .position(|s| a.scope_id() != 0 && s.iface.index == a.scope_id()),
            true,
        ),
        _ => return None,
    };
    let has_family = |s: &Served| {
        if v6 {
            !s.iface.v6.is_empty()
        } else {
            s.iface.v4.is_some()
        }
    };
    by_sender
        .or(arrived)
        .map(|i| &served[i])
        .filter(|s| has_family(s))
        .or_else(|| served.iter().find(|s| has_family(s)))
}

/// Receive one datagram from `sock` into `buf`.
fn recv<'b>(sock: &Socket, buf: &'b mut [MaybeUninit<u8>]) -> Option<(&'b [u8], SockAddr)> {
    let (n, from) = sock.recv_from(buf).ok()?;
    // SAFETY: `recv_from` initialised the first `n` bytes.
    let datagram = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, n) };
    Some((datagram, from))
}

/// The worker's state: what it advertises and the interfaces it serves.
struct Responder {
    adv: SovdAdvertiser,
    /// The names queries are matched against (no addresses).
    records: Records,
    shared_v4: Socket,
    v6: V6,
    served: Vec<Served>,
}

impl Responder {
    /// Bring `served` in line with the host's interfaces now: drop the gone or
    /// changed ones and serve the new ones. Returns the newly served indices.
    fn rescan(&mut self) -> Vec<usize> {
        let now = interfaces();
        let (shared_v4, v6) = (&self.shared_v4, &self.v6);
        self.served.retain(|s| {
            let keep = now.contains(&s.iface);
            if !keep {
                s.leave(shared_v4, v6);
                tracing::info!(interface = %s.iface.name, "SOVD mDNS: interface gone or changed");
            }
            keep
        });
        let mut added = Vec::new();
        for iface in now {
            if self.served.iter().any(|s| s.iface == iface) {
                continue;
            }
            let (name, v4, v6) = (iface.name.clone(), iface.v4, iface.v6.clone());
            match Served::new(iface, &self.adv, &self.shared_v4, &self.v6) {
                Ok(s) => {
                    tracing::info!(interface = %name, ipv4 = ?v4.map(|v| v.addr), ipv6 = ?v6,
                        "SOVD mDNS: serving interface");
                    self.served.push(s);
                    added.push(self.served.len() - 1);
                }
                Err(e) => {
                    tracing::warn!(interface = %name, error = %e, "mDNS records not encodable")
                }
            }
        }
        added
    }

    /// Unsolicited announcements on each of `added`: at least two, ~1s apart
    /// (RFC 6762 §8.3).
    fn announce(&self, added: &[usize], stop: &AtomicBool) {
        if added.is_empty() {
            return;
        }
        for i in 0..2 {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            for s in added.iter().map(|&n| &self.served[n]) {
                s.multicast(&self.shared_v4, &self.v6, &s.wire.announce, "announcement");
            }
            if i == 0 {
                nap(Duration::from_secs(1), stop);
            }
        }
    }

    /// Answer `datagram` from `from` if it is a query for us: unicast when it
    /// asks (QU), else multicast on its family, from the [`answering`]
    /// interface with that interface's records.
    fn respond(&self, datagram: &[u8], from: &SockAddr, arrived: Option<usize>) {
        let Some((shape, unicast)) = query_match(datagram, &self.records) else {
            return;
        };
        let Some(s) = answering(&self.served, arrived, from) else {
            return;
        };
        let bytes = s.wire.reply(shape);
        if unicast {
            s.unicast(&self.shared_v4, &self.v6, from, bytes);
        } else if from.as_socket_ipv6().is_some() {
            s.multicast_v6(&self.v6, bytes, "response");
        } else {
            s.multicast_v4(&self.shared_v4, bytes, "response");
        }
    }

    /// One serve-loop turn: wait up to [`POLL_INTERVAL`] on the shared IPv4
    /// socket (read timeout or a transient error just ends the wait), then
    /// drain the nonblocking ones.
    fn poll(&self, buf: &mut [MaybeUninit<u8>]) {
        if let Some((datagram, from)) = recv(&self.shared_v4, buf) {
            self.respond(datagram, &from, None);
        }
        if let V6::Shared(sock) = &self.v6 {
            while let Some((datagram, from)) = recv(sock, buf) {
                self.respond(datagram, &from, None);
            }
        }
        for (i, s) in self.served.iter().enumerate() {
            for sock in [&s.v4, &s.v6].into_iter().flatten() {
                while let Some((datagram, from)) = recv(sock, buf) {
                    self.respond(datagram, &from, Some(i));
                }
            }
        }
    }

    /// Withdraw our records (TTL 0) out of every interface and family so
    /// caches drop us promptly.
    fn goodbye(&self) {
        for s in &self.served {
            s.multicast(&self.shared_v4, &self.v6, &s.wire.goodbye, "goodbye");
        }
    }
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

/// The worker thread: serve and announce every interface, serve queries,
/// rescan the interfaces every [`RESCAN_INTERVAL`], and say goodbye on stop.
fn serve(mut rsp: Responder, stop: Arc<AtomicBool>) {
    let mut buf = [MaybeUninit::<u8>::uninit(); RX_BUF];
    let mut next_scan = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if Instant::now() >= next_scan {
            next_scan = Instant::now() + RESCAN_INTERVAL;
            let added = rsp.rescan();
            rsp.announce(&added, &stop);
        }
        // The read timeout bounds each turn, so the stop flag (and hence the
        // goodbye) is observed within `POLL_INTERVAL`.
        rsp.poll(&mut buf);
    }
    rsp.goodbye();
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
    let shared_v4 = shared_v4_socket().map_err(|e| AdvertiseError(format!("mDNS socket: {e}")))?;
    let v6 = open_v6();
    // The query-matching names do not depend on the addresses; each served
    // interface encodes its own record set ([`Responder::rescan`]). Encoding
    // once here surfaces an unencodable record set at start.
    let records = Records::new(&adv, None, Vec::new());
    Wire::build(&records).map_err(AdvertiseError)?;

    let fullname = records.instance_fqdn.clone();
    let (port, accessurl) = (adv.port, adv.accessurl.clone());
    let ipv6 = match v6 {
        V6::Off => "off",
        V6::PerInterface => "per interface",
        V6::Shared(_) => "shared socket",
    };
    let rsp = Responder {
        adv,
        records,
        shared_v4,
        v6,
        served: Vec::new(),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let stop_worker = Arc::clone(&stop);
    let handle = std::thread::Builder::new()
        .name("sovd-mdns".into())
        .spawn(move || serve(rsp, stop_worker))
        .map_err(|e| AdvertiseError(format!("spawn mDNS worker: {e}")))?;

    tracing::info!(
        service = %fullname,
        port,
        accessurl = %accessurl,
        ipv6,
        "SOVD mDNS/DNS-SD responder started (pure-Rust backend, every interface)"
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
            accessurl: "https://cvc-host-deadbeef.local:4000/v1".into(),
        }
    }

    fn records() -> Records {
        Records::new(&sample(), Some(Ipv4Addr::new(10, 1, 2, 3)), Vec::new())
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
                        Some(&Some("https://cvc-host-deadbeef.local:4000/v1".to_string()))
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

    #[test]
    fn aaaa_queries_on_the_host_are_answered() {
        let mut q = Packet::new_query(0);
        q.questions.push(Question::new(
            Name::new_unchecked("cvc-host-deadbeef.local"),
            QTYPE::TYPE(TYPE::AAAA),
            QCLASS::CLASS(CLASS::IN),
            false,
        ));
        assert_eq!(
            query_match(&q.build_bytes_vec().unwrap(), &records()),
            Some((Shape::InstanceReply, false))
        );
    }

    // ---- Per-interface record sets ----

    const LINK_LOCAL: Ipv6Addr = Ipv6Addr::new(0xfe80, 0, 0, 0, 0x642, 0x1aff, 0xfeeb, 0xbbb);
    const GLOBAL: Ipv6Addr = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1);

    /// The addresses (A, then AAAA) an encoded packet carries, with their TTLs.
    fn addresses(bytes: &[u8]) -> Vec<(IpAddr, u32)> {
        let pkt = Packet::parse(bytes).unwrap();
        pkt.answers
            .iter()
            .chain(pkt.additional_records.iter())
            .filter_map(|rr| match &rr.rdata {
                RData::A(a) => Some((IpAddr::V4(Ipv4Addr::from(a.address)), rr.ttl)),
                RData::AAAA(a) => Some((IpAddr::V6(Ipv6Addr::from(a.address)), rr.ttl)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_interface_record_set_carries_its_own_addresses() {
        let v4 = Ipv4Addr::new(192, 168, 1, 20);
        let a_only = Records::new(&sample(), Some(v4), Vec::new());
        let aaaa_only = Records::new(&sample(), None, vec![LINK_LOCAL, GLOBAL]);
        let both = Records::new(&sample(), Some(v4), vec![LINK_LOCAL]);
        for shape in [Shape::Announce, Shape::PtrReply, Shape::InstanceReply] {
            let ips = |r: &Records| -> Vec<IpAddr> {
                addresses(&encode(r, shape).unwrap())
                    .into_iter()
                    .map(|(ip, _)| ip)
                    .collect()
            };
            assert_eq!(ips(&a_only), [IpAddr::V4(v4)], "{shape:?}");
            assert_eq!(
                ips(&aaaa_only),
                [IpAddr::V6(LINK_LOCAL), IpAddr::V6(GLOBAL)],
                "{shape:?}"
            );
            assert_eq!(
                ips(&both),
                [IpAddr::V4(v4), IpAddr::V6(LINK_LOCAL)],
                "{shape:?}"
            );
        }
        let bytes = encode(&both, Shape::Announce).unwrap();
        let pkt = Packet::parse(&bytes).unwrap();
        assert!(pkt
            .answers
            .iter()
            .filter(|rr| matches!(rr.rdata, RData::A(_) | RData::AAAA(_)))
            .all(|rr| rr.cache_flush && rr.ttl == TTL_HOST));
    }

    #[test]
    fn goodbye_withdraws_both_families_at_ttl_zero() {
        let r = Records::new(
            &sample(),
            Some(Ipv4Addr::new(10, 0, 101, 1)),
            vec![LINK_LOCAL],
        );
        let bytes = encode(&r, Shape::Goodbye).unwrap();
        assert_eq!(
            addresses(&bytes),
            [
                (IpAddr::V4(Ipv4Addr::new(10, 0, 101, 1)), 0),
                (IpAddr::V6(LINK_LOCAL), 0)
            ]
        );
        let pkt = Packet::parse(&bytes).unwrap();
        assert_eq!(pkt.answers.len(), 5);
        assert!(pkt.answers.iter().all(|rr| rr.ttl == 0 && !rr.cache_flush));
    }

    // ---- Interface selection ----

    fn entry(name: &str, flags: (bool, bool, bool), addr: Option<IpAddr>) -> IfEntry {
        let (up, multicast, loopback) = flags;
        IfEntry {
            name: name.into(),
            index: name.len() as u32,
            up,
            multicast,
            loopback,
            addr,
            mask: match addr {
                Some(IpAddr::V4(_)) => Some(Ipv4Addr::new(255, 255, 255, 0)),
                _ => None,
            },
        }
    }

    const OK: (bool, bool, bool) = (true, true, false);

    #[test]
    fn only_up_multicast_non_loopback_interfaces_with_an_address_are_served() {
        let v4 = |a, b, c, d| Some(IpAddr::V4(Ipv4Addr::new(a, b, c, d)));
        let ifaces = select_interfaces([
            entry("lo", (true, true, true), v4(127, 0, 0, 1)),
            entry("down0", (false, true, false), v4(10, 9, 9, 9)),
            entry("p2p0", (true, false, false), v4(10, 8, 8, 8)),
            entry("noaddr0", OK, None), // e.g. an AF_PACKET entry
            entry("eth0", OK, None),
            entry("eth0", OK, v4(192, 168, 1, 20)),
            entry("eth0", OK, v4(192, 168, 1, 21)), // secondary: not advertised
            entry("eth0", OK, Some(IpAddr::V6(LINK_LOCAL))),
            entry("eth0", OK, Some(IpAddr::V6(LINK_LOCAL))), // duplicate
            entry("veth1", OK, Some(IpAddr::V6(GLOBAL))),
        ]);
        assert_eq!(
            ifaces,
            [
                Iface {
                    name: "eth0".into(),
                    index: 4,
                    v4: Some(V4 {
                        addr: Ipv4Addr::new(192, 168, 1, 20),
                        mask: Ipv4Addr::new(255, 255, 255, 0),
                    }),
                    v6: vec![LINK_LOCAL],
                },
                Iface {
                    name: "veth1".into(),
                    index: 5,
                    v4: None,
                    v6: vec![GLOBAL],
                },
            ]
        );
    }

    #[test]
    fn an_ipv4_entry_without_a_netmask_holds_only_itself() {
        let mut e = entry("eth0", OK, Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        e.mask = None;
        let v4 = select_interfaces([e])[0].v4.unwrap();
        assert!(v4.contains(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(!v4.contains(Ipv4Addr::new(10, 0, 0, 2)));
    }

    #[test]
    fn the_served_interfaces_are_never_empty_and_never_loopback() {
        let ifaces = interfaces();
        assert!(!ifaces.is_empty());
        assert!(ifaces.iter().all(|i| i.is_egress_fallback()
            || (i.v4.is_none_or(|v4| !v4.addr.is_loopback())
                && i.v6.iter().all(|a| !a.is_loopback()))));
    }

    // ---- Which interface answers ----

    fn served(name: &str, index: u32, v4: Option<([u8; 4], [u8; 4])>, v6: &[Ipv6Addr]) -> Served {
        let iface = Iface {
            name: name.into(),
            index,
            v4: v4.map(|(addr, mask)| V4 {
                addr: Ipv4Addr::from(addr),
                mask: Ipv4Addr::from(mask),
            }),
            v6: v6.to_vec(),
        };
        Served {
            wire: Wire::build(&Records::new(
                &sample(),
                iface.v4.map(|v| v.addr),
                iface.v6.clone(),
            ))
            .unwrap(),
            iface,
            v4: None,
            v6: None,
            failing: Cell::new(false),
        }
    }

    fn from_v4(ip: [u8; 4]) -> SockAddr {
        SockAddr::from(SocketAddr::from((Ipv4Addr::from(ip), MDNS_PORT)))
    }

    fn from_v6(ip: Ipv6Addr, scope: u32) -> SockAddr {
        SockAddr::from(SocketAddrV6::new(ip, MDNS_PORT, 0, scope))
    }

    #[test]
    fn a_query_is_answered_by_the_interface_it_came_from() {
        // The CVC host: the LAN, a /30 to vm1, and an IPv6-only link.
        let links = [
            served(
                "lan",
                2,
                Some(([192, 168, 1, 20], [255, 255, 255, 0])),
                &[LINK_LOCAL],
            ),
            served("vp1", 3, Some(([10, 0, 101, 1], [255, 255, 255, 252])), &[]),
            served("v6only", 4, None, &[GLOBAL]),
        ];
        let name = |s: Option<&Served>| s.map(|s| s.iface.name.clone());
        // IPv4: by the sender's subnet, else the first IPv4 interface.
        assert_eq!(
            name(answering(&links, None, &from_v4([10, 0, 101, 2]))),
            Some("vp1".into())
        );
        assert_eq!(
            name(answering(&links, None, &from_v4([192, 168, 1, 7]))),
            Some("lan".into())
        );
        assert_eq!(
            name(answering(&links, None, &from_v4([172, 16, 0, 9]))),
            Some("lan".into())
        );
        assert_eq!(
            name(answering(&links, Some(1), &from_v4([172, 16, 0, 9]))),
            Some("vp1".into())
        );
        // IPv6: by the sender's scope id, else the arrival socket, else the
        // first IPv6 interface — never one without IPv6.
        let peer = Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 7);
        assert_eq!(
            name(answering(&links, Some(0), &from_v6(peer, 4))),
            Some("v6only".into())
        );
        assert_eq!(
            name(answering(&links, Some(2), &from_v6(GLOBAL, 0))),
            Some("v6only".into())
        );
        assert_eq!(
            name(answering(&links, Some(1), &from_v6(GLOBAL, 0))),
            Some("lan".into())
        );
        assert_eq!(
            name(answering(&links, None, &from_v6(GLOBAL, 0))),
            Some("lan".into())
        );
        assert!(answering(&[], None, &from_v4([10, 0, 101, 2])).is_none());
    }

    #[test]
    fn an_interface_holds_the_addresses_of_its_subnet() {
        let vp1 = V4 {
            addr: Ipv4Addr::new(10, 0, 101, 1),
            mask: Ipv4Addr::new(255, 255, 255, 252),
        };
        assert!(vp1.contains(Ipv4Addr::new(10, 0, 101, 2)));
        assert!(!vp1.contains(Ipv4Addr::new(10, 0, 101, 5)));
        // The egress fallback (no mask) holds every sender.
        let fallback = V4 {
            addr: Ipv4Addr::new(192, 168, 1, 20),
            mask: Ipv4Addr::UNSPECIFIED,
        };
        assert!(fallback.contains(Ipv4Addr::new(10, 0, 101, 2)));
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
                    Some("https://cvc-host-deadbeef.local:4000/v1"),
                    "TXT accessurl"
                );
                assert!(a_addr.is_some(), "A record present");
                return; // success
            }
        }
    }
}
