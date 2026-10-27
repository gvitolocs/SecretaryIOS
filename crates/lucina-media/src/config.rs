use std::net::SocketAddr;
use std::path::{Path, PathBuf};

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
    /// HMAC key for the player's tokenised `/v1/media/s/…` URLs.
    pub play_secret: Option<Vec<u8>>,
    /// HLS output, probe cache, converted subtitles (NVMe).
    pub stream_cache_dir: PathBuf,
    pub progress_path: PathBuf,
    /// Folders indexed in place, `(album label, path)`.
    pub folders: Vec<(String, PathBuf)>,
    /// Prepare every film's HLS output in the background (`LUCINA_MEDIA_PREPARE=0` turns it off).
    pub prepare: bool,
    /// The folder labels that are films (the Films plugin, `scope=films`), out of the Drive.
    pub film_folders: Vec<String>,
    /// Background jobs run in a `systemd-run --user --scope` CPU quota.
    pub prepare_scope: bool,
    /// That scope's CPU quota in percent (100 = one core).
    pub prepare_cpu_quota: u32,
    /// VAAPI render node for background H.264, when the box has one.
    pub vaapi_device: Option<PathBuf>,
    /// The GPU VRAM semaphore VAAPI jobs run under.
    pub gpu_lease: Option<PathBuf>,
    /// `[start, end)` local hours for background CPU jobs.
    pub prepare_window_start: u32,
    pub prepare_window_end: u32,
    /// Tctl and one-minute load at which preparation pauses.
    pub prepare_max_temp_c: u32,
    /// The unit background scopes are bound to (`BindsTo=`), so a restart stops their ffmpeg:
    /// `LUCINA_MEDIA_PREPARE_BIND`, else this process's own `.service` from /proc/self/cgroup.
    pub prepare_bind_to: Option<String>,
    pub prepare_max_load: u32,
    /// Names the Drive leaves out (`LUCINA_MEDIA_DRIVE_IGNORE`, `;`-separated, `*` at either end).
    pub drive_ignore: Vec<String>,
    /// Index hidden files (`.name`) in the Drive too (`LUCINA_MEDIA_DRIVE_HIDDEN=1`).
    pub drive_hidden: bool,
    /// Tctl at which the Drive's background work pauses (`LUCINA_MEDIA_DRIVE_MAX_TEMP`, 90 °C).
    pub drive_max_temp_c: u32,
    /// CPU quota of a Drive job's scope in percent (`LUCINA_MEDIA_DRIVE_CPU_QUOTA`, 50).
    pub drive_cpu_quota: u32,
    /// Run the pure-Rust extractors in a capped child process (`lucina-media extract-text`).
    pub drive_isolate: bool,
    /// poppler's `pdftotext` and `pdftoppm`, LibreOffice's `soffice`, when installed
    /// (`LUCINA_MEDIA_PDFTOTEXT`, `LUCINA_MEDIA_PDFTOPPM`, `LUCINA_MEDIA_SOFFICE`: a path, or 0).
    pub pdftotext: Option<PathBuf>,
    pub pdftoppm: Option<PathBuf>,
    pub soffice: Option<PathBuf>,
}

