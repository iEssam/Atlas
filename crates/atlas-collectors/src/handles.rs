//! Per-process open-handle inspector (docs/phases.md Phase 2, PRD §9.4).
//!
//! `NtQuerySystemInformation(SystemExtendedHandleInformation)` returns every
//! open handle system-wide with its owning pid; we filter to the target. Each
//! handle's object-type index is mapped to a name via a cached type table built
//! from `NtQueryObject(ObjectTypesInformation)` — this needs no elevation and
//! covers cross-user processes.
//!
//! Object names are resolved by duplicating the handle into our process
//! (`DuplicateHandle`, needs `PROCESS_DUP_HANDLE` on the source) and calling
//! `NtQueryObject(ObjectNameInformation)`. That query can block forever on
//! certain handles (some named pipes), so it runs on a **killable worker thread
//! with a timeout** (the mandatory design): on timeout we abandon the worker
//! (leaving its blocked query parked) and mark `names_limited`, rather than
//! hanging the RPC. When duplication is denied (no elevation / other user) we
//! still return the handle values + types and set `names_limited`.

#![cfg(windows)]

use std::sync::mpsc;
use std::time::Duration;

use crate::ffi::{
    CloseHandle, DuplicateHandle, GetCurrentProcess, NtQueryObject, NtQuerySystemInformation,
    OpenProcess, DUPLICATE_SAME_ACCESS, HANDLE, OBJECT_NAME_INFORMATION_CLASS,
    OBJECT_TYPES_INFORMATION_CLASS, OBJECT_TYPE_INFORMATION, PROCESS_DUP_HANDLE,
    STATUS_BUFFER_OVERFLOW, STATUS_BUFFER_TOO_SMALL, STATUS_INFO_LENGTH_MISMATCH, STATUS_SUCCESS,
    SYSTEM_EXTENDED_HANDLE_INFORMATION_CLASS, SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX, UNICODE_STRING,
};

/// Per-handle timeout for the (potentially blocking) `NtQueryObject` name query.
const NAME_QUERY_TIMEOUT: Duration = Duration::from_millis(100);

/// Default cap on returned handles when the caller passes `limit == 0`.
const DEFAULT_LIMIT: usize = 10_000;

/// One open handle — mirrors the proto `HandleRow`.
#[derive(Debug, Clone)]
pub struct HandleRow {
    pub handle: u64,
    pub type_name: String,
    pub name: String,
    pub granted_access: u32,
}

/// Result of a handle request — mirrors the proto `ListHandlesReply`.
#[derive(Debug, Clone)]
pub struct HandlesResult {
    pub handles: Vec<HandleRow>,
    pub truncated: bool,
    /// Name resolution was restricted (no elevation / handle not duplicable).
    pub names_limited: bool,
}

/// A `HANDLE` moved into the name-resolution worker thread. The raw pointer is
/// only ever handed to `NtQueryObject`; wrapping it lets us `Send` it.
struct SendHandle(HANDLE);
// SAFETY: the duplicated handle is used solely by the worker thread we hand it
// to; it is never shared with, or freed by, another thread while in use.
unsafe impl Send for SendHandle {}

/// Outcome of resolving one handle's object name. Distinguishes a genuinely
/// nameless object (normal — many Events/Mutants have no name) from a real
/// restriction (duplication denied / query blocked), so `names_limited` reflects
/// only the latter (PRD §9.6.7 honesty).
enum NameResolution {
    Named(String),
    Unnamed,
    Restricted,
}

