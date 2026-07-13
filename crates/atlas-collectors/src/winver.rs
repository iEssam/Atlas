//! File version-info + Authenticode signature helpers (docs/phases.md Phase 2,
//! PRD §9.4). Shared by the process-detail and module collectors.
//!
//! `read_version_info` pulls CompanyName/ProductName/FileVersion/ProductVersion
//! from a file's version resource (`version.dll`). Signature verification first
//! checks an embedded Authenticode signature, then falls back to the system
//! catalog containing the file hash (`CryptCATAdmin` + `WTD_CHOICE_CATALOG`).
//! The publisher is taken from the verified signing certificate, not the
//! spoofable version-resource CompanyName. All operations are unprivileged,
//! offline reads of an on-disk image; a missing resource or unsigned file is a
//! normal, non-fatal outcome.

#![cfg(windows)]

use std::ffi::c_void;
use std::os::windows::io::AsRawHandle;
use std::ptr;
use std::sync::OnceLock;

use crate::ffi::{
    CertGetNameStringW, GetFileVersionInfoSizeW, GetFileVersionInfoW, GetProcAddress,
    LoadLibraryExW, VerQueryValueW, WinVerifyTrust, BOOL, CATALOG_INFO, CERT_NAME_ATTR_TYPE,
    CERT_NAME_SIMPLE_DISPLAY_TYPE, CRYPT_PROVIDER_CERT_PREFIX, DWORD, GUID, HANDLE,
    LOAD_LIBRARY_SEARCH_SYSTEM32, LPCWSTR, PVOID, TRUST_E_NOSIGNATURE, TRUST_E_PROVIDER_UNKNOWN,
    TRUST_E_SUBJECT_FORM_UNKNOWN, UINT, VS_FIXEDFILEINFO, WINTRUST_ACTION_GENERIC_VERIFY_V2,
    WINTRUST_CATALOG_INFO, WINTRUST_DATA, WINTRUST_FILE_INFO, WTD_CHOICE_CATALOG, WTD_CHOICE_FILE,
    WTD_REVOCATION_CHECK_NONE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY,
    WTD_UI_NONE,
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
    /// Embedded or catalog Authenticode signature present and trusted.
    Signed,
    /// No signature at all (or an unrecognised subject form).
    Unsigned,
    /// A signature exists but could not be validated (tampered / untrusted
    /// chain / verification error). Reported honestly rather than as "signed".
    Unknown,
}

