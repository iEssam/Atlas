//! Expert security metadata (docs/phases.md Phase 3, PRD §9.4.1/§9.4.6): the
//! on-demand deep security detail for one process.
//!
//! [`security_metadata`] reuses the process inspector for identity (image path,
//! user SID, integrity, elevation, signature label) and augments it with:
//! * the on-disk image **SHA-256** (CNG BCrypt, streamed in chunks);
//! * the signing **certificate chain** (leaf → root) walked from the verified
//!   WinTrust provider state (see `winver::verify_signature_detail`);
//! * the token's **privileges** (name + enabled bit), **groups** (resolved name
//!   or SID string), app-container flag, and **capabilities**; and
//! * the readable process **mitigation policies** (DEP, ASLR, CFG, dynamic-code,
//!   image-load, child-process) as friendly strings.
//!
//! Every field is best-effort and honest: a cross-user/protected field that the
//! unprivileged handle cannot read sets `limited=true` and is skipped — the whole
//! result is never failed because one field was denied (PRD §9.6.7). A pid that
//! is gone/fully inaccessible is reported `available=false` with a reason.

#![cfg(windows)]

use std::io::Read;

use crate::ffi::{
    BCryptCloseAlgorithmProvider, BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash,
    BCryptHashData, BCryptOpenAlgorithmProvider, GetProcessMitigationPolicy, LookupPrivilegeNameW,
    OpenProcess, OpenProcessToken, BCRYPT_ALG_HANDLE, BCRYPT_HASH_HANDLE, DWORD, HANDLE,
    LUID_AND_ATTRIBUTES, MITIGATION_DEP_POLICY_SIZE, MITIGATION_FLAGS_POLICY_SIZE,
    PROCESS_ASLR_POLICY, PROCESS_CHILD_PROCESS_POLICY, PROCESS_CONTROL_FLOW_GUARD_POLICY,
    PROCESS_DEP_POLICY, PROCESS_DYNAMIC_CODE_POLICY, PROCESS_IMAGE_LOAD_POLICY,
    PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, SE_PRIVILEGE_ENABLED,
    SHA256_DIGEST_LEN, SID_AND_ATTRIBUTES, TOKEN_CAPABILITIES_CLASS, TOKEN_GROUPS,
    TOKEN_GROUPS_CLASS, TOKEN_IS_APP_CONTAINER_CLASS, TOKEN_PRIVILEGES, TOKEN_PRIVILEGES_CLASS,
    TOKEN_QUERY,
};
use crate::inspector::{
    get_token_information, lookup_account_name, process_detail, sid_to_string, OwnedHandle,
};
use crate::reg::to_wide;
use crate::winver::{verify_signature_detail, CertDetail};

/// One token privilege — mirrors the proto `TokenPrivilege`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenPrivilegeInfo {
    pub name: String,
    pub enabled: bool,
}

/// The deep security detail for one process — mirrors the proto
/// `SecurityMetadata` field-for-field so the service mapping is a straight copy.
/// Every augmented field is best-effort; `limited` is set when a field needed
/// elevation/access this handle did not have.
#[derive(Debug, Clone, Default)]
pub struct SecurityMetadata {
    pub file_sha256: String,
    pub signature_status: String,
    pub cert_chain: Vec<CertDetail>,
    pub user_sid: String,
    pub integrity_level: String,
    pub elevated: bool,
    pub app_container: bool,
    pub privileges: Vec<TokenPrivilegeInfo>,
    pub groups: Vec<String>,
    pub capabilities: Vec<String>,
    pub mitigations: Vec<String>,
    pub limited: bool,
}

/// Outcome of a security-metadata request — mirrors the proto reply. Identity
/// comes from the snapshot via the inspector, so `available=false` means the pid
/// is gone or fully inaccessible.
#[derive(Debug, Clone)]
pub struct SecurityMetadataResult {
    pub available: bool,
    pub unavailable_reason: String,
    pub metadata: Option<SecurityMetadata>,
}