/// Lists the open handles owned by `pid`, filtered to `type_filter` (empty =
/// all). Respects `limit` (0 = default cap) and reports `truncated` /
/// `names_limited` honestly.
pub fn list_handles(pid: u32, type_filter: &str, limit: u32) -> HandlesResult {
    let cap = if limit == 0 {
        DEFAULT_LIMIT
    } else {
        limit as usize
    };

    let type_table = build_type_table();

    let entries = match query_extended_handles() {
        Some(e) => e,
        None => {
            return HandlesResult {
                handles: Vec::new(),
                truncated: false,
                names_limited: false,
            }
        }
    };

    // Open the source process once for duplication (name resolution). If denied,
    // every name is unresolved but types + values still come back.
    // SAFETY: plain OpenProcess; NULL on failure.
    let src = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, pid) };
    let src_ok = !src.is_null();
    let mut names_limited = !src_ok;

    let mut rows = Vec::new();
    let mut truncated = false;

    for e in entries.iter().filter(|e| e.UniqueProcessId as u32 == pid) {
        let type_name = type_table
            .get(&(e.ObjectTypeIndex as u32))
            .cloned()
            .unwrap_or_default();

        if !type_filter.is_empty() && !type_name.eq_ignore_ascii_case(type_filter) {
            continue;
        }
        if rows.len() >= cap {
            truncated = true;
            break;
        }

        let mut name = String::new();
        if src_ok {
            match resolve_handle_name(src, e.HandleValue as HANDLE) {
                NameResolution::Named(n) => name = n,
                NameResolution::Unnamed => {}
                NameResolution::Restricted => names_limited = true,
            }
        }

        rows.push(HandleRow {
            handle: e.HandleValue as u64,
            type_name,
            name,
            granted_access: e.GrantedAccess,
        });
    }

    if src_ok {
        // SAFETY: src came from OpenProcess and is closed once here.
        unsafe {
            CloseHandle(src);
        }
    }

    HandlesResult {
        handles: rows,
        truncated,
        names_limited,
    }
}

/// Duplicates `handle` out of `src_process` into ours and resolves its object
/// name on a worker thread with a timeout. Duplication failure or a query that
/// blocks past [`NAME_QUERY_TIMEOUT`] is `Restricted`; a successful query with no
/// name is `Unnamed`; otherwise `Named`.
fn resolve_handle_name(src_process: HANDLE, handle: HANDLE) -> NameResolution {
    let mut dup: HANDLE = std::ptr::null_mut();
    // SAFETY: src_process/handle are valid; dup is a live out-param. We request
    // no extra access (0) with DUPLICATE_SAME_ACCESS so the name query works.
    let ok = unsafe {
        DuplicateHandle(
            src_process,
            handle,
            GetCurrentProcess(),
            &mut dup,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 || dup.is_null() {
        return NameResolution::Restricted;
    }

    // Run the (possibly blocking) NtQueryObject on a worker thread. On success
    // we own `dup` again and close it; on timeout the worker is still inside
    // NtQueryObject holding `dup`, so we must NOT close it — we abandon both.
    let send = SendHandle(dup);
    let (tx, rx) = mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        let send = send;
        let name = query_object_name(send.0);
        // If the receiver already timed out and went away, this send is dropped.
        let _ = tx.send(name);
    });

    match rx.recv_timeout(NAME_QUERY_TIMEOUT) {
        Ok(name) => {
            // Worker finished and is done touching `dup`; safe to close it.
            // SAFETY: dup came from DuplicateHandle; closed once, worker done.
            unsafe {
                CloseHandle(dup);
            }
            match name {
                Some(n) if !n.is_empty() => NameResolution::Named(n),
                _ => NameResolution::Unnamed,
            }
        }
        Err(_) => {
            // Timed out: the worker is parked in NtQueryObject on `dup`. Leak
            // both (rare) rather than risk a use-after-close.
            NameResolution::Restricted
        }
    }
}

/// `NtQueryObject(ObjectNameInformation)` → the object name, or `None`.
fn query_object_name(h: HANDLE) -> Option<String> {
    // Start generous; ObjectNameInformation is a UNICODE_STRING + inline buffer.
    let mut buf = vec![0u8; 2048];
    for _ in 0..3 {
        let mut ret: u32 = 0;
        // SAFETY: buf is a live sink of buf.len() bytes; ret out-param live.
        let status = unsafe {
            NtQueryObject(
                h,
                OBJECT_NAME_INFORMATION_CLASS,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                &mut ret,
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH
            || status == STATUS_BUFFER_OVERFLOW
            || status == STATUS_BUFFER_TOO_SMALL
        {
            let need = (ret as usize).max(buf.len() * 2).max(4096);
            buf.resize(need, 0);
            continue;
        }
        if status != STATUS_SUCCESS {
            return None;
        }
        // The buffer starts with a UNICODE_STRING whose Buffer points inside it.
        let us: UNICODE_STRING = unsafe { std::ptr::read_unaligned(buf.as_ptr().cast()) };
        return unicode_string_in_buffer(&us, &buf);
    }
    None
}

/// Decodes a self-referential `UNICODE_STRING` (Buffer points inside `buf`).
fn unicode_string_in_buffer(us: &UNICODE_STRING, buf: &[u8]) -> Option<String> {
    let units = (us.Length / 2) as usize;
    if units == 0 {
        return None;
    }
    let base = buf.as_ptr() as usize;
    let ptr = us.Buffer as usize;
    if ptr < base || ptr + units * 2 > base + buf.len() {
        return None;
    }
    // SAFETY: bounds checked against `buf`.
    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u16, units) };
    Some(String::from_utf16_lossy(slice))
}

