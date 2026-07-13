//! File version-info + Authenticode signature helpers (docs/phases.md Phase 2,
//! PRD §9.4). Shared by the process-detail and module collectors.
//!
//! `read_version_info` pulls CompanyName/ProductName/FileVersion/ProductVersion
//! from a file's version resource (`version.dll`). `verify_signature` runs
//! `WinVerifyTrust` (Authenticode, no revocation/network) to classify a file as
//! Signed / Unsigned / Unknown. Both are unprivileged reads of an on-disk image;
//! a missing resource or unsigned file is a normal, non-fatal outcome.
//!
//! Publisher identity in this first slice is taken from the version resource's
//! CompanyName (authoritative for the vast majority of Windows binaries).
//! Extracting the exact signing-certificate subject via CryptQueryObject is
//! deferred (noted in the milestone report).

#![cfg(windows)]

use std::ptr;

use crate::ffi::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, WinVerifyTrust, DWORD, PVOID,
    TRUST_E_NOSIGNATURE, TRUST_E_PROVIDER_UNKNOWN, TRUST_E_SUBJECT_FORM_UNKNOWN, UINT,
    VS_FIXEDFILEINFO, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_FILE_INFO,
    WTD_CHOICE_FILE, WTD_REVOCATION_CHECK_NONE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE,
    WTD_STATEACTION_VERIFY, WTD_UI_NONE,
};
use crate::reg::to_wide;

/// Version-resource strings for an on-disk image. Every field is best-effort;
/// an image with no version resource yields `None`.
#[derive(Debug, Clone, Default)]
pub struct FileVersionInfo {
    pub file_version: String,
    pub product_version: String,
    pub product_name: String,
    pub company_name: String,
}

/// Authenticode verdict for an on-disk image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureStatus {
    /// Embedded Authenticode signature present and trusted.
    Signed,
    /// No signature at all (or an unrecognised subject form).
    Unsigned,
    /// A signature exists but could not be validated (tampered / untrusted
    /// chain / verification error). Reported honestly rather than as "signed".
    Unknown,
}

impl SignatureStatus {
    /// The proto `signature_status` string, tagging Microsoft when the version
    /// resource's CompanyName identifies Microsoft (PRD §9.4 wording).
    pub fn to_label(self, company_name: &str) -> String {
        match self {
            SignatureStatus::Signed => {
                if company_name.to_ascii_lowercase().contains("microsoft") {
                    "Signed (Microsoft)".to_string()
                } else {
                    "Signed".to_string()
                }
            }
            SignatureStatus::Unsigned => "Unsigned".to_string(),
            SignatureStatus::Unknown => "Unknown".to_string(),
        }
    }
}

/// Formats a packed `dwFileVersionMS`/`LS` (or product) pair as `a.b.c.d`.
fn format_version(ms: u32, ls: u32) -> String {
    format!("{}.{}.{}.{}", ms >> 16, ms & 0xFFFF, ls >> 16, ls & 0xFFFF)
}

/// Reads the version resource of `path`. `None` when the file has no version
/// resource or cannot be read (both normal for many binaries).
pub fn read_version_info(path: &str) -> Option<FileVersionInfo> {
    if path.is_empty() {
        return None;
    }
    let wpath = to_wide(path);
    let mut handle: DWORD = 0;
    // SAFETY: wpath is a live NUL-terminated buffer; handle out-param is local.
    let size = unsafe { GetFileVersionInfoSizeW(wpath.as_ptr(), &mut handle) };
    if size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    // SAFETY: buf sized to the reported length; wpath live.
    let ok = unsafe { GetFileVersionInfoW(wpath.as_ptr(), 0, size, buf.as_mut_ptr().cast()) };
    if ok == 0 {
        return None;
    }

    let mut info = FileVersionInfo::default();

    // Fixed block first: gives numeric file/product versions even when the
    // string table is absent or in an unexpected language.
    if let Some(fixed) = query_fixed(&buf) {
        info.file_version = format_version(fixed.dwFileVersionMS, fixed.dwFileVersionLS);
        info.product_version = format_version(fixed.dwProductVersionMS, fixed.dwProductVersionLS);
    }

    // String table: prefer its (human-authored) versions/names when present.
    if let Some((lang, cp)) = query_first_translation(&buf) {
        let prefix = format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\");
        if let Some(v) = query_string(&buf, &format!("{prefix}FileVersion")) {
            info.file_version = v;
        }
        if let Some(v) = query_string(&buf, &format!("{prefix}ProductVersion")) {
            info.product_version = v;
        }
        if let Some(v) = query_string(&buf, &format!("{prefix}ProductName")) {
            info.product_name = v;
        }
        if let Some(v) = query_string(&buf, &format!("{prefix}CompanyName")) {
            info.company_name = v;
        }
    }

    Some(info)
}

