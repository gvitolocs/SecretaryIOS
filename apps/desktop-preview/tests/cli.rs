//! End-to-end runs of the `ink-preview` binary on generated books.
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_ink-preview");
const FONT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../crates/ink-render/fixtures/LiberationSerif-Regular.ttf"
);

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-preview-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn book(dir: &Path) -> PathBuf {
    let mut md = String::from("# Test Book\n\n");
    for ch in 1..=3 {
        md.push_str(&format!("# Chapter {ch}\n\n"));
        for p in 0..30 {
            md.push_str(&format!(
                "Paragraph {p} of chapter {ch} has *some* words, enough of them to wrap over several \
                 lines of the emulated panel so that pages fill up and the reader must turn them. \
                 The quick brown fox jumps over the lazy dog, again and again and again.\n\n"
            ));
        }
    }
    let p = dir.join("book.md");
    std::fs::write(&p, md).unwrap();
    p
}

fn run(args: &[&str]) -> (i32, Value) {
    let out = Command::new(BIN).args(args).output().unwrap();
    let code = out.status.code().unwrap_or(-1);
    let json = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    if json.is_null() {
        eprintln!("stderr: {}", String::from_utf8_lossy(&out.stderr));
    }
    (code, json)
}

#[test]
fn renders_pages_and_reports_json() {
    let dir = tmp("render");
    let b = book(&dir);
    let out = dir.join("out");
    let (code, r) = run(&[
        b.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--font",
        FONT,
        "--pages",
        "4",
        "--format",
        "pgm",
    ]);
    assert_eq!(code, 0, "{r}");
    assert_eq!(r["panel"]["w"], 1072);
    assert!(r["pages"].as_u64().unwrap() > 6);
    assert_eq!(r["steps"].as_array().unwrap().len(), 4);
    assert!(r["page_turn_ms"]["mean"].as_f64().unwrap() >= 0.0);
    if cfg!(target_os = "linux") {
        assert!(r["peak_rss_kb"].as_u64().unwrap() > 0);
    }
    let first = &r["steps"][0];
    assert_eq!(first["updates"][0]["mode"], "GC16");
    assert_eq!(first["updates"][0]["full"], true);
    let turn = &r["steps"][1];
    assert_eq!(turn["updates"][0]["mode"], "GL16");
    assert!(turn["updated_fraction"].as_f64().unwrap() < 0.9);
    for n in 1..=4 {
        let f = out.join(format!("page-{n:04}.pgm"));
        let data = std::fs::read(&f).unwrap();
        assert!(data.starts_with(b"P5\n1072 1448\n255\n"), "{}", f.display());
        assert_eq!(data.len(), 17 + 1072 * 1448);
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn navigation_script_keeps_the_anchor_across_font_changes() {
    let dir = tmp("script");
    let b = book(&dir);
    let out = dir.join("out");
    let (code, r) = run(&[
        b.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--font",
        FONT,
        "--size",
        "1072x1448",
        "--script",
        "next,next,prev,font+,next,font-,font-,goto:3,full",
    ]);
    assert_eq!(code, 0, "{r}");
    assert_eq!(r["ok"], true);
    let checks = r["script_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 3);
    assert!(
        checks.iter().all(|c| c["anchor_survived"] == true),
        "{checks:?}"
    );
    let steps = r["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 10);
    assert!(steps.iter().all(|s| s["stale_pixels"] == 0));
    // font change = one full GC16 refresh; page turns = partial GL16
    assert_eq!(steps[4]["op"], "font+");
    assert_eq!(steps[4]["updates"][0]["full"], true);
    assert_eq!(steps[5]["updates"][0]["mode"], "GL16");
    assert_eq!(steps[8]["page"], 3);
    assert!(out.join("step-04-font_.png").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn drm_and_unknown_inputs_fail_cleanly() {
    let dir = tmp("errors");
    let p = dir.join("book.pdf");
    std::fs::write(&p, "%PDF-1.4").unwrap();
    let out = Command::new(BIN)
        .args([
            p.to_str().unwrap(),
            "--out",
            dir.to_str().unwrap(),
            "--font",
            FONT,
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unsupported"));
    let out = Command::new(BIN).arg("--bogus").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    std::fs::remove_dir_all(&dir).unwrap();
}
