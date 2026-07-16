//! Isolated NVIDIA NVML provider. The service-side supervisor never loads the
//! vendor DLL; a small helper process owns all native calls and communicates via
//! bounded length-prefixed JSON frames.

use crate::gpu::{
    AvailabilityReason, GpuAdapterSample, SensorAvailability, SensorKind, TelemetrySource,
    TemperatureKind, TemperatureSample, ThrottleReason,
};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread,
    time::{Duration, Instant},
};

const FRAME_LIMIT: usize = 1024 * 1024;
const NVML_VENDOR_ID: u32 = 0x10de;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VendorDeviceRequest {
    pub adapter_key: String,
    pub pci_bus_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VendorRequest {
    pub devices: Vec<VendorDeviceRequest>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum VendorReason {
    None,
    ProviderMissing,
    HelperStartFailure,
    HelperTimeout,
    HelperBackoff,
    StaleSample,
    UnsupportedMetric,
    IdentityUnmatched,
    DeviceLost,
    DriverError,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum VendorTemperatureKind {
    Core,
    Memory,
    Hotspot,
    Board,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VendorTemperature {
    pub kind: VendorTemperatureKind,
    pub celsius: f64,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VendorSample {
    pub adapter_key: String,
    pub temperature_c: Option<f64>,
    pub power_w: Option<f64>,
    pub core_clock_mhz: Option<u32>,
    pub memory_clock_mhz: Option<u32>,
    pub fan_rpm: Option<u32>,
    pub fan_percent: Option<f64>,
    pub temperatures: Vec<VendorTemperature>,
    pub throttle_mask: Option<u64>,
    pub unsupported: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VendorResponse {
    pub healthy: bool,
    pub reason: VendorReason,
    pub detail: String,
    pub samples: Vec<VendorSample>,
}

impl VendorResponse {
    fn unavailable(reason: VendorReason, detail: impl Into<String>) -> Self {
        Self {
            healthy: false,
            reason,
            detail: detail.into(),
            samples: vec![],
        }
    }
}

pub struct VendorSupervisor {
    request_tx: SyncSender<VendorRequest>,
    response_rx: Receiver<VendorResponse>,
    latest: Option<(Instant, VendorResponse)>,
    last_tick: Option<Instant>,
    sample_ttl: Duration,
}

impl VendorSupervisor {
    pub fn new() -> Self {
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (response_tx, response_rx) = mpsc::sync_channel(4);
        thread::Builder::new()
            .name("atlas-nvml-supervisor".into())
            .spawn(move || supervisor_loop(request_rx, response_tx))
            .ok();
        Self {
            request_tx,
            response_rx,
            latest: None,
            last_tick: None,
            sample_ttl: Duration::from_secs(3),
        }
    }

    pub fn merge_nonblocking(&mut self, adapters: &mut [GpuAdapterSample]) {
        let now = Instant::now();
        if let Some(previous) = self.last_tick.replace(now) {
            self.sample_ttl = previous
                .elapsed()
                .mul_f64(3.0)
                .clamp(Duration::from_millis(300), Duration::from_secs(30));
        }
        while let Ok(response) = self.response_rx.try_recv() {
            self.latest = Some((Instant::now(), response));
        }
        let devices = adapters
            .iter()
            .filter(|a| a.vendor_id == NVML_VENDOR_ID)
            .filter_map(|a| {
                a.pci_bus_id().map(|pci_bus_id| VendorDeviceRequest {
                    adapter_key: a.stable_key(),
                    pci_bus_id,
                })
            })
            .collect::<Vec<_>>();
        if !devices.is_empty() {
            match self.request_tx.try_send(VendorRequest { devices }) {
                Ok(()) | Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => {
                    self.latest = Some((
                        Instant::now(),
                        VendorResponse::unavailable(
                            VendorReason::HelperStartFailure,
                            "NVML supervisor is unavailable.",
                        ),
                    ));
                }
            }
        }
        let response = self
            .latest
            .as_ref()
            .and_then(|(at, response)| (at.elapsed() <= self.sample_ttl).then_some(response));
        for adapter in adapters
            .iter_mut()
            .filter(|a| a.vendor_id == NVML_VENDOR_ID)
        {
            match response {
                Some(r) if r.healthy => {
                    if let Some(sample) = r
                        .samples
                        .iter()
                        .find(|v| v.adapter_key == adapter.stable_key())
                    {
                        merge_sample(adapter, sample);
                    } else {
                        mark_vendor_unavailable(
                            adapter,
                            VendorReason::IdentityUnmatched,
                            "NVML returned no device for this PCI identity.",
                        );
                    }
                }
                Some(r) => mark_vendor_unavailable(adapter, r.reason, &r.detail),
                None => mark_vendor_unavailable(
                    adapter,
                    VendorReason::StaleSample,
                    "Waiting for a current NVML helper sample.",
                ),
            }
        }
    }
}

impl Default for VendorSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

fn availability_reason(reason: VendorReason) -> AvailabilityReason {
    match reason {
        VendorReason::None => AvailabilityReason::None,
        VendorReason::ProviderMissing => AvailabilityReason::ProviderMissing,
        VendorReason::HelperStartFailure => AvailabilityReason::HelperStartFailure,
        VendorReason::HelperTimeout => AvailabilityReason::HelperTimeout,
        VendorReason::HelperBackoff => AvailabilityReason::HelperBackoff,
        VendorReason::StaleSample => AvailabilityReason::StaleSample,
        VendorReason::UnsupportedMetric => AvailabilityReason::UnsupportedMetric,
        VendorReason::IdentityUnmatched => AvailabilityReason::IdentityUnmatched,
        VendorReason::DeviceLost => AvailabilityReason::DeviceLost,
        VendorReason::DriverError => AvailabilityReason::DriverError,
    }
}

fn set_effective_availability(
    adapter: &mut GpuAdapterSample,
    kind: SensorKind,
    source: TelemetrySource,
) {
    adapter
        .sensor_availability
        .retain(|v| !(v.kind == kind && v.source == source));
    adapter.sensor_availability.push(SensorAvailability {
        kind,
        available: true,
        source,
        reason: AvailabilityReason::None,
        detail: String::new(),
    });
}

fn mark_vendor_unavailable(adapter: &mut GpuAdapterSample, reason: VendorReason, detail: &str) {
    for kind in [
        SensorKind::CoreTemperature,
        SensorKind::MemoryTemperature,
        SensorKind::HotspotTemperature,
        SensorKind::BoardTemperature,
        SensorKind::PowerWatts,
        SensorKind::CoreClock,
        SensorKind::MemoryClock,
        SensorKind::FanRpm,
        SensorKind::FanPercent,
        SensorKind::ThrottleReasons,
    ] {
        adapter
            .sensor_availability
            .retain(|v| !(v.kind == kind && v.source == TelemetrySource::NvidiaNvml));
        adapter.sensor_availability.push(SensorAvailability {
            kind,
            available: false,
            source: TelemetrySource::NvidiaNvml,
            reason: availability_reason(reason),
            detail: detail.into(),
        });
    }
    if adapter.sensor_source.is_empty() {
        adapter.sensor_source = "Windows WDDM fallback".into();
    }
    adapter.sensor_unavailable_reason = detail.into();
}

fn merge_sample(adapter: &mut GpuAdapterSample, sample: &VendorSample) {
    if let Some(v) = sample.temperature_c {
        adapter.temperature_c = Some(v);
        adapter
            .temperatures
            .retain(|t| t.kind != TemperatureKind::Core);
        adapter.temperatures.push(TemperatureSample {
            kind: TemperatureKind::Core,
            celsius: v,
            source: TelemetrySource::NvidiaNvml,
            label: "GPU core".into(),
        });
        set_effective_availability(
            adapter,
            SensorKind::CoreTemperature,
            TelemetrySource::NvidiaNvml,
        );
    } else {
        mark_one_unsupported(
            adapter,
            SensorKind::CoreTemperature,
            "NVML did not expose core temperature; the current WDDM value remains in use.",
        );
    }
    if let Some(v) = sample.power_w {
        adapter.power_w = Some(v);
        set_effective_availability(adapter, SensorKind::PowerWatts, TelemetrySource::NvidiaNvml);
    } else {
        mark_one_unsupported(
            adapter,
            SensorKind::PowerWatts,
            "NVML did not expose power in watts.",
        );
    }
    if let Some(v) = sample.core_clock_mhz {
        adapter.core_clock_mhz = Some(v);
        set_effective_availability(adapter, SensorKind::CoreClock, TelemetrySource::NvidiaNvml);
    } else {
        mark_one_unsupported(
            adapter,
            SensorKind::CoreClock,
            "NVML did not expose graphics clock; the current WDDM value remains in use.",
        );
    }
    if let Some(v) = sample.memory_clock_mhz {
        adapter.memory_clock_mhz = Some(v);
        set_effective_availability(
            adapter,
            SensorKind::MemoryClock,
            TelemetrySource::NvidiaNvml,
        );
    } else {
        mark_one_unsupported(
            adapter,
            SensorKind::MemoryClock,
            "NVML did not expose memory clock; the current WDDM value remains in use.",
        );
    }
    if let Some(v) = sample.fan_rpm {
        adapter.fan_rpm = Some(v);
        set_effective_availability(adapter, SensorKind::FanRpm, TelemetrySource::NvidiaNvml);
    } else {
        mark_one_unsupported(
            adapter,
            SensorKind::FanRpm,
            "NVML did not expose fan RPM; the current WDDM value remains in use when available.",
        );
    }
    if let Some(v) = sample.fan_percent {
        adapter.fan_percent = Some(v);
        set_effective_availability(adapter, SensorKind::FanPercent, TelemetrySource::NvidiaNvml);
    } else {
        mark_one_unsupported(
            adapter,
            SensorKind::FanPercent,
            "NVML did not expose intended fan percentage.",
        );
    }
    for temp in &sample.temperatures {
        let kind = match temp.kind {
            VendorTemperatureKind::Core => TemperatureKind::Core,
            VendorTemperatureKind::Memory => TemperatureKind::Memory,
            VendorTemperatureKind::Hotspot => TemperatureKind::Hotspot,
            VendorTemperatureKind::Board => TemperatureKind::Board,
            VendorTemperatureKind::Other => TemperatureKind::Other,
        };
        if kind != TemperatureKind::Core {
            adapter.temperatures.retain(|v| v.kind != kind);
            adapter.temperatures.push(TemperatureSample {
                kind,
                celsius: temp.celsius,
                source: TelemetrySource::NvidiaNvml,
                label: temp.label.clone(),
            });
            let sensor = match kind {
                TemperatureKind::Memory => SensorKind::MemoryTemperature,
                TemperatureKind::Hotspot => SensorKind::HotspotTemperature,
                TemperatureKind::Board => SensorKind::BoardTemperature,
                _ => continue,
            };
            set_effective_availability(adapter, sensor, TelemetrySource::NvidiaNvml);
        }
    }
    if let Some(mask) = sample.throttle_mask {
        adapter.throttle_reasons = throttle_reasons(mask);
        adapter.thermal_throttling = Some(adapter.throttle_reasons.iter().any(|v| {
            matches!(
                v,
                ThrottleReason::SoftwareThermal | ThrottleReason::HardwareThermal
            )
        }));
        set_effective_availability(
            adapter,
            SensorKind::ThrottleReasons,
            TelemetrySource::NvidiaNvml,
        );
    } else {
        adapter.throttle_reasons.clear();
        adapter.thermal_throttling = None;
        mark_one_unsupported(
            adapter,
            SensorKind::ThrottleReasons,
            "NVML did not expose current clock-event reasons.",
        );
    }
    adapter.sensor_source = "NVIDIA NVML (Windows WDDM fallback)".into();
    adapter.sensor_unavailable_reason.clear();
}

fn mark_one_unsupported(adapter: &mut GpuAdapterSample, kind: SensorKind, detail: &str) {
    adapter
        .sensor_availability
        .retain(|v| !(v.kind == kind && v.source == TelemetrySource::NvidiaNvml));
    adapter.sensor_availability.push(SensorAvailability {
        kind,
        available: false,
        source: TelemetrySource::NvidiaNvml,
        reason: AvailabilityReason::UnsupportedMetric,
        detail: detail.into(),
    });
}

fn throttle_reasons(mask: u64) -> Vec<ThrottleReason> {
    let mut out = vec![];
    if mask & 0x01 != 0 {
        out.push(ThrottleReason::Idle);
    }
    if mask & 0x02 != 0 {
        out.push(ThrottleReason::ApplicationClocks);
    }
    if mask & 0x20 != 0 {
        out.push(ThrottleReason::SoftwareThermal);
    }
    if mask & 0x40 != 0 {
        out.push(ThrottleReason::HardwareThermal);
    }
    if mask & 0x04 != 0 {
        out.push(ThrottleReason::SoftwarePowerCap);
    }
    if mask & 0x08 != 0 {
        out.push(ThrottleReason::HardwareSlowdown);
    }
    if mask & 0x10 != 0 {
        out.push(ThrottleReason::SyncBoost);
    }
    if mask & 0x80 != 0 {
        out.push(ThrottleReason::HardwarePowerBrake);
    }
    if mask & 0x100 != 0 {
        out.push(ThrottleReason::DisplayClockSetting);
    }
    let known = 0x01 | 0x02 | 0x04 | 0x08 | 0x10 | 0x20 | 0x40 | 0x80 | 0x100;
    if mask & !known != 0 {
        out.push(ThrottleReason::Other);
    }
    out
}

fn helper_path() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("ATLAS_GPU_VENDOR_HOST") {
        return Some(path.into());
    }
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|p| p.join("atlas-gpu-vendor-host.exe"))
}

struct VendorChild {
    child: Child,
    stdin: ChildStdin,
    response_rx: Receiver<Result<VendorResponse, String>>,
    pending: bool,
}

impl VendorChild {
    fn spawn() -> Result<Self, String> {
        let path = helper_path().ok_or("cannot resolve vendor helper path")?;
        let mut child = Command::new(&path)
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("start {}: {e}", path.display()))?;
        let stdin = child.stdin.take().ok_or("helper stdin unavailable")?;
        let mut stdout = child.stdout.take().ok_or("helper stdout unavailable")?;
        let (tx, response_rx) = mpsc::channel();
        thread::Builder::new()
            .name("atlas-nvml-reader".into())
            .spawn(move || loop {
                let response = read_frame::<_, VendorResponse>(&mut stdout);
                let ended = response.is_err();
                if tx.send(response).is_err() || ended {
                    break;
                }
            })
            .map_err(|e| format!("start helper reader: {e}"))?;
        Ok(Self {
            child,
            stdin,
            response_rx,
            pending: false,
        })
    }
    fn request(&mut self, request: &VendorRequest) -> Result<VendorResponse, VendorReason> {
        if !self.pending {
            write_frame(&mut self.stdin, request).map_err(|_| VendorReason::DeviceLost)?;
            self.pending = true;
        }
        match self.response_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Ok(response)) => {
                self.pending = false;
                Ok(response)
            }
            Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.pending = false;
                Err(VendorReason::DeviceLost)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Err(VendorReason::HelperTimeout),
        }
    }
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn supervisor_loop(request_rx: Receiver<VendorRequest>, response_tx: SyncSender<VendorResponse>) {
    let mut child: Option<VendorChild> = None;
    let mut consecutive_timeouts = 0usize;
    let mut restart_attempts = 0usize;
    let mut backoff_until: Option<Instant> = None;
    while let Ok(request) = request_rx.recv() {
        if let Some(until) = backoff_until {
            if Instant::now() < until {
                let _ = response_tx.try_send(VendorResponse::unavailable(
                    VendorReason::HelperBackoff,
                    "NVML helper restart is in backoff.",
                ));
                continue;
            }
            backoff_until = None;
        }
        if child.is_none() {
            match VendorChild::spawn() {
                Ok(v) => child = Some(v),
                Err(e) => {
                    restart_attempts += 1;
                    let _ = response_tx.try_send(VendorResponse::unavailable(
                        if e.contains("not found") || e.contains("cannot find") {
                            VendorReason::ProviderMissing
                        } else {
                            VendorReason::HelperStartFailure
                        },
                        e,
                    ));
                    backoff_until = Some(Instant::now() + backoff_for(restart_attempts));
                    continue;
                }
            }
        }
        match child.as_mut().expect("created above").request(&request) {
            Ok(response) => {
                consecutive_timeouts = 0;
                let device_lost = response.reason == VendorReason::DeviceLost;
                let _ = response_tx.try_send(response);
                if device_lost {
                    restart_attempts += 1;
                    if let Some(mut old) = child.take() {
                        old.stop();
                    }
                    backoff_until = Some(Instant::now() + backoff_for(restart_attempts));
                } else {
                    restart_attempts = 0;
                }
            }
            Err(reason) => {
                if reason == VendorReason::HelperTimeout {
                    consecutive_timeouts += 1;
                } else {
                    consecutive_timeouts = 0;
                }
                let _ = response_tx.try_send(VendorResponse::unavailable(
                    reason,
                    "NVML helper did not return a usable sample.",
                ));
                if consecutive_timeouts >= 3 || reason == VendorReason::DeviceLost {
                    if let Some(mut old) = child.take() {
                        old.stop();
                    }
                    consecutive_timeouts = 0;
                    restart_attempts += 1;
                    backoff_until = Some(Instant::now() + backoff_for(restart_attempts));
                }
            }
        }
    }
    if let Some(mut child) = child {
        child.stop();
    }
}

fn backoff_for(failures: usize) -> Duration {
    Duration::from_secs(match failures {
        0 | 1 => 1,
        2 => 5,
        _ => 30,
    })
}

fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > FRAME_LIMIT {
        return Err("vendor frame too large".into());
    }
    writer
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .and_then(|_| writer.write_all(&bytes))
        .and_then(|_| writer.flush())
        .map_err(|e| e.to_string())
}

fn read_frame<R: Read, T: for<'de> Deserialize<'de>>(reader: &mut R) -> Result<T, String> {
    let mut len = [0u8; 4];
    reader.read_exact(&mut len).map_err(|e| e.to_string())?;
    let len = u32::from_le_bytes(len) as usize;
    if len > FRAME_LIMIT {
        return Err("vendor frame too large".into());
    }
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

/// Entry point used only by the isolated helper executable.
pub fn run_vendor_host_stdio() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let mut provider = nvml::NvmlProvider::load();
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let mut input = stdin.lock();
        let mut output = stdout.lock();
        loop {
            let request: VendorRequest = match read_frame(&mut input) {
                Ok(v) => v,
                Err(e) if e.contains("failed to fill whole buffer") => return Ok(()),
                Err(e) => anyhow::bail!(e),
            };
            let response = match provider.as_mut() {
                Ok(p) => p.sample(&request),
                Err(detail) => VendorResponse::unavailable(
                    if detail.contains("not found") || detail.contains("symbol") {
                        VendorReason::ProviderMissing
                    } else {
                        VendorReason::DriverError
                    },
                    detail.clone(),
                ),
            };
            write_frame(&mut output, &response).map_err(anyhow::Error::msg)?;
        }
    }
    #[cfg(not(windows))]
    anyhow::bail!("NVML helper is Windows-only")
}

