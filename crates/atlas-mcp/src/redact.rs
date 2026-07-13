//! MCP-strict redaction (tech-stack.md §4.7, §9.16.3).
//!
//! Every value an `atlas-mcp` tool returns egresses to the client's model
//! provider the moment the client reads it. That makes the MCP surface the most
//! sensitive view Atlas exposes, so redaction here is **default-ON and stricter
//! than the in-app views**: file paths, usernames, the computer name, DNS
//! domains, command lines, and (configurably) application names are scrubbed
//! before anything leaves this process.
//!
//! Each category is an independent toggle so the redaction can be relaxed one
//! axis at a time via CLI flags / env (e.g. `--no-redact-app-names`). Free-text
//! fields (summaries, evidence, descriptions) that may embed identifiers are run
//! through [`Redactor::scrub`], which substitutes the local identity tokens and
//! any Windows path-like runs it finds.

/// Placeholder tokens. Kept short + stable so the client model can recognise
/// them as redaction markers rather than data.
pub const PATH: &str = "<PATH>";
pub const USER: &str = "<USER>";
pub const HOST: &str = "<HOST>";
pub const DOMAIN: &str = "<DOMAIN>";
pub const CMD: &str = "<CMD>";
pub const APP: &str = "<APP>";

/// Per-category redaction toggles. All default to `true` (strictest).
#[derive(Clone, Debug)]
pub struct RedactConfig {
    pub paths: bool,
    pub user_names: bool,
    pub computer_name: bool,
    pub domains: bool,
    pub command_lines: bool,
    pub app_names: bool,
}

impl Default for RedactConfig {
    fn default() -> Self {
        Self {
            paths: true,
            user_names: true,
            computer_name: true,
            domains: true,
            command_lines: true,
            app_names: true,
        }
    }
}

/// Applies MCP-strict redaction to tool-output fields.
///
/// Constructed once at startup with the local machine's identity tokens
/// (username / computer name / user-profile dir) so `scrub` can find them inside
/// free text. Field-typed helpers (`path`, `command_line`, `app_name`,
/// `domain`, `user`, `host`) redact a whole value when their category is on.
#[derive(Clone, Debug)]
pub struct Redactor {
    cfg: RedactConfig,
    /// Lowercased local username (e.g. "essam"); empty when unknown.
    username: String,
    /// Lowercased computer name; empty when unknown.
    computer: String,
    /// Lowercased user-profile directory (e.g. "c:\\users\\essam").
    userprofile: String,
}

