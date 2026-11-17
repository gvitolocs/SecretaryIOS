//! Test support shared by this crate's unit tests and its integration tests.
//!
//! One file, two homes: `#[cfg(test)] mod testutil;` in `src/lib.rs` compiles it into the unit
//! test binary, and `#[path = "../../src/testutil.rs"] mod testutil;` in `tests/common/mod.rs`
//! (or directly in a test file that needs no daemon harness) compiles the same source into the
//! integration test binaries. Nothing of it is in the shipped library.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Writes `contents` to `path` with mode 0o755 and returns when the file can be executed.
///
/// A file that any process holds open for writing cannot be executed: `execve` answers ETXTBSY
/// and the shell exits 126 (`/bin/sh: bad interpreter: Text file busy`). `std::fs::write` opens
/// the file in *this* process, and the other threads of a test binary are forking children all
/// the time (git, cargo, a daemon); a child forked while that write fd is open inherits it and
/// keeps it until its own exec, which is exactly when the test that wrote a script wants to run
/// it. Measured in this crate's test binary, eight threads forking `/bin/true` non-stop, a
/// `#!/bin/sh` script written and executed once per round, 1500 rounds: `fs::write` +
/// `set_permissions` came back busy 110 times (7%), this helper never (0 of 1500).
///
/// So the writing happens in a child process that is reaped here: this process never holds a
/// write fd on the file, no thread's fork can carry one, and once the writer is reaped nothing
/// holds the script at all. The temp name keeps a half-written script out of the executed path;
/// renaming alone would not fix this, since the busy check is about the inode and not the name:
/// a write-to-temp-and-rename done in this process came back busy 65 times of the same 1500.
pub fn write_exec(path: &Path, contents: &str) {
    let tmp = unique_in_dir(path, ".new");
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"cat >"$1" && chmod 755 "$1" && mv -f -- "$1" "$2""#)
        .arg("write_exec")
        .arg(&tmp)
        .arg(path)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| {
            panic!(
                "write_exec {}: cannot start the writer: {e}",
                path.display()
            )
        });
    // An error here means the writer died on its own; its exit status says why below.
    let write_err = child
        .stdin
        .take()
        .and_then(|mut stdin| stdin.write_all(contents.as_bytes()).err());
    let out = child.wait_with_output().unwrap_or_else(|e| {
        panic!(
            "write_exec {}: the writer did not finish: {e}",
            path.display()
        )
    });
    let why = match write_err {
        Some(e) => Some(e.to_string()),
        None if !out.status.success() => Some(format!(
            "the writer exited {}: {}",
            out.status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "on a signal".into()),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        None => None,
    };
    if let Some(why) = why {
        let _ = std::fs::remove_file(&tmp);
        panic!("write_exec {}: {why}", path.display());
    }
}

/// A stand-in source environment for the built-environment tests ([`crate::env`]): plain
/// pairs, so a test never has to mutate the process environment. Only the unit tests in
/// `src/` call it; the integration tests compile this file too.
#[allow(dead_code)]
pub fn env_source(pairs: &[(&str, &str)]) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    pairs
        .iter()
        .map(|(k, v)| (std::ffi::OsString::from(*k), std::ffi::OsString::from(*v)))
        .collect()
}

/// A name next to `path`, based on its file name, that no other thread is using.
fn unique_in_dir(path: &Path, suffix: &str) -> PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let leaf = path
        .file_name()
        .map(|l| l.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(".{leaf}.{}.{n}{suffix}", std::process::id()))
}
