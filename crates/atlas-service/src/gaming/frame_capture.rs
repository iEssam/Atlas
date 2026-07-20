//! Process-bound PresentMon frame capture for Gaming sessions.
//!
//! Atlas runs the pinned Intel binary without overlays, injection, input
//! tracking, or game modification. It targets an exact verified PID, imports a
//! bounded CSV, keeps per-second evidence, and removes the raw file.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const PRESENTMON_FILE: &str = "PresentMon-2.5.1-x64.exe";
const PRESENTMON_SHA256: &str = "9bec3083069f58f911e6a512f4806db51a27bd096103087bc1d05ef54c80a191";
const RAW_CAPTURE_LIMIT_BYTES: u64 = 128 * 1024 * 1024;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub(super) const VALIDATION_LIMITATION: &str = "Measured frame data is diagnostic only in this build. The bundled PresentMon path has not completed Atlas's anti-cheat matrix and under-0.5%-CPU validation gate, so sessions are not yet comparable proof.";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct FrameCaptureSummary {
    pub average_fps: f64,
    pub one_percent_low_fps: f64,
    pub point_one_percent_low_fps: f64,
    pub frame_time_p50_ms: f64,
    pub frame_time_p95_ms: f64,
    pub frame_time_p99_ms: f64,
    pub long_frame_count: u32,
    pub sample_count: u64,
    pub provider: String,
    pub metric: String,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct FrameBucket {
    pub ts_ms: i64,
    pub frame_time_p95_ms: f64,
    pub incident: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct FrameCaptureResult {
    pub summary: Option<FrameCaptureSummary>,
    pub buckets: Vec<FrameBucket>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct FrameCaptureCapability {
    pub runnable: bool,
    pub explanation: String,
    pub limitations: Vec<String>,
}

pub(super) struct PresentMonCapture {
    executable: PathBuf,
    output_path: PathBuf,
    session_name: String,
    started_ms: i64,
    process_id: u32,
    child: Child,
    raw_limit_reached: bool,
}

impl PresentMonCapture {
    pub(super) fn capability() -> FrameCaptureCapability {
        match resolve_presentmon() {
            Ok(path) => FrameCaptureCapability {
                runnable: true,
                explanation: format!(
                    "Official PresentMon 2.5.1 is available at {}. Atlas will attempt to attach measured FPS and frame-time evidence to the next recording; ETW permission is verified when recording starts.",
                    path.display()
                ),
                limitations: vec![VALIDATION_LIMITATION.into()],
            },
            Err(reason) => FrameCaptureCapability {
                runnable: false,
                explanation: reason.clone(),
                limitations: vec![reason],
            },
        }
    }

    pub(super) fn start(process_id: u32, session_id: i64, started_ms: i64) -> Result<Self, String> {
        let executable = resolve_presentmon()?;
        let capture_dir = capture_directory();
        fs::create_dir_all(&capture_dir).map_err(|error| {
            format!("Atlas could not create its bounded frame-capture folder: {error}")
        })?;
        cleanup_stale_captures(&capture_dir);

        let output_path = capture_dir.join(format!(
            "session-{session_id}-{}-{process_id}.csv",
            std::process::id()
        ));
        let _ = fs::remove_file(&output_path);
        let session_name = format!("SystemAtlas-Gaming-{}-{session_id}", std::process::id());
        let mut child = presentmon_command(&executable)
            .args([
                "--process_id",
                &process_id.to_string(),
                "--output_file",
                &output_path.to_string_lossy(),
                "--session_name",
                &session_name,
                "--terminate_on_proc_exit",
                "--exclude_dropped",
                "--no_console_stats",
                "--no_track_input",
                "--qpc_time_ms",
                "--v2_metrics",
            ])
            .spawn()
            .map_err(|error| {
                format!("Atlas could not start the signed PresentMon collector: {error}")
            })?;
        thread::sleep(Duration::from_millis(250));
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("Atlas could not verify the PresentMon process: {error}"))?
        {
            let _ = fs::remove_file(&output_path);
            return Err(format!(
                "PresentMon exited before capture began ({status}). The Atlas service needs administrator or Performance Log Users ETW permission; no FPS values were guessed."
            ));
        }

        Ok(Self {
            executable,
            output_path,
            session_name,
            started_ms,
            process_id,
            child,
            raw_limit_reached: false,
        })
    }

    pub(super) fn enforce_raw_limit(&mut self) -> bool {
        if self.raw_limit_reached {
            return false;
        }
        let size = fs::metadata(&self.output_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if size <= RAW_CAPTURE_LIMIT_BYTES {
            return false;
        }
        self.raw_limit_reached = true;
        self.stop_trace();
        true
    }

    pub(super) fn finish(mut self) -> FrameCaptureResult {
        self.stop_trace();
        wait_for_exit(&mut self.child, Duration::from_secs(4));
        let mut result =
            match parse_presentmon_csv(&self.output_path, self.process_id, self.started_ms) {
                Ok(result) => result,
                Err(reason) => FrameCaptureResult {
                    limitations: vec![reason],
                    ..FrameCaptureResult::default()
                },
            };
        if self.raw_limit_reached {
            result.limitations.push(format!(
                "Raw frame capture reached the {} MB safety limit. Atlas stopped frame collection and retained only the evidence recorded before that point.",
                RAW_CAPTURE_LIMIT_BYTES / 1024 / 1024
            ));
        }
        let _ = fs::remove_file(&self.output_path);
        result
    }

    fn stop_trace(&mut self) {
        if self.child.try_wait().ok().flatten().is_some() {
            return;
        }
        if let Ok(mut terminator) = presentmon_command(&self.executable)
            .args([
                "--session_name",
                &self.session_name,
                "--terminate_existing_session",
            ])
            .spawn()
        {
            wait_for_exit(&mut terminator, Duration::from_secs(2));
        }
    }
}

fn presentmon_command(executable: &Path) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new(executable);
    command
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn wait_for_exit(child: &mut Child, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn resolve_presentmon() -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("ATLAS_PRESENTMON_PATH") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(directory) = current.parent() {
            candidates.push(directory.join(PRESENTMON_FILE));
            candidates.push(directory.join("PresentMon.exe"));
            if let Some(repo_root) = directory.parent().and_then(Path::parent) {
                candidates.push(
                    repo_root
                        .join("third_party")
                        .join("presentmon")
                        .join(PRESENTMON_FILE),
                );
            }
        }
    }
    if let Ok(current_dir) = std::env::current_dir() {
        candidates.push(
            current_dir
                .join("third_party")
                .join("presentmon")
                .join(PRESENTMON_FILE),
        );
    }

    for candidate in candidates {
        if !candidate.is_file() {
            continue;
        }
        let actual = sha256_file(&candidate).map_err(|error| {
            format!(
                "Atlas found PresentMon at {} but could not verify it: {error}",
                candidate.display()
            )
        })?;
        if actual.eq_ignore_ascii_case(PRESENTMON_SHA256) {
            return Ok(candidate);
        }
        return Err(format!(
            "Atlas refused PresentMon at {} because its SHA-256 does not match the pinned, Intel-signed 2.5.1 binary.",
            candidate.display()
        ));
    }
    Err("The pinned PresentMon 2.5.1 collector is not installed beside Atlas, so this recording can collect system evidence but not FPS or frame times.".into())
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn capture_directory() -> PathBuf {
    std::env::temp_dir()
        .join("SystemAtlas")
        .join("gaming-capture")
}

fn cleanup_stale_captures(directory: &Path) {
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(24 * 60 * 60))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .map(|modified| modified < cutoff)
            .unwrap_or(false);
        if stale && path.extension().is_some_and(|extension| extension == "csv") {
            let _ = fs::remove_file(path);
        }
    }
}

#[derive(Debug, Clone)]
struct FrameSample {
    relative_ms: f64,
    frame_time_ms: f64,
}

fn parse_presentmon_csv(
    path: &Path,
    process_id: u32,
    started_ms: i64,
) -> Result<FrameCaptureResult, String> {
    if !path.is_file() {
        return Err("PresentMon did not produce a frame file. ETW permission may be unavailable, or the game closed before a frame was observed.".into());
    }
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_path(path)
        .map_err(|error| format!("Atlas could not open PresentMon frame evidence: {error}"))?;
    let headers = reader
        .headers()
        .map_err(|error| format!("PresentMon returned an unreadable CSV header: {error}"))?
        .iter()
        .map(normalize_header)
        .collect::<Vec<_>>();
    let pid_index = header_index(&headers, &["processid"])
        .ok_or_else(|| "PresentMon CSV did not contain ProcessID.".to_string())?;
    let time_index = header_index(&headers, &["cpustarttime", "cpustartqpctime"])
        .ok_or_else(|| "PresentMon CSV did not contain CPUStartTime.".to_string())?;
    let frame_index = header_index(
        &headers,
        &["msbetweenpresents", "frametime", "msbetweendisplaychange"],
    )
    .ok_or_else(|| {
        "PresentMon CSV did not contain a supported frame interval metric.".to_string()
    })?;
    let swap_chain_index = header_index(&headers, &["swapchainaddress"]);

    let mut by_swap_chain: HashMap<String, Vec<FrameSample>> = HashMap::new();
    let mut malformed_rows = 0u64;
    for row in reader.records() {
        let Ok(row) = row else {
            malformed_rows += 1;
            continue;
        };
        let pid = row
            .get(pid_index)
            .and_then(|value| value.trim().parse::<u32>().ok());
        if pid != Some(process_id) {
            continue;
        }
        let Some(relative_ms) = row
            .get(time_index)
            .and_then(|value| value.trim().parse::<f64>().ok())
        else {
            malformed_rows += 1;
            continue;
        };
        let Some(frame_time_ms) = row
            .get(frame_index)
            .and_then(|value| value.trim().parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value > 0.0 && *value <= 10_000.0)
        else {
            malformed_rows += 1;
            continue;
        };
        let swap_chain = swap_chain_index
            .and_then(|index| row.get(index))
            .unwrap_or("primary")
            .trim()
            .to_string();
        by_swap_chain
            .entry(swap_chain)
            .or_default()
            .push(FrameSample {
                relative_ms,
                frame_time_ms,
            });
    }

    let swap_chain_count = by_swap_chain.len();
    let Some((_, mut samples)) = by_swap_chain
        .into_iter()
        .max_by_key(|(_, samples)| samples.len())
    else {
        return Err(
            "PresentMon produced no usable displayed-frame samples for the verified game process."
                .into(),
        );
    };
    samples.sort_by(|left, right| left.relative_ms.total_cmp(&right.relative_ms));
    if samples.len() < 30 {
        return Err(format!("Only {} usable frames were captured. Record active gameplay for at least a few seconds.", samples.len()));
    }

    let frame_times = samples
        .iter()
        .map(|sample| sample.frame_time_ms)
        .collect::<Vec<_>>();
    let mut limitations = vec![VALIDATION_LIMITATION.into()];
    if swap_chain_count > 1 {
        limitations.push(format!("PresentMon observed {swap_chain_count} swap chains. Atlas used the dominant swap chain so menus or secondary surfaces do not double-count FPS."));
    }
    if malformed_rows > 0 {
        limitations.push(format!(
            "Atlas ignored {malformed_rows} incomplete or unsupported PresentMon row(s)."
        ));
    }
    if samples.len() < 1_000 {
        limitations.push("The recording is too short for a stable 0.1% low; Atlas leaves that value unavailable.".into());
    }
    limitations.push("Missed-budget rate is unavailable until Atlas can verify the game's active frame cap; no budget is guessed from desktop refresh rate.".into());

    let mean_frame_time = frame_times.iter().sum::<f64>() / frame_times.len() as f64;
    let summary = FrameCaptureSummary {
        average_fps: 1000.0 / mean_frame_time,
        one_percent_low_fps: low_fps(&frame_times, 0.01),
        point_one_percent_low_fps: if frame_times.len() >= 1_000 {
            low_fps(&frame_times, 0.001)
        } else {
            0.0
        },
        frame_time_p50_ms: percentile(&frame_times, 0.50),
        frame_time_p95_ms: percentile(&frame_times, 0.95),
        frame_time_p99_ms: percentile(&frame_times, 0.99),
        long_frame_count: frame_times.iter().filter(|value| **value >= 50.0).count() as u32,
        sample_count: frame_times.len() as u64,
        provider: "Intel PresentMon 2.5.1 (ETW, no injection)".into(),
        metric: "Present interval reported by PresentMon on the dominant swap chain".into(),
        limitations: limitations.clone(),
    };

    let time_origin_ms = samples
        .first()
        .map(|sample| sample.relative_ms)
        .unwrap_or(0.0);
    let mut seconds: HashMap<i64, Vec<f64>> = HashMap::new();
    for sample in samples {
        let session_offset_ms = (sample.relative_ms - time_origin_ms).max(0.0).round() as i64;
        let ts_ms = ((started_ms + session_offset_ms) / 1_000) * 1_000;
        seconds.entry(ts_ms).or_default().push(sample.frame_time_ms);
    }
    let mut buckets = seconds
        .into_iter()
        .map(|(ts_ms, values)| {
            let p95 = percentile(&values, 0.95);
            FrameBucket {
                ts_ms,
                frame_time_p95_ms: p95,
                incident: p95 >= 50.0,
            }
        })
        .collect::<Vec<_>>();
    buckets.sort_by_key(|bucket| bucket.ts_ms);

    Ok(FrameCaptureResult {
        summary: Some(summary),
        buckets,
        limitations,
    })
}

fn normalize_header(value: &str) -> String {
    value
        .trim_start_matches('\u{feff}')
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn header_index(headers: &[String], candidates: &[&str]) -> Option<usize> {
    candidates
        .iter()
        .find_map(|candidate| headers.iter().position(|header| header == candidate))
}

fn percentile(values: &[f64], quantile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = ((sorted.len() as f64 * quantile).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[index]
}

fn low_fps(frame_times: &[f64], fraction: f64) -> f64 {
    if frame_times.is_empty() {
        return 0.0;
    }
    let mut sorted = frame_times.to_vec();
    sorted.sort_by(|left, right| right.total_cmp(left));
    let count = ((sorted.len() as f64 * fraction).ceil() as usize)
        .max(1)
        .min(sorted.len());
    let slow_mean = sorted[..count].iter().sum::<f64>() / count as f64;
    1000.0 / slow_mean
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn summary_uses_dominant_swap_chain_and_process_id() {
        let path = std::env::temp_dir().join(format!(
            "atlas-presentmon-test-{}-{}.csv",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let mut file = File::create(&path).expect("fixture file");
        writeln!(
            file,
            "ProcessID,SwapChainAddress,CPUStartTime,MsBetweenPresents"
        )
        .unwrap();
        for index in 0..120 {
            writeln!(file, "44,0xMAIN,{},10", index * 10).unwrap();
            if index < 10 {
                writeln!(file, "44,0xMENU,{},5", index * 5).unwrap();
            }
            writeln!(file, "99,0xOTHER,{},1", index).unwrap();
        }
        drop(file);
        let result = parse_presentmon_csv(&path, 44, 1_000).expect("parsed fixture");
        let summary = result.summary.expect("summary");
        assert!((summary.average_fps - 100.0).abs() < 0.01);
        assert_eq!(summary.sample_count, 120);
        assert_eq!(result.buckets.len(), 2);
        assert!(summary
            .limitations
            .iter()
            .any(|limitation| limitation.contains("2 swap chains")));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn percentile_and_low_fps_are_deterministic() {
        let values = vec![10.0; 99]
            .into_iter()
            .chain(std::iter::once(100.0))
            .collect::<Vec<_>>();
        assert_eq!(percentile(&values, 0.50), 10.0);
        assert_eq!(percentile(&values, 0.99), 10.0);
        assert!((low_fps(&values, 0.01) - 10.0).abs() < 0.001);
    }

    #[test]
    fn header_normalization_accepts_bom_and_spacing() {
        assert_eq!(
            normalize_header("\u{feff}MsBetweenPresents "),
            "msbetweenpresents"
        );
    }
}