impl Redactor {
    /// Builds a redactor from the given config, reading the local identity
    /// tokens from the environment (USERNAME / COMPUTERNAME / USERPROFILE).
    pub fn new(cfg: RedactConfig) -> Self {
        let env_lower = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.to_lowercase())
                .filter(|v| !v.is_empty())
                .unwrap_or_default()
        };
        Self {
            cfg,
            username: env_lower("USERNAME"),
            computer: env_lower("COMPUTERNAME"),
            userprofile: env_lower("USERPROFILE"),
        }
    }

    /// Test/explicit constructor with the identity tokens supplied directly.
    #[allow(dead_code)] // used by unit tests to pin identity tokens deterministically
    pub fn with_identity(
        cfg: RedactConfig,
        username: &str,
        computer: &str,
        userprofile: &str,
    ) -> Self {
        Self {
            cfg,
            username: username.to_lowercase(),
            computer: computer.to_lowercase(),
            userprofile: userprofile.to_lowercase(),
        }
    }

    /// A file-system path field (image path, binary path, working dir, module
    /// path). Replaced wholesale — the strictest, non-reconstructable form.
    pub fn path(&self, s: &str) -> String {
        if s.is_empty() {
            return String::new();
        }
        if self.cfg.paths {
            PATH.to_string()
        } else {
            s.to_string()
        }
    }

    /// A command line (executable + arguments). Command lines routinely embed
    /// paths, tokens, and secrets, so the whole value is replaced when on.
    pub fn command_line(&self, s: &str) -> String {
        if s.is_empty() {
            return String::new();
        }
        if self.cfg.command_lines {
            CMD.to_string()
        } else {
            // Even with command-line redaction relaxed, still scrub identity
            // tokens / embedded paths out of the retained value.
            self.scrub(s)
        }
    }

    /// An application / image name (e.g. "chrome.exe"). Configurable because an
    /// app *inventory* is often the point of the query; `--no-redact-app-names`
    /// keeps names while everything else stays scrubbed.
    pub fn app_name(&self, s: &str) -> String {
        if s.is_empty() {
            return String::new();
        }
        if self.cfg.app_names {
            APP.to_string()
        } else {
            s.to_string()
        }
    }

    /// A DNS domain (resolved remote host). Replaced wholesale when on.
    pub fn domain(&self, s: &str) -> String {
        if s.is_empty() {
            return String::new();
        }
        if self.cfg.domains {
            DOMAIN.to_string()
        } else {
            s.to_string()
        }
    }

    /// A user name / SID / account field. When user-name redaction is on the
    /// value is replaced wholesale; otherwise it is returned as-is.
    pub fn user(&self, s: &str) -> String {
        if s.is_empty() {
            return String::new();
        }
        if self.cfg.user_names {
            USER.to_string()
        } else {
            s.to_string()
        }
    }

    /// A computer/host name field. (Host names are also scrubbed out of free
    /// text via [`Redactor::scrub`]; this is the wholesale-field form.)
    #[allow(dead_code)]
    pub fn host(&self, s: &str) -> String {
        if s.is_empty() {
            return String::new();
        }
        if self.cfg.computer_name {
            HOST.to_string()
        } else {
            s.to_string()
        }
    }

    /// Free-text scrub for fields that may *embed* identifiers (diagnosis
    /// summaries, evidence text, task authors, service accounts, publishers).
    ///
    /// Substitutes, in order: the user-profile dir (→ `<PATH>`), any remaining
    /// Windows path-like runs (drive `X:\…` or UNC `\\…` → `<PATH>`), the
    /// computer name (→ `<HOST>`), and the username (→ `<USER>`), each gated by
    /// its own toggle. Case-insensitive for the identity tokens.
    pub fn scrub(&self, s: &str) -> String {
        if s.is_empty() {
            return String::new();
        }
        let mut out = s.to_string();
        if self.cfg.paths {
            // Collapse whole Windows path runs first (this also swallows any
            // user-profile-rooted path), then mop up a residual bare
            // user-profile prefix that wasn't in canonical drive form.
            out = scrub_paths(&out);
            if !self.userprofile.is_empty() {
                out = replace_ci(&out, &self.userprofile, PATH);
            }
        }
        if self.cfg.computer_name && !self.computer.is_empty() {
            out = replace_ci(&out, &self.computer, HOST);
        }
        if self.cfg.user_names && !self.username.is_empty() {
            out = replace_ci(&out, &self.username, USER);
        }
        out
    }
}

/// Case-insensitive substring replacement (no regex dependency).
fn replace_ci(haystack: &str, needle: &str, with: &str) -> String {
    if needle.is_empty() {
        return haystack.to_string();
    }
    let hay_lower = haystack.to_lowercase();
    let needle_lower = needle.to_lowercase();
    let mut out = String::with_capacity(haystack.len());
    let mut cursor = 0usize;
    while let Some(rel) = hay_lower[cursor..].find(&needle_lower) {
        let start = cursor + rel;
        out.push_str(&haystack[cursor..start]);
        out.push_str(with);
        cursor = start + needle_lower.len();
    }
    out.push_str(&haystack[cursor..]);
    out
}