/// `LUCINA_MEDIA_FILM_FOLDERS` when unset.
pub fn default_film_folders() -> Vec<String> {
    vec!["Film".into(), "Dansk film".into()]
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let home = PathBuf::from(var("HOME").unwrap_or_else(|| "/home/nez".into()));
        let default_key = home.join(".config/secretary-media/upstream.key");
        let upstream_key = match var("LUCINA_MEDIA_UPSTREAM_KEY_FILE") {
            Some(path) => Some(std::fs::read_to_string(&path)?.trim().to_string()),
            None => match var("LUCINA_MEDIA_UPSTREAM_KEY") {
                Some(k) => Some(k),
                None if default_key.is_file() => {
                    Some(std::fs::read_to_string(&default_key)?.trim().to_string())
                }
                None => None,
            },
        };
        let secret_file = var("LUCINA_MEDIA_PLAY_SECRET_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config/secretary-media/play-token.key"));
        let play_secret = if secret_file.is_file() {
            Some(std::fs::read(&secret_file)?)
        } else {
            None
        };
        let folders = var("LUCINA_MEDIA_FOLDERS")
            .map(|v| {
                v.split(';')
                    .filter_map(|p| p.split_once('='))
                    .map(|(l, p)| (l.trim().to_string(), PathBuf::from(p.trim())))
                    .collect()
            })
            .unwrap_or_default();
        if upstream_key.as_deref().is_some_and(|k| k.len() < 16) {
            anyhow::bail!("upstream key must be at least 16 characters");
        }
        let data_dir: PathBuf = var("LUCINA_MEDIA_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("data/lucina-media"));
        let ffmpeg: PathBuf = var("LUCINA_MEDIA_FFMPEG")
            .unwrap_or_else(|| "ffmpeg".into())
            .into();
        let ffprobe: PathBuf = var("LUCINA_MEDIA_FFPROBE")
            .unwrap_or_else(|| "ffprobe".into())
            .into();
        let prepare_scope = match var("LUCINA_MEDIA_PREPARE_SCOPE") {
            Some(v) => !matches!(v.as_str(), "0" | "false" | "off"),
            None => systemd_scope_works(),
        };
        let prepare_cpu_quota = var("LUCINA_MEDIA_PREPARE_CPU_QUOTA")
            .map(|v| v.parse())
            .transpose()?
            .unwrap_or(100);
        let (prepare_window_start, prepare_window_end) = var("LUCINA_MEDIA_PREPARE_WINDOW")
            .as_deref()
            .and_then(parse_window)
            .unwrap_or((1, 7));
        // The render node is only useful when this ffmpeg can encode with it.
        let render_node = Path::new("/dev/dri/renderD128");
        let vaapi_device = match var("LUCINA_MEDIA_VAAPI") {
            Some(v) if matches!(v.as_str(), "0" | "false" | "off") => None,
            Some(v) => Some(PathBuf::from(v)),
            None if render_node.exists() && ffmpeg_has_vaapi(&ffmpeg) => Some(render_node.into()),
            None => None,
        };
        let tool = |env: &str, names: &[&str]| match var(env) {
            Some(v) if matches!(v.as_str(), "0" | "false" | "off") => None,
            Some(v) => Some(PathBuf::from(v)),
            None => names.iter().find_map(|n| on_path(n)),
        };
        let pdftotext = tool("LUCINA_MEDIA_PDFTOTEXT", &["pdftotext"]);
        let pdftoppm = tool("LUCINA_MEDIA_PDFTOPPM", &["pdftoppm"]);
        let soffice = tool("LUCINA_MEDIA_SOFFICE", &["soffice", "libreoffice"]);
        let default_lease = home.join("bin/gpu-lease");
        let gpu_lease = match var("LUCINA_MEDIA_GPU_LEASE") {
            Some(v) if matches!(v.as_str(), "0" | "false" | "off") => None,
            Some(v) => Some(PathBuf::from(v)),
            None if is_executable(&default_lease) => Some(default_lease),
            None => None,
        };
        Ok(Self {
            listen: var("LUCINA_MEDIA_LISTEN")
                .unwrap_or_else(|| "127.0.0.1:8830".into())
                .parse()?,
            internal_listen: var("LUCINA_MEDIA_INTERNAL_LISTEN")
                .unwrap_or_else(|| "127.0.0.1:8830".into())
                .parse()?,
            stream_cache_dir: var("LUCINA_MEDIA_STREAM_CACHE")
                .map(PathBuf::from)
                .unwrap_or_else(|| data_dir.join("stream")),
            progress_path: var("LUCINA_MEDIA_PROGRESS")
                .map(PathBuf::from)
                .unwrap_or_else(|| data_dir.join("progress.json")),
            data_dir,
            play_secret,
            folders,
            prepare: var("LUCINA_MEDIA_PREPARE")
                .is_none_or(|v| !matches!(v.as_str(), "0" | "false" | "off")),
            film_folders: var("LUCINA_MEDIA_FILM_FOLDERS")
                .map(|v| {
                    v.split(';')
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty())
                        .collect()
                })
                .unwrap_or_else(default_film_folders),
            store_dir: var("LUCINA_MEDIA_STORE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("mnt/mybook/lucina-media")),
            upstream_key,
            quiet_start: var("LUCINA_MEDIA_QUIET_START")
                .map(|v| v.parse())
                .transpose()?
                .unwrap_or(22),
            quiet_end: var("LUCINA_MEDIA_QUIET_END")
                .map(|v| v.parse())
                .transpose()?
                .unwrap_or(9),
            tz: var("LUCINA_MEDIA_TZ")
                .unwrap_or_else(|| "Europe/Copenhagen".into())
                .parse()
                .map_err(anyhow::Error::msg)?,
            prepare_scope,
            prepare_cpu_quota,
            vaapi_device,
            gpu_lease,
            prepare_window_start,
            prepare_window_end,
            prepare_bind_to: match var("LUCINA_MEDIA_PREPARE_BIND") {
                Some(v) if v == "0" => None,
                Some(v) => Some(v),
                None => std::fs::read_to_string("/proc/self/cgroup").ok().and_then(|c| own_service(&c)),
            },
            prepare_max_temp_c: var("LUCINA_MEDIA_PREPARE_MAX_TEMP")
                .map(|v| v.parse())
                .transpose()?
                .unwrap_or(90),
            prepare_max_load: var("LUCINA_MEDIA_PREPARE_MAX_LOAD")
                .map(|v| v.parse())
                .transpose()?
                .unwrap_or(16),
            drive_ignore: var("LUCINA_MEDIA_DRIVE_IGNORE")
                .map(|v| {
                    v.split(';')
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty())
                        .collect()
                })
                .unwrap_or_else(crate::drive_kernel::default_ignore),
            drive_hidden: var("LUCINA_MEDIA_DRIVE_HIDDEN")
                .is_some_and(|v| matches!(v.as_str(), "1" | "true" | "on")),
            drive_max_temp_c: var("LUCINA_MEDIA_DRIVE_MAX_TEMP")
                .map(|v| v.parse())
                .transpose()?
                .unwrap_or(90),
            drive_cpu_quota: var("LUCINA_MEDIA_DRIVE_CPU_QUOTA")
                .map(|v| v.parse())
                .transpose()?
                .unwrap_or(50),
            drive_isolate: true,
            pdftotext,
            pdftoppm,
            soffice,
            ffmpeg,
            ffprobe,
        })
    }

    /// A config rooted in one directory, for tests and one-off CLI runs.
    pub fn for_dirs(data_dir: PathBuf, store_dir: PathBuf) -> Self {
        Self {
            listen: "127.0.0.1:0".parse().unwrap(),
            internal_listen: "127.0.0.1:0".parse().unwrap(),
            stream_cache_dir: data_dir.join("stream"),
            progress_path: data_dir.join("progress.json"),
            data_dir,
            store_dir,
            upstream_key: None,
            quiet_start: 22,
            quiet_end: 9,
            tz: chrono_tz::Europe::Copenhagen,
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
            play_secret: None,
            folders: Vec::new(),
            prepare: false,
            film_folders: default_film_folders(),
            prepare_scope: false,
            prepare_cpu_quota: 100,
            vaapi_device: None,
            gpu_lease: None,
            prepare_window_start: 1,
            prepare_window_end: 7,
            prepare_max_temp_c: 90,
            prepare_bind_to: None,
            prepare_max_load: 16,
            drive_ignore: crate::drive_kernel::default_ignore(),
            drive_hidden: false,
            drive_max_temp_c: 90,
            drive_cpu_quota: 50,
            drive_isolate: false,
            pdftotext: None,
            pdftoppm: None,
            soffice: None,
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
        is_quiet_hour(
            now.with_timezone(&self.tz).hour(),
            self.quiet_start,
            self.quiet_end,
        )
    }
}

