# Governance

Atlas is currently maintained by [@iEssam](https://github.com/iEssam).
The project is in an early release-candidate stage, so governance is intentionally
lightweight and can evolve as the contributor community grows.

## Decision making

Routine fixes and documentation changes are decided through pull request review.
Changes that affect architecture, security boundaries, data collection,
privileged actions, compatibility, or product direction should begin with a
public issue or an Architecture Decision Record.

Decisions prioritize:

1. User safety, privacy, and reversibility
2. Evidence quality and truthful handling of unavailable data
3. Reliability and compatibility on supported Windows systems
4. Performance and operational cost
5. Maintainability and contributor experience

The maintainer has final responsibility for merge and release decisions while
the project has a single maintainer. Reasons should be documented when a
reasonable proposal is declined or materially changed.

## Maintainer responsibilities

Maintainers are expected to:

- Apply the Code of Conduct consistently
- Review contributions fairly and in a reasonable time when capacity allows
- Protect security reports and coordinate responsible disclosure
- Require appropriate tests and documentation
- Keep release status and known limitations accurate
- Avoid merging their own high-risk changes without recorded validation

## Becoming a maintainer

Regular contributors may be invited to become maintainers after demonstrating:

- Sustained, constructive participation
- Sound judgment around security, privacy, and compatibility
- Consistent review quality
- Familiarity with the architecture and validation requirements
- Respectful community conduct

Maintainer access is granted gradually and may begin with triage or review
responsibilities before merge or release permissions.

## Conflicts of interest

Contributors and maintainers should disclose financial, employment, or other
interests that could materially affect a technical or release decision. A
conflicted maintainer should seek another reviewer when one is available.

## Changes to governance

Governance changes are proposed through a pull request and should explain the
problem being solved. Material changes require a public discussion period before
merging.
