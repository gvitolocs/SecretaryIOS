//! Which processes work in which checkout, from `/proc` (only this user's are readable):
//! the board's resource columns, the merge sweep, and who is using a main checkout.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const PAGE: u64 = 4096;

/// The processes whose working directory is inside one directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub procs: u32,
    pub rss_bytes: u64,
    /// Program name → how many.
    pub commands: BTreeMap<String, u32>,
}

/// The directory a working directory belongs to: the deepest one containing it.
fn owner(cwd: &Path, dirs: &[PathBuf]) -> Option<usize> {
    dirs.iter().enumerate().filter(|(_, d)| cwd.starts_with(d)).max_by_key(|(_, d)| d.as_os_str().len()).map(|(i, _)| i)
}

/// One [`Usage`] per directory, in order. This process doesn't count.
pub fn usage(dirs: &[PathBuf]) -> Vec<Usage> {
    let mut out = vec![Usage::default(); dirs.len()];
    let me = std::process::id().to_string();
    let Ok(entries) = std::fs::read_dir("/proc") else { return out };
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if !name.bytes().all(|b| b.is_ascii_digit()) || name == me {
            continue;
        }
        let Ok(cwd) = std::fs::read_link(e.path().join("cwd")) else { continue };
        let Some(i) = owner(&cwd, dirs) else { continue };
        let rss = std::fs::read_to_string(e.path().join("statm")).ok().and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok()).unwrap_or(0);
        let comm = std::fs::read_to_string(e.path().join("comm")).unwrap_or_default();
        let u = &mut out[i];
        u.procs += 1;
        u.rss_bytes += rss * PAGE;
        *u.commands.entry(comm.trim().to_string()).or_default() += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deepest_directory_owns_a_process() {
        let dirs = vec![PathBuf::from("/a"), PathBuf::from("/a/b"), PathBuf::from("/c")];
        assert_eq!(owner(Path::new("/a/b/src"), &dirs), Some(1));
        assert_eq!(owner(Path::new("/a/x"), &dirs), Some(0));
        assert_eq!(owner(Path::new("/ab"), &dirs), None);
    }

    #[test]
    fn a_child_in_a_directory_is_counted() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let mut child = std::process::Command::new("sleep").arg("30").current_dir(&dir).spawn().unwrap();
        let u = usage(std::slice::from_ref(&dir));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(u[0].procs, 1);
        assert_eq!(u[0].commands.get("sleep"), Some(&1));
        assert!(u[0].rss_bytes > 0);
    }
}
