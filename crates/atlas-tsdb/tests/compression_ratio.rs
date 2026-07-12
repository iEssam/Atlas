//! Compression-ratio acceptance test: a realistic 300-process, 1 s-tick
//! synthetic hour must average well under 3 bytes/sample across all series.
//!
//! Run with `cargo test -p atlas-tsdb --test compression_ratio -- --nocapture`
//! to see the measured ratio printed.

use atlas_tsdb::{HeadBlocks, Metric, SeriesKey};

/// Tiny deterministic xorshift RNG so the synthetic workload is reproducible
/// without pulling in the `rand` crate (no new deps).
struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// Uniform f64 in [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[test]
fn synthetic_hour_beats_three_bytes_per_sample() {
    const PROCS: i64 = 300;
    const SECONDS: i64 = 3600;
    const SEAL_POINTS: u32 = 120; // matches the service seal cadence.

    let mut rng = XorShift(0x0BAD_F00D_D15E_A5E5);
    let mut hb = HeadBlocks::new();

    // Per-process metric state (random walks with occasional CPU spikes and a
    // slow-drifting working set — the shape real telemetry takes).
    let mut cpu = vec![50.0f64; PROCS as usize];
    let mut ws = vec![80.0e6f64; PROCS as usize];
    let mut priv_b = vec![60.0e6f64; PROCS as usize];
    let mut read = vec![0.0f64; PROCS as usize];
    let mut write = vec![0.0f64; PROCS as usize];

    let base_ts = 1_700_000_000_000i64;

    let mut total_payload_bytes = 0usize;
    let mut total_points = 0u64;

    for s in 0..SECONDS {
        let ts = base_ts + s * 1000;
        for p in 0..PROCS {
            let i = p as usize;

            // CPU: random walk, permille, with rare spikes.
            let step = (rng.unit() - 0.5) * 40.0;
            cpu[i] = (cpu[i] + step).clamp(0.0, 1000.0);
            if rng.unit() < 0.01 {
                cpu[i] = (cpu[i] + 400.0).min(1000.0);
            }
            // Working set: slow drift in ~4 KB pages.
            ws[i] = (ws[i] + (rng.unit() - 0.5) * 8192.0 * 4.0).max(4.0e6);
            priv_b[i] = (priv_b[i] + (rng.unit() - 0.5) * 8192.0 * 3.0).max(2.0e6);
            // I/O: mostly zero, bursty.
            read[i] = if rng.unit() < 0.2 {
                (rng.unit() * 5.0e6).floor()
            } else {
                0.0
            };
            write[i] = if rng.unit() < 0.15 {
                (rng.unit() * 2.0e6).floor()
            } else {
                0.0
            };

            let scope = p + 1;
            assert!(hb.append(
                SeriesKey::new(Metric::CpuPermille, scope),
                ts,
                cpu[i].round()
            ));
            assert!(hb.append(SeriesKey::new(Metric::WorkingSet, scope), ts, ws[i].round()));
            assert!(hb.append(
                SeriesKey::new(Metric::PrivateBytes, scope),
                ts,
                priv_b[i].round()
            ));
            assert!(hb.append(SeriesKey::new(Metric::ReadBps, scope), ts, read[i]));
            assert!(hb.append(SeriesKey::new(Metric::WriteBps, scope), ts, write[i]));
        }

        // Seal on the point cap periodically, exactly as the service does.
        for blk in hb.drain_sealed(SEAL_POINTS, i64::MAX) {
            total_payload_bytes += blk.payload.len();
            total_points += blk.points as u64;
        }
    }

    // Final drain of everything still open.
    for blk in hb.drain_all() {
        total_payload_bytes += blk.payload.len();
        total_points += blk.points as u64;
    }

    let bytes_per_sample = total_payload_bytes as f64 / total_points as f64;
    let raw_bytes = total_points * 16; // 8-byte ts + 8-byte f64 uncompressed.
    let ratio = raw_bytes as f64 / total_payload_bytes as f64;

    println!("--- atlas-tsdb synthetic-hour compression ---");
    println!("processes           {PROCS}");
    println!("series              {}", PROCS * 5);
    println!("samples             {total_points}");
    println!("encoded bytes       {total_payload_bytes}");
    println!("bytes/sample        {bytes_per_sample:.3}");
    println!("vs raw 16 B/sample  {ratio:.1}x smaller");
    println!("---------------------------------------------");

    assert!(
        bytes_per_sample < 3.0,
        "expected well under 3 bytes/sample, got {bytes_per_sample:.3}"
    );
}
