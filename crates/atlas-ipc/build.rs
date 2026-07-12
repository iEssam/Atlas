//! Compiles `proto/atlas.proto` into Rust with tonic/prost at build time
//! (docs/phases.md M4). The generated `atlas.v0` module is included from
//! `src/lib.rs`.
//!
//! protoc is not assumed to be on PATH: we point tonic-build at the hermetic
//! binary shipped by the `protoc-bin-vendored` crate so builds are
//! reproducible on a clean machine with no system protobuf install. If that
//! crate ever fails to provide a binary for this target, install protoc
//! manually (`winget install --id Google.Protobuf`) and drop the PROTOC env
//! override below.

use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Make tonic-build/prost-build use the vendored protoc rather than a
    // system one, so the build is hermetic (see module comment).
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    std::env::set_var("PROTOC", &protoc);

    let proto = Path::new("../../proto/atlas.proto");
    let proto_dir = Path::new("../../proto");

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&[proto], &[proto_dir])?;

    // Recompile if the contract changes.
    println!("cargo:rerun-if-changed=../../proto/atlas.proto");
    Ok(())
}
