//! When background preparation may run. CPU encodes (libx264 and tone-mapping)
//! wait for the night window; VAAPI and remux jobs may start any hour. Nothing
//! starts when the CPU is hot or the box is loaded, and a running job is stopped
//! when it stays over the limit, so the owner's other work never queues behind
//! preparation.
//!
//! The decision itself is a verified kernel (`kernel_shared.rs`); this module
//! only reads the sensors and remembers the three-sample rule.

use chrono::Timelike;
use std::{fs, time::Duration};

/// One look at the box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    /// Local hour, 0–23.
    pub hour: u32,
    /// `k10temp` Tctl in °C, when the sensor exists.
    pub temp_c: Option<f32>,
    /// One-minute load average, when `/proc/loadavg` is readable.
    pub load: Option<f32>,
}

/// The queue's limits, from `StreamConfig`.
#[derive(Debug, Clone)]
pub struct Gate {
    pub window_start: u32,
    pub window_end: u32,
    pub max_temp_c: u32,
    pub max_load: u32,
}

impl From<&crate::StreamConfig> for Gate {
    fn from(cfg: &crate::StreamConfig) -> Self {
        Self::new(
            cfg.prepare_window_start,
            cfg.prepare_window_end,
            cfg.prepare_max_temp_c,
            cfg.prepare_max_load,
        )
    }
}

impl Gate {
    pub fn new(window_start: u32, window_end: u32, max_temp_c: u32, max_load: u32) -> Self {
        let hour = |h: u32| h.min(23);
        Self { window_start: hour(window_start), window_end: hour(window_end), max_temp_c, max_load }
    }

    /// The current local hour, Tctl and load.
    pub fn reading(&self) -> Reading {
        Reading { hour: local_hour(), temp_c: tctl_c(), load: load_1min() }
    }

    /// Why a job of this kind may not start now, if it may not. `cpu_path` is
    /// true for libx264 and tone-mapping; those are the jobs the window holds
    /// back.
    pub fn block_reason(&self, cpu_path: bool) -> Option<String> {
        self.block_reason_at(self.reading(), cpu_path)
    }

    /// [`block_reason`](Self::block_reason) for an already-taken reading.
    pub fn block_reason_at(&self, reading: Reading, cpu_path: bool) -> Option<String> {
        let in_window = crate::kernel::in_window(self.window_start, self.window_end, reading.hour);
        let temp_ok = reading.temp_c.is_none_or(|t| t < self.max_temp_c as f32);
        let load_ok = reading.load.is_none_or(|l| l < self.max_load as f32);
        if crate::kernel::may_start(cpu_path, in_window, temp_ok, load_ok) {
            return None;
        }
        Some(if !temp_ok {
            format!("Tctl {:.0} °C >= {} °C", reading.temp_c.unwrap_or_default(), self.max_temp_c)
        } else if !load_ok {
            format!("load {:.1} >= {}", reading.load.unwrap_or_default(), self.max_load)
        } else {
            format!("outside the night window {:02}:00-{:02}:00", self.window_start, self.window_end)
        })
    }

    /// Whether a running background job is over the stop limit (five degrees
    /// above the start limit, so a short spike does not stop it).
    pub fn over(&self, reading: Reading) -> bool {
        reading.temp_c.is_some_and(|t| t >= self.max_temp_c.saturating_add(5) as f32)
            || reading.load.is_some_and(|l| l >= self.max_load as f32)
    }

    /// The reason logged when the watchdog stops a job.
    pub fn stop_reason(&self, reading: Reading) -> String {
        if let Some(t) = reading.temp_c.filter(|t| *t >= self.max_temp_c.saturating_add(5) as f32) {
            format!("Tctl {t:.0} °C >= {} °C", self.max_temp_c.saturating_add(5))
        } else if let Some(l) = reading.load.filter(|l| *l >= self.max_load as f32) {
            format!("load {l:.1} >= {}", self.max_load)
        } else {
            "over the limit".into()
        }
    }

    /// Logs each missing sensor once, at startup: unknown never blocks.
    pub fn log_missing(&self) {
        let reading = self.reading();
        if reading.temp_c.is_none() {
            tracing::warn!("prepare: no k10temp Tctl sensor; temperature does not pause the queue");
        }
        if reading.load.is_none() {
            tracing::warn!("prepare: /proc/loadavg unreadable; load does not pause the queue");
        }
    }
}

/// Three consecutive over-limit samples stop the job. One normal sample resets
/// the count, so a single spike does not.
#[derive(Debug, Default)]
pub struct Watchdog {
    over: u32,
}

pub const STOP_AFTER_SAMPLES: u32 = 3;

impl Watchdog {
    pub fn sample(&mut self, over: bool) -> bool {
        if over {
            self.over += 1;
        } else {
            self.over = 0;
        }
        self.over >= STOP_AFTER_SAMPLES
    }
}

fn local_hour() -> u32 {
    chrono::Local::now().hour()
}

/// Tctl from the `k10temp` hwmon (`temp1_input`, millidegrees). Any other
/// hwmon is ignored, so a motherboard sensor cannot pause the queue.
fn tctl_c() -> Option<f32> {
    let entries = fs::read_dir("/sys/class/hwmon").ok()?;
    for entry in entries.flatten() {
        let dir = entry.path();
        let Ok(name) = fs::read_to_string(dir.join("name")) else { continue };
        if name.trim() != "k10temp" {
            continue;
        }
        let Ok(milli) = fs::read_to_string(dir.join("temp1_input")) else { continue };
        return milli.trim().parse::<f32>().ok().map(|m| m / 1000.0);
    }
    None
}

