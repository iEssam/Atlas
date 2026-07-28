## Problem

Describe the user or engineering problem this pull request addresses.

## Change

Explain the approach and the important implementation decisions.

## Validation

List the exact commands and manual checks you ran, with results.

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --locked`
- [ ] Relevant .NET build and tests
- [ ] `.\scripts\validate-docs.ps1`
- [ ] UI smoke or manual interaction checks, when applicable

Mark checks that do not apply and explain why.

## Risk

- Security or privilege-boundary impact:
- Privacy or redaction impact:
- Performance impact:
- Compatibility or migration impact:
- Reversibility and rollback:

## UI evidence

For visible UI changes, include before and after screenshots and describe
keyboard, focus, scaling, High Contrast, clipping, and interaction checks.

## Contributor checklist

- [ ] This pull request is focused and does not include unrelated changes.
- [ ] Tests or documentation cover the changed behavior.
- [ ] No secrets, signing material, private captures, or unredacted personal data
      are included.
- [ ] Every commit includes a DCO `Signed-off-by` trailer.
- [ ] I agree to license this contribution under Apache-2.0.
