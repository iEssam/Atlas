# Third-party notices

Atlas is licensed under the Apache License 2.0. Third-party components
remain under their respective licenses.

## PresentMon

Atlas distributes the official x64 PresentMon 2.5.1 console application
for process-bound ETW frame capture.

- Project: [GameTechDev/PresentMon](https://github.com/GameTechDev/PresentMon)
- Copyright: Copyright (C) 2017-2024 Intel Corporation
- License: MIT
- Local license copy: [third_party/presentmon/LICENSE.txt](third_party/presentmon/LICENSE.txt)
- Pinned artifact details: [third_party/presentmon/README.md](third_party/presentmon/README.md)

## Package dependencies

Rust and .NET dependencies are resolved through `Cargo.lock` and the committed
NuGet lock files. The supply-chain workflow checks Rust dependency licenses and
release builds publish a CycloneDX software bill of materials.

The generated SBOM and each dependency's own license are authoritative for the
complete dependency inventory of a particular release.
