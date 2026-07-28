//! Shared-memory live ring (tech-stack.md §5.1, docs/phases.md M4).
//!
//! A single fixed-size, pagefile-backed named section carries the 1 Hz "live"
//! snapshot — system gauges plus the top-N process rows — that unprivileged
//! readers (UI, tray, emergency UI) map read-only and copy out lock-free. This
//! is the zero-copy hot path of §5: the writer never blocks on a reader and a
//! reader never blocks the writer.
//!
//! # Seqlock protocol
//! Concurrency is a classic **seqlock** (sequence lock), the standard pattern
//! for a single writer publishing to many wait-free readers of a fixed record:
//!
//! * The header holds an [`AtomicU32`] `seq`. **Even = stable, odd = a write is
//!   in progress.**
//! * [`RingWriter::publish`] does: `seq += 1` (now odd) with `Release`, write
//!   every field, then `seq += 1` (now even again) with `Release`. A
//!   `fence(Release)` between the field stores and the closing increment pairs
//!   with the reader's `Acquire` re-read.
//! * [`RingReader::snapshot`] does: read `seq` with `Acquire` (retry while odd),
//!   copy the whole payload out, `fence(Acquire)`, re-read `seq` with `Acquire`;
//!   if it changed (or is now odd) a write raced the copy, so retry. After a
//!   bounded number of retries it returns `None` rather than spin forever.
//!
//! ## Why the copy-out cannot tear
//! A torn read would require the reader to observe *some* writer stores from
//! publish *N* interleaved with the reader's copy without the surrounding `seq`
//! changing. That is impossible here: the writer bumps `seq` to odd **before**
//! any field store (Release) and to even **after** all of them (Release, with a
//! Release fence ordering the field stores ahead of it). The reader samples
//! `seq` (Acquire) before the copy and again (Acquire, after an Acquire fence)
//! after it. If any writer field store from a publish is visible to the copy,
//! then that publish's opening increment happened-before the copy's stores in
//! the modification order of `seq`, so the reader's *second* `seq` load must
//! observe a value ≥ that opening (odd) value — either odd (caught directly) or
//! a later even value ≠ the first load (caught by the mismatch check). Only when
//! both `seq` loads are equal *and* even did no publish overlap the copy, so the
//! bytes the reader holds are exactly one writer's `publish` output. The copy
//! itself reads plain (non-atomic) bytes, which is sound because the seq
//! discipline guarantees no concurrent writer store is *used* — a mismatched
//! copy is discarded before its contents are trusted.
//!
//! # FFI style
//! Hand-written kernel32 bindings in the style of `security.rs` /
//! `atlas-collectors::ffi`: a handful of stable-ABI section calls kept
//! reviewable in one file rather than pulling in `windows-sys`. All the section
//! handles and mapped views are owned and released on drop.

#![cfg(windows)]
#![allow(non_snake_case, non_camel_case_types, clippy::upper_case_acronyms)]

use std::ffi::c_void;
use std::io;
use std::ptr;
use std::sync::atomic::{fence, AtomicU32, Ordering};

// ---------------------------------------------------------------------------
// Fixed shared layout (repr(C), stable size). Bump LAYOUT_VERSION on any change.
// ---------------------------------------------------------------------------

/// Identifies an Atlas metrics section; validated on every read.
pub const RING_MAGIC: u32 = 0x414C_5352; // "ALSR" (AtLas Shared Ring), LE.
/// Layout version. **Bump this whenever [`RingHeader`] or [`RingRow`] changes**
/// so a stale reader mapping an incompatible section rejects it instead of
/// misinterpreting bytes.
pub const LAYOUT_VERSION: u32 = 2;
/// Fixed number of process rows carried in the ring. Top-N live rows only
/// (tech-stack §5.1); the full set stays behind the gRPC snapshot.
pub const RING_ROWS: usize = 64;
/// Fixed capacity of a row's NUL-padded, truncated UTF-16 image name.
pub const RING_NAME_LEN: usize = 32;

