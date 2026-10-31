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

/// A synthetic black-and-white picture, half the panel size (never the owner's picture).
fn picture(dir: &Path) -> PathBuf {
    use ink_kindle_hal::{GrayImage, Rect, Size};
    let mut img = GrayImage::new(Size { w: 536, h: 724 }, 0xff);
    img.fill_rect(Rect::new(100, 150, 336, 424), 0);
    let p = dir.join("sleep.png");
    let f = std::fs::File::create(&p).unwrap();
    ink_kindle_hal::sim::dump::write_png(std::io::BufWriter::new(f), &img).unwrap();
    p
}

#[test]
fn sleep_wake_and_idle_script_renders_the_sleep_frame() {
    let dir = tmp("sleep");
    let b = book(&dir);
    let pic = picture(&dir);
    let out = dir.join("out");
    let state = dir.join("position");
    let (code, r) = run(&[
        b.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--font",
        FONT,
        "--sleep-image",
        pic.to_str().unwrap(),
        "--state",
        state.to_str().unwrap(),
        "--script",
        "next,sleep,wake,next,idle 700,next,wake",
    ]);
    assert_eq!(code, 0, "{r}");
    assert_eq!(r["ok"], true);
    // splash defaults to the sleep picture and is one full GC16 flash
    assert_eq!(r["splash"]["image"], pic.to_str().unwrap());
    assert_eq!(r["splash"]["updates"][0]["mode"], "GC16");
    assert_eq!(r["splash"]["updates"][0]["full"], true);
    assert!(out.join("splash.png").exists());
    let steps = r["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 8, "{steps:?}");
    let sleep = &steps[2];
    assert_eq!(sleep["op"], "sleep");
    assert_eq!(sleep["screen"], "sleep");
    assert_eq!(sleep["reason"], "power");
    assert_eq!(sleep["image"], pic.to_str().unwrap());
    assert_eq!(sleep["updates"].as_array().unwrap().len(), 1);
    assert_eq!(sleep["updates"][0]["mode"], "GC16");
    assert_eq!(sleep["updates"][0]["full"], true);
    assert_eq!(sleep["saved_anchor"], steps[1]["anchor"]);
    assert_eq!(sleep["stale_pixels"], 0);
    let wake = &steps[3];
    assert_eq!(wake["screen"], "resume");
    assert_eq!(wake["page"], steps[1]["page"]);
    assert_eq!(wake["updates"][0]["full"], true);
    let idle = &steps[5];
    assert_eq!(idle["op"], "idle 700");
    assert_eq!(idle["reason"], "idle");
    assert_eq!(
        idle["main_loop_wait_ms"], 600_000,
        "the loop blocks for the whole remaining idle time"
    );
    assert_eq!(steps[6]["skipped"], "asleep (only wake works)");
    assert_eq!(steps[7]["screen"], "resume");
    let checks = r["script_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 2);
    assert!(checks
        .iter()
        .all(|c| c["same_page_and_anchor"] == true && c["full_gc16"] == true));
    assert_eq!(r["sleeps"], 2);
    assert_eq!(r["sleep_frames"].as_array().unwrap().len(), 2);
    // the last sleep frame is also written as sleep-frame.png
    let png = std::fs::read(out.join("sleep-frame.png")).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert!(std::fs::read_to_string(&state)
        .unwrap()
        .starts_with("lucina-ink-anchor 1\n"));

    // a second run with the same state file opens at the saved page
    let out2 = dir.join("out2");
    let (code, r2) = run(&[
        b.to_str().unwrap(),
        "--out",
        out2.to_str().unwrap(),
        "--font",
        FONT,
        "--sleep-image",
        pic.to_str().unwrap(),
        "--state",
        state.to_str().unwrap(),
        "--pages",
        "1",
    ]);
    assert_eq!(code, 0, "{r2}");
    assert_eq!(r2["steps"][0]["page"], steps[4]["page"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn missing_device_picture_falls_back_to_the_text_screen() {
    let dir = tmp("fallback");
    let b = book(&dir);
    let out = dir.join("out");
    let (code, r) = run(&[
        b.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--font",
        FONT,
        "--screen-bg",
        "black",
        "--script",
        "sleep",
    ]);
    assert_eq!(code, 0, "{r}");
    if !Path::new("/mnt/us/screensaver/lucina.png").exists() {
        assert_eq!(r["splash"]["tried"], "/mnt/us/screensaver/lucina.png");
        assert!(r["splash"]["fallback"].as_str().unwrap().len() > 3);
        assert!(r["steps"][1]["fallback"].is_string());
    }
    assert_eq!(r["screens"]["background"], "black");
    std::fs::remove_dir_all(&dir).unwrap();
}
