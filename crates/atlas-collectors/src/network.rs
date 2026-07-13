//! Network inspector: TCP/UDP connections and listening ports (PRD §9.12,
//! docs/phases.md Phase 2).
//!
//! `GetExtendedTcpTable` / `GetExtendedUdpTable` (owner-pid variants, both
//! AF_INET and AF_INET6) give every connection/bind with its owning pid, local
//! and remote endpoints, and — for TCP — the connection state. Pids are mapped
//! to image names from one `NtQuerySystemInformation` process snapshot (the same
//! syscall the sampler already uses). Remote addresses are best-effort annotated
//! with a domain from the DNS resolver cache (`DnsGetCacheDataTable` + a
//! cache-only `DnsQuery_W` inversion) — never an active/reverse lookup, so the
//! collector emits no network traffic of its own.
//!
//! Everything here is a standard-user read: the extended tables and the DNS
//! cache are readable unprivileged. Cross-process pid→name mapping uses the same
//! snapshot that already succeeds unprivileged for the process list.

#![cfg(windows)]

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::ptr;

use crate::ffi::{
    DnsGetCacheDataTable, DnsQuery_W, DnsRecordListFree, GetExtendedTcpTable, GetExtendedUdpTable,
    AF_INET, AF_INET6, DNS_FREE_RECORD_LIST, DNS_QUERY_NO_WIRE_QUERY, DNS_RECORD_DATA_OFFSET,
    DNS_TYPE_A, DNS_TYPE_AAAA, DWORD, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID,
    MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, MIB_TCP_STATE_LISTEN, MIB_UDP6ROW_OWNER_PID,
    MIB_UDP6TABLE_OWNER_PID, MIB_UDPROW_OWNER_PID, MIB_UDPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_ALL,
    UDP_TABLE_OWNER_PID,
};

/// L4 protocol of a row. Mirrors the proto `L4Protocol`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum L4Protocol {
    Tcp,
    Udp,
}

/// TCP connection state. The discriminants match the `MIB_TCP_STATE` values and
/// the proto `TcpState` enum (CLOSED=1 … DELETE_TCB=12). `Unspecified` for UDP
/// rows or an unrecognised value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpState {
    Unspecified,
    Closed,
    Listen,
    SynSent,
    SynRcvd,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
    DeleteTcb,
}

/// Maps a raw `MIB_TCP_STATE` value to [`TcpState`].
pub fn map_tcp_state(raw: DWORD) -> TcpState {
    match raw {
        1 => TcpState::Closed,
        2 => TcpState::Listen,
        3 => TcpState::SynSent,
        4 => TcpState::SynRcvd,
        5 => TcpState::Established,
        6 => TcpState::FinWait1,
        7 => TcpState::FinWait2,
        8 => TcpState::CloseWait,
        9 => TcpState::Closing,
        10 => TcpState::LastAck,
        11 => TcpState::TimeWait,
        12 => TcpState::DeleteTcb,
        _ => TcpState::Unspecified,
    }
}

/// One connection row (TCP or a UDP flow with a known remote).
#[derive(Debug, Clone)]
pub struct Connection {
    pub pid: u32,
    pub image_name: String,
    pub protocol: L4Protocol,
    pub local_addr: String,
    pub local_port: u16,
    pub remote_addr: String,
    pub remote_port: u16,
    /// Domain resolved from the DNS cache, or empty.
    pub remote_domain: String,
    /// Only meaningful for TCP.
    pub state: TcpState,
    pub is_ipv6: bool,
}

/// One listening endpoint (TCP LISTEN or a bound UDP socket).
#[derive(Debug, Clone)]
pub struct ListeningPort {
    pub protocol: L4Protocol,
    pub bind_addr: String,
    pub port: u16,
    pub pid: u32,
    pub image_name: String,
    pub is_ipv6: bool,
}

