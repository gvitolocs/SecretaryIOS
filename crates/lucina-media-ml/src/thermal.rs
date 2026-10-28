//! nezopt's CPU temperature (AMD k10temp `Tctl`), so background embedding pauses when it runs hot.
//! The owner's limit is 90 °C.

use std::path::Path;

pub const DEFAULT_LIMIT_C: f32 = 90.0;
/// Work resumes this far below the limit, so it does not flap around it.
pub const HYSTERESIS_C: f32 = 3.0;

/// The hottest `k10temp` reading under `/sys/class/hwmon`, in °C; `None` without the sensor.
pub fn cpu_temp_c() -> Option<f32> {
    cpu_temp_in(Path::new("/sys/class/hwmon"))
}

/// [`cpu_temp_c`] for a given hwmon root (tests).
pub fn cpu_temp_in(root: &Path) -> Option<f32> {
    let mut best: Option<f32> = None;
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let dir = e.path();
        let name = std::fs::read_to_string(dir.join("name")).unwrap_or_default();
        if name.trim() != "k10temp" {
            continue;
        }
        // temp1 is Tctl on k10temp; the CCD sensors (temp3…) are never hotter for this purpose.
        if let Some(t) = std::fs::read_to_string(dir.join("temp1_input"))
            .ok()
            .and_then(|s| s.trim().parse::<f32>().ok())
        {
            let c = t / 1000.0;
            best = Some(best.map_or(c, |b: f32| b.max(c)));
        }
    }
    best
}

/// The 1-minute load average above which background work waits (the Drive worker's rule).
pub const DEFAULT_MAX_LOAD: f32 = 16.0;

/// The 1-minute load average (`/proc/loadavg`); `None` where there is none.
pub fn load_avg() -> Option<f32> {
    parse_loadavg(&std::fs::read_to_string("/proc/loadavg").ok()?)
}

pub fn parse_loadavg(s: &str) -> Option<f32> {
    s.split_whitespace().next()?.parse().ok()
}

/// Whether to stay paused: at or above `limit` start pausing, and keep pausing until
/// `HYSTERESIS_C` below it. An unknown temperature never pauses.
pub fn should_pause(temp: Option<f32>, limit: f32, paused: bool) -> bool {
    match temp {
        None => false,
        Some(t) if paused => t > limit - HYSTERESIS_C,
        Some(t) => t >= limit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_k10temp_only() {
        let dir = tempfile::tempdir().unwrap();
        let mk = |n: &str, name: &str, milli: &str| {
            let d = dir.path().join(n);
            std::fs::create_dir(&d).unwrap();
            std::fs::write(d.join("name"), format!("{name}\n")).unwrap();
            std::fs::write(d.join("temp1_input"), format!("{milli}\n")).unwrap();
        };
        mk("hwmon0", "amdgpu", "99000");
        mk("hwmon1", "k10temp", "88500");
        assert_eq!(cpu_temp_in(dir.path()), Some(88.5));
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(cpu_temp_in(empty.path()), None);
    }

    #[test]
    fn reads_the_one_minute_load() {
        assert_eq!(
            parse_loadavg("17.25 12.00 9.50 3/2000 12345\n"),
            Some(17.25)
        );
        assert_eq!(parse_loadavg(""), None);
    }

    #[test]
    fn pauses_at_the_limit_and_resumes_below_the_hysteresis() {
        assert!(!should_pause(None, 90.0, false));
        assert!(!should_pause(Some(89.9), 90.0, false));
        assert!(should_pause(Some(90.0), 90.0, false));
        assert!(should_pause(Some(88.0), 90.0, true));
        assert!(!should_pause(Some(87.0), 90.0, true));
    }
}