/// `VerQueryValueW("\\")` → the fixed `VS_FIXEDFILEINFO` block.
fn query_fixed(buf: &[u8]) -> Option<VS_FIXEDFILEINFO> {
    let sub = to_wide("\\");
    let mut p: PVOID = ptr::null_mut();
    let mut len: UINT = 0;
    // SAFETY: buf outlives the call; p/len are live out-params. On success p
    // points inside buf at a VS_FIXEDFILEINFO of at least `len` bytes.
    let ok = unsafe { VerQueryValueW(buf.as_ptr().cast(), sub.as_ptr(), &mut p, &mut len) };
    if ok == 0 || p.is_null() || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>() {
        return None;
    }
    // SAFETY: p points at a VS_FIXEDFILEINFO within buf, read unaligned to be safe.
    Some(unsafe { ptr::read_unaligned(p as *const VS_FIXEDFILEINFO) })
}

/// `VerQueryValueW("\\VarFileInfo\\Translation")` → the first (lang, codepage).
fn query_first_translation(buf: &[u8]) -> Option<(u16, u16)> {
    let sub = to_wide("\\VarFileInfo\\Translation");
    let mut p: PVOID = ptr::null_mut();
    let mut len: UINT = 0;
    // SAFETY: buf outlives the call; out-params live.
    let ok = unsafe { VerQueryValueW(buf.as_ptr().cast(), sub.as_ptr(), &mut p, &mut len) };
    if ok == 0 || p.is_null() || (len as usize) < 4 {
        return None;
    }
    // Each translation entry is two u16s: language id then code page.
    // SAFETY: p points at >= 4 bytes (two u16) inside buf.
    let lang = unsafe { ptr::read_unaligned(p as *const u16) };
    let cp = unsafe { ptr::read_unaligned((p as *const u16).add(1)) };
    Some((lang, cp))
}

/// `VerQueryValueW` for one `\\StringFileInfo\\…` entry, decoded to a `String`.
fn query_string(buf: &[u8], sub_block: &str) -> Option<String> {
    let sub = to_wide(sub_block);
    let mut p: PVOID = ptr::null_mut();
    let mut len: UINT = 0;
    // SAFETY: buf outlives the call; out-params live.
    let ok = unsafe { VerQueryValueW(buf.as_ptr().cast(), sub.as_ptr(), &mut p, &mut len) };
    if ok == 0 || p.is_null() || len == 0 {
        return None;
    }
    // `len` counts UTF-16 units including the trailing NUL.
    let units = len as usize;
    // SAFETY: p points at `units` u16 code units within buf.
    let slice = unsafe { std::slice::from_raw_parts(p as *const u16, units) };
    let end = slice.iter().position(|&u| u == 0).unwrap_or(units);
    let s = String::from_utf16_lossy(&slice[..end]).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Runs `WinVerifyTrust` (Authenticode, offline) on `path`. Never blocks on the
/// network (revocation disabled) and always releases its state. Any file that
/// cannot be opened classifies as `Unknown` rather than failing the caller.
pub fn verify_signature(path: &str) -> SignatureStatus {
    if path.is_empty() {
        return SignatureStatus::Unknown;
    }
    let wpath = to_wide(path);

    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as DWORD,
        pcwszFilePath: wpath.as_ptr(),
        hFile: ptr::null_mut(),
        pgKnownSubject: ptr::null(),
    };
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as DWORD,
        pPolicyCallbackData: ptr::null_mut(),
        pSIPClientData: ptr::null_mut(),
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        pFile: &mut file_info,
        dwStateAction: WTD_STATEACTION_VERIFY,
        hWVTStateData: ptr::null_mut(),
        pwszURLReference: ptr::null_mut(),
        dwProvFlags: WTD_REVOCATION_CHECK_NONE,
        dwUIContext: 0,
        pSignatureSettings: ptr::null_mut(),
    };

    // SAFETY: both structs live for the duration; action GUID is 'static. The
    // verify pass populates hWVTStateData, which the close pass frees.
    let status = unsafe {
        WinVerifyTrust(
            ptr::null_mut(),
            &WINTRUST_ACTION_GENERIC_VERIFY_V2,
            (&mut data as *mut WINTRUST_DATA).cast(),
        )
    };

    // Always release the verification state (second pass, close action).
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: same live struct; close pass frees hWVTStateData.
    unsafe {
        WinVerifyTrust(
            ptr::null_mut(),
            &WINTRUST_ACTION_GENERIC_VERIFY_V2,
            (&mut data as *mut WINTRUST_DATA).cast(),
        );
    }

    classify_trust(status)
}

