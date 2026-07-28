# PresentMon runtime

System Atlas pins the official x64 PresentMon 2.5.1 console application for
process-bound ETW frame capture.

- Upstream: https://github.com/GameTechDev/PresentMon
- Release: https://github.com/GameTechDev/PresentMon/releases/tag/v2.5.1
- File: `PresentMon-2.5.1-x64.exe`
- SHA-256: `9BEC3083069F58F911E6A512F4806DB51A27BD096103087BC1D05EF54C80A191`
- Authenticode subject verified on 2026-07-20: `Intel Corporation`
- License: MIT, reproduced in `LICENSE.txt`

Atlas verifies the exact SHA-256 before execution. It invokes the console
collector with an exact process ID, no overlay, no injection, no input tracking,
and a bounded temporary CSV. Raw CSV is removed after per-second and session
summaries are imported.

Frame evidence remains diagnostic until the release validation matrix confirms
anti-cheat compatibility for every pilot game and less than 0.5% incremental
CPU usage on representative hardware.
