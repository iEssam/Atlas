//! Pipe DACL construction (tech-stack.md §5, PRD §14: the unprivileged→
//! privileged boundary is the pipe ACL).
//!
//! We build a `SECURITY_ATTRIBUTES` whose security descriptor comes from an
//! SDDL string that grants full access to SYSTEM (`SY`) and the local
//! Administrators group (`BA`), and read/write/connect to the interactive
//! user's own SID, and nobody else. The current user's SID is resolved at
//! runtime from the process token and spliced into the SDDL so the pipe is
//! scoped to *this* user, not "any interactive session".
//!
//! Hand-written FFI in the style of `atlas-collectors/src/ffi.rs`: a handful of
//! stable-ABI advapi32/kernel32 calls kept reviewable in one file rather than
//! pulling in `windows-sys`.
//!
//! # Safety
//! All FFI here is `unsafe`. [`SecurityDescriptor`] owns the LocalAlloc'd
//! descriptor buffer and frees it on drop; the returned `SECURITY_ATTRIBUTES`
//! borrows that buffer, so the descriptor must outlive any pipe created with
//! it (the transport layer keeps it alive across the `create_*` call).

#![cfg(windows)]
#![allow(non_snake_case, non_camel_case_types, clippy::upper_case_acronyms)]

use std::ffi::c_void;
use std::io;
use std::ptr;

type BOOL = i32;
type DWORD = u32;
type HANDLE = *mut c_void;
type HLOCAL = *mut c_void;
type PSID = *mut c_void;

const TOKEN_QUERY: DWORD = 0x0008;
const ERROR_INSUFFICIENT_BUFFER: DWORD = 122;
const SDDL_REVISION_1: DWORD = 1;
// TokenUser = 1 in the TOKEN_INFORMATION_CLASS enum.
const TOKEN_USER_CLASS: u32 = 1;

#[repr(C)]
struct SECURITY_ATTRIBUTES_RAW {
    nLength: DWORD,
    lpSecurityDescriptor: *mut c_void,
    bInheritHandle: BOOL,
}

#[repr(C)]
struct TOKEN_USER {
    // SID_AND_ATTRIBUTES { Sid: PSID, Attributes: DWORD }
    Sid: PSID,
    Attributes: DWORD,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> HANDLE;
    fn CloseHandle(hObject: HANDLE) -> BOOL;
    fn LocalFree(hMem: HLOCAL) -> HLOCAL;
    fn GetLastError() -> DWORD;
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(
        ProcessHandle: HANDLE,
        DesiredAccess: DWORD,
        TokenHandle: *mut HANDLE,
    ) -> BOOL;

    fn GetTokenInformation(
        TokenHandle: HANDLE,
        TokenInformationClass: u32,
        TokenInformation: *mut c_void,
        TokenInformationLength: DWORD,
        ReturnLength: *mut DWORD,
    ) -> BOOL;

    fn ConvertSidToStringSidW(Sid: PSID, StringSid: *mut *mut u16) -> BOOL;

    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        StringSecurityDescriptor: *const u16,
        StringSDRevision: DWORD,
        SecurityDescriptor: *mut *mut c_void,
        SecurityDescriptorSize: *mut DWORD,
    ) -> BOOL;
}

/// Owns a LocalAlloc'd self-relative security descriptor built from SDDL, and
/// hands out a `SECURITY_ATTRIBUTES` pointing at it. The descriptor is freed
/// with `LocalFree` on drop.
pub struct SecurityDescriptor {
    descriptor: *mut c_void,
    sa: SECURITY_ATTRIBUTES_RAW,
}

// The raw pointers are owned and only read by the OS while the pipe is being
// created; the type is not shared across threads without external sync, but
// marking it Send lets the transport move it into an async task.
unsafe impl Send for SecurityDescriptor {}

impl SecurityDescriptor {
    /// Builds a descriptor granting: SYSTEM + Administrators full access, and
    /// the current process user generic read/write/execute (connect) — no one
    /// else. Owner/group set to the current user; protected DACL (`P`) so it
    /// does not inherit anything broader.
    pub fn for_current_user() -> io::Result<Self> {
        let user_sid = current_user_sid_string()?;
        // D:P               protected DACL
        // (A;;GA;;;SY)      allow, generic all, Local System
        // (A;;GA;;;BA)      allow, generic all, Builtin Administrators
        // (A;;GRGWGX;;;SID) allow, generic read/write/execute, current user
        let sddl =
            format!("O:{user_sid}G:{user_sid}D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGWGX;;;{user_sid})");
        Self::from_sddl(&sddl)
    }

