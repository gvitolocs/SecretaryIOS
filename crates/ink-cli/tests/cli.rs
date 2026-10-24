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