/// Assembles the [`SecurityMetadata`] for `pid`, guarding PID reuse with
/// `create_time_100ns` when it is nonzero. Never escalates; unreadable fields
/// set `limited` and are skipped.
pub fn security_metadata(pid: u32, create_time_100ns: i64) -> SecurityMetadataResult {
    // Reuse the inspector for identity + the base signature label. A pid absent
    // from the snapshot has exited → available=false.
    let detail_res = process_detail(pid, create_time_100ns);
    if !detail_res.available {
        return SecurityMetadataResult {
            available: false,
            unavailable_reason: detail_res.unavailable_reason,
            metadata: None,
        };
    }
    let detail = match detail_res.detail {
        Some(d) => d,
        None => {
            return SecurityMetadataResult {
                available: false,
                unavailable_reason: "process exited".to_string(),
                metadata: None,
            }
        }
    };

    let mut meta = SecurityMetadata {
        signature_status: detail.signature_status.clone(),
        user_sid: detail.user_sid.clone(),
        integrity_level: detail.integrity_level.clone(),
        elevated: detail.elevated,
        // Inherit the inspector's coverage flag: if identity was partial, we are
        // already limited before adding the deep fields.
        limited: detail.limited,
        ..Default::default()
    };

    // File identity + signing chain from the on-disk image.
    if detail.image_path.is_empty() {
        meta.limited = true;
    } else {
        match sha256_file(&detail.image_path) {
            Some(hash) => meta.file_sha256 = hash,
            None => meta.limited = true,
        }
        // The chain is populated only on a trusted Signed verdict; an unsigned
        // image simply yields an empty chain (not a limitation).
        let sig = verify_signature_detail(&detail.image_path);
        meta.cert_chain = sig.cert_chain;
    }

    // Token detail via a limited-query handle (broadest same-user reach).
    match open_token_query(pid) {
        Some(token) => {
            fill_privileges(token.0, &mut meta);
            meta.groups = token_sid_list(token.0, TOKEN_GROUPS_CLASS);
            meta.app_container = query_app_container(token.0);
            meta.capabilities = token_sid_list(token.0, TOKEN_CAPABILITIES_CLASS);
        }
        None => meta.limited = true,
    }

    // Mitigation policies need PROCESS_QUERY_INFORMATION. A denial degrades
    // silently (the list stays as far as we got); a fully denied handle is a
    // coverage limitation.
    fill_mitigations(pid, &mut meta);

    SecurityMetadataResult {
        available: true,
        unavailable_reason: String::new(),
        metadata: Some(meta),
    }
}

/// Opens `pid`'s token for read (QUERY_LIMITED handle → OpenProcessToken).
fn open_token_query(pid: u32) -> Option<OwnedHandle> {
    // SAFETY: plain OpenProcess; NULL on failure.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return None;
    }
    let h = OwnedHandle(h);
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: token out-param is live; TOKEN_QUERY is read-only.
    let ok = unsafe { OpenProcessToken(h.0, TOKEN_QUERY, &mut token) };
    if ok == 0 || token.is_null() {
        return None;
    }
    Some(OwnedHandle(token))
}

/// Fills the token privileges (name + enabled bit) via
/// `GetTokenInformation(TokenPrivileges)` + `LookupPrivilegeNameW`.
fn fill_privileges(token: HANDLE, meta: &mut SecurityMetadata) {
    let Some(buf) = get_token_information(token, TOKEN_PRIVILEGES_CLASS) else {
        return;
    };
    if buf.len() < std::mem::size_of::<u32>() {
        return;
    }
    // SAFETY: buffer head is a TOKEN_PRIVILEGES; the LUID_AND_ATTRIBUTES array
    // follows the count. Iteration is bounded by both the reported count and the
    // buffer length so a short/garbled buffer can never over-read.
    let hdr = buf.as_ptr() as *const TOKEN_PRIVILEGES;
    let count = unsafe { (*hdr).PrivilegeCount } as usize;
    let entry = std::mem::size_of::<LUID_AND_ATTRIBUTES>();
    let header = std::mem::size_of::<u32>();
    let max = buf.len().saturating_sub(header) / entry;
    let n = count.min(max);
    let first = unsafe { (*hdr).Privileges.as_ptr() };
    for i in 0..n {
        // SAFETY: index i < n <= fits-in-buffer.
        let e = unsafe { &*first.add(i) };
        let enabled = privilege_enabled(e.Attributes);
        match lookup_privilege_name(e.Luid.LowPart, e.Luid.HighPart) {
            Some(name) if !name.is_empty() => {
                meta.privileges.push(TokenPrivilegeInfo { name, enabled })
            }
            _ => {}
        }
    }
}