/// Replaces Windows path-like runs (drive-letter `X:\…` and UNC `\\…`) with
/// `<PATH>`. A "run" extends until whitespace or a quote — good enough to strip
/// embedded paths from free text without a regex engine.
fn scrub_paths(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let drive = i + 2 < bytes.len()
            && bytes[i].is_ascii_alphabetic()
            && bytes[i + 1] == b':'
            && (bytes[i + 2] == b'\\' || bytes[i + 2] == b'/');
        let unc = i + 1 < bytes.len() && bytes[i] == b'\\' && bytes[i + 1] == b'\\';
        if drive || unc {
            // Consume until a delimiter that never appears mid-path.
            let mut j = i;
            while j < bytes.len() && !matches!(bytes[j], b' ' | b'\t' | b'"' | b'\'' | b',' | b';')
            {
                j += 1;
            }
            out.push_str(PATH);
            i = j;
        } else {
            // Push one UTF-8 char.
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident_cfg() -> RedactConfig {
        RedactConfig::default()
    }

    fn redactor(cfg: RedactConfig) -> Redactor {
        Redactor::with_identity(cfg, "essam", "ATLAS-PC", r"C:\Users\essam")
    }

    #[test]
    fn paths_toggle() {
        let on = redactor(ident_cfg());
        assert_eq!(on.path(r"C:\Windows\explorer.exe"), PATH);
        let mut cfg = ident_cfg();
        cfg.paths = false;
        let off = redactor(cfg);
        assert_eq!(
            off.path(r"C:\Windows\explorer.exe"),
            r"C:\Windows\explorer.exe"
        );
    }

    #[test]
    fn command_lines_toggle() {
        let on = redactor(ident_cfg());
        assert_eq!(on.command_line(r#""C:\app.exe" --token abc"#), CMD);
        let mut cfg = ident_cfg();
        cfg.command_lines = false;
        // With command-lines relaxed, the embedded path is still scrubbed.
        let off = redactor(cfg);
        let got = off.command_line(r#""C:\app.exe" --flag"#);
        assert!(
            got.contains(PATH),
            "expected embedded path scrub, got {got}"
        );
        assert!(got.contains("--flag"));
    }

    #[test]
    fn app_names_toggle() {
        let on = redactor(ident_cfg());
        assert_eq!(on.app_name("chrome.exe"), APP);
        let mut cfg = ident_cfg();
        cfg.app_names = false;
        let off = redactor(cfg);
        assert_eq!(off.app_name("chrome.exe"), "chrome.exe");
    }

    #[test]
    fn domains_toggle() {
        let on = redactor(ident_cfg());
        assert_eq!(on.domain("telemetry.example.com"), DOMAIN);
        let mut cfg = ident_cfg();
        cfg.domains = false;
        let off = redactor(cfg);
        assert_eq!(off.domain("telemetry.example.com"), "telemetry.example.com");
    }

    #[test]
    fn user_names_toggle() {
        let on = redactor(ident_cfg());
        assert_eq!(on.user(r"ATLAS-PC\essam"), USER);
        let mut cfg = ident_cfg();
        cfg.user_names = false;
        let off = redactor(cfg);
        assert_eq!(off.user(r"ATLAS-PC\essam"), r"ATLAS-PC\essam");
    }

    #[test]
    fn computer_name_toggle() {
        let on = redactor(ident_cfg());
        assert_eq!(on.host("ATLAS-PC"), HOST);
        let mut cfg = ident_cfg();
        cfg.computer_name = false;
        let off = redactor(cfg);
        assert_eq!(off.host("ATLAS-PC"), "ATLAS-PC");
    }

    #[test]
    fn scrub_replaces_identity_tokens() {
        let r = redactor(ident_cfg());
        // Username (case-insensitive) and computer name inside free text.
        let got = r.scrub("user ESSAM on ATLAS-PC hit a wall");
        assert_eq!(got, format!("user {USER} on {HOST} hit a wall"));
    }

    #[test]
    fn scrub_strips_embedded_paths() {
        let r = redactor(ident_cfg());
        let got = r.scrub(r"loaded C:\Users\essam\app.dll and \\server\share\x.sys ok");
        // Both the drive path and the UNC path collapse to <PATH>.
        assert_eq!(got, format!("loaded {PATH} and {PATH} ok"));
        assert!(!got.to_lowercase().contains("essam"));
    }

    #[test]
    fn scrub_userprofile_prefix() {
        let r = redactor(ident_cfg());
        let got = r.scrub(r"C:\Users\essam\Downloads\report.pdf");
        assert_eq!(got, PATH);
    }

    #[test]
    fn scrub_noop_when_all_off() {
        let cfg = RedactConfig {
            paths: false,
            user_names: false,
            computer_name: false,
            domains: false,
            command_lines: false,
            app_names: false,
        };
        let r = redactor(cfg);
        let s = r"C:\Users\essam on ATLAS-PC";
        assert_eq!(r.scrub(s), s);
    }

    #[test]
    fn empty_stays_empty() {
        let r = redactor(ident_cfg());
        assert_eq!(r.path(""), "");
        assert_eq!(r.app_name(""), "");
        assert_eq!(r.scrub(""), "");
    }
}
