//! What this machine can hold, read once and shared by every size limit.
//!
//! Compositor 1.2.8 raised its limits to one image of 200 megapixels and a whole
//! document of 800, then scaled them to the machine's memory, because one fixed
//! number cannot be right for a small laptop and a workstation alike. The
//! absolute ceilings stay with the limits themselves; this module reports the
//! share of the machine's memory that one image, one document, or one heavy
//! operation may take.
//!
//! The total is read once and never re-read. Available memory moves with every
//! other program on the machine, and a limit that moved with it would refuse
//! work that was fine a minute ago, so the total is the stable half of the
//! answer and every budget is a share of it.

use std::sync::OnceLock;

/// Bytes one pixel of an RGBA8 raster takes. Every budget is measured against
/// this even where the raster itself is smaller, such as a mask, so that one
/// number describes the worst case.
pub const BYTES_PER_PIXEL: u64 = 4;

/// Assumed memory when the machine will not say. Two gibibytes is the smallest
/// assumption that still allows every image the port accepted before, so a
/// machine that cannot be measured never gets a tighter limit than it had.
const FALLBACK_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// The machine's memory in bytes, as the smaller of physical memory and any
/// container limit.
pub fn total_bytes() -> u64 {
    static TOTAL: OnceLock<u64> = OnceLock::new();
    *TOTAL.get_or_init(|| {
        let physical = physical_bytes().unwrap_or(FALLBACK_BYTES);
        container_limit().map_or(physical, |limit| physical.min(limit))
    })
}

/// `quarters` of the machine's memory in bytes: one is a quarter, four is all of
/// it. Saturated, because the multiplication is only a guard against overflow on
/// a machine that reports an absurd total.
pub fn share(quarters: u64) -> u64 {
    bytes_for(total_bytes(), quarters)
}

/// `quarters` of the machine's memory as a count of RGBA8 pixels.
pub fn pixels_within(quarters: u64) -> u64 {
    pixels_for(total_bytes(), quarters)
}

/// The byte share of a given amount of memory, which takes a machine other than
/// this one so that a limit can be tested without one.
pub fn bytes_for(total: u64, quarters: u64) -> u64 {
    total.saturating_mul(quarters) / 4
}

/// A memory amount the way the rest of the port writes one, for a message that
/// has to name a budget in bytes.
pub fn gibibytes(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else {
        format!("{} MiB", bytes / (1024 * 1024))
    }
}

/// The pixel share of a given amount of memory, for the same reason as
/// [`bytes_for`].
pub fn pixels_for(total: u64, quarters: u64) -> u64 {
    bytes_for(total, quarters) / BYTES_PER_PIXEL
}

/// Physical memory, which the kernel reports in kibibytes.
fn physical_bytes() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_meminfo(&meminfo)
}

/// Read `MemTotal` out of the contents of `/proc/meminfo`.
fn parse_meminfo(text: &str) -> Option<u64> {
    let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
    let kibibytes: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kibibytes.checked_mul(1024)
}

/// The memory a container is allowed, when there is one. A container reports the
/// machine's memory as its own, so without this a 64 GiB host would hand a
/// 2 GiB container a budget it cannot honour. cgroup v2 is asked first because a
/// system can carry both versions.
fn container_limit() -> Option<u64> {
    read_limit("/sys/fs/cgroup/memory.max")
        .or_else(|| read_limit("/sys/fs/cgroup/memory/memory.limit_in_bytes"))
}

/// Read one cgroup memory limit, where `max` and an unreadable value both mean
/// the machine's own memory applies.
fn read_limit(path: &str) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    let value = text.trim();
    if value == "max" {
        return None;
    }
    value.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EIGHT_GIB: u64 = 8 * 1024 * 1024 * 1024;

    #[test]
    fn reads_physical_memory_from_meminfo() {
        let text =
            "MemTotal:       16384160 kB\nMemFree:         1234 kB\nMemAvailable:    9000 kB\n";
        assert_eq!(parse_meminfo(text), Some(16_384_160 * 1024));
    }

    #[test]
    fn a_meminfo_without_a_total_is_no_answer() {
        assert_eq!(parse_meminfo("SwapTotal: 0 kB\n"), None);
        assert_eq!(parse_meminfo(""), None);
        assert_eq!(parse_meminfo("MemTotal:       not-a-number kB\n"), None);
        assert_eq!(parse_meminfo("MemTotal:\n"), None);
    }

    #[test]
    fn shares_are_quarters_of_the_memory() {
        assert_eq!(bytes_for(EIGHT_GIB, 0), 0);
        assert_eq!(bytes_for(EIGHT_GIB, 1), EIGHT_GIB / 4);
        assert_eq!(bytes_for(EIGHT_GIB, 2), EIGHT_GIB / 2);
        assert_eq!(bytes_for(EIGHT_GIB, 4), EIGHT_GIB);
        assert_eq!(bytes_for(u64::MAX, 4), u64::MAX / 4);
    }

    #[test]
    fn pixels_are_measured_in_rgba_bytes() {
        // A quarter of 8 GiB is 2 GiB, which is 512 Mi pixels.
        assert_eq!(pixels_for(EIGHT_GIB, 1), 536_870_912);
        assert_eq!(pixels_for(EIGHT_GIB, 3), 3 * 536_870_912);
        assert_eq!(pixels_for(FALLBACK_BYTES, 1), 134_217_728);
        assert_eq!(pixels_for(FALLBACK_BYTES, 3), 402_653_184);
    }

    #[test]
    fn the_fallback_keeps_the_limits_the_port_accepted_before() {
        // 100 megapixels was the fixed cap, so every image under it must still
        // pass on a machine that will not report its memory.
        assert!(pixels_for(FALLBACK_BYTES, 1) > 100_000_000);
    }

    #[test]
    fn amounts_are_written_the_way_the_port_writes_them() {
        assert_eq!(gibibytes(512 * 1024 * 1024), "512 MiB");
        assert_eq!(gibibytes(2 * 1024 * 1024 * 1024), "2.0 GiB");
        assert_eq!(
            gibibytes(16 * 1024 * 1024 * 1024 + 512 * 1024 * 1024),
            "16.5 GiB"
        );
    }

    #[test]
    fn this_machine_reports_a_plausible_amount_of_memory() {
        let total = total_bytes();
        assert!(total >= FALLBACK_BYTES / 2, "reported {total} bytes");
        assert_eq!(share(4), total);
        assert_eq!(pixels_within(1), pixels_for(total, 1));
    }
}