/// Reads a `TOKEN_GROUPS`-shaped info class (groups or capabilities) into a list
/// of resolved account names (falling back to the SID string).
fn token_sid_list(token: HANDLE, class: DWORD) -> Vec<String> {
    let mut out = Vec::new();
    let Some(buf) = get_token_information(token, class) else {
        return out;
    };
    // The array is 8-byte aligned after the u32 count.
    let header = std::mem::size_of::<usize>();
    if buf.len() < header {
        return out;
    }
    // SAFETY: buffer head is a TOKEN_GROUPS; SID_AND_ATTRIBUTES array follows.
    let hdr = buf.as_ptr() as *const TOKEN_GROUPS;
    let count = unsafe { (*hdr).GroupCount } as usize;
    let entry = std::mem::size_of::<SID_AND_ATTRIBUTES>();
    let max = buf.len().saturating_sub(header) / entry;
    let n = count.min(max);
    let first = unsafe { (*hdr).Groups.as_ptr() };
    for i in 0..n {
        // SAFETY: i < n <= fits-in-buffer; Sid points into the token buffer.
        let sa = unsafe { &*first.add(i) };
        if sa.Sid.is_null() {
            continue;
        }
        let label = lookup_account_name(sa.Sid)
            .filter(|s| !s.is_empty())
            .or_else(|| sid_to_string(sa.Sid))
            .unwrap_or_default();
        if !label.is_empty() {
            out.push(label);
        }
    }
    out
}

/// `TokenIsAppContainer` → true when the token runs in an app container.
fn query_app_container(token: HANDLE) -> bool {
    match get_token_information(token, TOKEN_IS_APP_CONTAINER_CLASS) {
        Some(buf) if buf.len() >= 4 => u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) != 0,
        _ => false,
    }
}

/// `LookupPrivilegeNameW` for a privilege LUID → its name (e.g.
/// "SeChangeNotifyPrivilege"). None when the lookup fails.
fn lookup_privilege_name(low: u32, high: i32) -> Option<String> {
    let luid = crate::ffi::LUID {
        LowPart: low,
        HighPart: high,
    };
    let mut len: DWORD = 0;
    // SAFETY: null buffer probe → required length (incl. NUL) in `len`.
    unsafe { LookupPrivilegeNameW(std::ptr::null(), &luid, std::ptr::null_mut(), &mut len) };
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    let mut cap = buf.len() as DWORD;
    // SAFETY: buf sized to the probed length; cap passed in/out.
    let ok = unsafe { LookupPrivilegeNameW(std::ptr::null(), &luid, buf.as_mut_ptr(), &mut cap) };
    if ok == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..cap as usize]))
}

/// Queries the readable process mitigation policies and turns the ones that are
/// on into friendly strings. Needs `PROCESS_QUERY_INFORMATION`; a denied handle
/// is a coverage limitation (mitigations stay empty).
fn fill_mitigations(pid: u32, meta: &mut SecurityMetadata) {
    // SAFETY: plain OpenProcess; NULL on failure.
    let h = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION, 0, pid) };
    if h.is_null() {
        meta.limited = true;
        return;
    }
    let h = OwnedHandle(h);
    meta.mitigations = decode_mitigations(
        mitigation_flags(h.0, PROCESS_DEP_POLICY, MITIGATION_DEP_POLICY_SIZE),
        mitigation_flags(h.0, PROCESS_ASLR_POLICY, MITIGATION_FLAGS_POLICY_SIZE),
        mitigation_flags(
            h.0,
            PROCESS_CONTROL_FLOW_GUARD_POLICY,
            MITIGATION_FLAGS_POLICY_SIZE,
        ),
        mitigation_flags(
            h.0,
            PROCESS_DYNAMIC_CODE_POLICY,
            MITIGATION_FLAGS_POLICY_SIZE,
        ),
        mitigation_flags(h.0, PROCESS_IMAGE_LOAD_POLICY, MITIGATION_FLAGS_POLICY_SIZE),
        mitigation_flags(
            h.0,
            PROCESS_CHILD_PROCESS_POLICY,
            MITIGATION_FLAGS_POLICY_SIZE,
        ),
    );
}

/// One `GetProcessMitigationPolicy` read: the leading flags DWORD, or None when
/// the policy is not readable (degrade silently).
fn mitigation_flags(h: HANDLE, policy: u32, size: usize) -> Option<u32> {
    let mut buf = [0u8; 16];
    // SAFETY: buf is at least `size` bytes (size is 4 or 8, both < 16); the API
    // writes exactly `size` bytes on success.
    let ok = unsafe { GetProcessMitigationPolicy(h, policy, buf.as_mut_ptr().cast(), size) };
    if ok == 0 {
        None
    } else {
        Some(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]))
    }
}