/// A `dwLocalPort`/`dwRemotePort` DWORD carries the port in network byte order
/// in its low 16 bits — swap to host order.
fn port_from_dword(raw: DWORD) -> u16 {
    let lo = (raw & 0xFF) as u16;
    let hi = ((raw >> 8) & 0xFF) as u16;
    (lo << 8) | hi
}

/// An IPv4 `in_addr` DWORD (network order) → dotted string. Its bytes in memory
/// order are the octets directly.
fn ipv4_to_string(raw: DWORD) -> String {
    let o = raw.to_ne_bytes();
    Ipv4Addr::new(o[0], o[1], o[2], o[3]).to_string()
}

/// A 16-byte `in6_addr` (network order) → canonical IPv6 string.
fn ipv6_to_string(bytes: &[u8; 16]) -> String {
    Ipv6Addr::from(*bytes).to_string()
}

/// The all-zeros IPv4/IPv6 "any" address a listener binds to (0.0.0.0 / ::).
fn is_unspecified(addr: &str) -> bool {
    addr == "0.0.0.0" || addr == "::"
}

/// Lists active connections. TCP rows in any non-LISTEN state are always
/// included; `include_listening` also folds in TCP LISTEN rows and UDP binds
/// (which have no remote). Domains are attached from the DNS cache best-effort.
pub fn list_connections(include_listening: bool) -> Vec<Connection> {
    let names = pid_name_map();
    let dns = DnsCache::load();
    let mut out = Vec::new();

    for r in tcp4_rows() {
        let state = map_tcp_state(r.dwState);
        if state == TcpState::Listen && !include_listening {
            continue;
        }
        let remote_addr = ipv4_to_string(r.dwRemoteAddr);
        let remote_domain = dns.lookup(&remote_addr);
        out.push(Connection {
            pid: r.dwOwningPid,
            image_name: names.get(&r.dwOwningPid).cloned().unwrap_or_default(),
            protocol: L4Protocol::Tcp,
            local_addr: ipv4_to_string(r.dwLocalAddr),
            local_port: port_from_dword(r.dwLocalPort),
            remote_addr,
            remote_port: port_from_dword(r.dwRemotePort),
            remote_domain,
            state,
            is_ipv6: false,
        });
    }
    for r in tcp6_rows() {
        let state = map_tcp_state(r.dwState);
        if state == TcpState::Listen && !include_listening {
            continue;
        }
        let remote_addr = ipv6_to_string(&r.ucRemoteAddr);
        let remote_domain = dns.lookup(&remote_addr);
        out.push(Connection {
            pid: r.dwOwningPid,
            image_name: names.get(&r.dwOwningPid).cloned().unwrap_or_default(),
            protocol: L4Protocol::Tcp,
            local_addr: ipv6_to_string(&r.ucLocalAddr),
            local_port: port_from_dword(r.dwLocalPort),
            remote_addr,
            remote_port: port_from_dword(r.dwRemotePort),
            remote_domain,
            state,
            is_ipv6: true,
        });
    }

    // UDP is connectionless — no remote endpoint — so it only appears when the
    // caller asked for listening/bound sockets too.
    if include_listening {
        for r in udp4_rows() {
            out.push(Connection {
                pid: r.dwOwningPid,
                image_name: names.get(&r.dwOwningPid).cloned().unwrap_or_default(),
                protocol: L4Protocol::Udp,
                local_addr: ipv4_to_string(r.dwLocalAddr),
                local_port: port_from_dword(r.dwLocalPort),
                remote_addr: String::new(),
                remote_port: 0,
                remote_domain: String::new(),
                state: TcpState::Unspecified,
                is_ipv6: false,
            });
        }
        for r in udp6_rows() {
            out.push(Connection {
                pid: r.dwOwningPid,
                image_name: names.get(&r.dwOwningPid).cloned().unwrap_or_default(),
                protocol: L4Protocol::Udp,
                local_addr: ipv6_to_string(&r.ucLocalAddr),
                local_port: port_from_dword(r.dwLocalPort),
                remote_addr: String::new(),
                remote_port: 0,
                remote_domain: String::new(),
                state: TcpState::Unspecified,
                is_ipv6: true,
            });
        }
    }
    out
}

