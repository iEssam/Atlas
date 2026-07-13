//! Battery + thermal collectors (PRD §9.6.6/§9.6.7, docs/phases.md Phase 2).
//!
//! **Battery.** `GetSystemPowerStatus` gives the coarse picture (AC line,
//! charging flag, percent, estimated runtime) and, crucially, whether a system
//! battery exists at all. When one does, the battery device interface
//! (`SetupDiGetClassDevs(GUID_DEVCLASS_BATTERY)` → open the device →
//! `IOCTL_BATTERY_QUERY_INFORMATION` / `IOCTL_BATTERY_QUERY_STATUS`) adds the
//! precise design/full-charge capacity (mWh), charge/discharge rate, and cycle
//! count, from which battery *health* (full ÷ design) is derived. On a desktop
//! the collector reports `available = false, "no battery present"`.
//!
//! **Thermal.** The ACPI thermal zones are read through WMI's
//! `MSAcpi_ThermalZoneTemperature` (`root\WMI`) over hand-written WBEM COM. Many
//! machines expose no ACPI thermal zone (the reading lives behind the EC / a
//! vendor driver, deferred to the v2 sensor driver per tech-stack §4.9); when
//! WMI returns nothing the collector says so honestly rather than inventing a
//! number.
//!
//! Both are read-only and unprivileged.

#![cfg(windows)]

use std::ptr::null_mut;

use crate::ffi::{
    CloseHandle, CoCreateInstance, CoInitializeEx, CoInitializeSecurity, CoSetProxyBlanket,
    CoUninitialize, CreateFileW, DeviceIoControl, GetSystemPowerStatus, IEnumWbemClassObjectVtbl,
    IUnknownVtbl, IWbemClassObjectVtbl, IWbemLocatorVtbl, IWbemServicesVtbl,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, SysAllocString, SysFreeString, VariantClear,
    BATTERY_CAPACITY_RELATIVE, BATTERY_CHARGING, BATTERY_INFORMATION, BATTERY_INFORMATION_LEVEL,
    BATTERY_POWER_ON_LINE, BATTERY_QUERY_INFORMATION, BATTERY_STATUS, BATTERY_UNKNOWN_CAPACITY,
    BATTERY_UNKNOWN_RATE, BATTERY_WAIT_STATUS, CLSCTX_INPROC_SERVER, CLSID_WBEM_LOCATOR,
    COINIT_APARTMENTTHREADED, COLE_DEFAULT_AUTHINFO, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    EOAC_NONE, FILE_SHARE_READ, FILE_SHARE_WRITE, GENERIC_READ, GENERIC_WRITE,
    GUID_DEVCLASS_BATTERY, HANDLE, IID_IWBEM_LOCATOR, INVALID_HANDLE_VALUE,
    IOCTL_BATTERY_QUERY_INFORMATION, IOCTL_BATTERY_QUERY_STATUS, IOCTL_BATTERY_QUERY_TAG,
    OPEN_EXISTING, PVOID, RPC_C_AUTHN_LEVEL_CALL, RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_AUTHN_WINNT,
    RPC_C_AUTHZ_NONE, RPC_C_IMP_LEVEL_IMPERSONATE, RPC_E_CHANGED_MODE, SP_DEVICE_INTERFACE_DATA,
    SP_DEVICE_INTERFACE_DETAIL_DATA_W, SYSTEM_POWER_STATUS, S_FALSE, S_OK, VARIANT, VT_I4,
    WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_INFINITE,
};

// ===========================================================================
// Battery
// ===========================================================================

/// A battery reading. Mirrors the proto `GetBatteryStatusReply` + `BatteryStatus`.
#[derive(Debug, Clone, Default)]
pub struct BatteryReading {
    pub available: bool,
    pub unavailable_reason: String,
    pub present: bool,
    pub charging: bool,
    pub on_ac: bool,
    pub percent: u32,
    /// Charge/discharge rate in mW; negative = discharging.
    pub rate_mw: i32,
    pub remaining_mwh: u64,
    pub full_charge_mwh: u64,
    pub design_mwh: u64,
    /// full_charge ÷ design × 100, when derivable.
    pub health_percent: u32,
    pub cycle_count: u32,
    pub est_runtime_s: i64,
}