/// One process row in the ring. `#[repr(C)]` with a fixed-size UTF-16 name so
/// the whole section is a constant, ABI-stable size across writer and reader.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RingRow {
    pub pid: u32,
    pub cpu_permille: u32,
    pub gpu_permille: u32,
    pub _pad_gpu: u32,
    pub working_set: u64,
    pub private_bytes: u64,
    pub read_bps: u64,
    pub write_bps: u64,
    pub gpu_dedicated_bytes: u64,
    pub gpu_shared_bytes: u64,
    /// Image name as UTF-16, NUL-padded, truncated to [`RING_NAME_LEN`] units.
    pub name: [u16; RING_NAME_LEN],
}

impl RingRow {
    const fn zeroed() -> Self {
        Self {
            pid: 0,
            cpu_permille: 0,
            gpu_permille: 0,
            _pad_gpu: 0,
            working_set: 0,
            private_bytes: 0,
            read_bps: 0,
            write_bps: 0,
            gpu_dedicated_bytes: 0,
            gpu_shared_bytes: 0,
            name: [0; RING_NAME_LEN],
        }
    }

    /// Decodes the NUL-padded UTF-16 name back into a `String`.
    pub fn name_string(&self) -> String {
        let len = self
            .name
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(RING_NAME_LEN);
        String::from_utf16_lossy(&self.name[..len])
    }
}

/// The ring header: magic + version for validation, the seqlock counter, a
/// publish timestamp, the system gauges, and the live row count. Followed
/// in-memory by `[RingRow; RING_ROWS]` (see [`RingLayout`]).
#[repr(C)]
pub struct RingHeader {
    pub magic: u32,
    pub layout_version: u32,
    /// Seqlock sequence: even = stable, odd = write in progress.
    pub seq: AtomicU32,
    /// Reserved so the 64-bit fields below are 8-byte aligned within the
    /// `#[repr(C)]` record regardless of `AtomicU32` placement.
    _pad: u32,
    pub ts_ms: i64,
    // System gauges (mirror of SystemGauges / SystemSample).
    pub cpu_permille: u32,
    pub process_count: u32,
    pub thread_count: u32,
    pub handle_count: u32,
    pub gpu_permille: u32,
    _pad_gpu: u32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub commit_used: u64,
    pub commit_limit: u64,
    pub gpu_dedicated_used: u64,
    pub gpu_dedicated_budget: u64,
    pub gpu_shared_used: u64,
    pub gpu_shared_budget: u64,
    /// Number of valid rows in the row array (0..=RING_ROWS).
    pub row_count: u32,
    _pad2: u32,
}

/// The full fixed-size section payload: header immediately followed by the row
/// array. Mapped in-place over the section view; never moved.
#[repr(C)]
pub struct RingLayout {
    pub header: RingHeader,
    pub rows: [RingRow; RING_ROWS],
}

/// Size of the mapped section in bytes.
pub const RING_SIZE: usize = std::mem::size_of::<RingLayout>();

/// A plain (non-atomic) system-gauge + row snapshot copied out by a reader.
/// Distinct from the in-section [`RingLayout`] because it owns its bytes and
/// carries no atomics.
#[derive(Clone)]
pub struct RingSnapshot {
    pub ts_ms: i64,
    pub cpu_permille: u32,
    pub process_count: u32,
    pub thread_count: u32,
    pub handle_count: u32,
    pub gpu_permille: u32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub commit_used: u64,
    pub commit_limit: u64,
    pub gpu_dedicated_used: u64,
    pub gpu_dedicated_budget: u64,
    pub gpu_shared_used: u64,
    pub gpu_shared_budget: u64,
    /// Valid rows, already truncated to `row_count`.
    pub rows: Vec<RowSnapshot>,
}

/// A copied-out process row (owns a decoded name).
#[derive(Clone)]
pub struct RowSnapshot {
    pub pid: u32,
    pub cpu_permille: u32,
    pub gpu_permille: u32,
    pub working_set: u64,
    pub private_bytes: u64,
    pub read_bps: u64,
    pub write_bps: u64,
    pub gpu_dedicated_bytes: u64,
    pub gpu_shared_bytes: u64,
    pub name: String,
}