/// Lists listening endpoints: TCP rows in the LISTEN state plus every UDP bind
/// (a bound UDP socket is the closest analogue of "listening").
pub fn list_listening_ports() -> Vec<ListeningPort> {
    let names = pid_name_map();
    let mut out = Vec::new();

    for r in tcp4_rows() {
        if r.dwState != MIB_TCP_STATE_LISTEN {
            continue;
        }
        out.push(ListeningPort {
            protocol: L4Protocol::Tcp,
            bind_addr: ipv4_to_string(r.dwLocalAddr),
            port: port_from_dword(r.dwLocalPort),
            pid: r.dwOwningPid,
            image_name: names.get(&r.dwOwningPid).cloned().unwrap_or_default(),
            is_ipv6: false,
        });
    }
    for r in tcp6_rows() {
        if r.dwState != MIB_TCP_STATE_LISTEN {
            continue;
        }
        out.push(ListeningPort {
            protocol: L4Protocol::Tcp,
            bind_addr: ipv6_to_string(&r.ucLocalAddr),
            port: port_from_dword(r.dwLocalPort),
            pid: r.dwOwningPid,
            image_name: names.get(&r.dwOwningPid).cloned().unwrap_or_default(),
            is_ipv6: true,
        });
    }
    for r in udp4_rows() {
        out.push(ListeningPort {
            protocol: L4Protocol::Udp,
            bind_addr: ipv4_to_string(r.dwLocalAddr),
            port: port_from_dword(r.dwLocalPort),
            pid: r.dwOwningPid,
            image_name: names.get(&r.dwOwningPid).cloned().unwrap_or_default(),
            is_ipv6: false,
        });
    }
    for r in udp6_rows() {
        out.push(ListeningPort {
            protocol: L4Protocol::Udp,
            bind_addr: ipv6_to_string(&r.ucLocalAddr),
            port: port_from_dword(r.dwLocalPort),
            pid: r.dwOwningPid,
            image_name: names.get(&r.dwOwningPid).cloned().unwrap_or_default(),
            is_ipv6: true,
        });
    }
    // Sort listeners by port for a stable, readable view.
    out.sort_by(|a, b| a.port.cmp(&b.port).then(a.pid.cmp(&b.pid)));
    out
}

/// pid → image name from one process snapshot (best-effort; an empty map just
/// leaves names blank).
fn pid_name_map() -> HashMap<u32, String> {
    match crate::snapshot::snapshot_processes() {
        Ok(procs) => procs.into_iter().map(|p| (p.pid, p.image_name)).collect(),
        Err(_) => HashMap::new(),
    }
}

// --- Extended-table readers -------------------------------------------------

/// Runs the two-call size dance for one extended table and returns the raw
/// buffer. `af` selects the address family; `tcp` picks the TCP vs UDP call.
fn read_table(tcp: bool, af: u32) -> Option<Vec<u8>> {
    let mut size: DWORD = 0;
    // First call: probe the required size.
    // SAFETY: null buffer with a live size out-param is the documented probe.
    unsafe {
        if tcp {
            GetExtendedTcpTable(
                ptr::null_mut(),
                &mut size,
                0,
                af,
                TCP_TABLE_OWNER_PID_ALL,
                0,
            );
        } else {
            GetExtendedUdpTable(ptr::null_mut(), &mut size, 0, af, UDP_TABLE_OWNER_PID, 0);
        }
    }
    if size == 0 {
        return None;
    }
    // The table can grow between the two calls; retry a few times on the
    // ERROR_INSUFFICIENT_BUFFER (122) that signals a racing grow.
    for _ in 0..4 {
        let mut buf = vec![0u8; size as usize];
        // SAFETY: buf is sized to `size`; the call fills it or reports a new size.
        let rc = unsafe {
            if tcp {
                GetExtendedTcpTable(
                    buf.as_mut_ptr() as *mut _,
                    &mut size,
                    0,
                    af,
                    TCP_TABLE_OWNER_PID_ALL,
                    0,
                )
            } else {
                GetExtendedUdpTable(
                    buf.as_mut_ptr() as *mut _,
                    &mut size,
                    0,
                    af,
                    UDP_TABLE_OWNER_PID,
                    0,
                )
            }
        };
        if rc == 0 {
            return Some(buf);
        }
        if rc != 122 {
            return None; // any error other than "buffer too small" is terminal
        }
        // else loop with the enlarged `size`.
    }
    None
}

