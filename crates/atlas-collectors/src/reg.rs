//! Minimal safe registry-read helpers over the hand-written advapi32 FFI
//! (docs/phases.md M7). Shared by the privacy (ConsentStore) and startup
//! (Run keys / StartupApproved) collectors. Read-only: every key is opened with
//! `KEY_READ` and closed on drop; nothing here ever writes the registry.
//!
//! The surface is deliberately tiny — open a subkey, enumerate subkey names,
//! enumerate/read values — because that is all the two collectors need. UTF-16
//! decoding is lossy (`from_utf16_lossy`) so a malformed name can never panic
//! the collector.

#![cfg(windows)]

use std::ptr;

use crate::ffi::{
    RegCloseKey, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW, RegQueryValueExW, DWORD,
    ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, HKEY, KEY_READ, REG_BINARY, REG_DWORD,
    REG_EXPAND_SZ, REG_QWORD, REG_SZ,
};

/// A raw registry value read from the store, typed enough for the two callers.
#[derive(Debug, Clone)]
pub enum RegValue {
    /// REG_SZ / REG_EXPAND_SZ (expansion is left to the caller — the collectors
    /// keep the raw command string as Windows stores it).
    Str(String),
    Dword(u32),
    Qword(u64),
    Binary(Vec<u8>),
    /// Any other type, kept as raw bytes so nothing is silently dropped.
    Other(DWORD, Vec<u8>),
}

impl RegValue {
    /// The string payload if this is a REG_SZ/REG_EXPAND_SZ, else `None`.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            RegValue::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// An owned, auto-closing registry key handle.
pub struct RegKey {
    handle: HKEY,
}

// The handle is only ever used from the owning thread's collector pass; it is
// not shared across threads. Marking it Send lets the collectors compose with
// the rest of the crate without wrapping every read in extra machinery.
unsafe impl Send for RegKey {}

impl RegKey {
    /// Opens `subkey` under the predefined `root` (e.g. `HKEY_LOCAL_MACHINE`) in
    /// the given registry view (`sam_extra` carries KEY_WOW64_* bits, or 0 for
    /// the default view). Returns `None` when the key is absent or access is
    /// denied — the collectors treat a missing hive as "no entries here".
    ///
    /// `root` is an opaque Win32 `HKEY` (a predefined pseudo-handle or a handle
    /// from a prior `RegOpenKeyExW`); it is never dereferenced by us, only handed
    /// back to the registry API, so the not-unsafe-ptr-arg-deref lint is a false
    /// positive here.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn open(root: HKEY, subkey: &str, sam_extra: DWORD) -> Option<RegKey> {
        let wide = to_wide(subkey);
        let mut handle: HKEY = ptr::null_mut();
        // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the
        // call; `phkResult` points at a live local. On failure the handle stays
        // null and we return None.
        let rc =
            unsafe { RegOpenKeyExW(root, wide.as_ptr(), 0, KEY_READ | sam_extra, &mut handle) };
        if rc == ERROR_SUCCESS && !handle.is_null() {
            Some(RegKey { handle })
        } else {
            None
        }
    }