/// SHA-256 of the on-disk image via CNG BCrypt, streamed in 64 KiB chunks and
/// lowercase-hex encoded. None on any open/hash failure. Every BCrypt object is
/// released before return.
fn sha256_file(path: &str) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;

    let alg_id = to_wide("SHA256");
    let mut alg: BCRYPT_ALG_HANDLE = std::ptr::null_mut();
    // SAFETY: alg out-param is live; the algorithm id is NUL-terminated.
    let st = unsafe { BCryptOpenAlgorithmProvider(&mut alg, alg_id.as_ptr(), std::ptr::null(), 0) };
    if st != 0 || alg.is_null() {
        return None;
    }

    let mut hash: BCRYPT_HASH_HANDLE = std::ptr::null_mut();
    // A NULL hash object lets CNG allocate it internally (Win7+).
    // SAFETY: hash out-param live; NULL object/secret asks CNG to self-manage.
    let st = unsafe {
        BCryptCreateHash(
            alg,
            &mut hash,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            0,
            0,
        )
    };
    if st != 0 || hash.is_null() {
        // SAFETY: alg was opened above; close it.
        unsafe { BCryptCloseAlgorithmProvider(alg, 0) };
        return None;
    }

    // Stream the file through the hash; any error aborts with a clean tear-down.
    let digest = (|| -> Option<[u8; SHA256_DIGEST_LEN]> {
        let mut buf = [0u8; 65536];
        loop {
            let n = file.read(&mut buf).ok()?;
            if n == 0 {
                break;
            }
            // SAFETY: buf holds `n` readable bytes; the hash object is live.
            let st = unsafe { BCryptHashData(hash, buf.as_ptr(), n as DWORD, 0) };
            if st != 0 {
                return None;
            }
        }
        let mut out = [0u8; SHA256_DIGEST_LEN];
        // SAFETY: out is exactly the SHA-256 digest length.
        let st = unsafe { BCryptFinishHash(hash, out.as_mut_ptr(), SHA256_DIGEST_LEN as DWORD, 0) };
        if st != 0 {
            None
        } else {
            Some(out)
        }
    })();

    // SAFETY: both objects were successfully created above; release once each.
    unsafe {
        BCryptDestroyHash(hash);
        BCryptCloseAlgorithmProvider(alg, 0);
    }
    digest.map(|d| hex_lower(&d))
}

// --- Pure helpers (unit-tested without touching the OS) ---------------------

/// True when a privilege's attribute bits mark it enabled.
pub fn privilege_enabled(attributes: u32) -> bool {
    attributes & SE_PRIVILEGE_ENABLED != 0
}