/// Input for one publish: the system gauges plus the (already top-N-sorted)
/// rows. The writer truncates to [`RING_ROWS`] and each name to
/// [`RING_NAME_LEN`].
pub struct RingUpdate<'a> {
    pub ts_ms: i64,
    pub cpu_permille: u32,
    pub process_count: u32,
    pub thread_count: u32,
    pub handle_count: u32,
    pub gpu_permille: u32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub commit_used: u64,
    pub commit_limit: u64,
    pub gpu_dedicated_used: u64,
    pub gpu_dedicated_budget: u64,
    pub gpu_shared_used: u64,
    pub gpu_shared_budget: u64,
    pub rows: &'a [RowInput<'a>],
}

/// One row to publish. `name` is truncated/encoded by the writer.
pub struct RowInput<'a> {
    pub pid: u32,
    pub cpu_permille: u32,
    pub gpu_permille: u32,
    pub working_set: u64,
    pub private_bytes: u64,
    pub read_bps: u64,
    pub write_bps: u64,
    pub gpu_dedicated_bytes: u64,
    pub gpu_shared_bytes: u64,
    pub name: &'a str,
}

// ---------------------------------------------------------------------------
// FFI: named section create/open/map. Hand-written, kernel32 only.
// ---------------------------------------------------------------------------

type BOOL = i32;
type DWORD = u32;
type HANDLE = *mut c_void;
type LPVOID = *mut c_void;
type SIZE_T = usize;

const PAGE_READWRITE: DWORD = 0x04;
/// `INVALID_HANDLE_VALUE` for `CreateFileMappingW`'s file handle argument means
/// "back this section with the system pagefile" (no on-disk file).
const INVALID_HANDLE_VALUE: HANDLE = usize::MAX as HANDLE;
const FILE_MAP_WRITE: DWORD = 0x0002;
const FILE_MAP_READ: DWORD = 0x0004;
const ERROR_ALREADY_EXISTS: DWORD = 183;

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileMappingW(
        hFile: HANDLE,
        lpFileMappingAttributes: *mut c_void,
        flProtect: DWORD,
        dwMaximumSizeHigh: DWORD,
        dwMaximumSizeLow: DWORD,
        lpName: *const u16,
    ) -> HANDLE;

    fn OpenFileMappingW(dwDesiredAccess: DWORD, bInheritHandle: BOOL, lpName: *const u16)
        -> HANDLE;

    fn MapViewOfFile(
        hFileMappingObject: HANDLE,
        dwDesiredAccess: DWORD,
        dwFileOffsetHigh: DWORD,
        dwFileOffsetLow: DWORD,
        dwNumberOfBytesToMap: SIZE_T,
    ) -> LPVOID;

    fn UnmapViewOfFile(lpBaseAddress: *const c_void) -> BOOL;
    fn CloseHandle(hObject: HANDLE) -> BOOL;
    fn GetLastError() -> DWORD;
}

/// Builds the section object name for a discriminator: `Local\SystemAtlas.
/// metrics.<who>`. The `Local\` prefix scopes it to the caller's session so
/// parallel dev instances (and tests, which pass a unique token) never collide.
pub fn section_name(who: &str) -> String {
    format!(r"Local\SystemAtlas.metrics.{who}")
}