/// Reads the IPv4 TCP owner-pid table into owned rows.
fn tcp4_rows() -> Vec<MIB_TCPROW_OWNER_PID> {
    let buf = match read_table(true, AF_INET) {
        Some(b) => b,
        None => return Vec::new(),
    };
    // SAFETY: the buffer starts with a MIB_TCPTABLE_OWNER_PID header; the row
    // array follows immediately and is `dwNumEntries` long (Windows fills it).
    unsafe { parse_rows::<MIB_TCPTABLE_OWNER_PID, MIB_TCPROW_OWNER_PID>(&buf) }
}

/// Reads the IPv6 TCP owner-pid table into owned rows.
fn tcp6_rows() -> Vec<MIB_TCP6ROW_OWNER_PID> {
    let buf = match read_table(true, AF_INET6) {
        Some(b) => b,
        None => return Vec::new(),
    };
    // SAFETY: as tcp4_rows, with the IPv6 row/table layout.
    unsafe { parse_rows::<MIB_TCP6TABLE_OWNER_PID, MIB_TCP6ROW_OWNER_PID>(&buf) }
}

/// Reads the IPv4 UDP owner-pid table into owned rows.
fn udp4_rows() -> Vec<MIB_UDPROW_OWNER_PID> {
    let buf = match read_table(false, AF_INET) {
        Some(b) => b,
        None => return Vec::new(),
    };
    // SAFETY: as above, UDP layout.
    unsafe { parse_rows::<MIB_UDPTABLE_OWNER_PID, MIB_UDPROW_OWNER_PID>(&buf) }
}

/// Reads the IPv6 UDP owner-pid table into owned rows.
fn udp6_rows() -> Vec<MIB_UDP6ROW_OWNER_PID> {
    let buf = match read_table(false, AF_INET6) {
        Some(b) => b,
        None => return Vec::new(),
    };
    // SAFETY: as above, IPv6 UDP layout.
    unsafe { parse_rows::<MIB_UDP6TABLE_OWNER_PID, MIB_UDP6ROW_OWNER_PID>(&buf) }
}

/// Walks a `{ DWORD dwNumEntries; Row table[] }` buffer into a `Vec<Row>`. The
/// table header type `H` must have `dwNumEntries` as its first field (all four
/// MIB_*TABLE_OWNER_PID headers do).
///
/// # Safety
/// `buf` must hold a valid table header followed by `dwNumEntries` `Row`s, as
/// filled by `GetExtended{Tcp,Udp}Table`.
unsafe fn parse_rows<H, Row: Copy>(buf: &[u8]) -> Vec<Row> {
    if buf.len() < std::mem::size_of::<u32>() {
        return Vec::new();
    }
    // dwNumEntries is the first DWORD of every table header.
    let count = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let header = std::mem::size_of::<H>();
    let row = std::mem::size_of::<Row>();
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let off = header + i * row;
        if off + row > buf.len() {
            break; // truncated buffer — return what parsed cleanly
        }
        // SAFETY: `off..off+row` is in bounds and `Row: Copy` is a plain
        // repr(C) POD; read unaligned to be safe against any packing.
        let r = ptr::read_unaligned(buf.as_ptr().add(off) as *const Row);
        out.push(r);
    }
    out
}