/// Runs the growing-buffer `SystemExtendedHandleInformation` query and returns a
/// copy of every handle-table entry.
fn query_extended_handles() -> Option<Vec<SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX>> {
    let mut buf = vec![0u8; 1 << 20];
    for _ in 0..8 {
        let mut ret: u32 = 0;
        // SAFETY: buf is a live sink; ret out-param live.
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_EXTENDED_HANDLE_INFORMATION_CLASS,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                &mut ret,
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH
            || status == STATUS_BUFFER_OVERFLOW
            || status == STATUS_BUFFER_TOO_SMALL
        {
            let need = (ret as usize).max(buf.len() * 2);
            buf.resize(need, 0);
            continue;
        }
        if status != STATUS_SUCCESS {
            return None;
        }
        return Some(parse_extended_handles(&buf));
    }
    None
}

/// Parses a `SystemExtendedHandleInformation` buffer: a `usize` count + reserved,
/// then that many `SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX`.
fn parse_extended_handles(buf: &[u8]) -> Vec<SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX> {
    let ptr_size = std::mem::size_of::<usize>();
    if buf.len() < ptr_size * 2 {
        return Vec::new();
    }
    // SAFETY: buffer is at least two usizes long.
    let count = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const usize) };
    let entry_size = std::mem::size_of::<SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX>();
    let arr_base = ptr_size * 2; // NumberOfHandles + Reserved
    let max = (buf.len() - arr_base) / entry_size;
    let n = count.min(max);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let off = arr_base + i * entry_size;
        // SAFETY: off + entry_size <= buf.len() by construction of `n`.
        let e = unsafe {
            std::ptr::read_unaligned(
                buf.as_ptr().add(off) as *const SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX
            )
        };
        out.push(e);
    }
    out
}

/// Builds the object-type-index → type-name table from
/// `NtQueryObject(ObjectTypesInformation)`. Best-effort: an empty table just
/// means handle rows carry empty type names.
fn build_type_table() -> std::collections::HashMap<u32, String> {
    let mut buf = vec![0u8; 64 << 10];
    let mut filled = false;
    for _ in 0..6 {
        let mut ret: u32 = 0;
        // SAFETY: a NULL handle is valid for the global ObjectTypesInformation
        // query; buf is a live sink.
        let status = unsafe {
            NtQueryObject(
                std::ptr::null_mut(),
                OBJECT_TYPES_INFORMATION_CLASS,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                &mut ret,
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH
            || status == STATUS_BUFFER_OVERFLOW
            || status == STATUS_BUFFER_TOO_SMALL
        {
            let need = (ret as usize).max(buf.len() * 2);
            buf.resize(need, 0);
            continue;
        }
        if status != STATUS_SUCCESS {
            return std::collections::HashMap::new();
        }
        filled = true;
        break;
    }
    if !filled {
        return std::collections::HashMap::new();
    }
    parse_type_table(&buf)
}

/// Parses an `OBJECT_TYPES_INFORMATION` buffer into an index → name map. The
/// header is a `u32 NumberOfTypes` (padded to pointer alignment); each
/// `OBJECT_TYPE_INFORMATION` is followed by its inline name buffer, and the next
/// entry is pointer-aligned after it.
fn parse_type_table(buf: &[u8]) -> std::collections::HashMap<u32, String> {
    let mut map = std::collections::HashMap::new();
    let ptr_size = std::mem::size_of::<usize>();
    if buf.len() < ptr_size {
        return map;
    }
    // SAFETY: buffer is at least a u32 long.
    let count = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const u32) } as usize;
    let struct_size = std::mem::size_of::<OBJECT_TYPE_INFORMATION>();
    let base = buf.as_ptr() as usize;
    // First entry: header (u32) rounded up to pointer alignment.
    let mut off = align_up(ptr_size, ptr_size); // == ptr_size (8 on 64-bit)
                                                // Sequential position (1-based) is the fallback index when the explicit
                                                // TypeIndex byte is 0 (pre-Win8.1 kernels).
    for i in 0..count {
        if off + struct_size > buf.len() {
            break;
        }
        // SAFETY: off + struct_size <= buf.len().
        let ti = unsafe {
            std::ptr::read_unaligned(buf.as_ptr().add(off) as *const OBJECT_TYPE_INFORMATION)
        };
        let name = read_type_name(&ti.TypeName, base, buf.len());
        let index = if ti.TypeIndex != 0 {
            ti.TypeIndex as u32
        } else {
            (i + 2) as u32
        };
        if !name.is_empty() {
            map.insert(index, name);
        }
        // Advance past this struct + its inline name buffer, pointer-aligned.
        let advance = struct_size + ti.TypeName.MaximumLength as usize;
        off += align_up(advance, ptr_size);
    }
    map
}