/// Encodes a section name as a NUL-terminated UTF-16 buffer.
fn wide(name: &str) -> Vec<u16> {
    name.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// The single writer end of the ring. Owns the section handle and a mutable
/// mapped view; [`publish`](RingWriter::publish) is the only mutator.
pub struct RingWriter {
    handle: HANDLE,
    view: *mut RingLayout,
}

// The handle/view are owned by this struct and only touched by the writer
// thread; Send lets `serve` move the writer into its sampler thread. Not Sync:
// there is exactly one writer.
unsafe impl Send for RingWriter {}

impl RingWriter {
    /// Creates (or re-opens) the named section for `who` and maps it read/write.
    /// The header magic/version/seq are initialized to a clean, stable (even)
    /// state so a reader that attaches before the first `publish` sees a valid
    /// but empty ring rather than garbage.
    pub fn create(who: &str) -> io::Result<Self> {
        let name = wide(&section_name(who));
        // SAFETY: pagefile-backed section (INVALID_HANDLE_VALUE file), fixed
        // size, RW protection, valid NUL-terminated name. Returns NULL on
        // failure. A pre-existing section of the same name is reused (we detect
        // ERROR_ALREADY_EXISTS to decide whether to re-initialize the header).
        let (handle, already) = unsafe {
            let h = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                ptr::null_mut(),
                PAGE_READWRITE,
                ((RING_SIZE as u64) >> 32) as DWORD,
                (RING_SIZE as u64 & 0xFFFF_FFFF) as DWORD,
                name.as_ptr(),
            );
            let already = GetLastError() == ERROR_ALREADY_EXISTS;
            (h, already)
        };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `handle` is a valid section object; map its full extent RW.
        let base = unsafe { MapViewOfFile(handle, FILE_MAP_WRITE, 0, 0, RING_SIZE) };
        if base.is_null() {
            let e = io::Error::last_os_error();
            // SAFETY: closing the handle we just created before returning.
            unsafe { CloseHandle(handle) };
            return Err(e);
        }
        let view = base as *mut RingLayout;
        let writer = Self { handle, view };
        // Initialize the header to a stable empty ring. If the section already
        // existed (another writer, or a leftover), we still re-stamp it: there
        // is only ever one legitimate writer, and a fresh, even seq is the
        // correct starting state.
        let _ = already;
        writer.init_header();
        Ok(writer)
    }

    /// Stamps magic/version, zeroes the gauges and rows, and sets `seq` to a
    /// clean even value. Done once at create time, outside the seqlock (no
    /// reader can trust the section until magic/version are valid anyway).
    fn init_header(&self) {
        // SAFETY: `view` points at a mapped RING_SIZE region we own exclusively
        // at construction time; write the whole layout in place.
        unsafe {
            let h = &mut (*self.view).header;
            h.magic = RING_MAGIC;
            h.layout_version = LAYOUT_VERSION;
            h.seq.store(0, Ordering::Release);
            h._pad = 0;
            h.ts_ms = 0;
            h.cpu_permille = 0;
            h.process_count = 0;
            h.thread_count = 0;
            h.handle_count = 0;
            h.mem_used = 0;
            h.mem_total = 0;
            h.commit_used = 0;
            h.commit_limit = 0;
            h.row_count = 0;
            h._pad2 = 0;
            for row in (*self.view).rows.iter_mut() {
                *row = RingRow::zeroed();
            }
        }
    }

    /// Publishes one update under the seqlock: bump `seq` to odd (Release),
    /// write every field, `fence(Release)`, bump `seq` to even (Release).
    /// Rows beyond [`RING_ROWS`] are dropped; names beyond [`RING_NAME_LEN`]
    /// are truncated.
    pub fn publish(&self, update: &RingUpdate<'_>) {
        // SAFETY: exclusive writer over a mapped region we own; all stores stay
        // in bounds ([`RING_ROWS`] cap enforced below). The atomic `seq`
        // ordering is what makes concurrent reads safe (see module docs).
        unsafe {
            let h = &mut (*self.view).header;

            // Enter the write: make seq odd so any reader in-flight retries.
            let start = h.seq.load(Ordering::Relaxed);
            h.seq.store(start.wrapping_add(1), Ordering::Release);
            // Ensure the odd store is visible before the field writes begin.
            fence(Ordering::Release);

            h.ts_ms = update.ts_ms;
            h.cpu_permille = update.cpu_permille;
            h.process_count = update.process_count;
            h.thread_count = update.thread_count;
            h.handle_count = update.handle_count;
            h.gpu_permille = update.gpu_permille;
            h.mem_used = update.mem_used;
            h.mem_total = update.mem_total;
            h.commit_used = update.commit_used;
            h.commit_limit = update.commit_limit;
            h.gpu_dedicated_used = update.gpu_dedicated_used;
            h.gpu_dedicated_budget = update.gpu_dedicated_budget;
            h.gpu_shared_used = update.gpu_shared_used;
            h.gpu_shared_budget = update.gpu_shared_budget;

            let n = update.rows.len().min(RING_ROWS);
            h.row_count = n as u32;

            let rows = &mut (*self.view).rows;
            for (dst, src) in rows.iter_mut().zip(update.rows.iter()).take(n) {
                dst.pid = src.pid;
                dst.cpu_permille = src.cpu_permille;
                dst.gpu_permille = src.gpu_permille;
                dst.working_set = src.working_set;
                dst.private_bytes = src.private_bytes;
                dst.read_bps = src.read_bps;
                dst.write_bps = src.write_bps;
                dst.gpu_dedicated_bytes = src.gpu_dedicated_bytes;
                dst.gpu_shared_bytes = src.gpu_shared_bytes;
                encode_name(src.name, &mut dst.name);
            }
            // Zero the unused tail so a shrinking row_count leaves no stale pids
            // for a reader that ignores row_count (defense in depth).
            for row in rows.iter_mut().skip(n) {
                *row = RingRow::zeroed();
            }

            // Publish: order all field stores before the closing increment, then
            // make seq even again.
            fence(Ordering::Release);
            h.seq.store(start.wrapping_add(2), Ordering::Release);
        }
    }
}

