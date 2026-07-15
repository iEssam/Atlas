//! Best-effort Windows GPU telemetry from the same PDH counter sets used by
//! Task Manager. Missing counters never fail the main system sampler.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AdapterLuid { pub high: u32, pub low: u32 }

impl AdapterLuid {
    pub fn stable_key(self) -> String { format!("{:08x}:{:08x}", self.high, self.low) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EngineClass {
    ThreeD, Compute, Copy, VideoEncode, VideoDecode,
    #[default] Other,
}

impl EngineClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::ThreeD => "3D", Self::Compute => "Compute", Self::Copy => "Copy",
            Self::VideoEncode => "Video Encode", Self::VideoDecode => "Video Decode",
            Self::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct GpuEngineSample { pub class: EngineClass, pub utilization_permille: u32 }

#[derive(Debug, Clone, Default)]
pub struct GpuAdapterSample {
    pub luid: AdapterLuid,
    pub name: String,
    pub driver_version: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub active_display: bool,
    pub utilization_permille: u32,
    pub dedicated_used: u64,
    pub dedicated_budget: u64,
    pub shared_used: u64,
    pub shared_budget: u64,
    pub engines: Vec<GpuEngineSample>,
    pub temperature_c: Option<f64>,
    pub power_w: Option<f64>,
    pub core_clock_mhz: Option<u32>,
    pub memory_clock_mhz: Option<u32>,
    pub fan_rpm: Option<u32>,
    pub thermal_throttling: Option<bool>,
    pub sensor_source: String,
    pub sensor_unavailable_reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct GpuProcessSample {
    pub pid: u32,
    pub utilization_permille: u32,
    pub dedicated_bytes: u64,
    pub shared_bytes: u64,
}

#[derive(Debug, Clone, Default)]
pub struct GpuSnapshot {
    pub available: bool,
    pub unavailable_reason: String,
    pub system_utilization_permille: u32,
    pub dedicated_used: u64,
    pub dedicated_budget: u64,
    pub shared_used: u64,
    pub shared_budget: u64,
    pub adapters: Vec<GpuAdapterSample>,
    pub processes: Vec<GpuProcessSample>,
}

#[derive(Debug)]
struct ParsedInstance { pid: Option<u32>, luid: AdapterLuid, engine: EngineClass }

fn parse_hex(v: &str) -> Option<u32> { u32::from_str_radix(v.trim_start_matches("0x"), 16).ok() }

fn classify_engine(value: &str) -> EngineClass {
    let v = value.to_ascii_lowercase().replace([' ', '_'], "");
    if v.contains("videoencode") { EngineClass::VideoEncode }
    else if v.contains("videodecode") { EngineClass::VideoDecode }
    else if v.contains("compute") { EngineClass::Compute }
    else if v.contains("copy") { EngineClass::Copy }
    else if v == "3d" || v.contains("graphics") { EngineClass::ThreeD }
    else { EngineClass::Other }
}

fn parse_instance(name: &str) -> Option<ParsedInstance> {
    let tokens: Vec<&str> = name.split('_').collect();
    let after = |key: &str| tokens.windows(2).find_map(|w| w[0].eq_ignore_ascii_case(key).then_some(w[1]));
    let pid = after("pid").and_then(|v| v.parse().ok());
    let li = tokens.iter().position(|v| v.eq_ignore_ascii_case("luid"))?;
    let luid = AdapterLuid { high: parse_hex(tokens.get(li + 1)?)?, low: parse_hex(tokens.get(li + 2)?)? };
    let engine = after("engtype").map(classify_engine).unwrap_or_default();
    Some(ParsedInstance { pid, luid, engine })
}

/// Normalizes raw `(counter instance, value)` rows. Adapter and process usage
/// are their busiest engine rather than a physically meaningless sum.
pub fn normalize(
    engine_rows: &[(String, f64)],
    dedicated_rows: &[(String, f64)],
    shared_rows: &[(String, f64)],
) -> GpuSnapshot {
    let mut adapters: HashMap<AdapterLuid, GpuAdapterSample> = HashMap::new();
    let mut processes: HashMap<u32, GpuProcessSample> = HashMap::new();
    let mut classes: HashMap<(AdapterLuid, EngineClass), u32> = HashMap::new();
    for (name, raw) in engine_rows {
        let Some(i) = parse_instance(name) else { continue };
        let value = if raw.is_finite() { (raw.clamp(0.0, 100.0) * 10.0).round() as u32 } else { 0 };
        classes.entry((i.luid, i.engine)).and_modify(|v| *v = (*v).max(value)).or_insert(value);
        if let Some(pid) = i.pid {
            let p = processes.entry(pid).or_default(); p.pid = pid;
            p.utilization_permille = p.utilization_permille.max(value);
        }
    }
    let mut add_memory = |rows: &[(String, f64)], dedicated: bool| {
        for (name, raw) in rows {
            let Some(i) = parse_instance(name) else { continue };
            let bytes = if raw.is_finite() && *raw > 0.0 { *raw as u64 } else { 0 };
            let a = adapters.entry(i.luid).or_insert_with(|| adapter_shell(i.luid));
            if dedicated { a.dedicated_used = a.dedicated_used.saturating_add(bytes); }
            else { a.shared_used = a.shared_used.saturating_add(bytes); }
            if let Some(pid) = i.pid {
                let p = processes.entry(pid).or_default(); p.pid = pid;
                if dedicated { p.dedicated_bytes = p.dedicated_bytes.saturating_add(bytes); }
                else { p.shared_bytes = p.shared_bytes.saturating_add(bytes); }
            }
        }
    };
    add_memory(dedicated_rows, true); add_memory(shared_rows, false);
    for ((luid, class), utilization_permille) in classes {
        let a = adapters.entry(luid).or_insert_with(|| adapter_shell(luid));
        a.utilization_permille = a.utilization_permille.max(utilization_permille);
        a.engines.push(GpuEngineSample { class, utilization_permille });
    }
    let mut adapters: Vec<_> = adapters.into_values().collect();
    for a in &mut adapters { a.engines.sort_by_key(|e| e.class.label()); }
    adapters.sort_by_key(|a| (a.luid.high, a.luid.low));
    let mut processes: Vec<_> = processes.into_values().collect();
    processes.sort_by(|a, b| b.utilization_permille.cmp(&a.utilization_permille));
    GpuSnapshot {
        available: !adapters.is_empty(),
        unavailable_reason: if adapters.is_empty() { "Windows did not expose GPU performance counters for this session.".into() } else { String::new() },
        system_utilization_permille: adapters.iter().map(|a| a.utilization_permille).max().unwrap_or(0),
        dedicated_used: adapters.iter().map(|a| a.dedicated_used).sum(),
        dedicated_budget: adapters.iter().map(|a| a.dedicated_budget).sum(),
        shared_used: adapters.iter().map(|a| a.shared_used).sum(),
        shared_budget: adapters.iter().map(|a| a.shared_budget).sum(),
        adapters, processes,
    }
}

fn adapter_shell(luid: AdapterLuid) -> GpuAdapterSample {
    GpuAdapterSample {
        luid, name: format!("GPU {}", luid.stable_key()),
        sensor_unavailable_reason: "No supported vendor sensor provider was detected.".into(),
        ..Default::default()
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{ffi::{c_void, OsStr}, os::windows::ffi::OsStrExt, ptr::{null, null_mut}};
    type Status = i32; type Query = *mut c_void; type Counter = *mut c_void;
    const SUCCESS: Status = 0; const MORE_DATA: Status = 0x8000_07D2u32 as i32; const FMT_DOUBLE: u32 = 0x200;
    #[repr(C)] union ValueUnion { double_value: f64, _large: i64 }
    #[repr(C)] struct Value { status: u32, value: ValueUnion }
    #[repr(C)] struct Item { name: *const u16, value: Value }
    #[link(name = "pdh")]
    extern "system" {
        fn PdhOpenQueryW(source: *const u16, user: usize, query: *mut Query) -> Status;
        fn PdhAddEnglishCounterW(query: Query, path: *const u16, user: usize, counter: *mut Counter) -> Status;
        fn PdhCollectQueryData(query: Query) -> Status;
        fn PdhGetFormattedCounterArrayW(counter: Counter, format: u32, size: *mut u32, count: *mut u32, buffer: *mut Item) -> Status;
        fn PdhCloseQuery(query: Query) -> Status;
    }
    #[repr(C)] struct Guid { d1: u32, d2: u16, d3: u16, d4: [u8; 8] }
    const IID_IDXGI_FACTORY1: Guid = Guid { d1: 0x770aae78, d2: 0xf26f, d3: 0x4dba, d4: [0xa8,0x29,0x25,0x3c,0x83,0xd1,0xb3,0x87] };
    #[repr(C)] struct DxgiLuid { low: u32, high: i32 }
    #[repr(C)] struct AdapterDesc1 {
        description: [u16; 128], vendor_id: u32, device_id: u32, subsys_id: u32,
        revision: u32, dedicated_video: usize, dedicated_system: usize, shared_system: usize,
        luid: DxgiLuid, flags: u32,
    }
    #[link(name = "dxgi")]
    extern "system" { fn CreateDXGIFactory1(iid: *const Guid, factory: *mut *mut c_void) -> i32; }
    unsafe fn com_method<T: Copy>(object: *mut c_void, index: usize) -> T {
        let vtable = *(object as *mut *mut *mut c_void);
        std::mem::transmute_copy(&*vtable.add(index))
    }
    unsafe fn release(object: *mut c_void) {
        let f: extern "system" fn(*mut c_void) -> u32 = com_method(object, 2); let _ = f(object);
    }
    unsafe fn enrich(adapters: &mut [GpuAdapterSample]) {
        let mut factory = null_mut();
        if CreateDXGIFactory1(&IID_IDXGI_FACTORY1, &mut factory) < 0 || factory.is_null() { return; }
        let enum_adapter: extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32 = com_method(factory, 12);
        for index in 0..64u32 {
            let mut adapter = null_mut();
            if enum_adapter(factory, index, &mut adapter) < 0 || adapter.is_null() { break; }
            let get_desc: extern "system" fn(*mut c_void, *mut AdapterDesc1) -> i32 = com_method(adapter, 10);
            let mut desc: AdapterDesc1 = std::mem::zeroed();
            if get_desc(adapter, &mut desc) >= 0 {
                let luid = AdapterLuid { high: desc.luid.high as u32, low: desc.luid.low };
                if let Some(a) = adapters.iter_mut().find(|a| a.luid == luid) {
                    let len = desc.description.iter().position(|v| *v == 0).unwrap_or(desc.description.len());
                    a.name = String::from_utf16_lossy(&desc.description[..len]);
                    a.vendor_id = desc.vendor_id; a.device_id = desc.device_id;
                    a.dedicated_budget = desc.dedicated_video as u64;
                    a.shared_budget = desc.shared_system as u64;
                    let enum_outputs: extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32 = com_method(adapter, 7);
                    let mut output = null_mut();
                    if enum_outputs(adapter, 0, &mut output) >= 0 && !output.is_null() { a.active_display = true; release(output); }
                }
            }
            release(adapter);
        }
        release(factory);
    }
    fn wide(v: &str) -> Vec<u16> { OsStr::new(v).encode_wide().chain(Some(0)).collect() }
    unsafe fn read(counter: Counter) -> Vec<(String, f64)> {
        let (mut bytes, mut count) = (0, 0);
        if PdhGetFormattedCounterArrayW(counter, FMT_DOUBLE, &mut bytes, &mut count, null_mut()) != MORE_DATA || bytes == 0 { return vec![]; }
        let mut buffer = vec![0u8; bytes as usize];
        if PdhGetFormattedCounterArrayW(counter, FMT_DOUBLE, &mut bytes, &mut count, buffer.as_mut_ptr().cast()) != SUCCESS { return vec![]; }
        std::slice::from_raw_parts(buffer.as_ptr().cast::<Item>(), count as usize).iter().filter_map(|item| {
            if item.name.is_null() || item.value.status != 0 { return None; }
            let mut n = 0; while *item.name.add(n) != 0 { n += 1; }
            Some((String::from_utf16_lossy(std::slice::from_raw_parts(item.name, n)), item.value.value.double_value))
        }).collect()
    }
    pub struct PlatformGpuCollector { query: Query, engine: Counter, dedicated: Counter, shared: Counter }
    unsafe impl Send for PlatformGpuCollector {}
    impl PlatformGpuCollector {
        pub fn new() -> anyhow::Result<Self> { unsafe {
            let mut query = null_mut();
            if PdhOpenQueryW(null(), 0, &mut query) != SUCCESS { anyhow::bail!("PDH GPU query unavailable"); }
            let mut c = [null_mut(); 3];
            for (index, path) in [r"\GPU Engine(*)\Utilization Percentage", r"\GPU Process Memory(*)\Dedicated Usage", r"\GPU Process Memory(*)\Shared Usage"].iter().enumerate() {
                if PdhAddEnglishCounterW(query, wide(path).as_ptr(), 0, &mut c[index]) != SUCCESS { PdhCloseQuery(query); anyhow::bail!("GPU counters unavailable"); }
            }
            let _ = PdhCollectQueryData(query);
            Ok(Self { query, engine: c[0], dedicated: c[1], shared: c[2] })
        }}
        pub fn sample(&mut self) -> GpuSnapshot { unsafe {
            if PdhCollectQueryData(self.query) != SUCCESS { return GpuSnapshot { unavailable_reason: "GPU counter collection failed.".into(), ..Default::default() }; }
            let mut snapshot = normalize(&read(self.engine), &read(self.dedicated), &read(self.shared));
            enrich(&mut snapshot.adapters);
            snapshot.dedicated_budget = snapshot.adapters.iter().map(|a| a.dedicated_budget).sum();
            snapshot.shared_budget = snapshot.adapters.iter().map(|a| a.shared_budget).sum();
            snapshot
        }}
    }
    impl Drop for PlatformGpuCollector { fn drop(&mut self) { unsafe { let _ = PdhCloseQuery(self.query); } } }
}

pub struct GpuCollector { #[cfg(windows)] inner: Option<platform::PlatformGpuCollector> }
impl GpuCollector {
    pub fn new() -> Self { Self { #[cfg(windows)] inner: platform::PlatformGpuCollector::new().ok() } }
    pub fn sample(&mut self) -> GpuSnapshot {
        #[cfg(windows)] if let Some(inner) = &mut self.inner { return inner.sample(); }
        GpuSnapshot { unavailable_reason: "GPU telemetry is unavailable on this system.".into(), ..Default::default() }
    }
}
impl Default for GpuCollector { fn default() -> Self { Self::new() } }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn busiest_engine_wins() {
        let e = vec![
            ("pid_42_luid_0x00000000_0x000000AA_phys_0_eng_0_engtype_3D".into(), 31.0),
            ("pid_42_luid_0x00000000_0x000000AA_phys_0_eng_1_engtype_Compute_0".into(), 72.0),
        ];
        let d = vec![("pid_42_luid_0x00000000_0x000000AA_phys_0".into(), 1024.0)];
        let got = normalize(&e, &d, &[]);
        assert_eq!(got.system_utilization_permille, 720);
        assert_eq!(got.processes[0].dedicated_bytes, 1024);
    }
    #[test] fn malformed_rows_degrade_honestly() {
        let got = normalize(&[("bad".into(), f64::NAN)], &[], &[]);
        assert!(!got.available); assert!(!got.unavailable_reason.is_empty());
    }
}
