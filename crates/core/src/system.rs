//! Cheap system metrics: syscalls only, no external processes.

use std::ffi::CString;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pressure {
    Normal,
    Warning,
    Critical,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStatus {
    pub timestamp: u64,
    pub disk_total: u64,
    /// Space available to the user (not counting purgeable).
    pub disk_available: u64,
    pub mem_total: u64,
    /// App + wired + compressed (equivalent to Activity Monitor's "Memory Used").
    pub mem_used: u64,
    pub mem_app: u64,
    pub mem_wired: u64,
    pub mem_compressed: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub pressure: Pressure,
    pub load: [f64; 3],
    pub cpu_count: u32,
    /// Cumulative CPU ticks of all cores: user, system, idle, nice. The usage between two
    /// readings is the share of non-idle ticks in their difference.
    pub cpu_ticks: [u64; 4],
}

fn sysctl<T: Copy>(name: &str) -> Option<T> {
    let cname = CString::new(name).ok()?;
    let mut value: T = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<T>();
    let rc = unsafe {
        libc::sysctlbyname(cname.as_ptr(), (&mut value as *mut T).cast(), &mut len, std::ptr::null_mut(), 0)
    };
    (rc == 0 && len == std::mem::size_of::<T>()).then_some(value)
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XswUsage {
    total: u64,
    avail: u64,
    used: u64,
    pagesize: u32,
    encrypted: u8,
}

#[repr(C, align(8))]
#[derive(Default)]
struct VmStatistics64 {
    free_count: u32,
    active_count: u32,
    inactive_count: u32,
    wire_count: u32,
    zero_fill_count: u64,
    reactivations: u64,
    pageins: u64,
    pageouts: u64,
    faults: u64,
    cow_faults: u64,
    lookups: u64,
    hits: u64,
    purges: u64,
    purgeable_count: u32,
    speculative_count: u32,
    decompressions: u64,
    compressions: u64,
    swapins: u64,
    swapouts: u64,
    compressor_page_count: u32,
    throttled_count: u32,
    external_page_count: u32,
    internal_page_count: u32,
    total_uncompressed_pages_in_compressor: u64,
}

const HOST_VM_INFO64: i32 = 4;
const HOST_CPU_LOAD_INFO: i32 = 3;

unsafe extern "C" {
    fn mach_host_self() -> u32;
    fn host_statistics(host: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
    fn host_statistics64(host: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
}

fn host_port() -> u32 {
    static PORT: OnceLock<u32> = OnceLock::new();
    *PORT.get_or_init(|| unsafe { mach_host_self() })
}

fn vm_stats() -> Option<VmStatistics64> {
    let mut stats = VmStatistics64::default();
    let mut count = (std::mem::size_of::<VmStatistics64>() / std::mem::size_of::<i32>()) as u32;
    let rc = unsafe { host_statistics64(host_port(), HOST_VM_INFO64, (&mut stats as *mut VmStatistics64).cast(), &mut count) };
    (rc == 0).then_some(stats)
}

fn cpu_ticks() -> [u64; 4] {
    let mut ticks = [0u32; 4];
    let mut count = ticks.len() as u32;
    let rc = unsafe { host_statistics(host_port(), HOST_CPU_LOAD_INFO, ticks.as_mut_ptr().cast(), &mut count) };
    if rc == 0 { ticks.map(u64::from) } else { [0; 4] }
}

fn disk(path: &str) -> (u64, u64) {
    let Ok(c) = CString::new(path) else { return (0, 0) };
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return (0, 0);
    }
    let bs = st.f_bsize as u64;
    (st.f_blocks * bs, st.f_bavail * bs)
}

/// System memory pressure (one `sysctl` call).
///
/// `MACCLEANER_SIMULATED_PRESSURE=normal|warning|critical` pins it to test the governor
/// without having to exhaust real memory.
pub fn pressure() -> Pressure {
    static SIMULATED: OnceLock<Option<Pressure>> = OnceLock::new();
    let simulated = SIMULATED.get_or_init(|| match std::env::var("MACCLEANER_SIMULATED_PRESSURE").ok()?.as_str() {
        "normal" => Some(Pressure::Normal),
        "warning" => Some(Pressure::Warning),
        "critical" => Some(Pressure::Critical),
        _ => None,
    });
    if let Some(p) = simulated {
        return *p;
    }
    match sysctl::<i32>("kern.memorystatus_vm_pressure_level") {
        Some(1) => Pressure::Normal,
        Some(2) => Pressure::Warning,
        Some(4) => Pressure::Critical,
        _ => Pressure::Unknown,
    }
}

pub fn status() -> SystemStatus {
    let (disk_total, disk_available) = disk("/System/Volumes/Data");
    let (disk_total, disk_available) = if disk_total == 0 { disk("/") } else { (disk_total, disk_available) };

    let page = sysctl::<u32>("vm.pagesize").unwrap_or(16384) as u64;
    let (mem_app, mem_wired, mem_compressed) = vm_stats()
        .map(|v| {
            (
                (v.internal_page_count as u64).saturating_sub(v.purgeable_count as u64) * page,
                v.wire_count as u64 * page,
                v.compressor_page_count as u64 * page,
            )
        })
        .unwrap_or((0, 0, 0));
    let swap = sysctl::<XswUsage>("vm.swapusage");
    let pressure = pressure();
    let mut load = [0f64; 3];
    unsafe { libc::getloadavg(load.as_mut_ptr(), 3) };

    SystemStatus {
        timestamp: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        disk_total,
        disk_available,
        mem_total: sysctl::<u64>("hw.memsize").unwrap_or(0),
        mem_used: mem_app + mem_wired + mem_compressed,
        mem_app,
        mem_wired,
        mem_compressed,
        swap_used: swap.map(|s| s.used).unwrap_or(0),
        swap_total: swap.map(|s| s.total).unwrap_or(0),
        pressure,
        load,
        cpu_count: sysctl::<i32>("hw.logicalcpu").unwrap_or(0).max(0) as u32,
        cpu_ticks: cpu_ticks(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes_match_kernel() {
        assert_eq!(std::mem::size_of::<VmStatistics64>(), 152);
    }

    #[test]
    fn reads_plausible_values() {
        let s = status();
        assert!(s.mem_total > 1 << 30);
        assert!(s.mem_used > 0 && s.mem_used <= s.mem_total);
        assert!(s.disk_total > 0 && s.disk_available <= s.disk_total);
        assert!(s.cpu_count > 0);
        assert_ne!(s.pressure, Pressure::Unknown);
        assert!(s.cpu_ticks.iter().sum::<u64>() > 0);
    }
}