#[cfg(windows)]
mod nvml {
    use super::*;
    use std::{
        cell::Cell,
        ffi::{c_char, c_void, CString},
        mem::{size_of, transmute_copy},
        os::windows::ffi::OsStrExt,
        path::Path,
        ptr::null_mut,
    };

    type Module = *mut c_void;
    type Device = *mut c_void;
    type Return = i32;
    const OK: Return = 0;
    const ERROR_NOT_SUPPORTED: Return = 3;
    const ERROR_GPU_IS_LOST: Return = 15;
    const ERROR_UNKNOWN: Return = 999;
    const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x8;
    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x800;
    const TEMP_GPU: u32 = 0;
    const CLOCK_GRAPHICS: u32 = 0;
    const CLOCK_MEMORY: u32 = 2;
    const MAX_THERMAL_SENSORS: usize = 3;

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryExW(path: *const u16, file: *mut c_void, flags: u32) -> Module;
        fn GetProcAddress(module: Module, name: *const u8) -> *mut c_void;
        fn FreeLibrary(module: Module) -> i32;
    }

    type InitFn = unsafe extern "C" fn() -> Return;
    type ShutdownFn = unsafe extern "C" fn() -> Return;
    type HandleByPciFn = unsafe extern "C" fn(*const c_char, *mut Device) -> Return;
    type TemperatureFn = unsafe extern "C" fn(Device, u32, *mut u32) -> Return;
    type TemperatureVFn = unsafe extern "C" fn(Device, *mut NvmlTemperature) -> Return;
    type ClockFn = unsafe extern "C" fn(Device, u32, *mut u32) -> Return;
    type PowerFn = unsafe extern "C" fn(Device, *mut u32) -> Return;
    type FanFn = unsafe extern "C" fn(Device, *mut u32) -> Return;
    type FanV2Fn = unsafe extern "C" fn(Device, u32, *mut u32) -> Return;
    type NumFansFn = unsafe extern "C" fn(Device, *mut u32) -> Return;
    type FanRpmFn = unsafe extern "C" fn(Device, *mut NvmlFanSpeedInfo) -> Return;
    type ReasonsFn = unsafe extern "C" fn(Device, *mut u64) -> Return;
    type ThermalFn = unsafe extern "C" fn(Device, u32, *mut NvmlThermalSettings) -> Return;

    #[repr(C)]
    #[derive(Default)]
    struct NvmlTemperature {
        version: u32,
        sensor_type: u32,
        temperature: i32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct NvmlFanSpeedInfo {
        version: u32,
        fan: u32,
        speed: u32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct NvmlThermalSensor {
        controller: i32,
        default_min_temp: u32,
        default_max_temp: u32,
        current_temp: u32,
        target: i32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct NvmlThermalSettings {
        count: u32,
        sensor: [NvmlThermalSensor; MAX_THERMAL_SENSORS],
    }

    pub struct NvmlProvider {
        module: Module,
        shutdown: ShutdownFn,
        handle_by_pci: HandleByPciFn,
        temperature: Option<TemperatureFn>,
        temperature_v: Option<TemperatureVFn>,
        clock: Option<ClockFn>,
        power: Option<PowerFn>,
        fan: Option<FanFn>,
        fan_v2: Option<FanV2Fn>,
        num_fans: Option<NumFansFn>,
        fan_rpm: Option<FanRpmFn>,
        reasons: Option<ReasonsFn>,
        thermal: Option<ThermalFn>,
        last_error: Cell<Option<VendorReason>>,
    }

    unsafe impl Send for NvmlProvider {}

    impl NvmlProvider {
        pub fn load() -> Result<Self, String> {
            unsafe {
                let module =
                    load_nvml().ok_or("nvml.dll was not found in System32 or NVIDIA NVSMI.")?;
                let Some(init): Option<InitFn> =
                    optional(module, b"nvmlInit_v2\0").or_else(|| optional(module, b"nvmlInit\0"))
                else {
                    FreeLibrary(module);
                    return Err("required NVML initialization symbol is missing".into());
                };
                let shutdown: ShutdownFn = match required(module, b"nvmlShutdown\0") {
                    Ok(value) => value,
                    Err(error) => {
                        FreeLibrary(module);
                        return Err(error);
                    }
                };
                let Some(handle_by_pci): Option<HandleByPciFn> =
                    optional(module, b"nvmlDeviceGetHandleByPciBusId_v2\0")
                        .or_else(|| optional(module, b"nvmlDeviceGetHandleByPciBusId\0"))
                else {
                    FreeLibrary(module);
                    return Err("required NVML PCI lookup symbol is missing".into());
                };
                if init() != OK {
                    FreeLibrary(module);
                    return Err(
                        "NVML initialization failed; the NVIDIA driver may be unavailable.".into(),
                    );
                }
                Ok(Self {
                    module,
                    shutdown,
                    handle_by_pci,
                    temperature: optional(module, b"nvmlDeviceGetTemperature\0"),
                    temperature_v: optional(module, b"nvmlDeviceGetTemperatureV\0"),
                    clock: optional(module, b"nvmlDeviceGetClockInfo\0"),
                    power: optional(module, b"nvmlDeviceGetPowerUsage\0"),
                    fan: optional(module, b"nvmlDeviceGetFanSpeed\0"),
                    fan_v2: optional(module, b"nvmlDeviceGetFanSpeed_v2\0"),
                    num_fans: optional(module, b"nvmlDeviceGetNumFans\0"),
                    fan_rpm: optional(module, b"nvmlDeviceGetFanSpeedRPM\0"),
                    reasons: optional(module, b"nvmlDeviceGetCurrentClocksEventReasons\0").or_else(
                        || optional(module, b"nvmlDeviceGetCurrentClocksThrottleReasons\0"),
                    ),
                    thermal: optional(module, b"nvmlDeviceGetThermalSettings\0"),
                    last_error: Cell::new(None),
                })
            }
        }

        pub fn sample(&mut self, request: &VendorRequest) -> VendorResponse {
            let mut samples = vec![];
            for target in &request.devices {
                let pci = match CString::new(target.pci_bus_id.as_str()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let mut device = null_mut();
                let status = unsafe { (self.handle_by_pci)(pci.as_ptr(), &mut device) };
                if status != OK || device.is_null() {
                    if status == ERROR_GPU_IS_LOST {
                        return VendorResponse::unavailable(
                            VendorReason::DeviceLost,
                            "NVML reports that the GPU device was lost.",
                        );
                    }
                    continue;
                }
                self.last_error.set(None);
                let mut sample = VendorSample {
                    adapter_key: target.adapter_key.clone(),
                    ..Default::default()
                };
                sample.temperature_c = self.read_temperature(device);
                sample.core_clock_mhz = self.read_clock(device, CLOCK_GRAPHICS);
                sample.memory_clock_mhz = self.read_clock(device, CLOCK_MEMORY);
                sample.power_w = self
                    .read_simple(self.power, device)
                    .map(|v| v as f64 / 1000.0);
                sample.fan_percent = self.read_fan_percent(device).map(f64::from);
                sample.fan_rpm = self.read_fan_rpm(device);
                sample.throttle_mask = self.read_reasons(device);
                sample.temperatures = self.read_thermal(device);
                if let Some(reason) = self.last_error.get() {
                    return VendorResponse::unavailable(
                        reason,
                        "NVML returned a driver/device error while sampling hardware telemetry.",
                    );
                }
                if sample.temperature_c.is_none() {
                    sample.unsupported.push("core_temperature".into());
                }
                if sample.power_w.is_none() {
                    sample.unsupported.push("power_watts".into());
                }
                if sample.fan_rpm.is_none() {
                    sample.unsupported.push("fan_rpm".into());
                }
                samples.push(sample);
            }
            if samples.is_empty() && !request.devices.is_empty() {
                VendorResponse::unavailable(
                    VendorReason::IdentityUnmatched,
                    "NVML could not match any requested PCI device.",
                )
            } else {
                VendorResponse {
                    healthy: true,
                    reason: VendorReason::None,
                    detail: String::new(),
                    samples,
                }
            }
        }

        fn read_temperature(&self, device: Device) -> Option<f64> {
            unsafe {
                if let Some(f) = self.temperature_v {
                    let mut data = NvmlTemperature {
                        version: version::<NvmlTemperature>(1),
                        sensor_type: TEMP_GPU,
                        temperature: 0,
                    };
                    let status = f(device, &mut data);
                    self.record_fatal(status);
                    if status == OK && (1..=200).contains(&data.temperature) {
                        return Some(data.temperature as f64);
                    }
                }
                let f = self.temperature?;
                let mut value = 0;
                let status = f(device, TEMP_GPU, &mut value);
                self.record_fatal(status);
                (status == OK && (1..=200).contains(&value)).then_some(value as f64)
            }
        }

        fn read_clock(&self, device: Device, selector: u32) -> Option<u32> {
            unsafe {
                let f = self.clock?;
                let mut value = 0;
                let status = f(device, selector, &mut value);
                self.record_fatal(status);
                (status == OK).then_some(value)
            }
        }

        fn read_simple(
            &self,
            f: Option<unsafe extern "C" fn(Device, *mut u32) -> Return>,
            device: Device,
        ) -> Option<u32> {
            unsafe {
                let f = f?;
                let mut value = 0;
                let status = f(device, &mut value);
                self.record_fatal(status);
                (status == OK).then_some(value)
            }
        }

        fn read_fan_rpm(&self, device: Device) -> Option<u32> {
            unsafe {
                let f = self.fan_rpm?;
                let mut best = None;
                for fan in 0..8u32 {
                    let mut data = NvmlFanSpeedInfo {
                        version: version::<NvmlFanSpeedInfo>(1),
                        fan,
                        speed: 0,
                    };
                    let status = f(device, &mut data);
                    self.record_fatal(status);
                    if status == OK {
                        best = Some(best.unwrap_or(0).max(data.speed));
                    } else if status == ERROR_NOT_SUPPORTED || fan > 0 {
                        break;
                    }
                }
                best
            }
        }

        fn read_fan_percent(&self, device: Device) -> Option<u32> {
            unsafe {
                if let (Some(count_fn), Some(fan_fn)) = (self.num_fans, self.fan_v2) {
                    let mut count = 0;
                    let count_status = count_fn(device, &mut count);
                    self.record_fatal(count_status);
                    if count_status == OK && count > 0 {
                        let mut best = None;
                        for fan in 0..count.min(32) {
                            let mut value = 0;
                            let status = fan_fn(device, fan, &mut value);
                            self.record_fatal(status);
                            if status == OK {
                                best = Some(best.unwrap_or(0).max(value));
                            }
                        }
                        if best.is_some() {
                            return best;
                        }
                    }
                }
                self.read_simple(self.fan, device)
            }
        }

        fn read_reasons(&self, device: Device) -> Option<u64> {
            unsafe {
                let f = self.reasons?;
                let mut value = 0;
                let status = f(device, &mut value);
                self.record_fatal(status);
                (status == OK).then_some(value)
            }
        }

        fn read_thermal(&self, device: Device) -> Vec<VendorTemperature> {
            unsafe {
                let Some(f) = self.thermal else { return vec![] };
                let mut out = vec![];
                for index in 0..MAX_THERMAL_SENSORS as u32 {
                    let mut settings = NvmlThermalSettings::default();
                    let status = f(device, index, &mut settings);
                    self.record_fatal(status);
                    if status != OK {
                        if status == ERROR_NOT_SUPPORTED || index > 0 {
                            break;
                        } else {
                            continue;
                        }
                    }
                    for sensor in settings
                        .sensor
                        .iter()
                        .take(settings.count.min(MAX_THERMAL_SENSORS as u32) as usize)
                    {
                        if !(1..=200).contains(&sensor.current_temp) {
                            continue;
                        }
                        let (kind, label) = match sensor.target {
                            1 => (VendorTemperatureKind::Core, "GPU core"),
                            2 => (VendorTemperatureKind::Memory, "GPU memory"),
                            8 => (VendorTemperatureKind::Board, "GPU board"),
                            _ => (VendorTemperatureKind::Other, "GPU sensor"),
                        };
                        if !out.iter().any(|v: &VendorTemperature| {
                            v.kind == kind
                                && (v.celsius - sensor.current_temp as f64).abs() < f64::EPSILON
                        }) {
                            out.push(VendorTemperature {
                                kind,
                                celsius: sensor.current_temp as f64,
                                label: label.into(),
                            });
                        }
                    }
                }
                out
            }
        }

        fn record_fatal(&self, status: Return) {
            if status == ERROR_GPU_IS_LOST {
                self.last_error.set(Some(VendorReason::DeviceLost));
            } else if status == ERROR_UNKNOWN {
                self.last_error.set(Some(VendorReason::DriverError));
            }
        }
    }

    impl Drop for NvmlProvider {
        fn drop(&mut self) {
            unsafe {
                let _ = (self.shutdown)();
                let _ = FreeLibrary(self.module);
            }
        }
    }

    fn version<T>(v: u32) -> u32 {
        (v << 24) | size_of::<T>() as u32
    }
    unsafe fn required<T: Copy>(module: Module, name: &[u8]) -> Result<T, String> {
        optional(module, name).ok_or_else(|| {
            format!(
                "required NVML symbol {} is missing",
                String::from_utf8_lossy(&name[..name.len() - 1])
            )
        })
    }
    unsafe fn optional<T: Copy>(module: Module, name: &[u8]) -> Option<T> {
        let ptr = GetProcAddress(module, name.as_ptr());
        (!ptr.is_null()).then(|| transmute_copy(&ptr))
    }
    unsafe fn load_nvml() -> Option<Module> {
        let system = wide(Path::new("nvml.dll"));
        let module = LoadLibraryExW(system.as_ptr(), null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
        if !module.is_null() {
            return Some(module);
        }
        let root = std::env::var_os("ProgramW6432")?;
        let explicit = Path::new(&root)
            .join("NVIDIA Corporation")
            .join("NVSMI")
            .join("nvml.dll");
        let path = wide(&explicit);
        let module = LoadLibraryExW(path.as_ptr(), null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH);
        (!module.is_null()).then_some(module)
    }
    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    #[allow(dead_code)]
    fn classify_error(status: Return) -> VendorReason {
        if status == ERROR_NOT_SUPPORTED {
            VendorReason::UnsupportedMetric
        } else if status == ERROR_GPU_IS_LOST {
            VendorReason::DeviceLost
        } else {
            VendorReason::DriverError
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn adapter() -> GpuAdapterSample {
        GpuAdapterSample {
            vendor_id: NVML_VENDOR_ID,
            pci_bus: 1,
            name: "NVIDIA".into(),
            ..Default::default()
        }
    }
    #[test]
    fn nvml_overrides_wddm_field_by_field() {
        let mut a = adapter();
        a.temperature_c = Some(55.0);
        a.fan_rpm = Some(900);
        a.memory_clock_mhz = Some(7000);
        let adapter_key = a.stable_key();
        merge_sample(
            &mut a,
            &VendorSample {
                adapter_key,
                temperature_c: Some(59.0),
                fan_rpm: Some(1200),
                memory_clock_mhz: Some(7100),
                power_w: Some(130.5),
                ..Default::default()
            },
        );
        assert_eq!(a.temperature_c, Some(59.0));
        assert_eq!(a.fan_rpm, Some(1200));
        assert_eq!(a.power_w, Some(130.5));
        assert!(a
            .sensor_availability
            .iter()
            .any(|v| v.kind == SensorKind::CoreTemperature
                && v.source == TelemetrySource::NvidiaNvml));
    }
    #[test]
    fn unsupported_vendor_fields_keep_wddm_values() {
        let mut a = adapter();
        a.temperature_c = Some(55.0);
        a.fan_rpm = Some(900);
        a.sensor_availability.push(SensorAvailability {
            kind: SensorKind::CoreTemperature,
            available: true,
            source: TelemetrySource::WindowsWddm,
            reason: AvailabilityReason::None,
            detail: String::new(),
        });
        let adapter_key = a.stable_key();
        merge_sample(
            &mut a,
            &VendorSample {
                adapter_key,
                ..Default::default()
            },
        );
        assert_eq!(a.temperature_c, Some(55.0));
        assert_eq!(a.fan_rpm, Some(900));
        assert_eq!(a.power_w, None);
        assert!(a
            .sensor_availability
            .iter()
            .any(|v| v.kind == SensorKind::CoreTemperature
                && v.available
                && v.source == TelemetrySource::WindowsWddm));
        assert!(a
            .sensor_availability
            .iter()
            .any(|v| v.kind == SensorKind::CoreTemperature
                && !v.available
                && v.source == TelemetrySource::NvidiaNvml
                && v.reason == AvailabilityReason::UnsupportedMetric));
        assert_eq!(
            a.thermal_throttling, None,
            "missing reason API must not become a false no-throttle reading"
        );
    }
    #[test]
    fn thermal_bits_are_explicit() {
        assert_eq!(
            throttle_reasons(0x20 | 0x40),
            vec![
                ThrottleReason::SoftwareThermal,
                ThrottleReason::HardwareThermal
            ]
        );
        assert!(throttle_reasons(0x04).iter().all(|v| !matches!(
            v,
            ThrottleReason::SoftwareThermal | ThrottleReason::HardwareThermal
        )));
        assert_eq!(throttle_reasons(0x01), vec![ThrottleReason::Idle]);
    }
    #[test]
    fn protocol_round_trip_is_bounded() {
        let request = VendorRequest {
            devices: vec![VendorDeviceRequest {
                adapter_key: "a".into(),
                pci_bus_id: "00000000:01:00.0".into(),
            }],
        };
        let mut bytes = vec![];
        write_frame(&mut bytes, &request).unwrap();
        let decoded: VendorRequest = read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(decoded.devices[0].pci_bus_id, "00000000:01:00.0");
    }

    #[test]
    fn restart_backoff_sequence_is_bounded() {
        assert_eq!(backoff_for(1), Duration::from_secs(1));
        assert_eq!(backoff_for(2), Duration::from_secs(5));
        assert_eq!(backoff_for(3), Duration::from_secs(30));
        assert_eq!(backoff_for(30), Duration::from_secs(30));
    }

    #[test]
    fn watts_and_power_percentage_never_alias() {
        let mut a = adapter();
        a.power_percent = Some(73.0);
        let adapter_key = a.stable_key();
        merge_sample(
            &mut a,
            &VendorSample {
                adapter_key,
                power_w: None,
                ..Default::default()
            },
        );
        assert_eq!(a.power_w, None);
        assert_eq!(a.power_percent, Some(73.0));
    }
}