/// Maps a `WinVerifyTrust` result code to a [`SignatureStatus`]. Extracted so
/// the mapping is unit-testable without touching the trust provider.
pub fn classify_trust(status: i32) -> SignatureStatus {
    if status == 0 {
        SignatureStatus::Signed
    } else if status == TRUST_E_NOSIGNATURE
        || status == TRUST_E_SUBJECT_FORM_UNKNOWN
        || status == TRUST_E_PROVIDER_UNKNOWN
    {
        SignatureStatus::Unsigned
    } else {
        // A signature is present but the chain/digest did not validate.
        SignatureStatus::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_format_splits_hi_lo_words() {
        assert_eq!(format_version(0x000A_0000, 0x0001_04D2), "10.0.1.1234");
    }

    #[test]
    fn trust_success_is_signed() {
        assert_eq!(classify_trust(0), SignatureStatus::Signed);
    }

    #[test]
    fn no_signature_is_unsigned() {
        assert_eq!(
            classify_trust(TRUST_E_NOSIGNATURE),
            SignatureStatus::Unsigned
        );
        assert_eq!(
            classify_trust(TRUST_E_SUBJECT_FORM_UNKNOWN),
            SignatureStatus::Unsigned
        );
    }

    #[test]
    fn bad_digest_is_unknown() {
        // TRUST_E_BAD_DIGEST — a real signature that failed to validate.
        assert_eq!(
            classify_trust(0x8009_2003_u32 as i32),
            SignatureStatus::Unknown
        );
    }

    #[test]
    fn microsoft_label_tags_publisher() {
        assert_eq!(
            SignatureStatus::Signed.to_label("Microsoft Corporation"),
            "Signed (Microsoft)"
        );
        assert_eq!(SignatureStatus::Signed.to_label("Acme Inc"), "Signed");
        assert_eq!(SignatureStatus::Unsigned.to_label("Microsoft"), "Unsigned");
    }

    /// A well-known system binary carries a version resource; reading it should
    /// succeed and expose a Microsoft CompanyName. (Self-target smoke — no other
    /// process required.)
    #[test]
    fn system_binary_has_version_info() {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        let kernel32 = format!("{root}\\System32\\kernel32.dll");
        let info = read_version_info(&kernel32).expect("kernel32 has a version resource");
        assert!(
            info.company_name.to_ascii_lowercase().contains("microsoft"),
            "kernel32 CompanyName should be Microsoft, got {:?}",
            info.company_name
        );
        assert!(!info.file_version.is_empty());
    }

    /// kernel32 is Microsoft-signed; verification should return Signed.
    #[test]
    fn system_binary_is_signed() {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        let kernel32 = format!("{root}\\System32\\kernel32.dll");
        assert_eq!(verify_signature(&kernel32), SignatureStatus::Signed);
    }
}