/// `SYSTEM_POWER_STATUS.BatteryFlag` bit: no system battery.
const BATTERY_FLAG_NO_BATTERY: u8 = 128;
/// `SYSTEM_POWER_STATUS.BatteryFlag` bit: charging.
const BATTERY_FLAG_CHARGING: u8 = 8;
/// Sentinel for "percent/time unknown".
const UNKNOWN_U8: u8 = 255;
const UNKNOWN_U32: u32 = 0xFFFF_FFFF;

/// Reads the current battery status. On a desktop (no battery) returns
/// `available = false, "no battery present"`.
pub fn battery_status() -> BatteryReading {
    let mut sps = SYSTEM_POWER_STATUS::default();
    // SAFETY: out-param is a live SYSTEM_POWER_STATUS.
    let ok = unsafe { GetSystemPowerStatus(&mut sps) };
    if ok == 0 {
        return BatteryReading {
            available: false,
            unavailable_reason: "power status unavailable".to_string(),
            ..Default::default()
        };
    }
    if sps.BatteryFlag & BATTERY_FLAG_NO_BATTERY != 0 {
        return BatteryReading {
            available: false,
            unavailable_reason: "no battery present".to_string(),
            ..Default::default()
        };
    }

    // Coarse fields always available from GetSystemPowerStatus.
    let mut reading = BatteryReading {
        available: true,
        present: true,
        on_ac: sps.ACLineStatus == 1,
        charging: sps.BatteryFlag & BATTERY_FLAG_CHARGING != 0,
        percent: if sps.BatteryLifePercent == UNKNOWN_U8 {
            0
        } else {
            sps.BatteryLifePercent as u32
        },
        est_runtime_s: if sps.BatteryLifeTime == UNKNOWN_U32 {
            0
        } else {
            sps.BatteryLifeTime as i64
        },
        ..Default::default()
    };

    // Precise capacity/rate/cycle from the battery device (best-effort — leaves
    // the coarse fields intact if the device path or an IOCTL fails).
    refine_from_device(&mut reading);
    reading
}

