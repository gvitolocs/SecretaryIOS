//! `deploy/lucina-secrets/store-identity.sh` with dummy values and a fake
//! `systemd-creds` in a temp dir: the values never show up in its output,
//! in any argv, or in clear text on disk; the drop-in loads them with
//! `LoadCredentialEncrypted=`.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ID: &str = "00000000-dummy-client-id-0000";
const SECRET: &str = "dummy-client-secret-0123456789abcdef";

const FAKE: &str = r#"#!/bin/sh
# A stand-in for systemd-creds: logs its argv, "encrypts" as base64.
printf '%s\n' "$*" >> "$FAKE_LOG"
case "$1" in
  has-tpm2) exit 0 ;;
  encrypt) out="$5"; { printf 'FAKECRED:'; base64 -w0; } > "$out" ;;
  decrypt) tail -c +10 "$3" | base64 -d ;;
  *) exit 2 ;;
esac
"#;

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/lucina-secrets/store-identity.sh")
}

struct Run {
    ok: bool,
    output: String,
}

fn run(dir: &Path, input: &str, extra: &[&str]) -> Run {
    let fake = dir.join("fake-systemd-creds");
    if !fake.exists() {
        std::fs::write(&fake, FAKE).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut child = Command::new("bash")
        .arg(script())
        .args(["--skip-tpm-check", "--out-dir"])
        .arg(dir.join("credstore"))
        .arg("--dropin")
        .arg(dir.join("dropin/secrets.conf"))
        .args(extra)
        .env("SYSTEMD_CREDS", &fake)
        .env("FAKE_LOG", dir.join("argv.log"))
        .env("HOME", dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A refusal can exit before reading stdin: a broken pipe is fine here.
    if let Err(e) = child.stdin.take().unwrap().write_all(input.as_bytes()) {
        assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe, "{e}");
    }
    let out = child.wait_with_output().unwrap();
    Run {
        ok: out.status.success(),
        output: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn stores_encrypted_without_echo_and_writes_the_dropin() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let r = run(d, &format!("{ID}\n{SECRET}\n"), &[]);
    assert!(r.ok, "{}", r.output);
    for v in [ID, SECRET] {
        assert!(
            !r.output.contains(v),
            "output echoes a value:\n{}",
            r.output
        );
    }
    let argv = std::fs::read_to_string(d.join("argv.log")).unwrap();
    assert!(argv.contains("encrypt --with-key=tpm2 --name=lucina-infisical-client-secret -"));
    for v in [ID, SECRET] {
        assert!(!argv.contains(v), "a value reached argv:\n{argv}");
    }
    let store = d.join("credstore");
    assert_eq!(mode(&store), 0o700);
    for (name, value) in [
        ("lucina-infisical-client-id", ID),
        ("lucina-infisical-client-secret", SECRET),
    ] {
        let f = store.join(format!("{name}.cred"));
        assert_eq!(mode(&f), 0o600, "{name}");
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(!text.contains(value), "{name} is stored in clear text");
        assert!(text.starts_with("FAKECRED:"));
    }
    let dropin = std::fs::read_to_string(d.join("dropin/secrets.conf")).unwrap();
    assert!(dropin.contains(&format!(
        "LoadCredentialEncrypted=lucina-infisical-client-id:{}",
        store.join("lucina-infisical-client-id.cred").display()
    )));
    assert!(dropin.contains("LoadCredentialEncrypted=lucina-infisical-client-secret:"));
    assert!(dropin.contains("Environment=LUCINA_SECRETS_CONFIG=%h/.config/lucina-rs/secrets.toml"));
    assert!(!dropin.contains(SECRET) && !dropin.contains(ID));
    assert!(r.output.contains("nothing was restarted"));

    // Again without --force: refused; with --force: replaced.
    let again = run(d, &format!("{ID}\n{SECRET}\n"), &[]);
    assert!(
        !again.ok && again.output.contains("--force"),
        "{}",
        again.output
    );
    let forced = run(
        d,
        &format!("{ID}\nrotated-dummy-secret-9876543210\n"),
        &["--force"],
    );
    assert!(forced.ok, "{}", forced.output);
}

#[test]
fn refuses_empty_or_odd_input_and_host_keys_as_a_user() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let r = run(d, &format!("{ID}\n\n"), &[]);
    assert!(
        !r.ok && r.output.contains("empty client secret"),
        "{}",
        r.output
    );
    let r = run(d, &format!("{ID}\nshort\n"), &[]);
    assert!(
        !r.ok && r.output.contains("shorter than 16"),
        "{}",
        r.output
    );
    assert!(!d.join("credstore/lucina-infisical-client-id.cred").exists());
    let is_root = Command::new("id").arg("-u").output().unwrap().stdout == b"0\n";
    if !is_root {
        let r = run(d, &format!("{ID}\n{SECRET}\n"), &["--with-key", "host"]);
        assert!(!r.ok && r.output.contains("needs root"), "{}", r.output);
    }
}