// --- DNS resolver cache -----------------------------------------------------

/// A best-effort inversion of the DNS resolver cache: remote address string →
/// domain name. Built by enumerating cached names (`DnsGetCacheDataTable`) and
/// resolving each A/AAAA name from cache only (`DnsQuery_W` +
/// `DNS_QUERY_NO_WIRE_QUERY`), so no packet is ever sent. Empty when the cache
/// is unreadable — the collector then simply leaves `remote_domain` blank.
struct DnsCache {
    addr_to_name: HashMap<String, String>,
}

impl DnsCache {
    fn load() -> Self {
        let mut addr_to_name = HashMap::new();
        for (name, wtype) in cache_names() {
            if wtype != DNS_TYPE_A && wtype != DNS_TYPE_AAAA {
                continue;
            }
            for addr in resolve_cached(&name, wtype) {
                // First writer wins; a name maps many addrs, an addr one name.
                addr_to_name.entry(addr).or_insert_with(|| name.clone());
            }
        }
        Self { addr_to_name }
    }

    fn lookup(&self, addr: &str) -> String {
        if addr.is_empty() || is_unspecified(addr) {
            return String::new();
        }
        self.addr_to_name.get(addr).cloned().unwrap_or_default()
    }
}

/// Enumerates `(name, record_type)` from the resolver cache. The linked list is
/// walked read-only and intentionally not freed: `DnsGetCacheDataTable`'s free
/// path is undocumented, so on an on-demand read we accept a small, bounded
/// per-call leak rather than risk corrupting the heap (PRD §9.6.7 — honesty
/// over cleverness). Returns empty if the cache is unreadable.
fn cache_names() -> Vec<(String, u16)> {
    let mut head: *mut crate::ffi::DNS_CACHE_ENTRY = ptr::null_mut();
    // SAFETY: `head` is a live out-param; the call fills it with the cache list.
    let ok = unsafe { DnsGetCacheDataTable(&mut head) };
    if ok == 0 || head.is_null() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cur = head;
    // Bound the walk so a corrupt list can never loop forever.
    let mut guard = 0;
    while !cur.is_null() && guard < 100_000 {
        // SAFETY: `cur` is a non-null node from the cache list.
        let node = unsafe { &*cur };
        if !node.pszName.is_null() {
            let name = wide_to_string(node.pszName);
            if !name.is_empty() {
                out.push((name, node.wType));
            }
        }
        cur = node.pNext;
        guard += 1;
    }
    out
}

/// Resolves `name` of `wtype` from the cache only and returns its address
/// strings (empty on a miss). Uses `DNS_QUERY_NO_WIRE_QUERY` so nothing goes on
/// the wire; the result record list is freed via the documented
/// `DnsRecordListFree`.
fn resolve_cached(name: &str, wtype: u16) -> Vec<String> {
    let wname = to_wide(name);
    let mut records: *mut crate::ffi::DNS_RECORD_HEAD = ptr::null_mut();
    // SAFETY: wname is NUL-terminated; records is a live out-param.
    let rc = unsafe {
        DnsQuery_W(
            wname.as_ptr(),
            wtype,
            DNS_QUERY_NO_WIRE_QUERY,
            ptr::null_mut(),
            &mut records,
            ptr::null_mut(),
        )
    };
    if rc != 0 || records.is_null() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cur = records;
    let mut guard = 0;
    while !cur.is_null() && guard < 10_000 {
        // SAFETY: `cur` is a non-null record from DnsQuery_W's list.
        let head = unsafe { &*cur };
        // SAFETY: the Data union sits at DNS_RECORD_DATA_OFFSET past the head.
        let data = unsafe { (cur as *const u8).add(DNS_RECORD_DATA_OFFSET) };
        if head.wType == DNS_TYPE_A && wtype == DNS_TYPE_A {
            // SAFETY: an A record's Data is a 4-byte IP4_ADDRESS (network order).
            let raw = unsafe { ptr::read_unaligned(data as *const u32) };
            out.push(ipv4_to_string(raw));
        } else if head.wType == DNS_TYPE_AAAA && wtype == DNS_TYPE_AAAA {
            // SAFETY: an AAAA record's Data is a 16-byte IP6_ADDRESS.
            let mut bytes = [0u8; 16];
            unsafe { ptr::copy_nonoverlapping(data, bytes.as_mut_ptr(), 16) };
            out.push(ipv6_to_string(&bytes));
        }
        cur = head.pNext;
        guard += 1;
    }
    // SAFETY: `records` is a DnsQuery_W list freed exactly once here.
    unsafe { DnsRecordListFree(records, DNS_FREE_RECORD_LIST) };
    out
}

