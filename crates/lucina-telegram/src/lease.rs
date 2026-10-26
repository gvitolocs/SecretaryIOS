//! Host-wide Telegram poller lease: Rust flock plus Node heartbeat rendezvous.
use crate::lease_decision::{is_record_live, lease_decision, Decision, Identity, LockRecord};
use fs4::fs_std::FileExt;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

pub fn bot_key_from_token(token: &str) -> String {
    let head = token.split(':').next().unwrap_or_default();
    if !head.is_empty() && head.bytes().all(|b| b.is_ascii_digit()) {
        head.into()
    } else {
        format!("{:x}", Sha256::digest(token.as_bytes()))[..12].into()
    }
}
pub fn default_lock_dir() -> io::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("HERMES_GATEWAY_LOCK_DIR").filter(|s| !s.is_empty()) {
        return Ok(dir.into());
    }
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".local/state/hermes-gateway-locks"))
        .ok_or_else(|| io::Error::other("gateway lock directory unavailable: HOME is unset"))
}
fn read_record(path: &Path) -> io::Result<Option<LockRecord>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
fn write_record(path: &Path, record: &LockRecord) -> io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let tmp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&tmp)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        let mut bytes = serde_json::to_vec_pretty(record)?;
        bytes.push(b'\n');
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}
struct Held {
    path: PathBuf,
    record: LockRecord,
    _flock: File,
}
impl Drop for Held {
    fn drop(&mut self) {
        if read_record(&self.path)
            .ok()
            .flatten()
            .is_some_and(|r| r.holder == self.record.holder)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
pub struct Lease {
    held: Arc<Mutex<Held>>,
    heartbeat: tokio::task::JoinHandle<()>,
    lost: watch::Receiver<bool>,
}
impl Lease {
    pub fn lost(&self) -> watch::Receiver<bool> {
        self.lost.clone()
    }
    pub fn record(&self) -> LockRecord {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record
            .clone()
    }
    pub async fn acquire(name: &str, token: &str) -> io::Result<Option<Self>> {
        Self::acquire_in(
            &default_lock_dir()?,
            name,
            token,
            &std::env::var("HERMES_GATEWAY_INSTANCE")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "lucina-rs".into()),
        )
        .await
    }
    pub async fn acquire_in(
        dir: &Path,
        name: &str,
        token: &str,
        instance: &str,
    ) -> io::Result<Option<Self>> {
        std::fs::create_dir_all(dir).map_err(|e| {
            io::Error::new(e.kind(), format!("gateway lock directory unusable: {e}"))
        })?;
        let bot_key = bot_key_from_token(token);
        let flock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(format!("telegram-{bot_key}.flock")))
            .map_err(|e| io::Error::new(e.kind(), format!("gateway flock unusable: {e}")))?;
        match flock.try_lock_exclusive() {
            Ok(true) => {}
            Ok(false) => return Ok(None),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(e) => {
                return Err(io::Error::new(
                    e.kind(),
                    format!("gateway flock failed: {e}"),
                ))
            }
        }
        let path = dir.join(format!("telegram-{bot_key}.lock"));
        let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap_or_default()
            .trim()
            .to_owned();
        let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .unwrap_or_default()
            .trim()
            .to_owned();
        let pid_ns = std::fs::read_link("/proc/self/ns/pid")
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let pid = std::process::id();
        let holder = format!("{hostname}:{pid}:{}", uuid::Uuid::new_v4());
        let current = read_record(&path)?;
        let me = Identity {
            holder: holder.clone(),
            instance: instance.trim().into(),
            pid_ns: pid_ns.clone(),
            pid,
            record_pid_alive: current
                .as_ref()
                .and_then(|r| r.pid)
                .is_some_and(|p| Path::new(&format!("/proc/{p}")).exists()),
        };
        if lease_decision(
            current.as_ref(),
            &me,
            chrono::Utc::now().timestamp_millis(),
            &boot_id,
        ) == Decision::Defer
        {
            return Ok(None);
        }
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let record = LockRecord {
            holder,
            name: name.into(),
            bot_key,
            hostname,
            pid: Some(pid),
            pid_ns,
            instance: me.instance,
            boot_id,
            started_at: now.clone(),
            heartbeat_at: now,
        };
        write_record(&path, &record)?;
        let held = Arc::new(Mutex::new(Held {
            path,
            record,
            _flock: flock,
        }));
        // Node's rendezvous: last rename wins; verify before polling.
        tokio::time::sleep(Duration::from_millis(250)).await;
        {
            let h = held.lock().unwrap_or_else(|e| e.into_inner());
            if read_record(&h.path)?.is_none_or(|r| r.holder != h.record.holder) {
                return Ok(None);
            }
        }
        let (tx, lost) = watch::channel(false);
        let weak = Arc::downgrade(&held);
        let heartbeat = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let Some(held) = weak.upgrade() else {
                    break;
                };
                let mut h = held.lock().unwrap_or_else(|e| e.into_inner());
                let result = read_record(&h.path).and_then(|r| {
                    if r.is_some_and(|r| {
                        r.holder != h.record.holder
                            && is_record_live(
                                &r,
                                chrono::Utc::now().timestamp_millis(),
                                &h.record.boot_id,
                            )
                    }) {
                        return Err(io::Error::other("gateway lease lost to another holder"));
                    }
                    h.record.heartbeat_at =
                        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                    write_record(&h.path, &h.record)
                });
                if result.is_err() {
                    let _ = tx.send(true);
                    break;
                }
            }
        });
        Ok(Some(Self {
            held,
            heartbeat,
            lost,
        }))
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.heartbeat.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn flock_record_and_release() {
        let dir = tempfile::tempdir().unwrap();
        let lease = Lease::acquire_in(dir.path(), "Lucina", "111:secret", "test")
            .await
            .unwrap()
            .unwrap();
        assert!(
            Lease::acquire_in(dir.path(), "Lucina", "111:secret", "test")
                .await
                .unwrap()
                .is_none()
        );
        let path = dir.path().join("telegram-111.lock");
        let bytes = std::fs::read_to_string(&path).unwrap();
        assert!(!bytes.contains("secret"));
        assert!(bytes.ends_with('\n'));
        assert!(bytes.starts_with("{\n  \"holder\":"));
        assert_eq!(read_record(&path).unwrap().unwrap().instance, "test");
        drop(lease);
        assert!(!path.exists());
        assert!(
            Lease::acquire_in(dir.path(), "Lucina", "111:secret", "test")
                .await
                .unwrap()
                .is_some()
        );
    }
    #[tokio::test]
    async fn foreign_live_record_defers_and_old_boot_is_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram-111.lock");
        let mut record = LockRecord {
            holder: "foreign".into(),
            instance: "foreign".into(),
            heartbeat_at: chrono::Utc::now().to_rfc3339(),
            ..Default::default()
        };
        write_record(&path, &record).unwrap();
        assert!(
            Lease::acquire_in(dir.path(), "Lucina", "111:secret", "ours")
                .await
                .unwrap()
                .is_none()
        );
        record.boot_id = "previous boot".into();
        write_record(&path, &record).unwrap();
        let lease = Lease::acquire_in(dir.path(), "Lucina", "111:secret", "ours")
            .await
            .unwrap()
            .unwrap();
        record.holder = "new holder".into();
        record.boot_id.clear();
        write_record(&path, &record).unwrap();
        drop(lease);
        assert!(path.exists(), "release never removes another holder");
    }
    #[tokio::test]
    async fn unusable_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        assert!(Lease::acquire_in(&file, "Lucina", "111:secret", "ours")
            .await
            .is_err());
    }
    #[test]
    fn bot_keys_match_node() {
        assert_eq!(bot_key_from_token("123456:SECRET"), "123456");
        assert_eq!(bot_key_from_token("not-a-token").len(), 12);
        assert_ne!(bot_key_from_token("not-a-token"), "not-a-token");
    }
}