impl SignatureStatus {
    /// The proto `signature_status` string, tagging Microsoft when the verified
    /// certificate publisher (or version-resource fallback) identifies it.
    pub fn to_label(self, publisher: &str) -> String {
        match self {
            SignatureStatus::Signed => {
                if publisher.to_ascii_lowercase().contains("microsoft") {
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

/// Trust result plus the subject organization/display name of the verified
/// signing certificate. `publisher` is empty when no trusted signer exists or
/// Windows does not expose a usable subject name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureInfo {
    pub status: SignatureStatus,
    pub publisher: String,
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

type AcquireCatalogContext =
    unsafe extern "system" fn(*mut HANDLE, *const GUID, LPCWSTR, *const c_void, DWORD) -> BOOL;
type CalculateCatalogHash =
    unsafe extern "system" fn(HANDLE, HANDLE, *mut DWORD, *mut u8, DWORD) -> BOOL;
type EnumerateCatalog =
    unsafe extern "system" fn(HANDLE, *mut u8, DWORD, DWORD, *mut HANDLE) -> HANDLE;
type CatalogInfoFromContext = unsafe extern "system" fn(HANDLE, *mut CATALOG_INFO, DWORD) -> BOOL;
type ReleaseCatalogContext = unsafe extern "system" fn(HANDLE, HANDLE, DWORD) -> BOOL;
type ReleaseAdminContext = unsafe extern "system" fn(HANDLE, DWORD) -> BOOL;
type ProviderDataFromState = unsafe extern "system" fn(HANDLE) -> PVOID;
type ProviderSignerFromChain = unsafe extern "system" fn(PVOID, DWORD, BOOL, DWORD) -> PVOID;
type ProviderCertFromChain =
    unsafe extern "system" fn(PVOID, DWORD) -> *mut CRYPT_PROVIDER_CERT_PREFIX;

/// Dynamically linked because Microsoft does not publish import-library entries
/// for the Context2/hash2 and WinTrust provider-helper functions.
struct WintrustApi {
    acquire_context: AcquireCatalogContext,
    calculate_hash: CalculateCatalogHash,
    enumerate_catalog: EnumerateCatalog,
    catalog_info: CatalogInfoFromContext,
    release_catalog: ReleaseCatalogContext,
    release_context: ReleaseAdminContext,
    provider_data: ProviderDataFromState,
    provider_signer: ProviderSignerFromChain,
    provider_cert: ProviderCertFromChain,
}

impl WintrustApi {
    fn load() -> Option<Self> {
        let dll = to_wide("wintrust.dll");
        // SAFETY: the name is NUL-terminated and the constrained search flag
        // loads only the Windows system copy. The module intentionally remains
        // loaded for the process lifetime alongside the cached function table.
        let module =
            unsafe { LoadLibraryExW(dll.as_ptr(), ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
        if module.is_null() {
            return None;
        }

        macro_rules! load {
            ($name:literal, $ty:ty) => {{
                // SAFETY: module is live and the byte literal is NUL-terminated.
                let address = unsafe { GetProcAddress(module, concat!($name, "\0").as_ptr()) };
                if address.is_null() {
                    return None;
                }
                // SAFETY: the export's ABI and signature match the Windows SDK
                // declaration represented by `$ty` above.
                unsafe { std::mem::transmute::<PVOID, $ty>(address) }
            }};
        }

        Some(Self {
            acquire_context: load!("CryptCATAdminAcquireContext2", AcquireCatalogContext),
            calculate_hash: load!("CryptCATAdminCalcHashFromFileHandle2", CalculateCatalogHash),
            enumerate_catalog: load!("CryptCATAdminEnumCatalogFromHash", EnumerateCatalog),
            catalog_info: load!("CryptCATCatalogInfoFromContext", CatalogInfoFromContext),
            release_catalog: load!("CryptCATAdminReleaseCatalogContext", ReleaseCatalogContext),
            release_context: load!("CryptCATAdminReleaseContext", ReleaseAdminContext),
            provider_data: load!("WTHelperProvDataFromStateData", ProviderDataFromState),
            provider_signer: load!("WTHelperGetProvSignerFromChain", ProviderSignerFromChain),
            provider_cert: load!("WTHelperGetProvCertFromChain", ProviderCertFromChain),
        })
    }
}

fn wintrust_api() -> Option<&'static WintrustApi> {
    static API: OnceLock<Option<WintrustApi>> = OnceLock::new();
    API.get_or_init(WintrustApi::load).as_ref()
}

fn empty_signature(status: SignatureStatus) -> SignatureInfo {
    SignatureInfo {
        status,
        publisher: String::new(),
    }
}

/// Runs embedded Authenticode verification, then checks matching security
/// catalogs when the file has no trusted embedded signature. Revocation is
/// disabled and every WinTrust/catalog state object is released before return.
pub fn verify_signature_info(path: &str) -> SignatureInfo {
    if path.is_empty() {
        return empty_signature(SignatureStatus::Unknown);
    }

    let embedded = verify_embedded_signature(path);
    if embedded.status == SignatureStatus::Signed {
        return embedded;
    }

    match verify_catalog_signature(path) {
        Some(catalog) if catalog.status == SignatureStatus::Signed => catalog,
        Some(_) if embedded.status == SignatureStatus::Unsigned => {
            empty_signature(SignatureStatus::Unknown)
        }
        _ => embedded,
    }
}

/// Status-only compatibility wrapper used by callers that do not display the
/// signing-certificate publisher.
pub fn verify_signature(path: &str) -> SignatureStatus {
    verify_signature_info(path).status
}

fn verify_embedded_signature(path: &str) -> SignatureInfo {
    let wpath = to_wide(path);
    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as DWORD,
        pcwszFilePath: wpath.as_ptr(),
        hFile: ptr::null_mut(),
        pgKnownSubject: ptr::null(),
    };
    let mut data = trust_data(
        WTD_CHOICE_FILE,
        (&mut file_info as *mut WINTRUST_FILE_INFO).cast(),
    );
    verify_wintrust(&mut data)
}

fn trust_data(choice: DWORD, info: PVOID) -> WINTRUST_DATA {
    WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as DWORD,
        pPolicyCallbackData: ptr::null_mut(),
        pSIPClientData: ptr::null_mut(),
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: choice,
        pInfo: info,
        dwStateAction: WTD_STATEACTION_VERIFY,
        hWVTStateData: ptr::null_mut(),
        pwszURLReference: ptr::null_mut(),
        dwProvFlags: WTD_REVOCATION_CHECK_NONE,
        dwUIContext: 0,
        pSignatureSettings: ptr::null_mut(),
    }
}

fn verify_wintrust(data: &mut WINTRUST_DATA) -> SignatureInfo {
    // SAFETY: data and its selected union payload remain live for both calls;
    // the action GUID is static. VERIFY populates state consumed before CLOSE.
    let status = unsafe {
        WinVerifyTrust(
            ptr::null_mut(),
            &WINTRUST_ACTION_GENERIC_VERIFY_V2,
            (data as *mut WINTRUST_DATA).cast(),
        )
    };
    let publisher = if status == 0 {
        certificate_publisher(data.hWVTStateData)
    } else {
        String::new()
    };

    data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: this is the required matching close pass for the live state.
    unsafe {
        WinVerifyTrust(
            ptr::null_mut(),
            &WINTRUST_ACTION_GENERIC_VERIFY_V2,
            (data as *mut WINTRUST_DATA).cast(),
        );
    }

    SignatureInfo {
        status: classify_trust(status),
        publisher,
    }
}

fn certificate_publisher(state: HANDLE) -> String {
    let Some(api) = wintrust_api() else {
        return String::new();
    };
    if state.is_null() {
        return String::new();
    }

    // SAFETY: all returned pointers belong to the live WinTrust state and are
    // consumed synchronously before its CLOSE pass.
    let cert = unsafe {
        let provider = (api.provider_data)(state);
        if provider.is_null() {
            return String::new();
        }
        let signer = (api.provider_signer)(provider, 0, 0, 0);
        if signer.is_null() {
            return String::new();
        }
        (api.provider_cert)(signer, 0)
    };
    if cert.is_null() {
        return String::new();
    }
    // SAFETY: `cert` is a provider-owned CRYPT_PROVIDER_CERT whose pCert field
    // is valid until WinTrust state is closed.
    let context = unsafe { (*cert).pCert };
    if context.is_null() {
        return String::new();
    }

    const ORGANIZATION_OID: &[u8] = b"2.5.4.10\0";
    let organization = cert_name(
        context,
        CERT_NAME_ATTR_TYPE,
        ORGANIZATION_OID.as_ptr() as PVOID,
    );
    if organization.is_empty() {
        cert_name(context, CERT_NAME_SIMPLE_DISPLAY_TYPE, ptr::null_mut())
    } else {
        organization
    }
}

fn cert_name(context: *const c_void, name_type: DWORD, parameter: PVOID) -> String {
    // SAFETY: context belongs to the live provider state; this first call asks
    // only for the required UTF-16 character count.
    let count = unsafe { CertGetNameStringW(context, name_type, 0, parameter, ptr::null_mut(), 0) };
    if count <= 1 {
        return String::new();
    }
    let mut name = vec![0u16; count as usize];
    // SAFETY: the buffer contains exactly `count` UTF-16 slots.
    let written =
        unsafe { CertGetNameStringW(context, name_type, 0, parameter, name.as_mut_ptr(), count) };
    if written <= 1 {
        return String::new();
    }
    String::from_utf16_lossy(&name[..written.saturating_sub(1) as usize])
        .trim()
        .to_string()
}

fn verify_catalog_signature(path: &str) -> Option<SignatureInfo> {
    let api = wintrust_api()?;
    let file = std::fs::File::open(path).ok()?;
    let file_handle = file.as_raw_handle() as HANDLE;

    let mut admin: HANDLE = ptr::null_mut();
    // SAFETY: admin is a live out-param; null algorithm/policy asks Windows to
    // select its current default, which hash2 then uses consistently.
    if unsafe { (api.acquire_context)(&mut admin, ptr::null(), ptr::null(), ptr::null(), 0) } == 0
        || admin.is_null()
    {
        return None;
    }

    let result = verify_catalog_with_context(api, admin, file_handle, path);
    // SAFETY: admin was successfully acquired above and no catalog payload is
    // live after the helper returns.
    unsafe {
        (api.release_context)(admin, 0);
    }
    result
}

fn verify_catalog_with_context(
    api: &WintrustApi,
    admin: HANDLE,
    file_handle: HANDLE,
    path: &str,
) -> Option<SignatureInfo> {
    let mut hash_len: DWORD = 0;
    // SAFETY: the first call obtains the required hash size for this context.
    if unsafe { (api.calculate_hash)(admin, file_handle, &mut hash_len, ptr::null_mut(), 0) } == 0
        || hash_len == 0
    {
        return None;
    }
    let mut hash = vec![0u8; hash_len as usize];
    // SAFETY: hash has `hash_len` writable bytes and the handles are live.
    if unsafe { (api.calculate_hash)(admin, file_handle, &mut hash_len, hash.as_mut_ptr(), 0) } == 0
    {
        return None;
    }
    hash.truncate(hash_len as usize);

    let member_tag = to_wide(&hash_to_member_tag(&hash));
    let member_path = to_wide(path);
    let mut previous: HANDLE = ptr::null_mut();
    let mut found_catalog = false;

    loop {
        // The enumerator consumes/releases `previous` when advancing. If we
        // return early, the current context is explicitly released below.
        let current = unsafe {
            (api.enumerate_catalog)(admin, hash.as_mut_ptr(), hash_len, 0, &mut previous)
        };
        if current.is_null() {
            break;
        }
        previous = current;
        found_catalog = true;

        let mut catalog = CATALOG_INFO {
            cbStruct: std::mem::size_of::<CATALOG_INFO>() as DWORD,
            wszCatalogFile: [0; crate::ffi::MAX_PATH],
        };
        // SAFETY: current is the live context returned above; catalog is a
        // correctly sized writable output structure.
        if unsafe { (api.catalog_info)(current, &mut catalog, 0) } != 0 {
            let mut catalog_info = WINTRUST_CATALOG_INFO {
                cbStruct: std::mem::size_of::<WINTRUST_CATALOG_INFO>() as DWORD,
                dwCatalogVersion: 0,
                pcwszCatalogFilePath: catalog.wszCatalogFile.as_ptr(),
                pcwszMemberTag: member_tag.as_ptr(),
                pcwszMemberFilePath: member_path.as_ptr(),
                hMemberFile: file_handle,
                pbCalculatedFileHash: hash.as_mut_ptr(),
                cbCalculatedFileHash: hash_len,
                pcCatalogContext: ptr::null_mut(),
                hCatAdmin: admin,
            };
            let mut data = trust_data(
                WTD_CHOICE_CATALOG,
                (&mut catalog_info as *mut WINTRUST_CATALOG_INFO).cast(),
            );
            let verified = verify_wintrust(&mut data);
            if verified.status == SignatureStatus::Signed {
                // SAFETY: early termination leaves current for us to release.
                unsafe {
                    (api.release_catalog)(admin, current, 0);
                }
                return Some(verified);
            }
        }
    }

    if found_catalog {
        Some(empty_signature(SignatureStatus::Unknown))
    } else {
        None
    }
}

fn hash_to_member_tag(hash: &[u8]) -> String {
    use std::fmt::Write;

    let mut tag = String::with_capacity(hash.len() * 2);
    for byte in hash {
        let _ = write!(tag, "{byte:02X}");
    }
    tag
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
    use std::io::Write;
    use std::mem::{offset_of, size_of};

    #[test]
    fn version_format_splits_hi_lo_words() {
        assert_eq!(format_version(0x000A_0000, 0x0001_04D2), "10.0.1.1234");
    }

    #[test]
    fn catalog_member_tag_is_uppercase_hex() {
        assert_eq!(hash_to_member_tag(&[0x00, 0x7f, 0xa5, 0xff]), "007FA5FF");
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn trust_ffi_layouts_match_windows_sdk() {
        assert_eq!(size_of::<WINTRUST_FILE_INFO>(), 32);
        assert_eq!(size_of::<CATALOG_INFO>(), 524);
        assert_eq!(size_of::<WINTRUST_CATALOG_INFO>(), 72);
        assert_eq!(offset_of!(WINTRUST_CATALOG_INFO, pcwszCatalogFilePath), 8);
        assert_eq!(offset_of!(WINTRUST_CATALOG_INFO, hMemberFile), 32);
        assert_eq!(offset_of!(WINTRUST_CATALOG_INFO, hCatAdmin), 64);
        assert_eq!(size_of::<WINTRUST_DATA>(), 88);
        assert_eq!(offset_of!(WINTRUST_DATA, pInfo), 40);
        assert_eq!(offset_of!(WINTRUST_DATA, hWVTStateData), 56);
        assert_eq!(offset_of!(CRYPT_PROVIDER_CERT_PREFIX, pCert), 8);
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

    /// PowerShell 5.1 is catalog-signed on supported Windows installations.
    /// The embedded-only pass demonstrates that this test exercises fallback.
    #[test]
    fn catalog_signed_system_binary_is_signed() {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        let powershell = format!("{root}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe");
        assert_eq!(
            verify_embedded_signature(&powershell).status,
            SignatureStatus::Unsigned,
            "PowerShell fixture should require catalog fallback"
        );
        let verified = verify_signature_info(&powershell);
        assert_eq!(verified.status, SignatureStatus::Signed);
        assert!(
            verified
                .publisher
                .to_ascii_lowercase()
                .contains("microsoft"),
            "expected Microsoft signing-certificate publisher, got {:?}",
            verified.publisher
        );
    }

    /// The .NET host is embedded-signed; catalog support must not regress the
    /// original fast path. System Atlas's Windows UI build requires this SDK.
    #[test]
    fn embedded_signed_dll_is_still_signed() {
        let hostfxr = find_hostfxr().expect("a .NET hostfxr.dll under Program Files");
        let embedded = verify_embedded_signature(&hostfxr.to_string_lossy());
        assert_eq!(
            embedded.status,
            SignatureStatus::Signed,
            "hostfxr fixture should carry an embedded signature: {}",
            hostfxr.display()
        );
        assert_eq!(
            verify_signature(&hostfxr.to_string_lossy()),
            SignatureStatus::Signed
        );
        assert!(!embedded.publisher.is_empty());
    }

    #[test]
    fn tampered_catalog_binary_is_unsigned() {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        let powershell = format!("{root}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe");
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let temp = std::env::temp_dir().join(format!(
            "atlas-tampered-powershell-{}-{}.exe",
            std::process::id(),
            nonce
        ));
        std::fs::copy(&powershell, &temp).expect("copy PowerShell fixture");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&temp)
            .expect("open copied fixture");
        file.write_all(b"atlas-tamper")
            .expect("tamper copied fixture");
        drop(file);

        let status = verify_signature(&temp.to_string_lossy());
        let _ = std::fs::remove_file(&temp);
        assert_eq!(status, SignatureStatus::Unsigned);
    }

    fn find_hostfxr() -> Option<std::path::PathBuf> {
        let program_files = std::env::var_os("ProgramFiles")?;
        let fxr_root = std::path::PathBuf::from(program_files)
            .join("dotnet")
            .join("host")
            .join("fxr");
        let mut candidates: Vec<_> = std::fs::read_dir(fxr_root)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("hostfxr.dll"))
            .filter(|path| path.is_file())
            .collect();
        candidates.sort();
        candidates.pop()
    }
}
