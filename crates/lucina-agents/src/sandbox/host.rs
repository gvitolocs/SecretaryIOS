//! The one part of the sandbox module that reads the file system: the real resolver for
//! [`Plan::resolve`](super::Plan::resolve), and [`bwrap_args`], which plans and resolves in one call.
//! Kept apart so the rest of the module stays pure and is tested with a fake resolver.

use std::io;
use std::path::Path;

use super::{plan, FileKind, Policy, Resolved, RunInputs, SandboxError};

/// What `path` is on this host: `NotFound` when nothing is there (not even a symlink); otherwise its
/// real path (`canonicalize`), kind and length. A dangling symlink is an error, not `NotFound`, so
/// an optional bind cannot quietly skip it.
pub fn resolve(path: &Path) -> io::Result<Resolved> {
    std::fs::symlink_metadata(path)?;
    let real = std::fs::canonicalize(path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            io::Error::other("a dangling symlink")
        } else {
            e
        }
    })?;
    let meta = std::fs::metadata(&real)?;
    let kind = if meta.is_file() {
        FileKind::File
    } else if meta.is_dir() {
        FileKind::Dir
    } else {
        FileKind::Other
    };
    Ok(Resolved {
        path: real,
        kind,
        len: meta.len(),
    })
}

/// The bubblewrap argument list for one run on this host: [`plan`], then
/// [`Plan::resolve`](super::Plan::resolve) with [`resolve`].
pub fn bwrap_args(policy: &Policy, run: &RunInputs) -> Result<Vec<String>, SandboxError> {
    plan(policy, run)?.resolve(&resolve)
}
