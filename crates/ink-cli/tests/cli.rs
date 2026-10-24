//! Exercise the real binary against temporary read-only source files and loopback HTTP.
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    process::Command,
};
fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lucina-ink"))
}
#[test]
fn scan_and_dry_run_never_send_or_read_token() {
    let dir = tempfile::tempdir().unwrap();
    let docs = dir.path().join("documents");
    fs::create_dir(&docs).unwrap();
    let p = docs.join("Book.md");
    fs::write(&p, "# A book\nBody").unwrap();
    let stamp = fs::metadata(&p).unwrap().modified().unwrap();
    let out = cli()
        .args(["scan", dir.path().to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let batch: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(batch["books"][0]["metadata"]["title"], "A book");
    assert_eq!(fs::metadata(&p).unwrap().modified().unwrap(), stamp);
    let saved = dir.path().join("batch.json");
    fs::write(&saved, out.stdout).unwrap();
    for args in [
        vec!["import", dir.path().to_str().unwrap()],
        vec!["import-json", saved.to_str().unwrap()],
    ] {
        let out = cli()
            .args(args)
            .args([
                "--server",
                "http://127.0.0.1:9",
                "--token-file",
                "does-not-exist",
                "--dry-run",
            ])
            .output()
            .unwrap();
        assert!(out.status.success(), "{:?}", out);
        let o: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(o["booksAdded"], 1);
    }
}
#[test]
fn saved_batch_posts_with_file_token() {
    let dir = tempfile::tempdir().unwrap();
    let saved = dir.path().join("batch.json");
    fs::write(&saved,json!({"source":"kindle-usb","device":null,"scannedAt":"2026-10-24T00:00:00Z","books":[],"clippings":[]}).to_string()).unwrap();
    let token = dir.path().join("token");
    fs::write(&token, "secret-test-token\n").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut b = [0; 4096];
        loop {
            let n = stream.read(&mut b).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&b[..n]);
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let request = String::from_utf8(bytes).unwrap();
        assert!(request.starts_with("POST /v1/library/import "));
        assert!(request
            .to_lowercase()
            .contains("authorization: bearer secret-test-token"));
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 16\r\nConnection: close\r\n\r\n{\"booksAdded\":0}").unwrap();
    });
    let out = cli()
        .args([
            "import-json",
            saved.to_str().unwrap(),
            "--server",
            &url,
            "--token-file",
            token.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(out.status.success(), "{:?}", out);
    assert!(!String::from_utf8_lossy(&out.stdout).contains("secret-test-token"));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("secret-test-token"));
}

#[test]
fn command_help_explains_subcommands_and_flags() {
    let output = cli().arg("--help").output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    for phrase in [
        "Scan books and clippings",
        "Scan a Kindle and send",
        "Import a previously saved",
        "Index an Obsidian vault read-only",
    ] {
        assert!(text.contains(phrase), "{text}");
    }
    let output = cli().args(["scan", "--help"]).output().unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Print the complete ImportBatch JSON"));
    assert!(text.contains("mount directory"));
    for name in ["import", "import-json"] {
        let output = cli().args([name, "--help"]).output().unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        for phrase in [
            "Secretary API base URL",
            "Read the authentication token",
            "Merge against an empty state",
        ] {
            assert!(text.contains(phrase), "{text}");
        }
    }
}

/// Accept one HTTP request on loopback, answer `body`, and return the raw request.
fn one_request(body: &'static str) -> (String, std::thread::JoinHandle<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut b = [0; 4096];
        loop {
            let n = stream.read(&mut b).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&b[..n]);
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(reply.as_bytes()).unwrap();
        String::from_utf8(bytes).unwrap()
    });
    (url, server)
}
fn synthetic_vault() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir_all(vault.join("Reading")).unwrap();
    fs::create_dir_all(vault.join("Private")).unwrap();
    fs::write(
        vault.join("Reading/Gatsby.md"),
        "---\ntitle: The Great Gatsby\nauthor: F. Scott Fitzgerald\n---\nSynthetic body text.\n",
    )
    .unwrap();
    fs::write(
        vault.join("Private/Secret.md"),
        "The Great Gatsby, privately.",
    )
    .unwrap();
    let lib = dir.path().join("library.json");
    fs::write(
        &lib,
        json!({"books":[{"id":"bk_0000000000000001","title":"The Great Gatsby","authors":["F. Scott Fitzgerald"],"status":"unknown"}]}).to_string(),
    )
    .unwrap();
    (dir, vault, lib)
}
fn snapshot(dir: &std::path::Path) -> Vec<(std::path::PathBuf, std::time::SystemTime, Vec<u8>)> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(snapshot(&p));
        } else {
            out.push((
                p.clone(),
                fs::metadata(&p).unwrap().modified().unwrap(),
                fs::read(&p).unwrap(),
            ));
        }
    }
    out.sort();
    out
}
#[test]
fn obsidian_index_is_read_only_and_private_by_default() {
    let (dir, vault, lib) = synthetic_vault();
    let before = snapshot(&vault);
    let run = |extra: &[&str]| {
        cli()
            .args([
                "obsidian-index",
                vault.to_str().unwrap(),
                "--library",
                lib.to_str().unwrap(),
            ])
            .args(extra)
            .output()
            .unwrap()
    };
    let out = run(&["--json"]);
    assert!(out.status.success(), "{out:?}");
    let batch: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(batch["vault"], "vault");
    assert_eq!(batch["refs"].as_array().unwrap().len(), 1, "{batch}");
    assert_eq!(batch["refs"][0]["notePath"], "Reading/Gatsby.md");
    assert_eq!(batch["refs"][0]["how"], "frontmatter");
    let out = run(&["--summary"]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("Refs: 1 in 1 notes for 1 books"), "{text}");
    assert!(text.contains("Excluded: 1 folders"), "{text}");
    assert!(
        !text.contains("Gatsby") && !text.contains("Reading/"),
        "{text}"
    );
    let state = dir.path().join("state.json");
    for read in ["read 1", "read 0"] {
        let out = run(&["--summary", "--state", state.to_str().unwrap()]);
        assert!(String::from_utf8(out.stdout).unwrap().contains(read));
    }
    let inside = vault.join("state.json");
    assert!(!run(&["--state", inside.to_str().unwrap()]).status.success());
    assert!(!inside.exists());
    let out = run(&["--no-default-excludes", "--json"]);
    let batch: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(batch["refs"].as_array().unwrap().len(), 2);
    assert_eq!(snapshot(&vault), before);
}
#[test]
fn obsidian_index_posts_refs_with_file_token() {
    let (dir, vault, lib) = synthetic_vault();
    let token = dir.path().join("token");
    fs::write(&token, "secret-test-token\n").unwrap();
    let (url, server) = one_request("{\"refsStored\":1}");
    let out = cli()
        .args([
            "obsidian-index",
            vault.to_str().unwrap(),
            "--library",
            lib.to_str().unwrap(),
            "--vault-name",
            "Notes",
            "--summary",
            "--server",
            &url,
            "--token-file",
            token.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let request = server.join().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(request.starts_with("POST /v1/library/notes-index "));
    assert!(request
        .to_lowercase()
        .contains("authorization: bearer secret-test-token"));
    let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["vault"], "Notes");
    assert_eq!(body["refs"][0]["bookId"], "bk_0000000000000001");
    assert!(String::from_utf8_lossy(&out.stdout).contains("refsStored"));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("secret-test-token"));
    let bad = cli()
        .args([
            "obsidian-index",
            vault.to_str().unwrap(),
            "--library",
            lib.to_str().unwrap(),
            "--server",
            &url,
        ])
        .output()
        .unwrap();
    assert!(!bad.status.success());
}