#[cfg(test)]
mod heartbeat_tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn heartbeat_and_loss_stop_the_lease() {
        let dir = tempfile::tempdir().unwrap();
        let lease = Lease::acquire_in(dir.path(), "Lucina", "222:secret", "ours")
            .await
            .unwrap()
            .unwrap();
        let path = dir.path().join("telegram-222.lock");
        let mut old = read_record(&path).unwrap().unwrap();
        old.heartbeat_at = "2000-01-01T00:00:00.000Z".into();
        write_record(&path, &old).unwrap();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(30)).await;
        tokio::task::yield_now().await;
        assert_ne!(
            read_record(&path).unwrap().unwrap().heartbeat_at,
            old.heartbeat_at
        );
        let mut lost = lease.lost();
        old.holder = "foreign".into();
        old.heartbeat_at = chrono::Utc::now().to_rfc3339();
        write_record(&path, &old).unwrap();
        tokio::time::advance(Duration::from_secs(30)).await;
        tokio::task::yield_now().await;
        lost.changed().await.unwrap();
        assert!(*lost.borrow());
        drop(lease);
        assert_eq!(read_record(&path).unwrap().unwrap().holder, "foreign");
    }
    #[tokio::test]
    async fn own_previous_instance_is_acquired() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telegram-333.lock");
        write_record(
            &path,
            &LockRecord {
                holder: "previous".into(),
                instance: "ours".into(),
                heartbeat_at: chrono::Utc::now().to_rfc3339(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            Lease::acquire_in(dir.path(), "Lucina", "333:secret", "ours")
                .await
                .unwrap()
                .is_some()
        );
    }
}
