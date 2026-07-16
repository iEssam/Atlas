fn main() {
    if let Err(error) = atlas_collectors::gpu_vendor::run_vendor_host_stdio() {
        eprintln!("atlas-gpu-vendor-host: {error:#}");
        std::process::exit(1);
    }
}
