//! Best-effort cleanup of the pipeline's shared Cargo cache, under the job's flock.

use std::collections::HashMap;
use std::fs::{self, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Remove artifacts that have not been modified for three hours.
const MAX_AGE: Duration = Duration::from_secs(3 * 60 * 60);
/// Evict oldest disposable artifacts until allocated disk use is at most 60 GiB.
const SIZE_CAP: u64 = 60 * 1024 * 1024 * 1024;
/// Never evict artifacts modified in the last 30 minutes, even above the size cap.
const MIN_AGE: Duration = Duration::from_secs(30 * 60);

type Inode = (u64, u64);

struct Artifact {
    path: PathBuf,
    modified: SystemTime,
    directory: bool,
    inodes: Vec<Inode>,
}

#[derive(Default)]
struct Inventory {
    bytes: u64,
    // Count links too: deleting one Cargo hard link does not release its blocks.
    inodes: HashMap<Inode, (u64, usize)>,
    artifacts: Vec<Artifact>,
}

impl Inventory {
    fn read(target: &Path) -> io::Result<Self> {
        let mut tree = Self::default();
        tree.walk(target, target, None)?;
        Ok(tree)
    }

    fn walk(&mut self, target: &Path, path: &Path, artifact: Option<usize>) -> io::Result<()> {
        let meta = fs::symlink_metadata(path)?;
        let key = (meta.dev(), meta.ino());
        let entry = self.inodes.entry(key).or_insert_with(|| {
            let bytes = meta.blocks() * 512;
            self.bytes += bytes;
            (bytes, 0)
        });
        entry.1 += 1;
        let modified = meta.modified()?;
        let artifact = match artifact {
            None if candidate(target, path, &meta) => {
                self.artifacts.push(Artifact { path: path.to_path_buf(), modified, directory: meta.is_dir(), inodes: Vec::new() });
                Some(self.artifacts.len() - 1)
            }
            other => other,
        };
        if let Some(i) = artifact {
            self.artifacts[i].modified = self.artifacts[i].modified.max(modified);
            self.artifacts[i].inodes.push(key);
        }
        // symlink_metadata and is_dir never follow symlinks, including inside artifacts.
        if meta.is_dir() {
            for entry in fs::read_dir(path)? {
                self.walk(target, &entry?.path(), artifact)?;
            }
        }
        Ok(())
    }

    fn remove(&mut self, target: &Path, artifact: &Artifact) -> io::Result<()> {
        // Refuse a replaced parent or symlink rather than following it during deletion.
        let resolved = fs::canonicalize(&artifact.path)?;
        if resolved != artifact.path || !resolved.starts_with(target) || resolved == target {
            return Err(io::Error::other(format!("unsafe artifact: {}", artifact.path.display())));
        }
        if artifact.directory {
            fs::remove_dir_all(&artifact.path)?;
        } else {
            fs::remove_file(&artifact.path)?;
        }
        for key in &artifact.inodes {
            let (bytes, links) = self.inodes.get_mut(key).ok_or_else(|| io::Error::other("artifact inode missing from inventory"))?;
            *links -= 1;
            if *links == 0 {
                self.bytes -= *bytes;
            }
        }
        // Removing entries can also shrink their containing directory's allocation.
        let parent = fs::symlink_metadata(artifact.path.parent().ok_or_else(|| io::Error::other("artifact has no parent"))?)?;
        let (bytes, _) = self.inodes.get_mut(&(parent.dev(), parent.ino())).ok_or_else(|| io::Error::other("artifact parent changed during sweep"))?;
        self.bytes = self.bytes - *bytes + parent.blocks() * 512;
        *bytes = parent.blocks() * 512;
        Ok(())
    }
}

/// Only Cargo's disposable outputs: profile/incremental entries, profile/build
/// directories and profile/{deps,examples} files, optionally below a target triple.
fn candidate(target: &Path, path: &Path, meta: &Metadata) -> bool {
    let Ok(relative) = path.strip_prefix(target) else { return false };
    let parts: Vec<_> = relative.iter().collect();
    if !(parts.len() == 3 || parts.len() == 4)
        || parts[..parts.len() - 2].iter().any(|p| matches!(p.to_str(), Some(".fingerprint" | "incremental" | "build" | "deps" | "examples")))
    {
        return false;
    }
    match parts[parts.len() - 2].to_str() {
        Some("incremental") => meta.is_dir() || meta.is_file(),
        Some("build") => meta.is_dir(),
        Some("deps" | "examples") => meta.is_file(),
        _ => false,
    }
}

/// A single log line, including failures; cleanup never participates in the job's outcome.
pub(super) fn run(target: &Path, root: &Path) -> String {
    sweep(target, root, MAX_AGE, SIZE_CAP, MIN_AGE)
}

fn sweep(target: &Path, root: &Path, max_age: Duration, cap: u64, min_age: Duration) -> String {
    let started = Instant::now();
    let now = SystemTime::now();
    let mut before = None;
    let mut after = None;
    let mut removed = 0;
    let mut over_cap = false;
    let result = (|| -> io::Result<()> {
        let root = fs::canonicalize(root)?;
        // A missing cache is normal for jobs that never built anything. Resolve its parent
        // too, so a missing path cannot bypass the boundary check through a symlink.
        let target = match fs::canonicalize(target) {
            Ok(path) => path,
            Err(e) if e.kind() == io::ErrorKind::NotFound && fs::symlink_metadata(target).is_err() => {
                let parent = target.parent().ok_or_else(|| io::Error::other("target has no parent"))?;
                fs::canonicalize(parent)?.join(target.file_name().ok_or_else(|| io::Error::other("target has no name"))?)
            }
            Err(e) => return Err(e),
        };
        if target == root || !target.starts_with(&root) {
            return Err(io::Error::other(format!("refusing target outside root: {}", target.display())));
        }
        if !target.exists() {
            before = Some(0);
            after = Some(0);
            return Ok(());
        }
        let mut tree = Inventory::read(&target)?;
        before = Some(tree.bytes);
        let mut artifacts = std::mem::take(&mut tree.artifacts);
        artifacts.sort_by(|a, b| a.modified.cmp(&b.modified).then_with(|| a.path.cmp(&b.path)));
        let deletion = (|| -> io::Result<()> {
            for artifact in &artifacts {
                let age = now.duration_since(artifact.modified).unwrap_or_default();
                if age >= min_age && (age >= max_age || tree.bytes > cap) {
                    tree.remove(&target, artifact)?;
                    removed += 1;
                }
            }
            Ok(())
        })();
        // Remeasure even after a partial delete: directory allocation can change, and a
        // failed remove_dir_all may already have unlinked some of its contents.
        after = Some(Inventory::read(&target)?.bytes);
        over_cap = after.is_some_and(|bytes| bytes > cap);
        deletion
    })();
    let bytes = |value: Option<u64>| value.map(|n| n.to_string()).unwrap_or_else(|| "unknown".into());
    format!(
        "target sweep: bytes before={} after={} items removed={removed} seconds={:.3}{}{}",
        bytes(before),
        bytes(after),
        started.elapsed().as_secs_f64(),
        if over_cap { "; still over cap (recent or protected artifacts remain)" } else { "" },
        match result {
            Ok(()) => String::new(),
            Err(e) => format!("; failed: {e}"),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, FileTimes};
    use std::os::unix::fs::symlink;
    use std::process::Command;

    fn file(target: &Path, name: &str, age: Duration) -> PathBuf {
        let path = target.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![1; 16 * 1024]).unwrap();
        modified(&path, age);
        path
    }

    fn modified(path: &Path, age: Duration) {
        File::open(path).unwrap().set_times(FileTimes::new().set_modified(SystemTime::now() - age)).unwrap();
    }

    #[test]
    fn age_sweep_covers_profiles_and_preserves_cargo_metadata_and_binaries() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        let old = MAX_AGE + Duration::from_secs(60);
        let mut removed = Vec::new();
        let mut kept = Vec::new();
        for profile in ["debug", "release", "custom", "x86_64-unknown-linux-musl/release"] {
            for area in ["deps", "examples", "incremental", "build"] {
                let directory = matches!(area, "incremental" | "build");
                let name = format!("{profile}/{area}/old{}", if directory { "/output" } else { "" });
                let path = file(&target, &name, old);
                if directory {
                    modified(path.parent().unwrap(), old);
                    removed.push(path.parent().unwrap().to_path_buf());
                } else {
                    removed.push(path);
                }
                kept.push(file(&target, &format!("{profile}/{area}/recent{}", if directory { "/output" } else { "" }), Duration::ZERO));
            }
            kept.push(file(&target, &format!("{profile}/.fingerprint/crate/hash"), old));
            kept.push(file(&target, &format!("{profile}/app"), old));
            // A stale directory timestamp must not hide a freshly written nested output.
            kept.push(file(&target, &format!("{profile}/build/active/output"), Duration::ZERO));
            modified(&target.join(profile).join("build/active"), old);
            // Only top-level files of deps/examples are disposable.
            kept.push(file(&target, &format!("{profile}/deps/subdir/file"), old));
        }
        removed.push(file(&target, "debug/incremental/old-file", old));
        let outside = file(root.path(), "outside/output", old);
        symlink(outside.parent().unwrap(), target.join("release/build/link")).unwrap();
        symlink(outside.parent().unwrap(), target.join("linked-triple")).unwrap();
        let log = run(&target, root.path());
        assert!(!log.contains("failed"), "{log}");
        assert!(log.contains("items removed=17"), "{log}");
        for path in removed {
            assert!(!path.exists(), "{}", path.display());
        }
        for path in kept {
            assert!(path.exists(), "{}", path.display());
        }
        assert!(outside.exists());
        assert!(target.join("release/build/link").symlink_metadata().unwrap().is_symlink());
    }

    #[test]
    fn cap_evicts_oldest_first_and_counts_hard_links_once() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        let oldest = file(&target, "debug/deps/oldest", Duration::from_secs(2 * 60 * 60));
        let next = file(&target, "debug/deps/next", Duration::from_secs(60 * 60));
        let recent = file(&target, "debug/deps/recent", Duration::from_secs(10 * 60));
        let alias = target.join("debug/binary");
        let before = Inventory::read(&target).unwrap().bytes;
        fs::hard_link(&oldest, &alias).unwrap();
        assert_eq!(Inventory::read(&target).unwrap().bytes, before, "a hard link adds no allocated blocks");
        let cap = before - fs::metadata(&next).unwrap().blocks() * 512;
        let log = sweep(&target, root.path(), MAX_AGE, cap, MIN_AGE);
        assert!(!log.contains("failed"), "{log}");
        assert!(!oldest.exists());
        assert!(!next.exists(), "removing oldest's link did not free its blocks");
        assert!(alias.exists());
        assert!(recent.exists());
        assert!(log.contains("items removed=2"), "{log}");

        let old = file(&target, "debug/deps/older", Duration::from_secs(2 * 60 * 60));
        let newer = file(&target, "debug/deps/newer", Duration::from_secs(60 * 60));
        let before = Inventory::read(&target).unwrap().bytes;
        let cap = before - fs::metadata(&old).unwrap().blocks() * 512;
        let log = sweep(&target, root.path(), MAX_AGE, cap, MIN_AGE);
        assert!(!old.exists());
        assert!(newer.exists(), "stop as soon as the cap is met: {log}");
        let log = sweep(&target, root.path(), MAX_AGE, 0, MIN_AGE);
        assert!(recent.exists());
        assert!(log.contains("still over cap"), "{log}");
    }

    #[test]
    fn refuses_outside_root_root_itself_and_symlink_escape() {
        let root = tempfile::tempdir().unwrap();
        let boundary = root.path().join("cache");
        fs::create_dir(&boundary).unwrap();
        let outside = root.path().join("cache-other");
        let marker = file(&outside, "debug/deps/old", MAX_AGE + MIN_AGE);
        let link = boundary.join("escape");
        symlink(&outside, &link).unwrap();
        for target in [&outside, &link, &boundary] {
            let log = sweep(target, &boundary, Duration::ZERO, 0, Duration::ZERO);
            assert!(log.contains("refusing target outside root"), "{log}");
            assert!(marker.exists());
        }
    }

    #[test]
    fn cargo_rebuilds_after_outputs_are_swept() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let target = root.path().join("target");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname = \"sweep-probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n").unwrap();
        fs::write(
            project.join("build.rs"),
            r#"fn main() {
            let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
            std::fs::write(out.join("message.rs"), "const MESSAGE: &str = \"rebuilt\";").unwrap();
        }"#,
        )
        .unwrap();
        fs::write(
            project.join("src/main.rs"),
            r#"include!(concat!(env!("OUT_DIR"), "/message.rs"));
            fn main() { println!("{}", MESSAGE); }"#,
        )
        .unwrap();
        let build = || {
            let output = Command::new("cargo")
                .args(["build", "--offline"])
                .current_dir(&project)
                .env("CARGO_TARGET_DIR", &target)
                .env_remove("CARGO_ENCODED_RUSTFLAGS")
                .env_remove("RUSTFLAGS")
                .output()
                .unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        };
        build();
        let log = sweep(&target, root.path(), Duration::ZERO, SIZE_CAP, Duration::ZERO);
        assert!(!log.contains("failed"), "{log}");
        assert!(!log.contains("items removed=0"), "{log}");
        assert!(fs::read_dir(target.join("debug/deps")).unwrap().next().is_none());
        assert!(target.join("debug/.fingerprint").is_dir());
        assert!(target.join("debug/sweep-probe").is_file());
        build();
        assert!(fs::read_dir(target.join("debug/deps")).unwrap().next().is_some());
        let output = Command::new(target.join("debug/sweep-probe")).output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"rebuilt\n");
    }
}