fn load_1min() -> Option<f32> {
    fs::read_to_string("/proc/loadavg").ok()?.split_whitespace().next()?.parse().ok()
}

/// The first CPU-path job in `jobs` that may start now: a newer remux or VAAPI
/// job is not held behind a CPU job waiting for the window. Returns the chosen
/// index and, when none may start, the first reason.
pub fn choose<T>(jobs: &[T], gate: &Gate, cpu_path: impl Fn(&T) -> bool) -> (Option<usize>, Option<String>) {
    choose_at(jobs, gate, gate.reading(), cpu_path)
}

/// [`choose`] for an already-taken reading (and for tests).
pub fn choose_at<T>(
    jobs: &[T],
    gate: &Gate,
    reading: Reading,
    cpu_path: impl Fn(&T) -> bool,
) -> (Option<usize>, Option<String>) {
    let mut blocked = None;
    for (i, job) in jobs.iter().enumerate() {
        match gate.block_reason_at(reading, cpu_path(job)) {
            None => return (Some(i), None),
            Some(reason) => {
                if blocked.is_none() {
                    blocked = Some(reason);
                }
            }
        }
    }
    (None, blocked)
}

/// The time the worker sleeps before it looks at the queue again while paused.
pub const PAUSE_RECHECK: Duration = Duration::from_secs(60);

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(hour: u32, temp: Option<f32>, load: Option<f32>) -> Reading {
        Reading { hour, temp_c: temp, load: load }
    }

    #[test]
    fn the_window_includes_its_start_and_excludes_its_end() {
        assert!(crate::kernel::in_window(1, 7, 1));
        assert!(crate::kernel::in_window(1, 7, 6));
        assert!(!crate::kernel::in_window(1, 7, 7));
        assert!(!crate::kernel::in_window(1, 7, 0));
        assert!(!crate::kernel::in_window(1, 7, 13));
    }

    #[test]
    fn the_window_wraps_past_midnight_and_equal_ends_never_open() {
        assert!(crate::kernel::in_window(22, 6, 22));
        assert!(crate::kernel::in_window(22, 6, 23));
        assert!(crate::kernel::in_window(22, 6, 0));
        assert!(crate::kernel::in_window(22, 6, 5));
        assert!(!crate::kernel::in_window(22, 6, 6));
        assert!(!crate::kernel::in_window(22, 6, 12));
        assert!(!crate::kernel::in_window(5, 5, 5));
    }

    #[test]
    fn cpu_jobs_need_the_window_but_vaapi_and_remux_do_not() {
        let gate = Gate::new(1, 7, 80, 16);
        // Noon: a remux may run, a CPU encode waits for 01:00.
        assert!(gate.block_reason_at(reading(12, Some(50.0), Some(1.0)), false).is_none());
        let reason = gate.block_reason_at(reading(12, Some(50.0), Some(1.0)), true).unwrap();
        assert!(reason.contains("night window"), "{reason}");
        // 02:00: both may run.
        assert!(gate.block_reason_at(reading(2, Some(50.0), Some(1.0)), true).is_none());
    }

    #[test]
    fn heat_and_load_block_every_kind_of_job() {
        let gate = Gate::new(1, 7, 80, 16);
        let hot = reading(2, Some(80.0), Some(1.0));
        assert!(gate.block_reason_at(hot, true).unwrap().contains("Tctl"));
        assert!(gate.block_reason_at(hot, false).unwrap().contains("Tctl"));
        let busy = reading(2, Some(50.0), Some(16.0));
        assert!(gate.block_reason_at(busy, true).unwrap().contains("load"));
        assert!(gate.block_reason_at(busy, false).unwrap().contains("load"));
        // Below the limits is fine.
        assert!(gate.block_reason_at(reading(2, Some(79.9), Some(15.9)), true).is_none());
    }

    #[test]
    fn unknown_sensors_do_not_block() {
        let gate = Gate::new(1, 7, 80, 16);
        assert!(gate.block_reason_at(reading(2, None, None), true).is_none());
        // The window still applies when only the sensors are unknown.
        assert!(gate.block_reason_at(reading(12, None, None), true).is_some());
    }

    #[test]
    fn the_watchdog_stops_after_three_samples_in_a_row() {
        let mut watchdog = Watchdog::default();
        assert!(!watchdog.sample(true));
        assert!(!watchdog.sample(true));
        // One normal sample resets the count.
        assert!(!watchdog.sample(false));
        assert!(!watchdog.sample(true));
        assert!(!watchdog.sample(true));
        assert!(watchdog.sample(true));
    }

    #[test]
    fn stop_uses_the_higher_temperature_and_the_same_load() {
        let gate = Gate::new(1, 7, 80, 16);
        assert!(!gate.over(reading(2, Some(84.9), Some(15.9))));
        assert!(gate.over(reading(2, Some(85.0), Some(1.0))));
        assert!(gate.over(reading(2, Some(50.0), Some(16.0))));
        assert!(!gate.over(reading(2, None, None)));
        assert!(gate.stop_reason(reading(2, Some(87.0), None)).contains("85"));
    }

    #[test]
    fn a_remux_is_chosen_before_a_cpu_job_that_must_wait() {
        let gate = Gate::new(1, 7, 80, 16);
        // Pin the reading so the test does not depend on the wall clock.
        let now = reading(12, Some(40.0), Some(0.5));
        let jobs = [true, true, false];
        let (chosen, blocked) = choose_at(&jobs, &gate, now, |cpu| *cpu);
        assert_eq!(chosen, Some(2));
        assert!(blocked.is_none());
        // On its own the CPU job reports why it must wait.
        assert!(gate.block_reason_at(now, true).unwrap().contains("night window"));
    }
}
