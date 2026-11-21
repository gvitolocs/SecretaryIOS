//! `scripts/check-fast` builds into one Cargo target per worktree under the check cache (GATE-24); see
//! `scripts/tests/check-fast-target-test.sh`, which runs the real launcher with stub tools and stops once the target is resolved.

use std::path::Path;
use std::process::Command;

#[test]
fn check_fast_uses_one_cargo_target_per_worktree() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../scripts/tests/check-fast-target-test.sh");
    let o = Command::new("bash")
        .arg(&script)
        .env("CHECK_FAST_TARGET_TEST_TMP", env!("CARGO_TARGET_TMPDIR"))
        .output()
        .unwrap_or_else(|e| panic!("running {} failed: {e}", script.display()));
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(o.status.success(), "exit {:?}:\n{msg}", o.status.code());
    assert!(msg.contains("check-fast-target-test: pass"), "{msg}");
}