/// `"1-7"` as `(1, 7)`, clamped to real hours. A malformed value keeps the default.
fn parse_window(value: &str) -> Option<(u32, u32)> {
    let (start, end) = value.split_once('-')?;
    Some((
        start.trim().parse::<u32>().ok()?.min(23),
        end.trim().parse::<u32>().ok()?.min(23),
    ))
}

/// Whether `systemd-run --user --scope` works here (the user cgroup is delegated).
fn systemd_scope_works() -> bool {
    std::process::Command::new("systemd-run")
        .args(["--user", "--scope", "--quiet", "--", "true"])
        .status()
        .is_ok_and(|s| s.success())
}

/// Whether this ffmpeg lists the VAAPI H.264 encoder.
fn ffmpeg_has_vaapi(ffmpeg: &Path) -> bool {
    std::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|o| {
            o.status.success() && String::from_utf8_lossy(&o.stdout).contains("h264_vaapi")
        })
}

/// `name` found on `PATH`, if it is there.
fn on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| Path::new(d).join(name))
        .find(|p| is_executable(p))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
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
    fn prepare_window_parses_and_clamps() {
        assert_eq!(parse_window("1-7"), Some((1, 7)));
        assert_eq!(parse_window("22-6"), Some((22, 6)));
        assert_eq!(parse_window(" 0 - 24 "), Some((0, 23)));
        assert_eq!(parse_window("7"), None);
        assert_eq!(parse_window("a-b"), None);
    }

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

/// This process's systemd service from `/proc/self/cgroup` (`0::/…/app.slice/lucina-media.service`),
/// or None outside a service (a shell, a test).
fn own_service(cgroup: &str) -> Option<String> {
    let path = cgroup.lines().find_map(|l| l.strip_prefix("0::"))?;
    let last = path.trim().rsplit('/').next()?;
    last.ends_with(".service").then(|| last.to_string())
}

#[cfg(test)]
mod own_service_tests {
    use super::own_service;

    #[test]
    fn finds_the_service_unit_and_nothing_else() {
        let svc = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/lucina-media.service\n";
        assert_eq!(own_service(svc).as_deref(), Some("lucina-media.service"));
        assert_eq!(own_service("0::/user.slice/user-1000.slice/session-3.scope\n"), None);
        assert_eq!(own_service(""), None);
    }
}
