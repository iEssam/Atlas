# Current implementation state

Last reconciled with the repository: 2026-07-19.

This document describes the current source tree. It keeps implementation facts separate from requirements and target architecture:

- [project.md](../project.md) defines product requirements. A requirement is not evidence that a feature has shipped.
- [tech-stack.md](../tech-stack.md) defines the proposed and target technical design. Some sections intentionally describe infrastructure that has not landed.
- [phases.md](phases.md) is the detailed implementation tracker and release-gate record.
- [release notes](releases/v0.3.0-rc.1.md) describe the immutable `v0.3.0-rc.1` tag, not every change on the current branch.

When these documents disagree about what exists today, the source tree and automated checks win, followed by this page and the phase tracker.

## Build and packaging baseline

- Core service and supporting tools: stable Rust with the MSVC toolchain.
- Desktop UI: C# on .NET 10 and Windows App SDK 1.6.
- The current WinUI project is **unpackaged and self-contained** (`WindowsPackageType=None`, `WindowsAppSDKSelfContained=true`). It builds and launches as an ordinary x64 executable.
- The release candidate installer is a per-machine WiX MSI that carries the unpackaged UI payload and installs the `SystemAtlas` service.
- The published release candidate and MSI are x64 and unsigned. They are for evaluation, not production distribution.

The following target-architecture items are not current shipping facts: NativeAOT UI publishing, sparse-MSIX Explorer integration, ARM64 release artifacts, signed update manifests, staged update channels, winget distribution, a tray helper, and an emergency UI.

## Automated validation on the current branch

The primary CI workflow has two independent Windows jobs:

1. Rust: formatting, Clippy with warnings denied, and all workspace tests.
2. Windows UI: .NET restore, x64 WinUI build, a UI Automation launch/navigation smoke test, IPC-client tests, and source-level UI contract tests.

The UI contract suite guards authored XAML parsing, XAML event-handler wiring, shell navigation destinations, required responsive states, High Contrast resource parity, and accessible names for icon-only buttons. The smoke test launches the real unpackaged app and verifies the shell and a requested navigation destination through UI Automation.

Performance validation remains separate in `perf.yml`: hosted CI enforces the working-set budget and a short soak, while idle CPU is advisory because hosted runners are noisy. The authoritative 72-hour soak and bare-metal performance gate remain open.

Supply-chain validation is enforced in separate workflows:

- RustSec and `cargo-deny` reject known advisories, yanked crates, wildcard dependencies, unapproved licenses, and dependencies from unknown registries or Git sources. Duplicate transitive versions are reported as warnings.
- NuGet restores direct and transitive dependencies with all advisory severities treated as errors. Five committed `packages.lock.json` files make restore drift fail in CI.
- Dependabot proposes weekly Cargo, NuGet, and GitHub Actions updates.
- `release-sbom.yml` generates and attaches a CycloneDX JSON SBOM when a future GitHub release is published. It does not retroactively add an SBOM to the existing `v0.3.0-rc.1` release.

Rust+C# CodeQL and pull-request dependency-review jobs are configured but entitlement-gated. This is currently a private, personal GitHub repository, where GitHub Code Security is not available. Those jobs run if the repository becomes public or `ENABLE_GITHUB_CODE_SECURITY=true` is defined after moving to an entitled organization. Until then, `cargo-audit`, `cargo-deny`, and fail-closed NuGet auditing provide the active advisory gates, while Clippy and the .NET compiler remain the active static analyzers.

## Release readiness

System Atlas is still a release candidate. The stable release remains blocked by at least:

- production signing for the MSI and shipped binaries;
- signed release manifests and a staged update path;
- a full 72-hour soak on representative hardware;
- stable-release review of every open or deferred tracker item;
- broader UI automation, accessibility, scaling, and compatibility coverage beyond the current smoke and contract checks.

See [phases.md](phases.md) for the complete itemized status.
