# Contributing to Atlas

Thank you for helping improve Atlas. Contributions to code,
documentation, tests, diagnostics, and accessibility are welcome.

By participating, you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
For usage questions, read [SUPPORT.md](SUPPORT.md). Do not report a security
vulnerability in a public issue; follow [SECURITY.md](SECURITY.md).

## Before starting

- Search existing issues and pull requests before opening a new one.
- For a substantial feature or architectural change, open an issue first so the
  design and safety boundaries can be agreed before implementation.
- Keep each pull request focused on one problem.
- Never commit private system captures, credentials, signing material, personal
  data, or unredacted support bundles.

Good first contributions include documentation corrections, tests, small
accessibility improvements, and narrowly scoped diagnostics fixes.

## Development environment

Atlas is a Windows project. The main prerequisites are:

- Windows 10 version 1809 or newer
- Stable Rust with the MSVC toolchain
- .NET 10 SDK
- Visual Studio Build Tools with C++ desktop tooling for the Explorer command

Install the two primary toolchains:

```powershell
winget install Rustlang.Rustup
winget install Microsoft.DotNet.SDK.10
```

Clone your fork and create a topic branch:

```powershell
git clone https://github.com/YOUR-USER/System-Atlas.git
Set-Location System-Atlas
git switch -c fix/short-description
```

Start the complete development stack:

```powershell
.\scripts\dev.ps1
```

The script builds the Rust service, GPU vendor helper, and WinUI application,
then starts the recorder, IPC backend, and desktop application. Press `Ctrl+C`
in that console to stop the complete stack cleanly.

Useful variants:

```powershell
.\scripts\dev.ps1 -SkipBuild
.\scripts\dev.ps1 -NoRecord
.\scripts\dev.ps1 -Configuration Release
```

Development data is stored outside the repository under
`%LOCALAPPDATA%\SystemAtlas\dev`.

## Required checks

Run the checks relevant to your change before opening a pull request.

Rust:

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

.NET and documentation:

```powershell
dotnet restore src-ui/Atlas.sln -p:Platform=x64 --locked-mode
dotnet test src-ui/Atlas.sln -c Debug -p:Platform=x64 --no-restore
.\scripts\validate-docs.ps1
```

UI changes:

```powershell
.\scripts\ui-smoke.ps1 -StartPage graphics -ExpectedElementName Graphics
```

For UI work, include before and after screenshots and test every changed
interactive control with a real pointer or keyboard. Check keyboard navigation,
visible focus, text scaling, High Contrast behavior, clipping, and readable
contrast. Content must remain visible if animation or JavaScript does not run.

Some validation requires elevation, physical hardware, or an appropriately
configured Windows machine. You are not expected to run every hardware test.
State exactly what you ran and what you could not run in the pull request.

## Project expectations

- Preserve the evidence-first product model: unknown data must remain unknown
  rather than being guessed.
- Privileged actions must be explicit, audited, reversible where promised, and
  verified after execution.
- Maintain compatibility with both x64 and arm64 code paths unless an existing
  documented limitation applies.
- Treat `proto/atlas.proto` as the IPC contract source of truth.
- Keep public APIs and user-facing behavior documented.
- Add or update tests for behavioral changes.
- Avoid unrelated formatting or refactoring in the same pull request.

## Commits and certificate of origin

Contributions use the [Developer Certificate of Origin 1.1](https://developercertificate.org/).
Sign off every commit to certify that you have the right to submit the work
under this project's license:

```powershell
git commit -s -m "fix: describe the change"
```

The sign-off adds a `Signed-off-by` trailer using your Git identity. It is not a
GPG signature.

## Pull requests

A pull request should:

- Explain the problem and the chosen solution.
- Link the related issue when one exists.
- Include tests or explain why a test is not practical.
- List the commands you ran and their results.
- Call out security, privacy, performance, compatibility, and migration impact.
- Include screenshots or a short recording for visible UI changes.
- Avoid committing generated build output, local databases, traces, or secrets.

Maintainers may ask for changes to keep the project safe, focused, and
maintainable. A contribution may be declined even when technically sound if it
does not fit the product direction or cannot meet the validation requirements.

## License

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in Atlas is licensed under the
[Apache License 2.0](LICENSE), as described in section 5 of that license.
