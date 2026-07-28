# ADR-0001 — Kernel-driver decision gate

**Status:** Accepted (2026-07-15) · Resolves the open gate in tech-stack §4.9 · Revisit trigger below
**Deciders:** maintainer
**Scope:** Whether Atlas should ship a kernel-mode driver, now or on the roadmap.

---

## Context

Atlas ships **no kernel driver today** (tech-stack §3.1, §4.9). Everything in the collector table — ETW, `NtQuerySystemInformation`, SCM, registry/ConsentStore, Restart Manager, WUA/event-log forensics, `D3DKMT` + vendor GPU libraries, battery IOCTLs, ACPI thermal via WMI, the security-metadata cert-chain/token/mitigation reads — is user-mode. That covers essentially the entire product surface across Phases 1–3.

tech-stack §4.9 deliberately left a **v2 decision gate**: run the checklist with real data if telemetry shows a capability gap. This ADR resolves that gate. The only material capability the current design cannot reach in user mode is **a slice of hardware sensor coverage** — specifically CPU **package temperature** (MSR reads), and motherboard/EC/SuperIO **fan RPM and voltages**. User mode already provides: ACPI thermal-zone temperatures (coarse, often a single zone), OEM ACPI-WMI sensors where the vendor exposes them (Dell/Lenovo/ASUS — spotty), and full GPU temp/power/clocks/fan via NVML/ADLX/IGCL. The gap is real but narrow, and the product already labels absent sensors honestly (PRD §9.6.7, the Sensors page shows "no thermal sensors exposed" rather than faking data).

## Decision drivers

Per the tech-stack §4.9 / §13.6 checklist:

1. **Security risk** — the dominant driver. A kernel driver is the single largest attack-surface addition possible. The sensor-driver category specifically has a documented history of exploitation: WinRing0 and numerous "read-any-MSR / map-any-physical-memory" hardware-monitoring drivers have become **BYOVD (bring-your-own-vulnerable-driver)** primitives — a signed driver that exposes arbitrary MSR or physical-memory access is a local privilege-escalation and detection-evasion tool for *any* attacker on the box, forever, even after Atlas is uninstalled if the driver file persists.
2. **Signing & platform posture** — kernel drivers require Microsoft attestation/EV signing; on HVCI / Memory Integrity systems (increasingly default) the driver must be HVCI-compatible; broad distribution wants WHQL. This is a standing operational burden, not a one-time cost.
3. **Blast radius of a bug** — a user-mode collector bug is an isolated, recoverable process crash (and the architecture already survives it, PRD §13.2). A kernel-driver bug is a **bugcheck (BSOD)** — a whole-machine failure, on a tool whose entire pitch is stability and low overhead.
4. **Compatibility & servicing** — kernel behavior shifts across Windows builds; driver updates are heavier (reboots, separate servicing, a separate signed update channel) and complicate the installer (Store/MSIX cannot cleanly ship kernel drivers).
5. **Value delivered** — narrow: CPU package temp + fan/EC sensors. Valuable to enthusiasts/gamers, but a minority of the product's daily value, and partially covered already.
6. **Product principles** — "safe by default," "low overhead," "least privilege," and the non-goals (not an EDR, not an overclocking utility) all point away from shipping kernel code for a sensor feature.

## Options considered

| Option | Verdict |
|---|---|
| **A. Ship a first-party general sensor driver** (WinRing0-style: arbitrary MSR / physical-memory reads) | **Rejected.** Maximum risk, exactly the BYOVD pattern with a documented CVE history; disproportionate to a narrow sensor gain; contradicts safe-by-default and least-privilege. |
| **B. Ship a first-party *constrained* read-only sensor driver** (specific MSR/EC/SuperIO read paths only, no arbitrary physical-memory mapping, attestation-signed, HVCI-compatible) | **Deferred, not adopted now.** Technically the "right" way to build one, but still a standing signing/servicing/security-review burden and BSOD blast radius for a minority feature. Only revisit if the trigger below fires. |
| **C. Integrate an existing, maintained, *sandboxed*, signed sensor driver** (e.g. the PawnIO model — a signed driver that executes sandboxed bytecode for sensor reads, avoiding the arbitrary-primitive problem) | **Preferred path IF the gate ever opens.** Shifts the driver's security/signing/maintenance to a dedicated project and avoids exposing a raw MSR/physmem primitive. Verify licensing, signing, HVCI posture, and maintenance health at adoption. |
| **D. No first-party driver; keep honest user-mode coverage + honest gaps** | **Accepted (current + default).** Ship the sensor coverage user mode allows, label the rest as unavailable per PRD §9.6.7, and provide extensibility through the already-shipped read-only surfaces (MCP server, signed plugin framework) rather than kernel code. |

## Decision

**Do not ship a first-party kernel driver in the current roadmap. Adopt Option D as the permanent default; hold Option C as the only pre-approved path if the gate later opens.**

- The **no-driver mode is the permanent baseline**, not a temporary limitation. The product must always function fully (minus the specific hardware sensors) without any driver — this is already true.
- The narrow sensor gap is handled by **honest labeling** (PRD §9.6.7), not by kernel code. "No thermal sensors exposed by this hardware" is an acceptable, truthful state.
- Extensibility that might have motivated a driver is already met in user mode by the **read-only MCP server** and the **signed, capability-scoped plugin framework** — neither requires kernel code, and both preserve the security boundary.

## Consequences

- **Positive:** the product's largest attack surface stays closed; no BSOD risk from Atlas; no attestation-signing/HVCI/WHQL burden; the installer and update story stay simple; consistent with least-privilege, safe-by-default, and the non-goals.
- **Negative (accepted):** CPU package temperature and fan/EC/voltage sensors remain unavailable on hardware where only a driver could read them. Enthusiast/gamer users who want those will see them labeled unavailable. This is a deliberate, disclosed trade of a minority feature for the whole product's safety posture.

## Revisit trigger

Reopen this gate **only** if telemetry (opt-in, PRD §15) or user research shows hardware-sensor coverage is a **top-tier** user gap, *and* then evaluate **Option C first**. If a first-party driver is ever built, it is bound by these non-negotiable guardrails (from tech-stack §4.9):

- Read-only sensor paths only (specific MSRs / EC / SuperIO) — **no arbitrary physical-memory mapping** (the WinRing0 lesson).
- Attestation-signed and **HVCI-compatible**.
- Its own signed update channel, isolated from the main service.
- **External security review before shipping**, and explicit per-user opt-in with a plain-language risk disclosure.
- A full no-driver mode retained unconditionally.

---

*This ADR closes the R3 kernel-driver decision gate. Related: tech-stack §4.9 (driver policy), §13.6 (driver requirement checklist), §14 (security), PRD §6 (non-goals: not an overclocking utility), §9.6.7 (honest sensor labeling), §13.2 (process isolation).*
