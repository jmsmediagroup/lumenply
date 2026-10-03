//! How much memory the machine can give a model run, and how much this
//! process has used: the guard that refuses a run which would not fit
//! (each model file's measured peak is in the registry), and the numbers
//! behind those measurements.

use crate::{AiError, Result};

/// Bytes the system can hand out now without memory pressure, as the OS
/// itself estimates it; `None` where unknown.
///
/// - macOS: `kern.memorystatus_level` (the share of memory the kernel
///   counts as available, compressible memory included: the figure its
///   memory-pressure states use) of `hw.memsize`;
/// - Linux: `MemAvailable` from `/proc/meminfo`;
/// - Windows: `GlobalMemoryStatusEx`'s available physical memory.
pub fn available() -> Option<u64> {
    imp::available()
}

/// This process's peak resident memory so far, in bytes (macOS and Linux).
pub fn peak_resident() -> Option<u64> {
    imp::peak_resident()
}

/// Gigabytes with one decimal, for messages.
pub(crate) fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

/// `Ok` if a run of `model` that needs `needed` bytes on top of what is
/// loaded fits in `available` (unknown availability passes).
pub(crate) fn check(model: &str, needed: u64, available: Option<u64>) -> Result<()> {
    match available {
        Some(free) if needed > free => Err(AiError::OutOfMemory {
            model: model.to_string(),
            needed,
            available: free,
        }),
        _ => Ok(()),
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::CStr;

    fn sysctl<T: Copy + Default>(name: &CStr) -> Option<T> {
        let mut value = T::default();
        let mut len = std::mem::size_of::<T>();
        // SAFETY: `value` is a plain integer of `len` bytes and lives for
        // the call; a name the kernel doesn't know, or a size mismatch,
        // returns an error code instead of writing.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&mut value as *mut T).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (rc == 0 && len == std::mem::size_of::<T>()).then_some(value)
    }

    pub fn available() -> Option<u64> {
        let total: u64 = sysctl(c"hw.memsize")?;
        let level: u32 = sysctl(c"kern.memorystatus_level")?;
        Some(total / 100 * level.min(100) as u64)
    }

    pub fn peak_resident() -> Option<u64> {
        super::max_rss().map(|b| b as u64) // bytes on macOS
    }
}

#[cfg(target_os = "linux")]
mod imp {
    pub fn available() -> Option<u64> {
        let info = std::fs::read_to_string("/proc/meminfo").ok()?;
        let line = info.lines().find(|l| l.starts_with("MemAvailable:"))?;
        let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kib * 1024)
    }

    pub fn peak_resident() -> Option<u64> {
        super::max_rss().map(|kib| kib as u64 * 1024) // KiB on Linux
    }
}

#[cfg(windows)]
mod imp {
    /// `MEMORYSTATUSEX`.
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }

    pub fn available() -> Option<u64> {
        let mut status = MemoryStatusEx {
            length: std::mem::size_of::<MemoryStatusEx>() as u32,
            memory_load: 0,
            total_phys: 0,
            avail_phys: 0,
            total_page_file: 0,
            avail_page_file: 0,
            total_virtual: 0,
            avail_virtual: 0,
            avail_extended_virtual: 0,
        };
        // SAFETY: a correctly sized MEMORYSTATUSEX with its length set.
        let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
        (ok != 0).then_some(status.avail_phys)
    }

    pub fn peak_resident() -> Option<u64> {
        None
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod imp {
    pub fn available() -> Option<u64> {
        None
    }

    pub fn peak_resident() -> Option<u64> {
        None
    }
}

/// `getrusage`'s peak resident size, in the platform's unit.
#[cfg(unix)]
fn max_rss() -> Option<libc::c_long> {
    // SAFETY: getrusage fills the zeroed struct it is given.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    (rc == 0).then_some(usage.ru_maxrss)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_that_does_not_fit_is_refused_with_the_numbers() {
        assert!(check("BiRefNet", 9_000_000_000, Some(9_000_000_000)).is_ok());
        assert!(check("BiRefNet", 9_000_000_000, None).is_ok(), "unknown passes");
        let err = check("BiRefNet (high detail)", 9_600_000_000, Some(4_200_000_000)).unwrap_err();
        match &err {
            AiError::OutOfMemory {
                model,
                needed,
                available,
            } => {
                assert_eq!(model, "BiRefNet (high detail)");
                assert_eq!((*needed, *available), (9_600_000_000, 4_200_000_000));
            }
            e => panic!("{e}"),
        }
        assert_eq!(
            err.to_string(),
            "BiRefNet (high detail) needs about 9.6 GB of free memory for a run; 4.2 GB is available"
        );
    }

    #[test]
    fn this_machine_reports_plausible_figures() {
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            let free = available().expect("available memory");
            assert!(free > 100_000_000 && free < 1 << 50, "{free}");
            let peak = peak_resident().expect("peak resident");
            assert!(peak > 1_000_000 && peak < 1 << 50, "{peak}");
        }
    }
}