/// Opens the first battery device and fills capacity/rate/cycle/health from the
/// battery IOCTLs. Best-effort: any failure leaves `reading` as-is.
fn refine_from_device(reading: &mut BatteryReading) {
    let path = match first_battery_device_path() {
        Some(p) => p,
        None => return,
    };
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: wide is NUL-terminated; standard device open.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null_mut(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return;
    }
    let _guard = HandleGuard(handle);

    // Query the battery tag (identifies the battery for the other IOCTLs).
    let mut wait: u32 = 0;
    let mut tag: u32 = 0;
    let mut returned: u32 = 0;
    // SAFETY: in/out buffers are live locals of the documented sizes.
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_BATTERY_QUERY_TAG,
            &mut wait as *mut _ as PVOID,
            std::mem::size_of::<u32>() as u32,
            &mut tag as *mut _ as PVOID,
            std::mem::size_of::<u32>() as u32,
            &mut returned,
            null_mut(),
        )
    };
    if ok == 0 || tag == 0 {
        return;
    }

    // Static information: capacities, cycle count, capabilities.
    let mut qi = BATTERY_QUERY_INFORMATION {
        BatteryTag: tag,
        InformationLevel: BATTERY_INFORMATION_LEVEL,
        AtRate: 0,
    };
    let mut info = BATTERY_INFORMATION::default();
    // SAFETY: in = BATTERY_QUERY_INFORMATION, out = BATTERY_INFORMATION.
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_BATTERY_QUERY_INFORMATION,
            &mut qi as *mut _ as PVOID,
            std::mem::size_of::<BATTERY_QUERY_INFORMATION>() as u32,
            &mut info as *mut _ as PVOID,
            std::mem::size_of::<BATTERY_INFORMATION>() as u32,
            &mut returned,
            null_mut(),
        )
    };
    if ok != 0 {
        let relative = info.Capabilities & BATTERY_CAPACITY_RELATIVE != 0;
        if !relative {
            // Capacities are in mWh only when not relative.
            if info.DesignedCapacity != BATTERY_UNKNOWN_CAPACITY {
                reading.design_mwh = info.DesignedCapacity as u64;
            }
            if info.FullChargedCapacity != BATTERY_UNKNOWN_CAPACITY {
                reading.full_charge_mwh = info.FullChargedCapacity as u64;
            }
            if reading.design_mwh > 0 && reading.full_charge_mwh > 0 {
                reading.health_percent =
                    ((reading.full_charge_mwh * 100) / reading.design_mwh) as u32;
            }
        }
        reading.cycle_count = info.CycleCount;
    }

    // Dynamic status: remaining capacity, rate, power state.
    let mut ws = BATTERY_WAIT_STATUS {
        BatteryTag: tag,
        ..Default::default()
    };
    let mut status = BATTERY_STATUS::default();
    // SAFETY: in = BATTERY_WAIT_STATUS, out = BATTERY_STATUS.
    let ok = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_BATTERY_QUERY_STATUS,
            &mut ws as *mut _ as PVOID,
            std::mem::size_of::<BATTERY_WAIT_STATUS>() as u32,
            &mut status as *mut _ as PVOID,
            std::mem::size_of::<BATTERY_STATUS>() as u32,
            &mut returned,
            null_mut(),
        )
    };
    if ok != 0 {
        if status.Capacity != BATTERY_UNKNOWN_CAPACITY {
            reading.remaining_mwh = status.Capacity as u64;
        }
        if status.Rate != BATTERY_UNKNOWN_RATE {
            reading.rate_mw = status.Rate;
        }
        reading.charging = status.PowerState & BATTERY_CHARGING != 0;
        reading.on_ac = status.PowerState & BATTERY_POWER_ON_LINE != 0;
        // Prefer an exact percent from mWh when we have both.
        if reading.full_charge_mwh > 0 && reading.remaining_mwh > 0 {
            reading.percent =
                ((reading.remaining_mwh * 100) / reading.full_charge_mwh).min(100) as u32;
        }
    }
}

