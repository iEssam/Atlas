# Architecture Decision Records

Lightweight ADRs for System Atlas (tech-stack Appendix B). Each records one
load-bearing decision, its context, the options weighed, and the consequences —
so a future maintainer can see *why*, not just *what*.

| ADR | Decision | Status |
|---|---|---|
| [0001](0001-kernel-driver-decision-gate.md) | Kernel-driver decision gate — ship no first-party kernel driver; permanent no-driver default; sandboxed existing driver the only pre-approved path if reopened | Accepted (2026-07-15) |

Many earlier decisions are recorded inline as dated notes in
[../phases.md](../phases.md) ("Decision notes (ADR seeds)") — e.g. the
Rust/C# language split, LocalSystem service, tonic 0.13 pin, the
read-only-MCP-instead-of-hosted-model pivot, the Gorilla TSDB, the
worktree-orchestration rule, and the schema-versioning lesson. Promote any of
those to a full ADR here if it needs deeper rationale.
