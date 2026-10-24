//! `ink-preview`: render a book through the emulated Kindle display and print a JSON report.
//!
//! ```text
//! ink-preview <book.epub|txt|md> --out DIR [--font PATH] [--font-bold PATH] [--font-italic PATH]
//!             [--pages N] [--size 1072x1448] [--font-px 40] [--format png|pgm] [--full-every 6]
//!             [--script "next,next,prev,font+,next"]
//! ```
//!
//! Without `--script` the first N pages are shown one after another (page turns, as on the device) and
//! dumped as images. With `--script` the steps are executed instead; every step records the refreshed
//! regions, and each font change checks that the reading anchor is still on the shown page.
//! Exit status: 0 ok, 1 error, 2 a script check failed (anchor lost or stale pixels on the panel).
use ink_kindle_hal::sim::{dump, SimDisplay};
use ink_kindle_hal::{Display, RefreshPolicy, Size, UpdateRecord, PW3_SIZE};
use ink_render::{Face, Fonts, Reader, StepReport, Style};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Instant;

const DEFAULT_FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/liberation/LiberationSerif-Regular.ttf",
    "/usr/share/fonts/liberation-serif/LiberationSerif-Regular.ttf",
    "/mnt/us/fonts/LiberationSerif-Regular.ttf",
];

struct Args {
    book: PathBuf,
    out: PathBuf,
    font: Option<PathBuf>,
    font_bold: Option<PathBuf>,
    font_italic: Option<PathBuf>,
    pages: usize,
    size: Size,
    font_px: u16,
    png: bool,
    full_every: u32,
    script: Option<String>,
}

fn usage() -> ! {
    eprintln!(
        "usage: ink-preview <book.epub|txt|md> --out DIR [--font PATH] [--font-bold PATH] [--font-italic PATH] \
         [--pages N] [--size WxH] [--font-px PX] [--format png|pgm] [--full-every N] [--script \"next,prev,font+,font-,full,goto:N\"]"
    );
    std::process::exit(1)
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut book = None;
    let mut a = Args {
        book: PathBuf::new(),
        out: PathBuf::new(),
        font: None,
        font_bold: None,
        font_italic: None,
        pages: 10,
        size: PW3_SIZE,
        font_px: Style::default().font_px,
        png: true,
        full_every: 6,
        script: None,
    };
    let mut out = None;
    while let Some(arg) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(val("--out")?)),
            "--font" => a.font = Some(val("--font")?.into()),
            "--font-bold" => a.font_bold = Some(val("--font-bold")?.into()),
            "--font-italic" => a.font_italic = Some(val("--font-italic")?.into()),
            "--pages" => {
                a.pages = val("--pages")?
                    .parse()
                    .map_err(|e| format!("--pages: {e}"))?
            }
            "--font-px" => {
                a.font_px = val("--font-px")?
                    .parse()
                    .map_err(|e| format!("--font-px: {e}"))?
            }
            "--full-every" => {
                a.full_every = val("--full-every")?
                    .parse()
                    .map_err(|e| format!("--full-every: {e}"))?
            }
            "--size" => {
                let v = val("--size")?;
                let (w, h) = v.split_once('x').ok_or("--size wants WxH")?;
                a.size = Size {
                    w: w.parse().map_err(|e| format!("--size: {e}"))?,
                    h: h.parse().map_err(|e| format!("--size: {e}"))?,
                };
                if a.size.w < 200 || a.size.h < 200 || a.size.w > 8192 || a.size.h > 8192 {
                    return Err("--size out of range (200..8192)".into());
                }
            }
            "--format" => {
                a.png = match val("--format")?.as_str() {
                    "png" => true,
                    "pgm" => false,
                    f => return Err(format!("unknown format {f}")),
                }
            }
            "--script" => a.script = Some(val("--script")?),
            "-h" | "--help" => usage(),
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            s => book = Some(PathBuf::from(s)),
        }
    }
    a.book = book.ok_or("missing book path")?;
    a.out = out.ok_or("missing --out DIR")?;
    Ok(a)
}