/// UTF-16, NUL-terminated, for a `*const u16` Win32 argument.
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Reads a NUL-terminated UTF-16 string from a raw pointer (bounded scan).
///
/// # Safety
/// `ptr` must be null or point to a NUL-terminated UTF-16 string.
fn wide_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    // SAFETY: bounded scan for the terminating NUL (cap guards a missing NUL).
    unsafe {
        while len < 4096 && *ptr.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_swaps_network_byte_order() {
        // Port 80 (0x0050) stored network-order in the low 16 bits reads as a
        // DWORD whose bytes are [0x00, 0x50, ..], i.e. 0x5000 little-endian.
        assert_eq!(port_from_dword(0x0000_5000), 80);
        // Port 443 (0x01BB) → network bytes [0x01, 0xBB] → DWORD 0xBB01.
        assert_eq!(port_from_dword(0x0000_BB01), 443);
        // Port 65535.
        assert_eq!(port_from_dword(0x0000_FFFF), 65535);
    }

    #[test]
    fn ipv4_octets_in_memory_order() {
        // 127.0.0.1 as an in_addr (network order) reads as 0x0100007F on LE.
        assert_eq!(ipv4_to_string(0x0100_007F), "127.0.0.1");
        // 192.168.1.10 → bytes C0 A8 01 0A → LE DWORD 0x0A01A8C0.
        assert_eq!(ipv4_to_string(0x0A01_A8C0), "192.168.1.10");
        // 0.0.0.0 (the "any" bind).
        assert_eq!(ipv4_to_string(0), "0.0.0.0");
    }

    #[test]
    fn ipv6_formats_canonically() {
        let mut loopback = [0u8; 16];
        loopback[15] = 1;
        assert_eq!(ipv6_to_string(&loopback), "::1");
        assert_eq!(ipv6_to_string(&[0u8; 16]), "::");
    }

    #[test]
    fn tcp_state_maps_to_proto_offsets() {
        assert_eq!(map_tcp_state(2), TcpState::Listen);
        assert_eq!(map_tcp_state(5), TcpState::Established);
        assert_eq!(map_tcp_state(11), TcpState::TimeWait);
        assert_eq!(map_tcp_state(12), TcpState::DeleteTcb);
        assert_eq!(map_tcp_state(99), TcpState::Unspecified);
    }

    #[test]
    fn unspecified_addr_detection() {
        assert!(is_unspecified("0.0.0.0"));
        assert!(is_unspecified("::"));
        assert!(!is_unspecified("10.0.0.1"));
    }

    /// The MIB row structs are offset-locked so a wrong field order (which would
    /// misparse every connection) is caught at test time, matching the existing
    /// snapshot/inspector offset tests.
    #[test]
    fn mib_row_layouts() {
        use std::mem::{offset_of, size_of};
        assert_eq!(size_of::<MIB_TCPROW_OWNER_PID>(), 24);
        assert_eq!(offset_of!(MIB_TCPROW_OWNER_PID, dwState), 0);
        assert_eq!(offset_of!(MIB_TCPROW_OWNER_PID, dwLocalAddr), 4);
        assert_eq!(offset_of!(MIB_TCPROW_OWNER_PID, dwLocalPort), 8);
        assert_eq!(offset_of!(MIB_TCPROW_OWNER_PID, dwRemoteAddr), 12);
        assert_eq!(offset_of!(MIB_TCPROW_OWNER_PID, dwRemotePort), 16);
        assert_eq!(offset_of!(MIB_TCPROW_OWNER_PID, dwOwningPid), 20);

        assert_eq!(size_of::<MIB_TCP6ROW_OWNER_PID>(), 56);
        assert_eq!(offset_of!(MIB_TCP6ROW_OWNER_PID, ucLocalAddr), 0);
        assert_eq!(offset_of!(MIB_TCP6ROW_OWNER_PID, dwLocalScopeId), 16);
        assert_eq!(offset_of!(MIB_TCP6ROW_OWNER_PID, dwLocalPort), 20);
        assert_eq!(offset_of!(MIB_TCP6ROW_OWNER_PID, ucRemoteAddr), 24);
        assert_eq!(offset_of!(MIB_TCP6ROW_OWNER_PID, dwRemoteScopeId), 40);
        assert_eq!(offset_of!(MIB_TCP6ROW_OWNER_PID, dwRemotePort), 44);
        assert_eq!(offset_of!(MIB_TCP6ROW_OWNER_PID, dwState), 48);
        assert_eq!(offset_of!(MIB_TCP6ROW_OWNER_PID, dwOwningPid), 52);

        assert_eq!(size_of::<MIB_UDPROW_OWNER_PID>(), 12);
        assert_eq!(offset_of!(MIB_UDPROW_OWNER_PID, dwOwningPid), 8);
        assert_eq!(size_of::<MIB_UDP6ROW_OWNER_PID>(), 28);
        assert_eq!(offset_of!(MIB_UDP6ROW_OWNER_PID, dwLocalPort), 20);
        assert_eq!(offset_of!(MIB_UDP6ROW_OWNER_PID, dwOwningPid), 24);
    }

    /// `parse_rows` reads exactly `dwNumEntries` rows and stops at a truncated
    /// tail rather than reading out of bounds.
    #[test]
    fn parse_rows_respects_count_and_bounds() {
        // Build a 2-row IPv4 UDP table by hand: header {count=2} + 2 rows.
        let mut buf = Vec::new();
        buf.extend_from_slice(&2u32.to_ne_bytes()); // dwNumEntries
        for pid in [111u32, 222u32] {
            buf.extend_from_slice(&0x0100_007Fu32.to_ne_bytes()); // 127.0.0.1
            buf.extend_from_slice(&0x0000_5000u32.to_ne_bytes()); // port 80
            buf.extend_from_slice(&pid.to_ne_bytes());
        }
        // SAFETY: buf is a well-formed MIB_UDPTABLE_OWNER_PID with 2 rows.
        let rows: Vec<MIB_UDPROW_OWNER_PID> =
            unsafe { parse_rows::<MIB_UDPTABLE_OWNER_PID, MIB_UDPROW_OWNER_PID>(&buf) };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].dwOwningPid, 111);
        assert_eq!(rows[1].dwOwningPid, 222);
        assert_eq!(port_from_dword(rows[0].dwLocalPort), 80);

        // A header claiming 5 rows but only 1 present returns just the 1.
        let mut trunc = Vec::new();
        trunc.extend_from_slice(&5u32.to_ne_bytes());
        trunc.extend_from_slice(&0u32.to_ne_bytes());
        trunc.extend_from_slice(&0u32.to_ne_bytes());
        trunc.extend_from_slice(&7u32.to_ne_bytes());
        let rows2: Vec<MIB_UDPROW_OWNER_PID> =
            unsafe { parse_rows::<MIB_UDPTABLE_OWNER_PID, MIB_UDPROW_OWNER_PID>(&trunc) };
        assert_eq!(rows2.len(), 1);
        assert_eq!(rows2[0].dwOwningPid, 7);
    }
}
