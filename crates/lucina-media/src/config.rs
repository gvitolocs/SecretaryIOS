use std::net::SocketAddr;
use std::path::PathBuf;

use chrono::{DateTime, Timelike, Utc};
use chrono_tz::Tz;

/// Runtime configuration, read from the environment (`LUCINA_MEDIA_*`).
#[derive(Clone, Debug)]
pub struct Config {
    /// Tailnet listener for the proxied `/v1/media` API.
    pub listen: SocketAddr,
    /// Loopback listener for `/internal` (memo registration, imports).
    pub internal_listen: SocketAddr,
    /// NVMe: catalog.db, thumbs/, spool/, uploads/.
    pub data_dir: PathBuf,
    /// Big disk: content-addressed originals.
    pub store_dir: PathBuf,
    /// Shared secret the p1 proxy sends as `X-Lucina-Upstream-Key`. `None` only in tests.
    pub upstream_key: Option<String>,
    /// Quiet hours for the big disk, local time `[quiet_start, quiet_end)`.
    pub quiet_start: u32,
    pub quiet_end: u32,
    pub tz: Tz,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let home = PathBuf::from(var("HOME").unwrap_or_else(|| "/home/nez".into()));
        let upstream_key = match var("LUCINA_MEDIA_UPSTREAM_KEY_FILE") {
            Some(path) => Some(std::fs::read_to_string(&path)?.trim().to_string()),
            None => var("LUCINA_MEDIA_UPSTREAM_KEY"),
        };
        if upstream_key.as_deref().is_some_and(|k| k.len() < 32) {
            anyhow::bail!("upstream key must be at least 32 characters");
        }
        Ok(Self {
            listen: var("LUCINA_MEDIA_LISTEN").unwrap_or_else(|| "127.0.0.1:8830".into()).parse()?,
            internal_listen: var("LUCINA_MEDIA_INTERNAL_LISTEN")
                .unwrap_or_else(|| "127.0.0.1:8830".into())
                .parse()?,
            data_dir: var("LUCINA_MEDIA_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| home.join("data/lucina-media")),
            store_dir: var("LUCINA_MEDIA_STORE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("mnt/mybook/lucina-media")),
            upstream_key,
            quiet_start: var("LUCINA_MEDIA_QUIET_START").map(|v| v.parse()).transpose()?.unwrap_or(22),
            quiet_end: var("LUCINA_MEDIA_QUIET_END").map(|v| v.parse()).transpose()?.unwrap_or(9),
            tz: var("LUCINA_MEDIA_TZ").unwrap_or_else(|| "Europe/Copenhagen".into()).parse().map_err(anyhow::Error::msg)?,
            ffmpeg: var("LUCINA_MEDIA_FFMPEG").unwrap_or_else(|| "ffmpeg".into()).into(),
            ffprobe: var("LUCINA_MEDIA_FFPROBE").unwrap_or_else(|| "ffprobe".into()).into(),
        })
    }

    /// A config rooted in one directory, for tests and one-off CLI runs.
    pub fn for_dirs(data_dir: PathBuf, store_dir: PathBuf) -> Self {
        Self {
            listen: "127.0.0.1:0".parse().unwrap(),
            internal_listen: "127.0.0.1:0".parse().unwrap(),
            data_dir,
            store_dir,
            upstream_key: None,
            quiet_start: 22,
            quiet_end: 9,
            tz: chrono_tz::Europe::Copenhagen,
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("catalog.db")
    }
    pub fn spool_dir(&self) -> PathBuf {
        self.data_dir.join("spool")
    }
    pub fn uploads_dir(&self) -> PathBuf {
        self.data_dir.join("uploads")
    }
    pub fn thumbs_dir(&self) -> PathBuf {
        self.data_dir.join("thumbs")
    }

    /// Whether the big disk should be left alone at `now`.
    pub fn is_quiet(&self, now: DateTime<Utc>) -> bool {
        is_quiet_hour(now.with_timezone(&self.tz).hour(), self.quiet_start, self.quiet_end)
    }
}

/// `[start, end)` in hours, wrapping past midnight when `start > end`. `start == end` means never quiet.
pub fn is_quiet_hour(hour: u32, start: u32, end: u32) -> bool {
    if start == end {
        false
    } else if start < end {
        (start..end).contains(&hour)
    } else {
        hour >= start || hour < end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_window_wraps_midnight() {
        assert!(is_quiet_hour(22, 22, 9));
        assert!(is_quiet_hour(3, 22, 9));
        assert!(!is_quiet_hour(9, 22, 9));
        assert!(!is_quiet_hour(15, 22, 9));
        assert!(is_quiet_hour(13, 12, 14));
        assert!(!is_quiet_hour(14, 12, 14));
        assert!(!is_quiet_hour(5, 7, 7));
    }
}