impl Drop for RingWriter {
    fn drop(&mut self) {
        // SAFETY: `view`/`handle` were produced by MapViewOfFile/CreateFileMapping
        // and are unmapped/closed exactly once here.
        unsafe {
            if !self.view.is_null() {
                UnmapViewOfFile(self.view as *const c_void);
                self.view = ptr::null_mut();
            }
            if !self.handle.is_null() {
                CloseHandle(self.handle);
                self.handle = ptr::null_mut();
            }
        }
    }
}

/// Encodes `name` as NUL-padded, truncated UTF-16 into `dst`. A name longer
/// than [`RING_NAME_LEN`] code units is cut; the remainder is zeroed so
/// [`RingRow::name_string`] stops at the first NUL.
fn encode_name(name: &str, dst: &mut [u16; RING_NAME_LEN]) {
    *dst = [0; RING_NAME_LEN];
    for (slot, unit) in dst.iter_mut().zip(name.encode_utf16().take(RING_NAME_LEN)) {
        *slot = unit;
    }
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// The read end of the ring. Opens an existing named section read-only and maps
/// a read view; [`snapshot`](RingReader::snapshot) copies out under the seqlock.
pub struct RingReader {
    handle: HANDLE,
    view: *const RingLayout,
}

unsafe impl Send for RingReader {}

/// Bounded seqlock retry budget. A live writer republishing at 1 Hz never
/// collides for long; this cap only guards against a stuck/odd `seq` (e.g. a
/// writer that crashed mid-publish) so a reader returns `None` instead of
/// spinning forever.
const SNAPSHOT_RETRIES: u32 = 1024;

impl RingReader {
    /// Opens the named section for `who` read-only and validates magic/version.
    /// Returns an error if the section does not exist yet (writer not started)
    /// or the layout is incompatible.
    pub fn open(who: &str) -> io::Result<Self> {
        let name = wide(&section_name(who));
        // SAFETY: read access, no inherit, valid NUL-terminated name. NULL on
        // failure (e.g. section not created yet).
        let handle = unsafe { OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: valid section object; map its full extent read-only.
        let base = unsafe { MapViewOfFile(handle, FILE_MAP_READ, 0, 0, RING_SIZE) };
        if base.is_null() {
            let e = io::Error::last_os_error();
            unsafe { CloseHandle(handle) };
            return Err(e);
        }
        let reader = Self {
            handle,
            view: base as *const RingLayout,
        };
        reader.validate()?;
        Ok(reader)
    }

    /// Validates magic + layout version, rejecting a foreign or stale section.
    fn validate(&self) -> io::Result<()> {
        // SAFETY: `view` is a mapped RING_SIZE read region; the header fields
        // are plain integers safe to read at any time (magic/version never
        // change after init, so no seqlock needed for this check).
        let (magic, version) = unsafe {
            let h = &(*self.view).header;
            (h.magic, h.layout_version)
        };
        if magic != RING_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("ring magic mismatch: got {magic:#010x}, want {RING_MAGIC:#010x}"),
            ));
        }
        if version != LAYOUT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("ring layout version {version} != supported {LAYOUT_VERSION}"),
            ));
        }
        Ok(())
    }

    /// Copies out a consistent snapshot under the seqlock. Returns `None` if the
    /// writer is mid-publish for the entire retry budget (writer stalled/odd).
    pub fn snapshot(&self) -> Option<RingSnapshot> {
        // SAFETY: `view` is a live mapped read region for `self`'s lifetime.
        let h = unsafe { &(*self.view).header };
        for _ in 0..SNAPSHOT_RETRIES {
            // Acquire-load the sequence; odd means a write is in progress.
            let s1 = h.seq.load(Ordering::Acquire);
            if s1 & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            // Copy the payload with plain reads. The seq discipline (checked
            // right after) is what makes trusting these bytes sound; a copy that
            // raced a publish is discarded below before it is used.
            let snap = unsafe { self.copy_out(h) };
            // Order the payload reads before the second seq load.
            fence(Ordering::Acquire);
            let s2 = h.seq.load(Ordering::Acquire);
            if s1 == s2 {
                // Even, unchanged: the copy is exactly one publish's output.
                return Some(snap);
            }
            // A publish overlapped the copy; retry.
            std::hint::spin_loop();
        }
        None
    }

    /// Reads the gauges and the first `row_count` rows into an owned snapshot.
    /// Caller holds the seqlock invariant around this (see [`snapshot`]).
    ///
    /// # Safety
    /// `h` must be the header of `self`'s mapped view; the view must be live.
    unsafe fn copy_out(&self, h: &RingHeader) -> RingSnapshot {
        let n = (h.row_count as usize).min(RING_ROWS);
        let rows_ptr = (*self.view).rows.as_ptr();
        let mut rows = Vec::with_capacity(n);
        for i in 0..n {
            // Plain copy of a POD row, then decode its name.
            let r = ptr::read(rows_ptr.add(i));
            rows.push(RowSnapshot {
                pid: r.pid,
                cpu_permille: r.cpu_permille,
                gpu_permille: r.gpu_permille,
                working_set: r.working_set,
                private_bytes: r.private_bytes,
                read_bps: r.read_bps,
                write_bps: r.write_bps,
                gpu_dedicated_bytes: r.gpu_dedicated_bytes,
                gpu_shared_bytes: r.gpu_shared_bytes,
                name: r.name_string(),
            });
        }
        RingSnapshot {
            ts_ms: h.ts_ms,
            cpu_permille: h.cpu_permille,
            process_count: h.process_count,
            thread_count: h.thread_count,
            handle_count: h.handle_count,
            gpu_permille: h.gpu_permille,
            mem_used: h.mem_used,
            mem_total: h.mem_total,
            commit_used: h.commit_used,
            commit_limit: h.commit_limit,
            gpu_dedicated_used: h.gpu_dedicated_used,
            gpu_dedicated_budget: h.gpu_dedicated_budget,
            gpu_shared_used: h.gpu_shared_used,
            gpu_shared_budget: h.gpu_shared_budget,
            rows,
        }
    }
}

