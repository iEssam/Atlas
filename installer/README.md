# System Atlas installer (M9)

WiX v5/v6 authoring for the per-machine System Atlas MSI. Everything a
maintainer needs to produce, sign, and ship the installer lives here.

> Scope: this directory only. It touches no proto/UI/Rust source. The service's
> SCM entry point and the emergency-UI/tray/CLI binaries are built elsewhere;
> this package wires them in by convention (see "Coordination" below).

## Contents

| File | Purpose |
|---|---|
| `Package.wxs` | The MSI: service + WinUI app + data dir + Start Menu shortcut, feature tree, MajorUpgrade. |
| `build.ps1` | Builds prereqs (cargo + dotnet), then invokes WiX. Fails loudly if WiX is missing. |
| `harvest.md` | How the WinUI publish tree (hundreds of files) is harvested (`<Files>`). |
| `README.md` | This file: signing, staged updates, winget, ARM64. |
| `out/` | Build output (`SystemAtlas-<version>-<arch>.msi`). Created on build; git-ignore it. |

## Prerequisites

- **WiX v5 (or v6)** as a .NET global tool, plus the Util extension:
  ```powershell
  dotnet tool install --global wix --version 5.0.2
  wix extension add -g WixToolset.Util.wixext/5.0.2
  # ensure %USERPROFILE%\.dotnet\tools is on PATH
  ```
  (Alternatively `winget install --id WiXToolset.WiXToolset`.)
- Rust (MSVC) toolchain and the .NET 10 SDK for the prereq binaries.

## Build

```powershell
# Full build (service + app + MSI):
pwsh installer/build.ps1 -Version 0.1.0.0 -Platform x64

# Validate the WiX authoring only, against already-staged/placeholder binaries
# (use where App Control blocks fresh build outputs):
pwsh installer/build.ps1 -Version 0.1.0.0 -SkipPrereqs
```

The two inputs `Package.wxs` needs:
- `atlas-service.exe` from `cargo build --release -p atlas-service`
- the `Atlas.App` publish folder from
  `dotnet publish src-ui/Atlas.App -c Release -r win-x64 --self-contained true`

Output: `installer/out/SystemAtlas-<version>-<arch>.msi` (**unsigned**).

## What the MSI does

- **Service** (`tech-stack.md` §4.1, §8): installs `atlas-service.exe` to
  `%ProgramFiles%\System Atlas\` and registers the **`SystemAtlas`** Windows
  service (auto-start, LocalSystem) via `ServiceInstall` + `ServiceControl`
  (start on install, stop+remove on uninstall). Crash-restart recovery via
  `util:ServiceConfig` (restart on the first two failures, daily reset).
- **App** (§4.6): installs the self-contained WinUI `Atlas.App.exe` payload to
  the same directory (harvested with `<Files>`, see `harvest.md`) and adds an
  all-users Start Menu shortcut.
- **Data dir** (§7, §14.4): creates `%ProgramData%\SystemAtlas\` with a DACL of
  SYSTEM=full, Administrators=full, Users=read/traverse. Per-user SID-keyed
  subfolders and inheritance-stripping are a documented **runtime** job for the
  service (see the ACL note in `Package.wxs`).
- **Upgrades**: stable `UpgradeCode`, `MajorUpgrade` (downgrade blocked, same-
  version reinstall allowed), version from the `-Version` parameter.

### Feature tree

```
ProductCore (required)          service + data dir
└─ DesktopApp (optional)        WinUI app payload + Start Menu shortcut
   (EmergencyUi / TrayHelper / CommandLine: stubbed, land when binaries exist)
```

An enterprise admin can do a service-only fleet install with
`msiexec /i SystemAtlas-...msi ADDLOCAL=ProductCore`.

## Coordination with the service agent (IMPORTANT)

Two by-convention contracts this installer assumes; both are commented in
`Package.wxs`:

1. **Service name = `SystemAtlas`.** The Rust agent adding SCM support (M9
   "Windows service mode") must register under this exact short name.
2. **SCM entry point.** `atlas-service.exe` is today a console dev binary and
   does not yet implement `StartServiceCtrlDispatcher`. The MSI invokes it with
   the argument `run-service`; the service must add a matching branch that
   checks in with the SCM. Until that lands, `ServiceControl Start="install"`
   will fail at install time (the service won't report running) - expected, and
   the single integration point to close.

## Signing (tech-stack.md §7, §8)

The MSI **and every binary inside it** must be Authenticode-signed with an EV
certificate or **Azure Trusted Signing** before distribution. `build.ps1` never
signs; sign as a discrete post-build step.

Sign the payload binaries *before* building the MSI (so their signatures are
embedded), then sign the MSI itself:

```powershell
# 1. sign the input binaries (example: local EV cert in the store by /a auto-select)
signtool sign /fd SHA256 /tr http://timestamp.acme-ca.example/rfc3161 /td SHA256 /a `
    installer\stage\atlas-service.exe
#   ...and each file in the WinUI publish dir you author signing for.

# 2. build the MSI (installer/build.ps1)

# 3. sign the MSI
signtool sign /fd SHA256 /tr http://timestamp.acme-ca.example/rfc3161 /td SHA256 /a `
    installer\out\SystemAtlas-0.1.0.0-x64.msi
```

**Azure Trusted Signing** (recommended, no HSM to manage) uses the
`Azure.CodeSigning` Dlib with signtool:

```powershell
signtool sign /v /debug /fd SHA256 /tr http://timestamp.acme.example /td SHA256 `
    /dlib "C:\path\Azure.CodeSigning.Dlib.dll" `
    /dmdf "C:\path\metadata.json" `   # holds Endpoint, CodeSigningAccountName, CertificateProfileName
    installer\out\SystemAtlas-0.1.0.0-x64.msi