/// Lowercase-hex of a byte slice (file SHA-256 form).
pub fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Turns the readable mitigation-policy flag DWORDs into friendly on-strings.
/// Each argument is `None` when its policy was not readable (skipped silently);
/// only the mitigations that are *on* are listed.
pub fn decode_mitigations(
    dep: Option<u32>,
    aslr: Option<u32>,
    cfg: Option<u32>,
    dynamic_code: Option<u32>,
    image_load: Option<u32>,
    child_process: Option<u32>,
) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(f) = dep {
        if f & 0x1 != 0 {
            out.push("DEP".to_string());
        }
    }
    if let Some(f) = aslr {
        let bottom_up = f & 0x1 != 0; // EnableBottomUpRandomization
        let high_entropy = f & 0x4 != 0; // EnableHighEntropy
        if high_entropy {
            out.push("ASLR (high-entropy)".to_string());
        } else if bottom_up {
            out.push("ASLR".to_string());
        }
    }
    if let Some(f) = cfg {
        if f & 0x1 != 0 {
            // EnableControlFlowGuard
            out.push("CFG".to_string());
        }
    }
    if let Some(f) = dynamic_code {
        if f & 0x1 != 0 {
            // ProhibitDynamicCode
            out.push("no dynamic code".to_string());
        }
    }
    if let Some(f) = image_load {
        if f & 0x1 != 0 {
            // NoRemoteImages
            out.push("no remote image loads".to_string());
        }
        if f & 0x4 != 0 {
            // PreferSystem32Images
            out.push("prefer System32 images".to_string());
        }
    }
    if let Some(f) = child_process {
        if f & 0x1 != 0 {
            // NoChildProcessCreation
            out.push("no child processes".to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn hex_lower_encodes_lowercase() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(hex_lower(&[]), "");
        // A 32-byte digest is 64 hex chars.
        assert_eq!(hex_lower(&[0xab; 32]).len(), 64);
    }

    #[test]
    fn privilege_enabled_reads_attribute_bit() {
        assert!(privilege_enabled(SE_PRIVILEGE_ENABLED));
        assert!(privilege_enabled(SE_PRIVILEGE_ENABLED | 0x1));
        assert!(!privilege_enabled(0));
        assert!(!privilege_enabled(0x1)); // SE_PRIVILEGE_ENABLED_BY_DEFAULT only
    }

    #[test]
    fn decode_mitigations_lists_only_on_flags() {
        // All off / unreadable → empty.
        assert!(decode_mitigations(None, None, None, None, None, None).is_empty());
        assert!(
            decode_mitigations(Some(0), Some(0), Some(0), Some(0), Some(0), Some(0)).is_empty()
        );

        // DEP on, CFG on, child-process on.
        let m = decode_mitigations(Some(0x1), None, Some(0x1), None, None, Some(0x1));
        assert_eq!(m, vec!["DEP", "CFG", "no child processes"]);

        // ASLR high-entropy wins over the plain label.
        let m = decode_mitigations(None, Some(0x1 | 0x4), None, None, None, None);
        assert_eq!(m, vec!["ASLR (high-entropy)"]);
        // Bottom-up only → plain ASLR.
        let m = decode_mitigations(None, Some(0x1), None, None, None, None);
        assert_eq!(m, vec!["ASLR"]);

        // Dynamic-code + image-load flags.
        let m = decode_mitigations(None, None, None, Some(0x1), Some(0x1 | 0x4), None);
        assert_eq!(
            m,
            vec![
                "no dynamic code",
                "no remote image loads",
                "prefer System32 images"
            ]
        );
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn token_ffi_layouts_match_windows_sdk() {
        // LUID_AND_ATTRIBUTES: LUID (8) + Attributes (4), 4-aligned → 12.
        assert_eq!(size_of::<LUID_AND_ATTRIBUTES>(), 12);
        // TOKEN_PRIVILEGES: count (4) then the 4-aligned entry array at 4.
        assert_eq!(offset_of!(TOKEN_PRIVILEGES, Privileges), 4);
        // TOKEN_GROUPS: count (4) then the 8-aligned SID_AND_ATTRIBUTES array at 8.
        assert_eq!(size_of::<SID_AND_ATTRIBUTES>(), 16);
        assert_eq!(offset_of!(TOKEN_GROUPS, Groups), 8);
    }

    /// Self-target smoke: our own process always yields full security metadata.
    /// The file hash is 64 hex chars, at least SeChangeNotifyPrivilege (enabled)
    /// shows up, groups are nonempty, and mitigations resolve for our own image.
    #[test]
    fn own_security_metadata_is_populated() {
        let me = std::process::id();
        let res = security_metadata(me, 0);
        assert!(res.available, "own security metadata must be available");
        let m = res.metadata.expect("metadata present");

        assert_eq!(
            m.file_sha256.len(),
            64,
            "own image SHA-256 should be 64 hex chars, got {:?}",
            m.file_sha256
        );
        assert!(
            m.file_sha256.chars().all(|c| c.is_ascii_hexdigit()),
            "SHA-256 should be hex"
        );

        assert!(!m.user_sid.is_empty(), "own user SID resolvable");
        assert!(!m.integrity_level.is_empty(), "own integrity resolvable");

        // Every normal user token holds SeChangeNotifyPrivilege, enabled.
        let change_notify = m
            .privileges
            .iter()
            .find(|p| p.name == "SeChangeNotifyPrivilege")
            .expect("SeChangeNotifyPrivilege present on our token");
        assert!(
            change_notify.enabled,
            "SeChangeNotify is enabled by default"
        );

        assert!(!m.groups.is_empty(), "own token has groups");
        // Our own dev/test binary is not an app container.
        assert!(!m.app_container);
        // Mitigations vary by build, but DEP/ASLR are on for a modern image.
        assert!(
            m.mitigations.iter().any(|s| s == "DEP")
                || m.mitigations.iter().any(|s| s.starts_with("ASLR")),
            "expected at least DEP or ASLR on our own image, got {:?}",
            m.mitigations
        );
    }

    #[test]
    fn absent_pid_security_metadata_unavailable() {
        let res = security_metadata(0xFFFF_FFF0, 0);
        assert!(!res.available);
        assert!(res.unavailable_reason.contains("exited"));
    }
}