    /// Collects the names of all immediate subkeys. Best-effort: enumeration
    /// stops on the first error (returning what was gathered so far).
    pub fn subkey_names(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut index: DWORD = 0;
        loop {
            // Key names are bounded at 255 chars; +1 for the NUL.
            let mut buf = [0u16; 256];
            let mut len: DWORD = buf.len() as DWORD;
            // SAFETY: buf/len are live locals sized per the API contract; all
            // optional out-params are null.
            let rc = unsafe {
                RegEnumKeyExW(
                    self.handle,
                    index,
                    buf.as_mut_ptr(),
                    &mut len,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            match rc {
                ERROR_SUCCESS => {
                    out.push(String::from_utf16_lossy(&buf[..len as usize]));
                    index += 1;
                }
                ERROR_NO_MORE_ITEMS => break,
                _ => break,
            }
        }
        out
    }

    /// Opens an immediate subkey by name (same view as this key by default).
    pub fn open_subkey(&self, name: &str) -> Option<RegKey> {
        RegKey::open(self.handle, name, 0)
    }

    /// Enumerates every `(name, value)` pair under this key. Best-effort — a
    /// single unreadable value is skipped, not fatal.
    pub fn values(&self) -> Vec<(String, RegValue)> {
        let mut out = Vec::new();
        let mut index: DWORD = 0;
        loop {
            let mut name_buf = [0u16; 16384 + 1];
            let mut name_len: DWORD = name_buf.len() as DWORD;
            let mut vtype: DWORD = 0;
            // First probe with no data buffer to learn the size.
            let mut data_len: DWORD = 0;
            // SAFETY: name buffer live; data ptr null so only the size is
            // written to data_len.
            let rc = unsafe {
                RegEnumValueW(
                    self.handle,
                    index,
                    name_buf.as_mut_ptr(),
                    &mut name_len,
                    ptr::null_mut(),
                    &mut vtype,
                    ptr::null_mut(),
                    &mut data_len,
                )
            };
            match rc {
                ERROR_SUCCESS | ERROR_MORE_DATA => {}
                ERROR_NO_MORE_ITEMS => break,
                _ => break,
            }
            let name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
            let mut data = vec![0u8; data_len as usize];
            let mut name_len2: DWORD = name_buf.len() as DWORD;
            let mut data_len2: DWORD = data_len;
            let mut vtype2: DWORD = 0;
            // Second call reads the actual bytes.
            // SAFETY: data sized to the probed length; name buffer reused.
            let rc2 = unsafe {
                RegEnumValueW(
                    self.handle,
                    index,
                    name_buf.as_mut_ptr(),
                    &mut name_len2,
                    ptr::null_mut(),
                    &mut vtype2,
                    data.as_mut_ptr(),
                    &mut data_len2,
                )
            };
            if rc2 == ERROR_SUCCESS {
                data.truncate(data_len2 as usize);
                out.push((name, decode_value(vtype2, data)));
            }
            index += 1;
        }
        out
    }

    /// Reads a single named value (the two-call size pattern). `""` reads the
    /// key's default value.
    pub fn get_value(&self, name: &str) -> Option<RegValue> {
        let wide = to_wide(name);
        let mut vtype: DWORD = 0;
        let mut len: DWORD = 0;
        // SAFETY: data ptr null → only the required size is written to len.
        let rc = unsafe {
            RegQueryValueExW(
                self.handle,
                wide.as_ptr(),
                ptr::null_mut(),
                &mut vtype,
                ptr::null_mut(),
                &mut len,
            )
        };
        if rc != ERROR_SUCCESS {
            return None;
        }
        let mut data = vec![0u8; len as usize];
        let mut len2 = len;
        let mut vtype2: DWORD = 0;
        // SAFETY: data sized to the probed length.
        let rc2 = unsafe {
            RegQueryValueExW(
                self.handle,
                wide.as_ptr(),
                ptr::null_mut(),
                &mut vtype2,
                data.as_mut_ptr(),
                &mut len2,
            )
        };
        if rc2 != ERROR_SUCCESS {
            return None;
        }
        data.truncate(len2 as usize);
        Some(decode_value(vtype2, data))
    }
}

impl Drop for RegKey {
    fn drop(&mut self) {
        // Predefined roots are pseudo-handles and must not be closed, but a
        // RegKey only ever wraps a handle returned by RegOpenKeyExW.
        // SAFETY: handle came from a successful RegOpenKeyExW and is closed once.
        unsafe {
            RegCloseKey(self.handle);
        }
    }
}

/// Decodes a raw registry value buffer into a [`RegValue`] by its type tag.
fn decode_value(vtype: DWORD, data: Vec<u8>) -> RegValue {
    match vtype {
        REG_SZ | REG_EXPAND_SZ => RegValue::Str(decode_utf16_bytes(&data)),
        REG_DWORD => {
            let mut b = [0u8; 4];
            let n = data.len().min(4);
            b[..n].copy_from_slice(&data[..n]);
            RegValue::Dword(u32::from_le_bytes(b))
        }
        REG_QWORD => {
            let mut b = [0u8; 8];
            let n = data.len().min(8);
            b[..n].copy_from_slice(&data[..n]);
            RegValue::Qword(u64::from_le_bytes(b))
        }
        REG_BINARY => RegValue::Binary(data),
        other => RegValue::Other(other, data),
    }
}

/// Decodes a UTF-16LE byte buffer (as stored for REG_SZ) into a `String`,
/// trimming a trailing NUL. Lossy so malformed data cannot panic.
pub fn decode_utf16_bytes(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

/// UTF-16, NUL-terminated, for passing a Rust `&str` to a `*const u16` Win32
/// parameter.
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_utf16_trims_nul() {
        // "Run\0" as UTF-16LE bytes.
        let bytes = [0x52, 0x00, 0x75, 0x00, 0x6E, 0x00, 0x00, 0x00];
        assert_eq!(decode_utf16_bytes(&bytes), "Run");
    }

    #[test]
    fn decode_utf16_no_nul() {
        let bytes = [0x41, 0x00, 0x42, 0x00];
        assert_eq!(decode_utf16_bytes(&bytes), "AB");
    }

    #[test]
    fn decode_value_dword_le() {
        let v = decode_value(REG_DWORD, vec![0x01, 0x00, 0x00, 0x00]);
        matches!(v, RegValue::Dword(1));
    }

    #[test]
    fn to_wide_terminates() {
        let w = to_wide("Hi");
        assert_eq!(w, vec![0x48, 0x69, 0x00]);
    }
}