/// Peak resident set size (VmHWM) in kB, Linux only.
fn peak_rss_kb() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines()
        .find(|l| l.starts_with("VmHWM:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn update_json(u: &UpdateRecord) -> Value {
    json!({
        "seq": u.seq,
        "x": u.region.x, "y": u.region.y, "w": u.region.w, "h": u.region.h,
        "mode": u.mode.name(),
        "full": u.full,
        "promoted": u.promoted,
    })
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn save(
    img: &ink_kindle_hal::GrayImage,
    dir: &Path,
    stem: &str,
    png: bool,
) -> Result<String, String> {
    let name = format!("{stem}.{}", if png { "png" } else { "pgm" });
    let f = std::fs::File::create(dir.join(&name)).map_err(|e| format!("{name}: {e}"))?;
    let w = std::io::BufWriter::new(f);
    if png {
        dump::write_png(w, img)
    } else {
        dump::write_pgm(w, img)
    }
    .map_err(|e| format!("{name}: {e}"))?;
    Ok(name)
}

fn step_json(op: &str, r: &StepReport, file: &str, stale: usize, panel_area: u64) -> Value {
    let area: u64 = r.updates.iter().map(|u| u.region.area()).sum();
    json!({
        "op": op,
        "page": r.page + 1,
        "pages": r.pages,
        "anchor": {"chapter": r.anchor.chapter, "offset": r.anchor.offset},
        "layout_ms": round2(r.layout_ms),
        "render_ms": round2(r.render_ms),
        "update_ms": round2(r.update_ms),
        "updates": r.updates.iter().map(update_json).collect::<Vec<_>>(),
        "updated_fraction": round2(area as f64 / panel_area as f64),
        "stale_pixels": stale,
        "file": file,
    })
}

fn run() -> Result<i32, String> {
    let a = parse_args().unwrap_or_else(|e| {
        eprintln!("{e}");
        usage()
    });
    std::fs::create_dir_all(&a.out).map_err(|e| format!("{}: {e}", a.out.display()))?;

    let font_path = a
        .font
        .clone()
        .or_else(|| std::env::var_os("INK_FONT").map(PathBuf::from))
        .or_else(|| DEFAULT_FONTS.iter().map(PathBuf::from).find(|p| p.exists()))
        .ok_or("no font found: pass --font PATH (any TTF/OTF, e.g. an OFL font)")?;
    let mut fonts = Fonts::load(&font_path).map_err(|e| e.to_string())?;
    for (face, p) in [(Face::Bold, &a.font_bold), (Face::Italic, &a.font_italic)] {
        if let Some(p) = p {
            fonts = fonts.with_face(face, p).map_err(|e| e.to_string())?;
        }
    }

    let t = Instant::now();
    let book = ink_epub::open(&a.book).map_err(|e| format!("{}: {e}", a.book.display()))?;
    let open_ms = t.elapsed().as_secs_f64() * 1000.0;
    let book_info = json!({
        "path": a.book.display().to_string(),
        "format": format!("{:?}", book.format).to_lowercase(),
        "title": book.title,
        "authors": book.authors,
        "chapters": book.chapters.len(),
        "chars": book.char_len(),
        "toc_entries": book.toc.len(),
        "warnings": book.warnings,
    });

    let style = Style {
        font_px: a.font_px,
        ..Style::default()
    };
    let display = SimDisplay::new(a.size, RefreshPolicy::new(a.full_every));
    let panel_area = a.size.area();
    let (mut reader, first) =
        Reader::open(book, fonts, style, display).map_err(|e| e.to_string())?;
    let mut status = 0;
    let mut steps = Vec::new();
    let f = save(reader.display().panel(), &a.out, "page-0001", a.png)?;
    steps.push(step_json(
        "open",
        &first,
        &f,
        reader.display().stale_pixels(),
        panel_area,
    ));

    let mut script_checks = Vec::new();
    if let Some(script) = &a.script {
        for (i, op) in script
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .enumerate()
        {
            let before = reader.current_anchor();
            let rep = match op {
                "next" => reader.next_page(),
                "prev" => reader.prev_page(),
                "font+" => reader.step_font(1).map(Some),
                "font-" => reader.step_font(-1).map(Some),
                "full" => {
                    let p = reader.page();
                    reader
                        .goto_page(p)
                        .and_then(|_| reader.display_mut().full_refresh().map_err(Into::into))
                        .map(|u| {
                            Some(StepReport {
                                page: p,
                                pages: reader.page_count(),
                                anchor: reader.current_anchor(),
                                updates: vec![u],
                                layout_ms: 0.0,
                                render_ms: 0.0,
                                update_ms: 0.0,
                            })
                        })
                }
                g if g.starts_with("goto:") => {
                    let n: usize = g[5..].parse().map_err(|e| format!("{g}: {e}"))?;
                    reader.goto_page(n.saturating_sub(1)).map(Some)
                }
                other => return Err(format!("unknown script op {other:?}")),
            }
            .map_err(|e| e.to_string())?;
            let stale = reader.display().stale_pixels();
            if stale != 0 {
                status = 2;
            }
            let Some(rep) = rep else {
                steps.push(json!({"op": op, "skipped": "at the first/last page"}));
                continue;
            };
            if op.starts_with("font") {
                let page = &reader.pagination().pages[rep.page];
                let survived = page.chapter == before.chapter
                    && page.start <= before.offset
                    && (before.offset < page.end || page.start == page.end);
                if !survived {
                    status = 2;
                }
                script_checks.push(json!({
                    "step": i + 1,
                    "op": op,
                    "font_px": reader.style().font_px,
                    "anchor_before": {"chapter": before.chapter, "offset": before.offset},
                    "page_after": rep.page + 1,
                    "page_range": [page.start, page.end],
                    "anchor_survived": survived,
                }));
            }
            let f = save(
                reader.display().panel(),
                &a.out,
                &format!("step-{:02}-{}", i + 1, op.replace([':', '+', '-'], "_")),
                a.png,
            )?;
            steps.push(step_json(op, &rep, &f, stale, panel_area));
        }
    } else {
        for n in 2..=a.pages.min(reader.page_count()) {
            let Some(rep) = reader.next_page().map_err(|e| e.to_string())? else {
                break;
            };
            let f = save(
                reader.display().panel(),
                &a.out,
                &format!("page-{n:04}"),
                a.png,
            )?;
            steps.push(step_json(
                "next",
                &rep,
                &f,
                reader.display().stale_pixels(),
                panel_area,
            ));
        }
    }

    let times: Vec<f64> = steps
        .iter()
        .filter(|s| s["op"] == "next" || s["op"] == "prev")
        .filter_map(|s| Some(s["render_ms"].as_f64()? + s["update_ms"].as_f64()?))
        .collect();
    let stats = |v: &[f64]| {
        if v.is_empty() {
            return Value::Null;
        }
        let mut s = v.to_vec();
        s.sort_by(f64::total_cmp);
        json!({
            "count": s.len(),
            "mean": round2(s.iter().sum::<f64>() / s.len() as f64),
            "median": round2(s[s.len() / 2]),
            "max": round2(s[s.len() - 1]),
        })
    };
    let log = reader.display().log();
    let report = json!({
        "book": book_info,
        "panel": {"w": a.size.w, "h": a.size.h},
        "font": {"path": font_path.display().to_string(), "px": reader.style().font_px},
        "full_every": a.full_every,
        "open_ms": round2(open_ms),
        "paginate_ms": round2(first.layout_ms),
        "pages": reader.page_count(),
        "page_turn_ms": stats(&times),
        "peak_rss_kb": peak_rss_kb(),
        "updates_total": log.len(),
        "full_refreshes": log.iter().filter(|u| u.full).count(),
        "glyphs_cached": reader.fonts.cached_glyphs(),
        "steps": steps,
        "script_checks": script_checks,
        "ok": status == 0,
        "note": "E Ink waveform timing, flashing and power are not emulated; times are CPU-side only on this host.",
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
    );
    Ok(status)
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("ink-preview: {e}");
            std::process::exit(1);
        }
    }
}