/// Reads an `OBJECT_TYPE_INFORMATION.TypeName` whose Buffer points into the same
/// query buffer, bounds-checked.
fn read_type_name(us: &UNICODE_STRING, base: usize, buf_len: usize) -> String {
    let units = (us.Length / 2) as usize;
    if units == 0 {
        return String::new();
    }
    let ptr = us.Buffer as usize;
    if ptr < base || ptr + units * 2 > base + buf_len {
        return String::new();
    }
    // SAFETY: bounds checked against the query buffer.
    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u16, units) };
    String::from_utf16_lossy(slice)
}

/// Rounds `value` up to a multiple of `align` (a power of two).
fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    /// Locks the SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX layout to the documented
    /// 64-bit offsets — the entry stride and the pid/type-index reads depend on it.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn handle_entry_layout() {
        assert_eq!(
            offset_of!(SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX, UniqueProcessId),
            0x08
        );
        assert_eq!(
            offset_of!(SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX, HandleValue),
            0x10
        );
        assert_eq!(
            offset_of!(SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX, GrantedAccess),
            0x18
        );
        assert_eq!(
            offset_of!(SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX, ObjectTypeIndex),
            0x1E
        );
        assert_eq!(size_of::<SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX>(), 0x28);
    }

    /// Locks the OBJECT_TYPE_INFORMATION layout — the TypeIndex byte position and
    /// the struct size drive the type-table walk.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn object_type_information_layout() {
        assert_eq!(offset_of!(OBJECT_TYPE_INFORMATION, TypeName), 0x00);
        assert_eq!(offset_of!(OBJECT_TYPE_INFORMATION, TypeIndex), 0x5A);
        assert_eq!(size_of::<OBJECT_TYPE_INFORMATION>(), 0x68);
    }

    #[test]
    fn align_up_rounds_to_pointer() {
        assert_eq!(align_up(1, 8), 8);
        assert_eq!(align_up(8, 8), 8);
        assert_eq!(align_up(9, 8), 16);
        assert_eq!(align_up(0, 8), 0);
    }

    /// Self-target smoke: our own process owns handles, the type table resolves
    /// common types, and at least some names come back (own handles duplicate).
    #[test]
    fn own_process_handles_present() {
        let me = std::process::id();
        let res = list_handles(me, "", 0);
        assert!(!res.handles.is_empty(), "own process holds handles");
        // The type table should resolve most handles to a named type.
        let typed = res
            .handles
            .iter()
            .filter(|h| !h.type_name.is_empty())
            .count();
        assert!(typed > 0, "type table should name at least some handles");
        // At least one named object should resolve (we hold registry keys, named
        // sections, etc.) — proof the duplicate+NtQueryObject path works. Note
        // `names_limited` may still be true: some kernel handles are not
        // duplicable even in our own process (the proto folds "not duplicable"
        // into the same flag as "needs elevation").
        assert!(
            res.handles.iter().any(|h| !h.name.is_empty()),
            "at least one own handle should resolve to a name"
        );
    }

    /// The type filter is case-insensitive and only returns the matched type.
    #[test]
    fn type_filter_narrows_results() {
        let me = std::process::id();
        let all = list_handles(me, "", 0);
        // Pick a type that is present (every process has at least one).
        if let Some(sample) = all.handles.iter().find(|h| !h.type_name.is_empty()) {
            let ty = sample.type_name.clone();
            let filtered = list_handles(me, &ty.to_ascii_uppercase(), 0);
            assert!(!filtered.handles.is_empty());
            assert!(filtered
                .handles
                .iter()
                .all(|h| h.type_name.eq_ignore_ascii_case(&ty)));
        }
    }

    /// A tiny limit truncates and flags it.
    #[test]
    fn limit_truncates() {
        let me = std::process::id();
        let res = list_handles(me, "", 1);
        assert!(res.handles.len() <= 1);
        // If the process has more than one handle (it does), truncated is set.
        assert!(res.truncated);
    }
}