/// Returns the device path of the first present battery, or `None`.
fn first_battery_device_path() -> Option<String> {
    // SAFETY: enumerate present devices exposing the battery interface.
    let set = unsafe {
        SetupDiGetClassDevsW(
            &GUID_DEVCLASS_BATTERY,
            null_mut(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if set == INVALID_HANDLE_VALUE || set.is_null() {
        return None;
    }
    let _guard = DevInfoGuard(set);

    let mut did = SP_DEVICE_INTERFACE_DATA {
        cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
        InterfaceClassGuid: GUID_DEVCLASS_BATTERY,
        Flags: 0,
        Reserved: 0,
    };
    // Only the first battery (index 0) is read — the primary system battery.
    // SAFETY: `did` cbSize is set; out-param is live.
    let ok = unsafe {
        SetupDiEnumDeviceInterfaces(set, null_mut(), &GUID_DEVCLASS_BATTERY, 0, &mut did)
    };
    if ok == 0 {
        return None;
    }

    // Two-call detail sizing.
    let mut required: u32 = 0;
    // SAFETY: null detail probes the required size into `required`.
    unsafe {
        SetupDiGetDeviceInterfaceDetailW(set, &did, null_mut(), 0, &mut required, null_mut());
    }
    if required == 0 {
        return None;
    }
    let mut buf = vec![0u8; required as usize];
    let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
    // The fixed-header cbSize is 8 on 64-bit (DWORD + WCHAR[1], padded).
    // SAFETY: buf is at least `required` bytes; write the header size field.
    unsafe {
        (*detail).cbSize = 8;
    }
    // SAFETY: detail buffer sized to `required`; fills DevicePath in-place.
    let ok = unsafe {
        SetupDiGetDeviceInterfaceDetailW(set, &did, detail, required, &mut required, null_mut())
    };
    if ok == 0 {
        return None;
    }
    // DevicePath is a NUL-terminated WCHAR array starting at byte offset 4.
    Some(wide_from_bytes(&buf[4..]))
}

/// Decodes a NUL-terminated UTF-16LE string embedded in a byte buffer.
fn wide_from_bytes(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

/// A file/device `HANDLE` closed on drop.
struct HandleGuard(HANDLE);
impl Drop for HandleGuard {
    fn drop(&mut self) {
        if self.0 != INVALID_HANDLE_VALUE && !self.0.is_null() {
            // SAFETY: handle came from CreateFileW; close once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// A SetupAPI device-info set freed on drop.
struct DevInfoGuard(HANDLE);
impl Drop for DevInfoGuard {
    fn drop(&mut self) {
        if self.0 != INVALID_HANDLE_VALUE && !self.0.is_null() {
            // SAFETY: set came from SetupDiGetClassDevsW; destroy once.
            unsafe { SetupDiDestroyDeviceInfoList(self.0) };
        }
    }
}

// ===========================================================================
// Thermal (WMI)
// ===========================================================================

/// One thermal sensor reading. Mirrors the proto `ThermalSensor`.
#[derive(Debug, Clone)]
pub struct ThermalSensor {
    pub name: String,
    pub celsius: f64,
    /// Where the reading came from (e.g. "ACPI thermal zone (WMI)").
    pub source: String,
}

/// The thermal read result. Mirrors the proto `GetThermalReply`.
#[derive(Debug, Clone, Default)]
pub struct ThermalReading {
    pub available: bool,
    pub unavailable_reason: String,
    pub sensors: Vec<ThermalSensor>,
}

/// Reads ACPI thermal-zone temperatures via WMI. Returns
/// `available = false, "no thermal sensors exposed"` when the machine exposes no
/// `MSAcpi_ThermalZoneTemperature` instance (common — the reading often lives
/// behind the EC / a vendor driver, deferred per tech-stack §4.9).
pub fn thermal_status() -> ThermalReading {
    // COM per this thread.
    // SAFETY: initialize COM; balance only a successful init.
    let hr = unsafe { CoInitializeEx(null_mut(), COINIT_APARTMENTTHREADED) };
    let must_uninit = hr == S_OK || hr == S_FALSE;
    if hr != S_OK && hr != S_FALSE && hr != RPC_E_CHANGED_MODE {
        return ThermalReading {
            available: false,
            unavailable_reason: "thermal unavailable (COM init failed)".to_string(),
            ..Default::default()
        };
    }
    // Best-effort process-wide security (ignored if already set — WMI still
    // works via the per-proxy blanket below).
    // SAFETY: documented default-security arguments.
    unsafe {
        CoInitializeSecurity(
            null_mut(),
            COLE_DEFAULT_AUTHINFO,
            null_mut(),
            null_mut(),
            RPC_C_AUTHN_LEVEL_DEFAULT,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            null_mut(),
            EOAC_NONE,
            null_mut(),
        );
    }

    let out = thermal_inner();

    if must_uninit {
        // SAFETY: balances the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    out
}

/// The WMI query, with COM already initialized on this thread.
fn thermal_inner() -> ThermalReading {
    // Create the WBEM locator.
    let mut loc: PVOID = null_mut();
    // SAFETY: standard CoCreateInstance for the WBEM locator.
    let hr = unsafe {
        CoCreateInstance(
            &CLSID_WBEM_LOCATOR,
            null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_IWBEM_LOCATOR,
            &mut loc,
        )
    };
    if hr != S_OK || loc.is_null() {
        return unavailable("thermal unavailable (WMI locator unavailable)");
    }
    let _loc_guard = ComPtr(loc);

    // Connect to root\WMI.
    let ns = SysString::new("ROOT\\WMI");
    let mut svc: PVOID = null_mut();
    // SAFETY: loc valid; ns is a live BSTR; other args are the documented NULLs.
    let hr = unsafe {
        let v = &**(loc as *const *const IWbemLocatorVtbl);
        (v.ConnectServer)(
            loc,
            ns.0,
            null_mut(),
            null_mut(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            &mut svc,
        )
    };
    if hr != S_OK || svc.is_null() {
        return unavailable("thermal unavailable (root\\WMI not reachable)");
    }
    let _svc_guard = ComPtr(svc);

    // Set the proxy blanket so the query is allowed to run.
    // SAFETY: svc is the IWbemServices proxy; documented blanket parameters.
    unsafe {
        CoSetProxyBlanket(
            svc,
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            null_mut(),
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            null_mut(),
            EOAC_NONE,
        );
    }

    // ExecQuery for the thermal-zone temperatures.
    let lang = SysString::new("WQL");
    let query = SysString::new("SELECT * FROM MSAcpi_ThermalZoneTemperature");
    let mut enumerator: PVOID = null_mut();
    // SAFETY: svc valid; lang/query live BSTRs; out-param live.
    let hr = unsafe {
        let v = &**(svc as *const *const IWbemServicesVtbl);
        (v.ExecQuery)(
            svc,
            lang.0,
            query.0,
            WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
            null_mut(),
            &mut enumerator,
        )
    };
    if hr != S_OK || enumerator.is_null() {
        return unavailable("no thermal sensors exposed");
    }
    let _enum_guard = ComPtr(enumerator);

    let mut sensors = Vec::new();
    loop {
        let mut obj: PVOID = null_mut();
        let mut returned: u32 = 0;
        // SAFETY: enumerator valid; out-params live; one object per iteration.
        let hr = unsafe {
            let v = &**(enumerator as *const *const IEnumWbemClassObjectVtbl);
            (v.Next)(enumerator, WBEM_INFINITE, 1, &mut obj, &mut returned)
        };
        if hr != S_OK || returned == 0 || obj.is_null() {
            break;
        }
        let obj = ComPtr(obj);
        if let Some(sensor) = read_thermal_object(obj.0) {
            sensors.push(sensor);
        }
    }

    if sensors.is_empty() {
        return unavailable("no thermal sensors exposed");
    }
    ThermalReading {
        available: true,
        unavailable_reason: String::new(),
        sensors,
    }
}

/// Reads `CurrentTemperature` (tenths of a Kelvin) and `InstanceName` from one
/// `MSAcpi_ThermalZoneTemperature` object into a [`ThermalSensor`].
fn read_thermal_object(obj: PVOID) -> Option<ThermalSensor> {
    let temp_raw = get_u32_prop(obj, "CurrentTemperature")?;
    // Tenths of Kelvin → Celsius. Guard against a bogus 0 reading.
    if temp_raw == 0 {
        return None;
    }
    let celsius = temp_raw as f64 / 10.0 - 273.15;
    // Reject physically impossible values (bad/idle sensors sometimes report).
    if !(-40.0..=150.0).contains(&celsius) {
        return None;
    }
    let name = get_str_prop(obj, "InstanceName").unwrap_or_else(|| "Thermal zone".to_string());
    Some(ThermalSensor {
        name,
        celsius: (celsius * 10.0).round() / 10.0,
        source: "ACPI thermal zone (WMI)".to_string(),
    })
}

/// Reads an integer WMI property via `IWbemClassObject::Get`.
fn get_u32_prop(obj: PVOID, name: &str) -> Option<u32> {
    let wname: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut var = VARIANT::empty();
    // SAFETY: obj valid; wname NUL-terminated; var is a live VARIANT out-param.
    let hr = unsafe {
        let v = &**(obj as *const *const IWbemClassObjectVtbl);
        (v.Get)(obj, wname.as_ptr(), 0, &mut var, null_mut(), null_mut())
    };
    let result = if hr == S_OK {
        // CurrentTemperature comes back as an integer VARIANT; the value sits in
        // the low 32 bits of the union regardless of VT_I4/VT_UI4.
        Some(var.val as u32)
    } else {
        None
    };
    // SAFETY: clear any owned resources in the VARIANT.
    unsafe { VariantClear(&mut var) };
    result
}

/// Reads a string WMI property via `IWbemClassObject::Get`.
fn get_str_prop(obj: PVOID, name: &str) -> Option<String> {
    let wname: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut var = VARIANT::empty();
    // SAFETY: obj valid; wname NUL-terminated; var live.
    let hr = unsafe {
        let v = &**(obj as *const *const IWbemClassObjectVtbl);
        (v.Get)(obj, wname.as_ptr(), 0, &mut var, null_mut(), null_mut())
    };
    let result = if hr == S_OK && var.vt != VT_I4 && var.val != 0 {
        // A VT_BSTR: the union holds the BSTR pointer.
        let bstr = var.val as usize as *const u16;
        // SAFETY: bstr is a valid NUL-terminated BSTR while `var` is uncleared.
        Some(unsafe { wide_ptr_to_string(bstr) })
    } else {
        None
    };
    // SAFETY: frees the BSTR the VARIANT owns.
    unsafe { VariantClear(&mut var) };
    result
}

/// Reads a NUL-terminated UTF-16 string from a raw pointer (bounded).
///
/// # Safety
/// `ptr` must be null or a NUL-terminated UTF-16 string.
unsafe fn wide_ptr_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while len < 4096 && *ptr.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
}

/// Builds an unavailable [`ThermalReading`].
fn unavailable(reason: &str) -> ThermalReading {
    ThermalReading {
        available: false,
        unavailable_reason: reason.to_string(),
        sensors: Vec::new(),
    }
}

/// An owned COM interface pointer, `Release`d on drop.
struct ComPtr(PVOID);
impl Drop for ComPtr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: self.0 is a live COM interface; Release once.
            unsafe {
                let v = *(self.0 as *const *const IUnknownVtbl);
                ((*v).Release)(self.0);
            }
        }
    }
}

/// An owned `BSTR`, freed on drop.
struct SysString(crate::ffi::BSTR);
impl SysString {
    fn new(s: &str) -> Self {
        let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: wide is NUL-terminated UTF-16.
        SysString(unsafe { SysAllocString(wide.as_ptr()) })
    }
}
impl Drop for SysString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: from SysAllocString; free once.
            unsafe { SysFreeString(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_bytes_stops_at_nul() {
        // "\\.\A\0" padded — decode stops at the NUL.
        let s = "AB";
        let mut bytes: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        bytes.extend_from_slice(&[0, 0, 0x43, 0x00]); // NUL then 'C' (ignored)
        assert_eq!(wide_from_bytes(&bytes), "AB");
    }

    #[test]
    fn health_percent_from_capacities() {
        let mut r = BatteryReading {
            design_mwh: 50_000,
            full_charge_mwh: 40_000,
            ..Default::default()
        };
        // Recompute the way refine_from_device does.
        if r.design_mwh > 0 && r.full_charge_mwh > 0 {
            r.health_percent = ((r.full_charge_mwh * 100) / r.design_mwh) as u32;
        }
        assert_eq!(r.health_percent, 80);
    }

    #[test]
    fn kelvin_tenths_to_celsius() {
        // 3131 tenths-K = 313.1 K = 39.95 °C → rounded to 40.0 (1 dp).
        let raw = 3131u32;
        let c = raw as f64 / 10.0 - 273.15;
        assert!((c - 39.95).abs() < 0.001);
        let rounded = (c * 10.0).round() / 10.0;
        assert_eq!(rounded, 40.0);
    }
}