```

> Placeholder values above (`acme-ca.example`, cert store `/a`, dlib/metadata
> paths) must be replaced with the real CA/timestamp URL and the real Trusted
> Signing account/profile. These are the ONLY stubbed pieces of the signing
> flow.

## Staged updates (tech-stack.md §8, §14.3)

The updater is a separate scheduled task (not built here). This installer is one
artifact in that channel model:

- **Rings:** `canary → beta → stable`, with a separate expedited **security**
  channel. Health-gated promotion: a build graduates only after the ring's
  telemetry stays green.
- **Signed manifest (TUF-inspired):** an offline **root** key signs the
  **manifest** key; the manifest key signs a JSON manifest per ring listing
  version, arch, MSI URL, size, and SHA-256. HTTPS + signature both required.
  The updater refuses an unsigned or mismatched manifest.
- **Apply:** service-drain (flush + stop) → `msiexec` major upgrade → start. The
  UI prompts and shows release notes pre-apply; never force-restarts a session.
- **Rollback:** the previous MSI is retained locally for one-click rollback.

Manifest sketch (`manifest.stable.json`, signature detached in `.sig`):

```json
{
  "schemaVersion": 1,
  "channel": "stable",
  "product": "SystemAtlas",
  "releases": [
    {
      "version": "0.1.0.0",
      "arch": "x64",
      "url": "https://updates.systematlas.example/stable/SystemAtlas-0.1.0.0-x64.msi",
      "sizeBytes": 0,
      "sha256": "<hex>",
      "releaseNotesUrl": "https://systematlas.example/notes/0.1.0",
      "minFromVersion": "0.0.0.0"
    }
  ]
}
```

*(URLs/keys are placeholders. Root/manifest key management and the updater task
itself are out of scope for this directory.)*

## winget (tech-stack.md §8)

Manifest sketch for `winget` submission (three-file v1.6 manifest form). Fill in
the real `InstallerSha256` (from the signed MSI), `ProductCode` (the MSI's
`ProductCode` GUID from its Property table), and publisher/URLs:

```yaml
# SystemAtlasProject.SystemAtlas.installer.yaml
PackageIdentifier: SystemAtlasProject.SystemAtlas
PackageVersion: 0.1.0.0
InstallerType: wix          # MSI authored with WiX
Scope: machine
InstallModes: [ silent, silentWithProgress ]
Installers:
  - Architecture: x64
    InstallerUrl: https://updates.systematlas.example/stable/SystemAtlas-0.1.0.0-x64.msi
    InstallerSha256: <HEX>
    ProductCode: '{<MSI ProductCode GUID>}'
  - Architecture: arm64      # see ARM64 note
    InstallerUrl: https://updates.systematlas.example/stable/SystemAtlas-0.1.0.0-arm64.msi
    InstallerSha256: <HEX>
    ProductCode: '{<arm64 MSI ProductCode GUID>}'
ManifestType: installer
ManifestVersion: 1.6.0
```

Install command once published: `winget install --id SystemAtlasProject.SystemAtlas`.

## ARM64 (tech-stack.md §8 "ARM64 first-class from day one")

The authoring is arch-agnostic; `build.ps1 -Platform arm64` maps to the
`aarch64-pc-windows-msvc` Rust target and the `win-arm64` .NET RID, and passes
`-arch arm64` to WiX. Two things the ARM64 build needs that x64 already has:

1. **A distinct `UpgradeCode`** for the ARM64 package, so x64 and ARM64 are
   independent products that never cross-upgrade. Add an `<?if $(var.Platform) =
   arm64 ?>`-guarded UpgradeCode in `Package.wxs` (currently the x64 UpgradeCode
   `4E70F468-...` is hard-coded).
2. **ARM64 prereq binaries** - the Rust and .NET publishes must target ARM64;
   vendor GPU libraries are feature-flagged per arch upstream (not an installer
   concern).

Ship both MSIs side by side; winget/manifest select by `Architecture`.

## Shell extension (tech-stack.md §4.8) - not in this MSI

The File Explorer "Find what is using this file" `IExplorerCommand` handler
requires **package identity** (a sparse MSIX), which an MSI cannot grant. It
ships as a separate signed `.msix` registered post-install (out of scope until
the shell-ext binary exists; noted here so it is not forgotten).

## Do NOT install on a dev machine

Installing this MSI registers the real `SystemAtlas` LocalSystem service. Build
and inspect the MSI here; install only on a throwaway/VM. On this repo's build
machine an Application Control policy also blocks executing freshly built,
unsigned binaries - see the verification note below and `docs/phases.md`.

## Verification status on this machine

- WiX **v5.0.2** installed cleanly as a dotnet global tool; the
  `WixToolset.Util.wixext` extension added successfully. `winget` is present
  (v1.29) but the dotnet-tool route was used.
- `Package.wxs` **compiles** to a valid MSI (`wix build`, exit 0) against
  placeholder binaries. The compiled MSI was inspected: `ServiceInstall` and
  `ServiceControl` both name `SystemAtlas`; the `<Files>` harvest packaged the
  app payload and excluded `.pdb`; `CreateFolder` targets the data dir; the
  ServiceConfig (crash-restart) and SecureObjects (ACL) custom actions and the
  MajorUpgrade `Upgrade` row are all present.
- The MSI was **not installed** (would register a real service), and the real
  prereq binaries were **not built** (no cargo run; App Control blocks fresh
  unsigned build outputs per `docs/phases.md`). Producing a *shippable* MSI
  requires a maintainer to build+sign the real binaries on a policy-exempt
  machine, then run `build.ps1`.
