//! Windows: CPU package power from the Energy Meter performance counters, the elevation check,
//! and detaching the collector from its console window.
//!
//! Untested on real hardware. Windows 11 exposes the CPU's RAPL counters as the `Energy Meter`
//! counter set (instances such as `rapl_package0_pkg`, in milliwatts) without a driver. Older
//! versions and some CPUs do not, and then the CPU has no sensor. NVIDIA GPUs work through NVML
//! as on Linux.

use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
use windows_sys::Win32::System::Performance::{
    PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
    PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
    PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::sensors::{PlatformSensor, Reading, Sensor, SourceInfo, SourceKind, cpu_label};

const ENERGY_METER: &str = "\\Energy Meter(*)\\Power";

pub fn probe(sensors: &mut Vec<Sensor>, notes: &mut Vec<String>) {
    match EnergyMeter::open() {
        Some(m) => sensors.push(Sensor::Platform(Box::new(m))),
        None => notes.push(
            "Windows shows no CPU energy meter on this machine (it needs Windows 11 and a \
             supported CPU); the CPU is covered by the base load estimate"
                .into(),
        ),
    }
}

/// The CPU package power from PDH.
struct EnergyMeter {
    info: SourceInfo,
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
}

impl EnergyMeter {
    fn open() -> Option<Self> {
        let path = wide(ENERGY_METER);
        let mut query: PDH_HQUERY = ptr::null_mut();
        let mut counter: PDH_HCOUNTER = ptr::null_mut();
        // SAFETY: out pointers are valid locals, the path is NUL terminated.
        unsafe {
            if PdhOpenQueryW(ptr::null(), 0, &mut query) != 0 {
                return None;
            }
            if PdhAddEnglishCounterW(query, path.as_ptr(), 0, &mut counter) != 0 {
                PdhCloseQuery(query);
                return None;
            }
            PdhCollectQueryData(query);
        }
        let mut m = Self {
            info: SourceInfo {
                key: "cpu".into(),
                label: cpu_label("Energy Meter"),
                kind: SourceKind::Cpu,
            },
            query,
            counter,
        };
        m.package_mw().map(|_| m)
    }

    /// Sum of the package instances, in milliwatts. `None` when there are none.
    fn package_mw(&mut self) -> Option<f64> {
        // SAFETY: the query and counter handles stay valid until Drop; the buffer is sized by
        // the first call and only read up to the item count PDH reports.
        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return None;
            }
            let mut bytes = 0u32;
            let mut count = 0u32;
            let first = PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut bytes,
                &mut count,
                ptr::null_mut(),
            );
            if first != PDH_MORE_DATA || bytes == 0 {
                return None;
            }
            let item = size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            let mut buf = vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); bytes as usize / item + 1];
            if PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut bytes,
                &mut count,
                buf.as_mut_ptr(),
            ) != 0
            {
                return None;
            }
            let mut total = 0.0;
            let mut any = false;
            for it in buf.iter().take(count as usize) {
                if it.FmtValue.CStatus != PDH_CSTATUS_VALID_DATA {
                    continue;
                }
                let name = from_wide(it.szName).to_ascii_lowercase();
                if is_package(&name) {
                    total += it.FmtValue.Anonymous.doubleValue;
                    any = true;
                }
            }
            any.then_some(total)
        }
    }
}

impl PlatformSensor for EnergyMeter {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn read(&mut self, dt: f64) -> Option<Reading> {
        let mw = self.package_mw()?;
        Some(Reading {
            joules: mw / 1000.0 * dt,
        })
    }
}

impl Drop for EnergyMeter {
    fn drop(&mut self) {
        // SAFETY: the query was opened in `open` and is closed once.
        unsafe {
            PdhCloseQuery(self.query);
        }
    }
}

/// `rapl_package0_pkg` counts; the core (`pp0`), graphics (`pp1`) and memory parts are inside
/// the package or not CPU.
fn is_package(instance: &str) -> bool {
    instance.ends_with("_pkg")
}

/// True when this process runs elevated (as an administrator or SYSTEM).
pub fn is_elevated() -> bool {
    // SAFETY: the token handle is checked and closed; the output struct has the right size.
    unsafe {
        let mut token: HANDLE = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// Closes the console window when the collector owns it alone, as when the scheduled task starts
/// it at logon. Started from a terminal, the terminal shares the console and it stays.
pub fn detach_console() {
    // SAFETY: plain calls without pointers we keep.
    unsafe {
        let mut pids = [0u32; 2];
        if GetConsoleProcessList(pids.as_mut_ptr(), 2) == 1 {
            FreeConsole();
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// # Safety
/// `p` must be null or point at a NUL terminated UTF-16 string.
unsafe fn from_wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0;
    // SAFETY: the caller guarantees a terminating NUL.
    unsafe {
        while *p.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_only_package_instances() {
        assert!(is_package("rapl_package0_pkg"));
        assert!(!is_package("rapl_package0_pp0"));
        assert!(!is_package("rapl_package0_dram"));
    }
}
