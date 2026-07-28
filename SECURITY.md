# Security policy

Atlas collects system telemetry and can broker privileged actions.
Security reports are taken seriously, especially reports involving privilege
boundaries, IPC authorization, plugin verification, update or signing paths,
support-bundle redaction, and unsafe process control.

## Supported versions

| Version | Security fixes |
|---|---|
| Latest published release candidate | Supported |
| Older builds and development snapshots | Upgrade and reproduce on the latest build |

Until a stable release exists, security fixes are made against the latest
release candidate and the `main` branch.

## Reporting a vulnerability

Use GitHub's
[private vulnerability reporting form](https://github.com/iEssam/System-Atlas/security/advisories/new).
If the form is unavailable, email
[hello@iessam.com](mailto:hello@iessam.com) and use
`Atlas security` as the subject. Do not open a public issue.

Include as much of the following as is safe:

- Affected version, commit, and installation method
- Windows version, architecture, and relevant security configuration
- Vulnerability class and expected impact
- Reproduction steps or a minimal proof of concept
- Whether administrator or LocalSystem access is required
- Suggested remediation, if known
- Any disclosure deadline or existing public disclosure

Do not include real credentials, signing keys, personal system captures, or
unredacted private data. A maintainer may provide a safer transfer method when
large or sensitive artifacts are necessary.

The project aims to acknowledge a complete report within seven days. Validation
and remediation timelines depend on severity, reproducibility, affected
privilege boundaries, and release-signing requirements. Please allow a
reasonable coordinated-disclosure period before publishing details.

## Scope notes

The following are generally not vulnerabilities by themselves:

- The documented fact that current release-candidate MSI artifacts are unsigned
- Expected failures caused by Smart App Control blocking unsigned development
  binaries
- Unsupported hardware sensors being reported as unavailable or unknown
- Actions that already require local administrator access and do not cross an
  additional documented security boundary

Reports showing a boundary bypass, unsafe default, secret disclosure, signature
verification failure, authorization flaw, or material redaction failure remain
in scope even when local access is required.