impl Drop for RingReader {
    fn drop(&mut self) {
        // SAFETY: `view`/`handle` from MapViewOfFile/OpenFileMapping; released once.
        unsafe {
            if !self.view.is_null() {
                UnmapViewOfFile(self.view as *const c_void);
                self.view = ptr::null();
            }
            if !self.handle.is_null() {
                CloseHandle(self.handle);
                self.handle = ptr::null_mut();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering as O};
    use std::sync::Arc;

    /// Unique section discriminator per test so parallel runs never collide.
    fn disc(tag: &str) -> String {
        format!("test.{tag}.{}", std::process::id())
    }

    fn update<'a>(ts: i64, cpu: u32, rows: &'a [RowInput<'a>]) -> RingUpdate<'a> {
        RingUpdate {
            ts_ms: ts,
            cpu_permille: cpu,
            process_count: 100,
            thread_count: 2000,
            handle_count: 40000,
            gpu_permille: 420,
            mem_used: 8 << 30,
            mem_total: 16 << 30,
            commit_used: 9 << 30,
            commit_limit: 20 << 30,
            gpu_dedicated_used: 2 << 30,
            gpu_dedicated_budget: 8 << 30,
            gpu_shared_used: 1 << 30,
            gpu_shared_budget: 8 << 30,
            rows,
        }
    }

    #[test]
    fn writer_reader_round_trip() {
        let who = disc("round_trip");
        let writer = RingWriter::create(&who).expect("create ring");
        let rows = [
            RowInput {
                pid: 4,
                cpu_permille: 250,
                gpu_permille: 500,
                working_set: 1 << 20,
                private_bytes: 2 << 20,
                read_bps: 1000,
                write_bps: 2000,
                gpu_dedicated_bytes: 512 << 20,
                gpu_shared_bytes: 64 << 20,
                name: "system.exe",
            },
            RowInput {
                pid: 1234,
                cpu_permille: 125,
                gpu_permille: 0,
                working_set: 3 << 20,
                private_bytes: 4 << 20,
                read_bps: 0,
                write_bps: 0,
                gpu_dedicated_bytes: 0,
                gpu_shared_bytes: 0,
                name: "notepad.exe",
            },
        ];
        writer.publish(&update(42, 333, &rows));

        let reader = RingReader::open(&who).expect("open ring");
        let snap = reader.snapshot().expect("snapshot");
        assert_eq!(snap.ts_ms, 42);
        assert_eq!(snap.cpu_permille, 333);
        assert_eq!(snap.process_count, 100);
        assert_eq!(snap.rows.len(), 2);
        assert_eq!(snap.rows[0].pid, 4);
        assert_eq!(snap.rows[0].name, "system.exe");
        assert_eq!(snap.rows[0].cpu_permille, 250);
        assert_eq!(snap.rows[1].pid, 1234);
        assert_eq!(snap.rows[1].name, "notepad.exe");
    }

    #[test]
    fn reader_before_first_publish_sees_empty_ring() {
        // A reader attaching after create but before any publish gets a valid,
        // empty snapshot (magic/version already stamped, seq even at 0).
        let who = disc("empty");
        let writer = RingWriter::create(&who).expect("create ring");
        let reader = RingReader::open(&who).expect("open ring");
        let snap = reader.snapshot().expect("snapshot");
        assert_eq!(snap.rows.len(), 0);
        assert_eq!(snap.ts_ms, 0);
        drop(writer);
    }

    #[test]
    fn long_names_truncate_at_ring_name_len() {
        let who = disc("truncate");
        let writer = RingWriter::create(&who).expect("create ring");
        // A name longer than RING_NAME_LEN (32) code units.
        let long = "this-is-a-very-long-process-name-that-exceeds-the-limit.exe";
        assert!(long.encode_utf16().count() > RING_NAME_LEN);
        let rows = [RowInput {
            pid: 1,
            cpu_permille: 0,
            gpu_permille: 0,
            working_set: 0,
            private_bytes: 0,
            read_bps: 0,
            write_bps: 0,
            gpu_dedicated_bytes: 0,
            gpu_shared_bytes: 0,
            name: long,
        }];
        writer.publish(&update(1, 0, &rows));

        let reader = RingReader::open(&who).expect("open ring");
        let snap = reader.snapshot().expect("snapshot");
        let got = &snap.rows[0].name;
        assert_eq!(got.encode_utf16().count(), RING_NAME_LEN);
        let expected: String = long.chars().take(RING_NAME_LEN).collect();
        assert_eq!(*got, expected);
    }

    #[test]
    fn open_missing_section_errors() {
        let who = disc("missing");
        assert!(RingReader::open(&who).is_err());
    }

    #[test]
    fn shrinking_row_count_leaves_no_stale_rows() {
        let who = disc("shrink");
        let writer = RingWriter::create(&who).expect("create ring");
        let many: Vec<RowInput> = (0..5)
            .map(|i| RowInput {
                pid: i + 1,
                cpu_permille: 0,
                gpu_permille: 0,
                working_set: 0,
                private_bytes: 0,
                read_bps: 0,
                write_bps: 0,
                gpu_dedicated_bytes: 0,
                gpu_shared_bytes: 0,
                name: "p.exe",
            })
            .collect();
        writer.publish(&update(1, 0, &many));
        let one = [RowInput {
            pid: 99,
            cpu_permille: 0,
            gpu_permille: 0,
            working_set: 0,
            private_bytes: 0,
            read_bps: 0,
            write_bps: 0,
            gpu_dedicated_bytes: 0,
            gpu_shared_bytes: 0,
            name: "one.exe",
        }];
        writer.publish(&update(2, 0, &one));

        let reader = RingReader::open(&who).expect("open ring");
        let snap = reader.snapshot().expect("snapshot");
        assert_eq!(snap.rows.len(), 1);
        assert_eq!(snap.rows[0].pid, 99);
    }

    /// Concurrency invariant: while a writer republishes in a tight loop, every
    /// reader snapshot must be internally consistent. Each row's fields are all
    /// derived from a single per-publish counter `c`, so any torn read (fields
    /// from two different publishes mixed) is detectable as a broken invariant.
    #[test]
    fn reader_sees_consistent_data_under_concurrent_writes() {
        let who = disc("concurrent");
        let writer = RingWriter::create(&who).expect("create ring");
        let stop = Arc::new(AtomicBool::new(false));

        let writer_stop = stop.clone();
        let writer_handle = std::thread::spawn(move || {
            let mut c: u32 = 1;
            while !writer_stop.load(O::Relaxed) {
                // Every field of every row derives from `c`: pid = c + i,
                // cpu = c, working_set = c*1000 + i, name encodes c. A consistent
                // snapshot has all rows agreeing on the same `c`.
                let rows: Vec<RowInput> = (0..8)
                    .map(|i| RowInput {
                        pid: c.wrapping_add(i),
                        cpu_permille: c,
                        gpu_permille: c,
                        working_set: (c as u64) * 1000 + i as u64,
                        private_bytes: (c as u64) * 7,
                        read_bps: c as u64,
                        write_bps: c as u64,
                        gpu_dedicated_bytes: c as u64,
                        gpu_shared_bytes: c as u64,
                        name: "x.exe",
                    })
                    .collect();
                writer.publish(&update(c as i64, c, &rows));
                c = c.wrapping_add(1).max(1);
                // A brief yield keeps the writer from monopolizing the seqlock
                // so readers get frequent stable windows — the real writer
                // publishes at 1 Hz, not in an unthrottled spin. The invariant
                // under test (no torn snapshot) holds regardless of the pause;
                // the pause only affects how often a copy races a publish.
                std::thread::yield_now();
            }
        });

        let reader = RingReader::open(&who).expect("open ring");
        let mut consistent = 0u32;
        let mut retried = 0u32;
        for _ in 0..20_000 {
            if let Some(snap) = reader.snapshot() {
                // Derive the publish counter from the header cpu gauge; every
                // field must agree with it (no tearing across the seqlock).
                let c = snap.cpu_permille;
                assert_eq!(snap.ts_ms, c as i64, "header ts torn from cpu");
                for (i, row) in snap.rows.iter().enumerate() {
                    assert_eq!(row.cpu_permille, c, "row cpu torn");
                    assert_eq!(row.pid, c.wrapping_add(i as u32), "row pid torn");
                    assert_eq!(row.working_set, (c as u64) * 1000 + i as u64, "row ws torn");
                    assert_eq!(row.private_bytes, (c as u64) * 7, "row priv torn");
                }
                consistent += 1;
            } else {
                retried += 1;
            }
        }
        stop.store(true, O::Relaxed);
        writer_handle.join().expect("writer thread");
        // Liveness: the reader completes a large number of consistent snapshots
        // even while the writer hammers the ring. (The per-row asserts above are
        // the real safety check — any torn read fails the test immediately; this
        // bound just proves the reader is not perpetually starved.) A `None` is
        // a *correct* outcome: it means a publish raced the copy and the reader
        // declined to trust it, so `retried` is expected to be non-trivial.
        assert!(
            consistent > 1_000,
            "reader starved: only {consistent} consistent snapshots ({retried} retried out)"
        );
    }
}
