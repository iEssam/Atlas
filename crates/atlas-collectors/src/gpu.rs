//! Windows GPU telemetry. PDH supplies scheduler utilization and per-process
//! memory; D3DKMT supplies adapter performance/capabilities; optional vendor
//! data is merged field-by-field without weakening the Windows fallback.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AdapterLuid {
    pub high: u32,
    pub low: u32,
}

impl AdapterLuid {
    pub fn stable_key(self) -> String {
        format!("{:08x}:{:08x}", self.high, self.low)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AdapterId {
    pub luid: AdapterLuid,
    pub physical_index: u32,
}

impl AdapterId {
    pub fn stable_key(self) -> String {
        let base = self.luid.stable_key();
        if self.physical_index == 0 {
            base
        } else {
            format!("{base}:p{}", self.physical_index)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EngineClass {
    ThreeD,
    Compute,
    Copy,
    VideoEncode,
    VideoDecode,
    #[default]
    Other,
}

impl EngineClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::ThreeD => "3D",
            Self::Compute => "Compute",
            Self::Copy => "Copy",
            Self::VideoEncode => "Video Encode",
            Self::VideoDecode => "Video Decode",
            Self::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetrySource {
    WindowsWddm,
    NvidiaNvml,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SensorKind {
    CoreTemperature,
    MemoryTemperature,
    HotspotTemperature,
    BoardTemperature,
    PowerWatts,
    PowerPercent,
    CoreClock,
    MemoryClock,
    FanRpm,
    FanPercent,
    ThrottleReasons,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvailabilityReason {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemperatureKind {
    Core,
    Memory,
    Hotspot,
    Board,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleReason {
    SoftwareThermal,
    HardwareThermal,
    SoftwarePowerCap,
    HardwareSlowdown,
    HardwarePowerBrake,
    Idle,
    ApplicationClocks,
    SyncBoost,
    DisplayClockSetting,
    Other,
}

#[derive(Debug, Clone)]
pub struct TemperatureSample {
    pub kind: TemperatureKind,
    pub celsius: f64,
    pub source: TelemetrySource,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct SensorAvailability {
    pub kind: SensorKind,
    pub available: bool,
    pub source: TelemetrySource,
    pub reason: AvailabilityReason,
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
pub struct GpuEngineSample {
    pub class: EngineClass,
    pub utilization_permille: u32,
}

#[derive(Debug, Clone, Default)]
pub struct GpuEngineNodeSample {
    pub ordinal: u32,
    pub class: EngineClass,
    pub utilization_permille: u32,
}

#[derive(Debug, Clone, Default)]
pub struct GpuAdapterSample {
    pub luid: AdapterLuid,
    pub physical_index: u32,
    pub name: String,
    pub driver_version: String,
    pub driver_date: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub pci_domain: u32,
    pub pci_bus: u32,
    pub pci_device: u32,
    pub pci_function: u32,
    pub pci_identity_available: bool,
    pub active_display: bool,
    pub utilization_permille: u32,
    pub dedicated_used: u64,
    pub dedicated_budget: u64,
    pub shared_used: u64,
    pub shared_budget: u64,
    pub engines: Vec<GpuEngineSample>,
    pub engine_nodes: Vec<GpuEngineNodeSample>,
    pub temperature_c: Option<f64>,
    pub power_w: Option<f64>,
    pub power_percent: Option<f64>,
    pub core_clock_mhz: Option<u32>,
    pub memory_clock_mhz: Option<u32>,
    pub fan_rpm: Option<u32>,
    pub fan_percent: Option<f64>,
    pub temperature_warning_c: Option<f64>,
    pub temperature_max_c: Option<f64>,
    pub temperatures: Vec<TemperatureSample>,
    pub throttle_reasons: Vec<ThrottleReason>,
    pub thermal_throttling: Option<bool>,
    pub sensor_availability: Vec<SensorAvailability>,
    pub sensor_source: String,
    pub sensor_unavailable_reason: String,
}

impl GpuAdapterSample {
    pub fn id(&self) -> AdapterId {
        AdapterId {
            luid: self.luid,
            physical_index: self.physical_index,
        }
    }
    pub fn stable_key(&self) -> String {
        self.id().stable_key()
    }
    pub fn pci_bus_id(&self) -> Option<String> {
        (self.vendor_id == 0x10de && self.pci_identity_available).then(|| {
            format!(
                "{:08x}:{:02x}:{:02x}.{}",
                self.pci_domain, self.pci_bus, self.pci_device, self.pci_function
            )
        })
    }
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
struct ParsedInstance {
    pid: Option<u32>,
    luid: AdapterLuid,
    physical_index: u32,
    node_ordinal: Option<u32>,
    engine: EngineClass,
}

fn parse_hex(v: &str) -> Option<u32> {
    u32::from_str_radix(v.trim_start_matches("0x"), 16).ok()
}

fn classify_engine(value: &str) -> EngineClass {
    let v = value.to_ascii_lowercase().replace([' ', '_'], "");
    if v.contains("videoencode") {
        EngineClass::VideoEncode
    } else if v.contains("videodecode") {
        EngineClass::VideoDecode
    } else if v.contains("compute") {
        EngineClass::Compute
    } else if v.contains("copy") {
        EngineClass::Copy
    } else if v == "3d" || v.contains("graphics") {
        EngineClass::ThreeD
    } else {
        EngineClass::Other
    }
}

fn parse_instance(name: &str) -> Option<ParsedInstance> {
    let tokens: Vec<&str> = name.split('_').collect();
    let after = |key: &str| {
        tokens
            .windows(2)
            .find_map(|w| w[0].eq_ignore_ascii_case(key).then_some(w[1]))
    };
    let pid = after("pid").and_then(|v| v.parse().ok());
    let li = tokens.iter().position(|v| v.eq_ignore_ascii_case("luid"))?;
    let luid = AdapterLuid {
        high: parse_hex(tokens.get(li + 1)?)?,
        low: parse_hex(tokens.get(li + 2)?)?,
    };
    let physical_index = after("phys").and_then(|v| v.parse().ok()).unwrap_or(0);
    let node_ordinal = after("eng").and_then(|v| v.parse().ok());
    let engine = after("engtype").map(classify_engine).unwrap_or_default();
    Some(ParsedInstance {
        pid,
        luid,
        physical_index,
        node_ordinal,
        engine,
    })
}

/// Normalizes raw PDH rows. Adapter and process usage are their busiest engine,
/// while memory values are summed. Physical-index and node identity are retained
/// for the D3DKMT query layer.
pub fn normalize(
    engine_rows: &[(String, f64)],
    dedicated_rows: &[(String, f64)],
    shared_rows: &[(String, f64)],
) -> GpuSnapshot {
    let mut adapters: HashMap<AdapterId, GpuAdapterSample> = HashMap::new();
    let mut processes: HashMap<u32, GpuProcessSample> = HashMap::new();
    let mut classes: HashMap<(AdapterId, EngineClass), u32> = HashMap::new();
    let mut nodes: HashMap<(AdapterId, u32), GpuEngineNodeSample> = HashMap::new();
    for (name, raw) in engine_rows {
        let Some(i) = parse_instance(name) else {
            continue;
        };
        let value = if raw.is_finite() {
            (raw.clamp(0.0, 100.0) * 10.0).round() as u32
        } else {
            0
        };
        let id = AdapterId {
            luid: i.luid,
            physical_index: i.physical_index,
        };
        classes
            .entry((id, i.engine))
            .and_modify(|v| *v = (*v).max(value))
            .or_insert(value);
        if let Some(ordinal) = i.node_ordinal {
            nodes
                .entry((id, ordinal))
                .and_modify(|n| {
                    if value > n.utilization_permille {
                        n.utilization_permille = value;
                        n.class = i.engine;
                    }
                })
                .or_insert(GpuEngineNodeSample {
                    ordinal,
                    class: i.engine,
                    utilization_permille: value,
                });
        }
        if let Some(pid) = i.pid {
            let p = processes.entry(pid).or_default();
            p.pid = pid;
            p.utilization_permille = p.utilization_permille.max(value);
        }
    }
    let mut add_memory = |rows: &[(String, f64)], dedicated: bool| {
        for (name, raw) in rows {
            let Some(i) = parse_instance(name) else {
                continue;
            };
            let bytes = if raw.is_finite() && *raw > 0.0 {
                *raw as u64
            } else {
                0
            };
            let id = AdapterId {
                luid: i.luid,
                physical_index: i.physical_index,
            };
            let a = adapters.entry(id).or_insert_with(|| adapter_shell(id));
            if dedicated {
                a.dedicated_used = a.dedicated_used.saturating_add(bytes);
            } else {
                a.shared_used = a.shared_used.saturating_add(bytes);
            }
            if let Some(pid) = i.pid {
                let p = processes.entry(pid).or_default();
                p.pid = pid;
                if dedicated {
                    p.dedicated_bytes = p.dedicated_bytes.saturating_add(bytes);
                } else {
                    p.shared_bytes = p.shared_bytes.saturating_add(bytes);
                }
            }
        }
    };
    add_memory(dedicated_rows, true);
    add_memory(shared_rows, false);
    for ((id, class), utilization_permille) in classes {
        let a = adapters.entry(id).or_insert_with(|| adapter_shell(id));
        a.utilization_permille = a.utilization_permille.max(utilization_permille);
        a.engines.push(GpuEngineSample {
            class,
            utilization_permille,
        });
    }
    for ((id, _), node) in nodes {
        adapters
            .entry(id)
            .or_insert_with(|| adapter_shell(id))
            .engine_nodes
            .push(node);
    }
    let mut adapters: Vec<_> = adapters.into_values().collect();
    for a in &mut adapters {
        a.engines.sort_by_key(|e| e.class.label());
        a.engine_nodes.sort_by_key(|n| n.ordinal);
    }
    adapters.sort_by_key(|a| (a.luid.high, a.luid.low, a.physical_index));
    let mut processes: Vec<_> = processes.into_values().collect();
    processes.sort_by(|a, b| b.utilization_permille.cmp(&a.utilization_permille));
    aggregate_snapshot(adapters, processes)
}

fn aggregate_snapshot(
    adapters: Vec<GpuAdapterSample>,
    processes: Vec<GpuProcessSample>,
) -> GpuSnapshot {
    GpuSnapshot {
        available: !adapters.is_empty(),
        unavailable_reason: if adapters.is_empty() {
            "Windows did not expose GPU performance counters for this session.".into()
        } else {
            String::new()
        },
        system_utilization_permille: adapters
            .iter()
            .map(|a| a.utilization_permille)
            .max()
            .unwrap_or(0),
        dedicated_used: adapters.iter().map(|a| a.dedicated_used).sum(),
        dedicated_budget: adapters.iter().map(|a| a.dedicated_budget).sum(),
        shared_used: adapters.iter().map(|a| a.shared_used).sum(),
        shared_budget: adapters.iter().map(|a| a.shared_budget).sum(),
        adapters,
        processes,
    }
}

fn adapter_shell(id: AdapterId) -> GpuAdapterSample {
    GpuAdapterSample {
        luid: id.luid,
        physical_index: id.physical_index,
        name: format!("GPU {}", id.stable_key()),
        sensor_unavailable_reason: "Windows WDDM performance data has not been queried yet.".into(),
        ..Default::default()
    }
}

fn valid_temperature(deci_c: u32) -> Option<f64> {
    (deci_c > 0 && deci_c <= 2000).then_some(deci_c as f64 / 10.0)
}

fn hz_to_mhz(hz: u64) -> Option<u32> {
    (hz > 0).then_some((hz / 1_000_000).min(u32::MAX as u64) as u32)
}

#[cfg(windows)]
mod platform {
    use super::*;
    use crate::gpu_vendor::VendorSupervisor;
    use std::{
        collections::{HashMap, HashSet},
        ffi::c_void,
        mem::{size_of, zeroed},
        ptr::{null, null_mut},
        time::{Duration, Instant},
    };

    type Status = i32;
    type Query = *mut c_void;
    type Counter = *mut c_void;
    type D3dHandle = u32;
    const SUCCESS: Status = 0;
    const MORE_DATA: Status = 0x8000_07D2u32 as i32;
    const FMT_DOUBLE: u32 = 0x200;
    const KMT_ADAPTER_ADDRESS_RENDER: u32 = 53;
    const KMT_NODE_PERFDATA: u32 = 61;
    const KMT_ADAPTER_PERFDATA: u32 = 62;
    const KMT_ADAPTER_PERFDATA_CAPS: u32 = 63;
    const DXGI_LOCAL: u32 = 0;
    const DXGI_NON_LOCAL: u32 = 1;

    #[repr(C)]
    union ValueUnion {
        double_value: f64,
        _large: i64,
    }
    #[repr(C)]
    struct Value {
        status: u32,
        value: ValueUnion,
    }
    #[repr(C)]
    struct Item {
        name: *const u16,
        value: Value,
    }
    #[link(name = "pdh")]
    extern "system" {
        fn PdhOpenQueryW(source: *const u16, user: usize, query: *mut Query) -> Status;
        fn PdhAddEnglishCounterW(
            query: Query,
            path: *const u16,
            user: usize,
            counter: *mut Counter,
        ) -> Status;
        fn PdhCollectQueryData(query: Query) -> Status;
        fn PdhGetFormattedCounterArrayW(
            counter: Counter,
            format: u32,
            size: *mut u32,
            count: *mut u32,
            buffer: *mut Item,
        ) -> Status;
        fn PdhCloseQuery(query: Query) -> Status;
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct WinLuid {
        low: u32,
        high: i32,
    }
    #[repr(C)]
    struct OpenAdapter {
        luid: WinLuid,
        handle: D3dHandle,
    }
    #[repr(C)]
    struct CloseAdapter {
        handle: D3dHandle,
    }
    #[repr(C)]
    struct QueryAdapterInfo {
        handle: D3dHandle,
        kind: u32,
        data: *mut c_void,
        size: u32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct AdapterAddress {
        bus: u32,
        device: u32,
        function: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct AdapterPerfData {
        physical_index: u32,
        memory_frequency: u64,
        max_memory_frequency: u64,
        max_memory_frequency_oc: u64,
        memory_bandwidth: u64,
        pcie_bandwidth: u64,
        fan_rpm: u32,
        power: u32,
        temperature: u32,
        power_state_override: u8,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct AdapterPerfCaps {
        physical_index: u32,
        max_memory_bandwidth: u64,
        max_pcie_bandwidth: u64,
        max_fan_rpm: u32,
        temperature_max: u32,
        temperature_warning: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct NodePerfData {
        node_ordinal: u32,
        physical_index: u32,
        frequency: u64,
        max_frequency: u64,
        max_frequency_oc: u64,
        voltage: u32,
        voltage_max: u32,
        voltage_max_oc: u32,
        max_transition_latency: u64,
    }
    #[link(name = "gdi32")]
    extern "system" {
        fn D3DKMTOpenAdapterFromLuid(data: *mut OpenAdapter) -> Status;
        fn D3DKMTQueryAdapterInfo(data: *mut QueryAdapterInfo) -> Status;
        fn D3DKMTCloseAdapter(data: *const CloseAdapter) -> Status;
    }

    struct KmtAdapter {
        handle: D3dHandle,
    }
    impl KmtAdapter {
        fn open(luid: AdapterLuid) -> Result<Self, Status> {
            let mut data = OpenAdapter {
                luid: WinLuid {
                    low: luid.low,
                    high: luid.high as i32,
                },
                handle: 0,
            };
            let status = unsafe { D3DKMTOpenAdapterFromLuid(&mut data) };
            if status >= 0 && data.handle != 0 {
                Ok(Self {
                    handle: data.handle,
                })
            } else {
                Err(status)
            }
        }
        fn query<T>(&self, kind: u32, data: &mut T) -> Status {
            let mut query = QueryAdapterInfo {
                handle: self.handle,
                kind,
                data: (data as *mut T).cast(),
                size: size_of::<T>() as u32,
            };
            unsafe { D3DKMTQueryAdapterInfo(&mut query) }
        }
    }
    impl Drop for KmtAdapter {
        fn drop(&mut self) {
            unsafe {
                let _ = D3DKMTCloseAdapter(&CloseAdapter {
                    handle: self.handle,
                });
            }
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Guid {
        d1: u32,
        d2: u16,
        d3: u16,
        d4: [u8; 8],
    }
    const IID_IDXGI_FACTORY1: Guid = Guid {
        d1: 0x770aae78,
        d2: 0xf26f,
        d3: 0x4dba,
        d4: [0xa8, 0x29, 0x25, 0x3c, 0x83, 0xd1, 0xb3, 0x87],
    };
    const IID_IDXGI_ADAPTER3: Guid = Guid {
        d1: 0x645967a4,
        d2: 0x1392,
        d3: 0x4310,
        d4: [0xa7, 0x98, 0x80, 0x53, 0xce, 0x3e, 0x93, 0xfd],
    };
    const GUID_DEVCLASS_DISPLAY: Guid = Guid {
        d1: 0x4d36e968,
        d2: 0xe325,
        d3: 0x11ce,
        d4: [0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18],
    };
    #[repr(C)]
    struct AdapterDesc1 {
        description: [u16; 128],
        vendor_id: u32,
        device_id: u32,
        subsys_id: u32,
        revision: u32,
        dedicated_video: usize,
        dedicated_system: usize,
        shared_system: usize,
        luid: WinLuid,
        flags: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct VideoMemoryInfo {
        budget: u64,
        current_usage: u64,
        current_reservation: u64,
        available_for_reservation: u64,
    }
    #[link(name = "dxgi")]
    extern "system" {
        fn CreateDXGIFactory1(iid: *const Guid, factory: *mut *mut c_void) -> i32;
    }

    type DevInfoSet = *mut c_void;
    type RegKey = *mut c_void;
    #[repr(C)]
    struct SpDevInfoData {
        size: u32,
        class_guid: Guid,
        dev_inst: u32,
        reserved: usize,
    }
    const DIGCF_PRESENT: u32 = 0x2;
    const SPDRP_HARDWAREID: u32 = 0x1;
    const DICS_FLAG_GLOBAL: u32 = 0x1;
    const DIREG_DRV: u32 = 0x2;
    const KEY_READ: u32 = 0x20019;
    #[link(name = "setupapi")]
    extern "system" {
        fn SetupDiGetClassDevsW(
            class: *const Guid,
            enumerator: *const u16,
            parent: *mut c_void,
            flags: u32,
        ) -> DevInfoSet;
        fn SetupDiEnumDeviceInfo(set: DevInfoSet, index: u32, data: *mut SpDevInfoData) -> i32;
        fn SetupDiGetDeviceRegistryPropertyW(
            set: DevInfoSet,
            data: *mut SpDevInfoData,
            property: u32,
            property_type: *mut u32,
            buffer: *mut u8,
            buffer_size: u32,
            required: *mut u32,
        ) -> i32;
        fn SetupDiOpenDevRegKey(
            set: DevInfoSet,
            data: *mut SpDevInfoData,
            scope: u32,
            profile: u32,
            key_type: u32,
            access: u32,
        ) -> RegKey;
        fn SetupDiDestroyDeviceInfoList(set: DevInfoSet) -> i32;
    }
    #[link(name = "advapi32")]
    extern "system" {
        fn RegQueryValueExW(
            key: RegKey,
            value_name: *const u16,
            reserved: *mut u32,
            value_type: *mut u32,
            data: *mut u8,
            size: *mut u32,
        ) -> i32;
        fn RegCloseKey(key: RegKey) -> i32;
    }

    unsafe fn com_method<T: Copy>(object: *mut c_void, index: usize) -> T {
        let vtable = *(object as *mut *mut *mut c_void);
        std::mem::transmute_copy(&*vtable.add(index))
    }
    unsafe fn release(object: *mut c_void) {
        let f: extern "system" fn(*mut c_void) -> u32 = com_method(object, 2);
        let _ = f(object);
    }
    unsafe fn query_interface(object: *mut c_void, iid: &Guid) -> Option<*mut c_void> {
        let f: extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 =
            com_method(object, 0);
        let mut result = null_mut();
        (f(object, iid, &mut result) >= 0 && !result.is_null()).then_some(result)
    }

    struct StaticAdapterInfo {
        name: String,
        vendor_id: u32,
        device_id: u32,
        active_display: bool,
        memory_budgets: HashMap<u32, (u64, u64)>,
        driver_version: String,
        driver_date: String,
        software: bool,
    }

    #[derive(Clone, Copy, Default)]
    struct KmtStaticInfo {
        address: Option<AdapterAddress>,
        caps: Option<AdapterPerfCaps>,
    }

    #[derive(Default)]
    struct DriverMetadata {
        vendor_id: u32,
        device_id: u32,
        version: String,
        date: String,
    }

    unsafe fn registry_string(key: RegKey, name: &str) -> String {
        let mut bytes = 0u32;
        let value_name = wide(name);
        if RegQueryValueExW(
            key,
            value_name.as_ptr(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut bytes,
        ) != 0
            || bytes < 2
        {
            return String::new();
        }
        let mut buffer = vec![0u16; (bytes as usize + 1) / 2];
        if RegQueryValueExW(
            key,
            value_name.as_ptr(),
            null_mut(),
            null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        ) != 0
        {
            return String::new();
        }
        let len = buffer.iter().position(|v| *v == 0).unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..len])
    }

    fn parse_hardware_id(value: &str, marker: &str) -> Option<u32> {
        let upper = value.to_ascii_uppercase();
        let start = upper.find(marker)? + marker.len();
        u32::from_str_radix(upper.get(start..start + 4)?, 16).ok()
    }

    unsafe fn enumerate_driver_metadata() -> Vec<DriverMetadata> {
        let set = SetupDiGetClassDevsW(&GUID_DEVCLASS_DISPLAY, null(), null_mut(), DIGCF_PRESENT);
        if set as isize == -1 {
            return vec![];
        }
        let mut result = vec![];
        for index in 0..256u32 {
            let mut data = SpDevInfoData {
                size: size_of::<SpDevInfoData>() as u32,
                class_guid: GUID_DEVCLASS_DISPLAY,
                dev_inst: 0,
                reserved: 0,
            };
            if SetupDiEnumDeviceInfo(set, index, &mut data) == 0 {
                break;
            }
            let mut hardware = [0u16; 1024];
            let mut property_type = 0u32;
            let mut required = 0u32;
            if SetupDiGetDeviceRegistryPropertyW(
                set,
                &mut data,
                SPDRP_HARDWAREID,
                &mut property_type,
                hardware.as_mut_ptr().cast(),
                size_of::<[u16; 1024]>() as u32,
                &mut required,
            ) == 0
            {
                continue;
            }
            let len = hardware
                .iter()
                .position(|v| *v == 0)
                .unwrap_or(hardware.len());
            let hardware_id = String::from_utf16_lossy(&hardware[..len]);
            let (Some(vendor_id), Some(device_id)) = (
                parse_hardware_id(&hardware_id, "VEN_"),
                parse_hardware_id(&hardware_id, "DEV_"),
            ) else {
                continue;
            };
            let key =
                SetupDiOpenDevRegKey(set, &mut data, DICS_FLAG_GLOBAL, 0, DIREG_DRV, KEY_READ);
            let (version, date) = if key as isize != -1 {
                let values = (
                    registry_string(key, "DriverVersion"),
                    registry_string(key, "DriverDate"),
                );
                let _ = RegCloseKey(key);
                values
            } else {
                (String::new(), String::new())
            };
            result.push(DriverMetadata {
                vendor_id,
                device_id,
                version,
                date,
            });
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
        result
    }

    unsafe fn enumerate_dxgi() -> HashMap<AdapterLuid, StaticAdapterInfo> {
        let mut out = HashMap::new();
        let driver_metadata = enumerate_driver_metadata();
        let mut factory = null_mut();
        if CreateDXGIFactory1(&IID_IDXGI_FACTORY1, &mut factory) < 0 || factory.is_null() {
            return out;
        }
        let enum_adapter: extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32 =
            com_method(factory, 12);
        for index in 0..64u32 {
            let mut adapter = null_mut();
            if enum_adapter(factory, index, &mut adapter) < 0 || adapter.is_null() {
                break;
            }
            let get_desc: extern "system" fn(*mut c_void, *mut AdapterDesc1) -> i32 =
                com_method(adapter, 10);
            let mut desc: AdapterDesc1 = zeroed();
            if get_desc(adapter, &mut desc) >= 0 {
                let luid = AdapterLuid {
                    high: desc.luid.high as u32,
                    low: desc.luid.low,
                };
                let len = desc
                    .description
                    .iter()
                    .position(|v| *v == 0)
                    .unwrap_or(desc.description.len());
                let enum_outputs: extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32 =
                    com_method(adapter, 7);
                let mut output = null_mut();
                let active_display =
                    enum_outputs(adapter, 0, &mut output) >= 0 && !output.is_null();
                if !output.is_null() {
                    release(output);
                }
                let mut memory_budgets = HashMap::new();
                if let Some(adapter3) = query_interface(adapter, &IID_IDXGI_ADAPTER3) {
                    let query_mem: extern "system" fn(
                        *mut c_void,
                        u32,
                        u32,
                        *mut VideoMemoryInfo,
                    ) -> i32 = com_method(adapter3, 14);
                    for physical_index in 0..32u32 {
                        let mut local = VideoMemoryInfo::default();
                        let mut non_local = VideoMemoryInfo::default();
                        let local_ok =
                            query_mem(adapter3, physical_index, DXGI_LOCAL, &mut local) >= 0;
                        let non_local_ok =
                            query_mem(adapter3, physical_index, DXGI_NON_LOCAL, &mut non_local)
                                >= 0;
                        if !local_ok && !non_local_ok {
                            break;
                        }
                        let dedicated = if local_ok && local.budget > 0 {
                            local.budget
                        } else if physical_index == 0 {
                            desc.dedicated_video as u64
                        } else {
                            0
                        };
                        let shared = if non_local_ok && non_local.budget > 0 {
                            non_local.budget
                        } else if physical_index == 0 {
                            desc.shared_system as u64
                        } else {
                            0
                        };
                        memory_budgets.insert(physical_index, (dedicated, shared));
                    }
                    release(adapter3);
                }
                memory_budgets
                    .entry(0)
                    .or_insert((desc.dedicated_video as u64, desc.shared_system as u64));
                out.insert(
                    luid,
                    StaticAdapterInfo {
                        name: String::from_utf16_lossy(&desc.description[..len]),
                        vendor_id: desc.vendor_id,
                        device_id: desc.device_id,
                        active_display,
                        memory_budgets,
                        driver_version: driver_metadata
                            .iter()
                            .find(|m| {
                                m.vendor_id == desc.vendor_id && m.device_id == desc.device_id
                            })
                            .map(|m| m.version.clone())
                            .unwrap_or_default(),
                        driver_date: driver_metadata
                            .iter()
                            .find(|m| {
                                m.vendor_id == desc.vendor_id && m.device_id == desc.device_id
                            })
                            .map(|m| m.date.clone())
                            .unwrap_or_default(),
                        software: desc.flags & 0x2 != 0,
                    },
                );
            }
            release(adapter);
        }
        release(factory);
        out
    }

    fn wide(v: &str) -> Vec<u16> {
        v.encode_utf16().chain(Some(0)).collect()
    }
    unsafe fn read(counter: Counter) -> Vec<(String, f64)> {
        let (mut bytes, mut count) = (0, 0);
        if PdhGetFormattedCounterArrayW(counter, FMT_DOUBLE, &mut bytes, &mut count, null_mut())
            != MORE_DATA
            || bytes == 0
        {
            return vec![];
        }
        let mut buffer = vec![0u8; bytes as usize];
        if PdhGetFormattedCounterArrayW(
            counter,
            FMT_DOUBLE,
            &mut bytes,
            &mut count,
            buffer.as_mut_ptr().cast(),
        ) != SUCCESS
        {
            return vec![];
        }
        std::slice::from_raw_parts(buffer.as_ptr().cast::<Item>(), count as usize)
            .iter()
            .filter_map(|item| {
                if item.name.is_null() || item.value.status != 0 {
                    return None;
                }
                let mut n = 0;
                while *item.name.add(n) != 0 {
                    n += 1;
                }
                Some((
                    String::from_utf16_lossy(std::slice::from_raw_parts(item.name, n)),
                    item.value.value.double_value,
                ))
            })
            .collect()
    }

    pub struct PlatformGpuCollector {
        query: Query,
        engine: Counter,
        dedicated: Counter,
        shared: Counter,
        handles: HashMap<AdapterLuid, KmtAdapter>,
        static_info: HashMap<AdapterLuid, StaticAdapterInfo>,
        kmt_static: HashMap<AdapterId, KmtStaticInfo>,
        last_static_refresh: Option<Instant>,
        vendor: VendorSupervisor,
    }
    unsafe impl Send for PlatformGpuCollector {}

    impl PlatformGpuCollector {
        pub fn new() -> anyhow::Result<Self> {
            unsafe {
                let mut query = null_mut();
                if PdhOpenQueryW(null(), 0, &mut query) != SUCCESS {
                    anyhow::bail!("PDH GPU query unavailable");
                }
                let mut c = [null_mut(); 3];
                for (index, path) in [
                    r"\GPU Engine(*)\Utilization Percentage",
                    r"\GPU Process Memory(*)\Dedicated Usage",
                    r"\GPU Process Memory(*)\Shared Usage",
                ]
                .iter()
                .enumerate()
                {
                    if PdhAddEnglishCounterW(query, wide(path).as_ptr(), 0, &mut c[index])
                        != SUCCESS
                    {
                        PdhCloseQuery(query);
                        anyhow::bail!("GPU counters unavailable");
                    }
                }
                let _ = PdhCollectQueryData(query);
                Ok(Self {
                    query,
                    engine: c[0],
                    dedicated: c[1],
                    shared: c[2],
                    handles: HashMap::new(),
                    static_info: HashMap::new(),
                    kmt_static: HashMap::new(),
                    last_static_refresh: None,
                    vendor: VendorSupervisor::new(),
                })
            }
        }

        fn refresh_static(&mut self) {
            if self
                .last_static_refresh
                .map(|v| v.elapsed() < Duration::from_secs(30))
                .unwrap_or(false)
            {
                return;
            }
            self.static_info = unsafe { enumerate_dxgi() };
            self.kmt_static.clear();
            self.last_static_refresh = Some(Instant::now());
        }

        fn query_wddm(&mut self, adapter: &mut GpuAdapterSample) {
            let luid = adapter.luid;
            if !self.handles.contains_key(&luid) {
                match KmtAdapter::open(luid) {
                    Ok(handle) => {
                        self.handles.insert(luid, handle);
                    }
                    Err(status) => {
                        adapter.sensor_unavailable_reason = format!(
                            "D3DKMT adapter open failed (NTSTATUS 0x{:08x}).",
                            status as u32
                        );
                        add_unavailable(
                            adapter,
                            SensorKind::CoreTemperature,
                            AvailabilityReason::DriverError,
                            &adapter.sensor_unavailable_reason.clone(),
                        );
                        return;
                    }
                }
            }
            let Some(handle) = self.handles.get(&luid) else {
                return;
            };
            let adapter_id = adapter.id();
            if !self.kmt_static.contains_key(&adapter_id) {
                let mut entry = KmtStaticInfo::default();
                let mut address = AdapterAddress::default();
                if handle.query(KMT_ADAPTER_ADDRESS_RENDER, &mut address) >= 0 {
                    entry.address = Some(address);
                }
                let mut caps = AdapterPerfCaps {
                    physical_index: adapter.physical_index,
                    ..Default::default()
                };
                if handle.query(KMT_ADAPTER_PERFDATA_CAPS, &mut caps) >= 0 {
                    entry.caps = Some(caps);
                }
                self.kmt_static.insert(adapter_id, entry);
            }
            let static_data = self
                .kmt_static
                .get(&adapter_id)
                .copied()
                .unwrap_or_default();
            if let Some(address) = static_data.address {
                if address.bus <= 0xff && address.device <= 0x1f && address.function <= 0x7 {
                    adapter.pci_bus = address.bus;
                    adapter.pci_device = address.device;
                    adapter.pci_function = address.function;
                    adapter.pci_identity_available = true;
                }
            }
            let caps = static_data.caps.unwrap_or_default();
            if static_data.caps.is_some() {
                adapter.temperature_warning_c = valid_temperature(caps.temperature_warning);
                adapter.temperature_max_c = valid_temperature(caps.temperature_max);
            }
            let mut perf = AdapterPerfData {
                physical_index: adapter.physical_index,
                ..Default::default()
            };
            let status = handle.query(KMT_ADAPTER_PERFDATA, &mut perf);
            if status < 0 {
                adapter.sensor_unavailable_reason = format!(
                    "D3DKMT performance query failed (NTSTATUS 0x{:08x}).",
                    status as u32
                );
                for kind in [
                    SensorKind::CoreTemperature,
                    SensorKind::FanRpm,
                    SensorKind::MemoryClock,
                    SensorKind::PowerPercent,
                ] {
                    add_unavailable(
                        adapter,
                        kind,
                        AvailabilityReason::DriverError,
                        &adapter.sensor_unavailable_reason.clone(),
                    );
                }
                self.handles.remove(&luid);
                self.kmt_static.remove(&adapter_id);
                self.last_static_refresh = None;
                return;
            }
            if let Some(value) = valid_temperature(perf.temperature) {
                adapter.temperature_c = Some(value);
                adapter.temperatures.push(TemperatureSample {
                    kind: TemperatureKind::Core,
                    celsius: value,
                    source: TelemetrySource::WindowsWddm,
                    label: "GPU core".into(),
                });
                add_available(
                    adapter,
                    SensorKind::CoreTemperature,
                    TelemetrySource::WindowsWddm,
                );
            } else {
                add_unavailable(
                    adapter,
                    SensorKind::CoreTemperature,
                    AvailabilityReason::UnsupportedMetric,
                    "The display driver did not return a valid core temperature.",
                );
            }
            adapter.power_percent = (perf.power <= 1000).then_some(perf.power as f64 / 10.0);
            if adapter.power_percent.is_some() {
                add_available(
                    adapter,
                    SensorKind::PowerPercent,
                    TelemetrySource::WindowsWddm,
                );
            } else {
                add_unavailable(
                    adapter,
                    SensorKind::PowerPercent,
                    AvailabilityReason::UnsupportedMetric,
                    "The display driver did not return power percentage.",
                );
            }
            adapter.memory_clock_mhz = hz_to_mhz(perf.memory_frequency);
            if adapter.memory_clock_mhz.is_some() {
                add_available(
                    adapter,
                    SensorKind::MemoryClock,
                    TelemetrySource::WindowsWddm,
                );
            } else {
                add_unavailable(
                    adapter,
                    SensorKind::MemoryClock,
                    AvailabilityReason::UnsupportedMetric,
                    "The display driver did not return memory clock frequency.",
                );
            }
            if caps.max_fan_rpm > 0 {
                adapter.fan_rpm = Some(perf.fan_rpm);
                add_available(adapter, SensorKind::FanRpm, TelemetrySource::WindowsWddm);
            } else {
                add_unavailable(
                    adapter,
                    SensorKind::FanRpm,
                    AvailabilityReason::UnsupportedMetric,
                    "The display driver did not expose fan RPM capability.",
                );
            }

            let mut best: Option<(u32, u32)> = None;
            for node in &adapter.engine_nodes {
                let mut data = NodePerfData {
                    node_ordinal: node.ordinal,
                    physical_index: adapter.physical_index,
                    ..Default::default()
                };
                if handle.query(KMT_NODE_PERFDATA, &mut data) >= 0 {
                    if let Some(mhz) = hz_to_mhz(data.frequency) {
                        if best
                            .map(|(load, _)| node.utilization_permille > load)
                            .unwrap_or(true)
                        {
                            best = Some((node.utilization_permille, mhz));
                        }
                    }
                }
            }
            adapter.core_clock_mhz = best.map(|(_, mhz)| mhz);
            if adapter.core_clock_mhz.is_some() {
                add_available(adapter, SensorKind::CoreClock, TelemetrySource::WindowsWddm);
            } else {
                add_unavailable(
                    adapter,
                    SensorKind::CoreClock,
                    AvailabilityReason::UnsupportedMetric,
                    "No observed engine node returned a clock frequency.",
                );
            }
            adapter.sensor_source = "Windows WDDM".into();
            adapter.sensor_unavailable_reason.clear();
        }

        pub fn sample(&mut self) -> GpuSnapshot {
            unsafe {
                if PdhCollectQueryData(self.query) != SUCCESS {
                    return GpuSnapshot {
                        unavailable_reason: "GPU counter collection failed.".into(),
                        ..Default::default()
                    };
                }
                let mut snapshot = normalize(
                    &read(self.engine),
                    &read(self.dedicated),
                    &read(self.shared),
                );
                self.refresh_static();
                snapshot.adapters.retain(|adapter| {
                    self.static_info
                        .get(&adapter.luid)
                        .map(|info| !info.software)
                        .unwrap_or(true)
                });
                let present_luids: HashSet<_> = snapshot
                    .adapters
                    .iter()
                    .map(|adapter| adapter.luid)
                    .collect();
                let present_ids: HashSet<_> =
                    snapshot.adapters.iter().map(GpuAdapterSample::id).collect();
                self.handles.retain(|luid, _| present_luids.contains(luid));
                self.kmt_static.retain(|id, _| present_ids.contains(id));
                for adapter in &mut snapshot.adapters {
                    if let Some(info) = self.static_info.get(&adapter.luid) {
                        adapter.name = if adapter.physical_index == 0 {
                            info.name.clone()
                        } else {
                            format!("{} (physical {})", info.name, adapter.physical_index)
                        };
                        adapter.vendor_id = info.vendor_id;
                        adapter.device_id = info.device_id;
                        adapter.driver_version = info.driver_version.clone();
                        adapter.driver_date = info.driver_date.clone();
                        adapter.active_display = info.active_display;
                        if let Some((dedicated, shared)) =
                            info.memory_budgets.get(&adapter.physical_index)
                        {
                            adapter.dedicated_budget = *dedicated;
                            adapter.shared_budget = *shared;
                        }
                    }
                    self.query_wddm(adapter);
                }
                self.vendor.merge_nonblocking(&mut snapshot.adapters);
                snapshot.available = !snapshot.adapters.is_empty();
                if !snapshot.available {
                    snapshot.unavailable_reason =
                        "Windows did not expose a physical GPU adapter for this session.".into();
                }
                snapshot.system_utilization_permille = snapshot
                    .adapters
                    .iter()
                    .map(|a| a.utilization_permille)
                    .max()
                    .unwrap_or(0);
                snapshot.dedicated_used = snapshot.adapters.iter().map(|a| a.dedicated_used).sum();
                snapshot.shared_used = snapshot.adapters.iter().map(|a| a.shared_used).sum();
                snapshot.dedicated_budget =
                    snapshot.adapters.iter().map(|a| a.dedicated_budget).sum();
                snapshot.shared_budget = snapshot.adapters.iter().map(|a| a.shared_budget).sum();
                snapshot
            }
        }
    }

    impl Drop for PlatformGpuCollector {
        fn drop(&mut self) {
            unsafe {
                let _ = PdhCloseQuery(self.query);
            }
        }
    }

    fn add_available(adapter: &mut GpuAdapterSample, kind: SensorKind, source: TelemetrySource) {
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
    fn add_unavailable(
        adapter: &mut GpuAdapterSample,
        kind: SensorKind,
        reason: AvailabilityReason,
        detail: &str,
    ) {
        adapter
            .sensor_availability
            .retain(|v| !(v.kind == kind && v.source == TelemetrySource::WindowsWddm));
        adapter.sensor_availability.push(SensorAvailability {
            kind,
            available: false,
            source: TelemetrySource::WindowsWddm,
            reason,
            detail: detail.into(),
        });
    }

    #[cfg(test)]
    mod abi_tests {
        use super::*;
        #[test]
        fn d3dkmt_abi_matches_windows_sdk_64_bit() {
            if cfg!(target_pointer_width = "64") {
                assert_eq!(size_of::<OpenAdapter>(), 12);
                assert_eq!(size_of::<CloseAdapter>(), 4);
                assert_eq!(size_of::<QueryAdapterInfo>(), 24);
                assert_eq!(size_of::<AdapterAddress>(), 12);
                assert_eq!(size_of::<AdapterPerfData>(), 64);
                assert_eq!(size_of::<AdapterPerfCaps>(), 40);
                assert_eq!(size_of::<NodePerfData>(), 56);
                assert_eq!(std::mem::offset_of!(QueryAdapterInfo, data), 8);
                assert_eq!(std::mem::offset_of!(QueryAdapterInfo, size), 16);
                assert_eq!(std::mem::offset_of!(AdapterPerfData, memory_frequency), 8);
                assert_eq!(std::mem::offset_of!(AdapterPerfData, fan_rpm), 48);
                assert_eq!(
                    std::mem::offset_of!(AdapterPerfCaps, max_memory_bandwidth),
                    8
                );
                assert_eq!(std::mem::offset_of!(NodePerfData, frequency), 8);
                assert_eq!(
                    std::mem::offset_of!(NodePerfData, max_transition_latency),
                    48
                );
            }
            assert_eq!(size_of::<AdapterAddress>(), 12);
            assert_eq!(KMT_NODE_PERFDATA, 61);
            assert_eq!(KMT_ADAPTER_PERFDATA, 62);
            assert_eq!(KMT_ADAPTER_PERFDATA_CAPS, 63);
        }
    }
}

pub struct GpuCollector {
    #[cfg(windows)]
    inner: Option<platform::PlatformGpuCollector>,
}
impl GpuCollector {
    pub fn new() -> Self {
        Self {
            #[cfg(windows)]
            inner: platform::PlatformGpuCollector::new().ok(),
        }
    }
    pub fn sample(&mut self) -> GpuSnapshot {
        #[cfg(windows)]
        if let Some(inner) = &mut self.inner {
            return inner.sample();
        }
        GpuSnapshot {
            unavailable_reason: "GPU telemetry is unavailable on this system.".into(),
            ..Default::default()
        }
    }
}
impl Default for GpuCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn busiest_engine_and_physical_index_win() {
        let e = vec![
            (
                "pid_42_luid_0x00000000_0x000000AA_phys_1_eng_0_engtype_3D".into(),
                31.0,
            ),
            (
                "pid_42_luid_0x00000000_0x000000AA_phys_1_eng_1_engtype_Compute_0".into(),
                72.0,
            ),
        ];
        let d = vec![("pid_42_luid_0x00000000_0x000000AA_phys_1".into(), 1024.0)];
        let got = normalize(&e, &d, &[]);
        assert_eq!(got.system_utilization_permille, 720);
        assert_eq!(got.processes[0].dedicated_bytes, 1024);
        assert_eq!(got.adapters[0].physical_index, 1);
        assert_eq!(got.adapters[0].stable_key(), "00000000:000000aa:p1");
        assert_eq!(got.adapters[0].engine_nodes.len(), 2);
    }
    #[test]
    fn malformed_rows_degrade_honestly() {
        let got = normalize(&[("bad".into(), f64::NAN)], &[], &[]);
        assert!(!got.available);
        assert!(!got.unavailable_reason.is_empty());
    }
    #[test]
    fn unit_conversions_reject_invalid_values() {
        assert_eq!(valid_temperature(590), Some(59.0));
        assert_eq!(valid_temperature(0), None);
        assert_eq!(valid_temperature(3000), None);
        assert_eq!(hz_to_mhz(1_755_000_000), Some(1755));
    }
    #[test]
    fn pci_identity_must_be_confirmed_before_vendor_matching() {
        let mut adapter = GpuAdapterSample {
            vendor_id: 0x10de,
            ..Default::default()
        };
        assert_eq!(adapter.pci_bus_id(), None);
        adapter.pci_identity_available = true;
        assert_eq!(adapter.pci_bus_id().as_deref(), Some("00000000:00:00.0"));
    }
}