    /// Builds a descriptor from an explicit SDDL string. Kept public for tests
    /// and for callers that want a custom policy.
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut descriptor: *mut c_void = ptr::null_mut();
        // SAFETY: `wide` is a valid NUL-terminated UTF-16 buffer; on success
        // the function LocalAllocs `descriptor` which we own and free on drop.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        };
        if ok == 0 || descriptor.is_null() {
            return Err(io::Error::last_os_error());
        }
        let sa = SECURITY_ATTRIBUTES_RAW {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES_RAW>() as DWORD,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        Ok(Self { descriptor, sa })
    }

    /// Raw `*mut SECURITY_ATTRIBUTES` to pass as `lpSecurityAttributes` to
    /// `ServerOptions::create_with_security_attributes_raw`. Valid only while
    /// `self` is alive.
    pub fn as_ptr(&self) -> *mut c_void {
        &self.sa as *const _ as *mut c_void
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            // SAFETY: `descriptor` was allocated by the convert call above.
            unsafe { LocalFree(self.descriptor) };
            self.descriptor = ptr::null_mut();
        }
    }
}

/// Resolves the current process user's SID as an SDDL SID string (e.g.
/// `S-1-5-21-...`).
fn current_user_sid_string() -> io::Result<String> {
    // SAFETY: pseudo-handle; OpenProcessToken duplicates a real token handle
    // into `token` which we close below.
    let mut token: HANDLE = ptr::null_mut();
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        // First call sizes the buffer.
        let mut needed: DWORD = 0;
        // SAFETY: querying required length; a NULL buffer with 0 length is the
        // documented sizing call and returns ERROR_INSUFFICIENT_BUFFER.
        let ok = unsafe {
            GetTokenInformation(token, TOKEN_USER_CLASS, ptr::null_mut(), 0, &mut needed)
        };
        if ok == 0 {
            let e = unsafe { GetLastError() };
            if e != ERROR_INSUFFICIENT_BUFFER {
                return Err(io::Error::from_raw_os_error(e as i32));
            }
        }
        let mut buf = vec![0u8; needed as usize];
        // SAFETY: buffer is `needed` bytes; on success it holds a TOKEN_USER
        // whose Sid pointer aliases into this same buffer.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TOKEN_USER_CLASS,
                buf.as_mut_ptr() as *mut c_void,
                needed,
                &mut needed,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: buffer begins with a TOKEN_USER.
        let token_user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
        sid_to_string(token_user.Sid)
    })();
    // SAFETY: token is a valid handle from OpenProcessToken.
    unsafe { CloseHandle(token) };
    result
}

/// Converts a raw PSID into its `S-1-...` string form via
/// `ConvertSidToStringSidW`, freeing the LocalAlloc'd string.
fn sid_to_string(sid: PSID) -> io::Result<String> {
    let mut wide: *mut u16 = ptr::null_mut();
    // SAFETY: `sid` points at a valid SID inside the TOKEN_USER buffer; on
    // success `wide` is a LocalAlloc'd NUL-terminated UTF-16 string.
    let ok = unsafe { ConvertSidToStringSidW(sid, &mut wide) };
    if ok == 0 || wide.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `wide` is NUL-terminated; measure then copy.
    let s = unsafe {
        let mut len = 0usize;
        while *wide.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(wide, len);
        String::from_utf16_lossy(slice)
    };
    // SAFETY: `wide` was LocalAlloc'd by ConvertSidToStringSidW.
    unsafe { LocalFree(wide as HLOCAL) };
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_user_sid_resolves() {
        let sid = current_user_sid_string().expect("resolve current user SID");
        assert!(sid.starts_with("S-1-"), "unexpected SID form: {sid}");
    }

    #[test]
    fn descriptor_builds_from_current_user() {
        // Exercises the full SDDL → security descriptor path unprivileged.
        let sd = SecurityDescriptor::for_current_user().expect("build descriptor");
        assert!(!sd.as_ptr().is_null());
    }

    #[test]
    fn descriptor_rejects_bad_sddl() {
        assert!(SecurityDescriptor::from_sddl("not-valid-sddl").is_err());
    }
}
