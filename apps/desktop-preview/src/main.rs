//! `ink-preview`: render a book through the emulated Kindle display and print a JSON report.
//!
//! ```text
//! ink-preview <book.epub|txt|md> --out DIR [--font PATH] [--font-bold PATH] [--font-italic PATH]
//!             [--pages N] [--size 1072x1448] [--font-px 40] [--format png|pgm] [--full-every 6]
//!             [--script "next,next,prev,font+,next,sleep,wake,idle 700"]
//!             [--splash PNG] [--sleep-image PNG]... [--screen-bg white|black] [--screen-text]
//!             [--idle SECS] [--state FILE]
//! ```
//!
//! The app's splash screen is drawn first (saved as `splash.png`), then the book opens. Without `--script`
//! the first N pages are shown one after another (page turns, as on the device) and dumped as images. With
//! `--script` the steps are executed instead; every step records the refreshed regions, and each font
//! change checks that the reading anchor is still on the shown page.
//!
//! Script verbs: `next`, `prev`, `font+`, `font-`, `full`, `goto:N`, `sleep` (power button), `wake`, and
//! `idle SECS` (`idle:SECS`): let SECS seconds pass without input on the emulated clock, which shows the
//! sleep screen once the idle timeout (`--idle`, default 600 s) is reached. Every other step takes one
//! emulated second. Each sleep frame is also written to `sleep-frame.png`, and each wake checks that the
//! reader came back to the same page and anchor.
//!
//! Without `--splash`/`--sleep-image` the device paths are used (`/mnt/us/screensaver/lucina.png`); on a
//! desktop they are missing and the generated text screen is shown instead. `--splash` defaults to the
//! first `--sleep-image`. `--state FILE` keeps the reading position in FILE (atomic writes) across runs;
//! without it the position lives in memory.
//! Exit status: 0 ok, 1 error, 2 a script check failed (anchor lost, wrong page after wake, or stale
//! pixels on the panel).
use ink_kindle_hal::sim::{dump, SimDisplay};
use ink_kindle_hal::{Display, PowerEvent, RefreshPolicy, Size, UpdateRecord, PW3_SIZE};
use ink_render::screens::DEVICE_SCREENSAVER;
use ink_render::session::show_screen;
use ink_render::{
    Anchor, AnchorStore, Background, Face, FileAnchorStore, Fonts, MemoryAnchorStore, Outcome,
    ScreenKind, ScreenReport, ScreenSource, Session, SessionConfig, SessionState, StepReport,
    Style, SystemScreens, DEFAULT_IDLE_MS,
};
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
    splash: Option<PathBuf>,
    sleep_images: Vec<PathBuf>,
    background: Background,
    screen_text: bool,
    idle_secs: u64,
    state: Option<PathBuf>,
}

/// Reading position: in memory, or in a file given with `--state`.
enum Store {
    Memory(MemoryAnchorStore),
    File(FileAnchorStore),
}

impl AnchorStore for Store {
    fn load(&mut self) -> std::io::Result<Option<Anchor>> {
        match self {
            Store::Memory(m) => m.load(),
            Store::File(f) => f.load(),
        }
    }
    fn save(&mut self, a: Anchor) -> std::io::Result<()> {
        match self {
            Store::Memory(m) => m.save(a),
            Store::File(f) => f.save(a),
        }
    }
}

fn usage() -> ! {
    eprintln!(
        "usage: ink-preview <book.epub|txt|md> --out DIR [--font PATH] [--font-bold PATH] [--font-italic PATH] \
         [--pages N] [--size WxH] [--font-px PX] [--format png|pgm] [--full-every N] \
         [--script \"next,prev,font+,font-,full,goto:N,sleep,wake,idle SECS\"] [--splash PNG] [--sleep-image PNG]... \
         [--screen-bg white|black] [--screen-text] [--idle SECS] [--state FILE]"
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
        splash: None,
        sleep_images: Vec::new(),
        background: Background::White,
        screen_text: false,
        idle_secs: DEFAULT_IDLE_MS / 1000,
        state: None,
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
            "--splash" => a.splash = Some(val("--splash")?.into()),
            "--sleep-image" => a.sleep_images.push(val("--sleep-image")?.into()),
            "--screen-bg" => {
                a.background = match val("--screen-bg")?.as_str() {
                    "white" => Background::White,
                    "black" => Background::Black,
                    b => return Err(format!("unknown --screen-bg {b} (white|black)")),
                }
            }
            "--screen-text" => a.screen_text = true,
            "--idle" => a.idle_secs = val("--idle")?.parse().map_err(|e| format!("--idle: {e}"))?,
            "--state" => a.state = Some(val("--state")?.into()),
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

fn source_json(src: &ScreenSource) -> Value {
    match src {
        ScreenSource::Image(p) => json!({"image": p.display().to_string(), "fallback": null}),
        ScreenSource::Fallback { tried, reason } => json!({
            "image": null,
            "tried": tried.as_ref().map(|p| p.display().to_string()),
            "fallback": reason,
        }),
    }
}

fn screen_json(op: &str, r: &ScreenReport, file: &str, stale: usize) -> Value {
    let mut v = json!({
        "op": op,
        "screen": r.kind.name(),
        "updates": [update_json(&r.update)],
        "render_ms": round2(r.render_ms),
        "stale_pixels": stale,
        "file": file,
    });
    if let Value::Object(m) = &mut v {
        if let Value::Object(src) = source_json(&r.source) {
            m.extend(src);
        }
        if r.kind == ScreenKind::Sleep {
            m.insert("reason".into(), json!(r.reason.map(|x| x.name())));
            m.insert(
                "saved_anchor".into(),
                json!(r
                    .saved_anchor
                    .map(|a| json!({"chapter": a.chapter, "offset": a.offset}))),
            );
            m.insert("persist_error".into(), json!(r.persist_error));
        }
    }
    v
}

/// Seconds for `idle SECS` / `idle:SECS`.
fn idle_arg(op: &str) -> Option<Result<u64, String>> {
    let rest = op.strip_prefix("idle")?;
    let rest = rest.trim_start_matches([' ', ':']).trim();
    Some(rest.parse().map_err(|e| format!("{op:?}: {e}")))
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

    let style = Style {
        font_px: a.font_px,
        ..Style::default()
    };
    let sleep = if a.sleep_images.is_empty() {
        vec![PathBuf::from(DEVICE_SCREENSAVER)]
    } else {
        a.sleep_images.clone()
    };
    let screens = SystemScreens {
        splash: Some(a.splash.clone().unwrap_or_else(|| sleep[0].clone())),
        sleep,
        background: a.background,
        show_text: a.screen_text,
    };
    let screens_info = json!({
        "splash": screens.splash.as_ref().map(|p| p.display().to_string()),
        "sleep": screens.sleep.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "background": format!("{:?}", screens.background).to_lowercase(),
        "show_text": screens.show_text,
        "idle_secs": a.idle_secs,
        "state": a.state.as_ref().map(|p| p.display().to_string()),
    });
    let mut display = SimDisplay::new(a.size, RefreshPolicy::new(a.full_every));
    let panel_area = a.size.area();
    // The splash goes up first and stays while the book is opened and paginated.
    let splash = show_screen(&mut display, &mut fonts, &screens, ScreenKind::Splash, 0)
        .map_err(|e| e.to_string())?;
    eprintln!("ink-preview: splash: {}", splash.source.describe());
    let f = save(display.panel(), &a.out, "splash", a.png)?;
    let splash_json = screen_json("splash", &splash, &f, display.stale_pixels());

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

    let store = match &a.state {
        Some(p) => Store::File(FileAnchorStore::new(p)),
        None => Store::Memory(MemoryAnchorStore::default()),
    };
    let config = SessionConfig {
        style,
        screens,
        idle_ms: a.idle_secs.saturating_mul(1000),
    };
    // Emulated clock (ms): every step takes a second, `idle SECS` lets SECS seconds pass.
    let mut now: u64 = 0;
    let (mut session, first) = Session::open_after_splash(display, fonts, config, store, now, book)
        .map_err(|e| e.to_string())?;
    let mut status = 0;
    let mut steps = Vec::new();
    let f = save(
        session.reader().display().panel(),
        &a.out,
        "page-0001",
        a.png,
    )?;
    steps.push(step_json(
        "open",
        &first,
        &f,
        session.reader().display().stale_pixels(),
        panel_area,
    ));
    let mut sleep_frames = Vec::new();
    // page and anchor when the last sleep began, to check the wake
    let mut before_sleep: Option<(usize, Anchor)> = None;

    let mut script_checks = Vec::new();
    if let Some(script) = &a.script {
        for (i, op) in script
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .enumerate()
        {
            let stem = format!(
                "step-{:02}-{}",
                i + 1,
                op.replace([':', '+', '-', ' '], "_")
            );
            // system-screen verbs
            let power = match op {
                "sleep" => Some(PowerEvent::SleepRequested),
                "wake" => Some(PowerEvent::Wake),
                _ => None,
            };
            let idle = idle_arg(op).transpose()?;
            if power.is_some() || idle.is_some() {
                let page_anchor = (session.reader().page(), session.reader().current_anchor());
                let (outcome, waited) = if let Some(secs) = idle {
                    // What the device's main loop would block for, then the time passes.
                    let wait = session.wait_timeout_ms(now);
                    now += secs * 1000;
                    (session.tick(now), Some(wait))
                } else {
                    now += 1000;
                    let ev = ink_kindle_hal::InputEvent::Power(power.unwrap());
                    (session.handle(ev, now), None)
                };
                let outcome = outcome.map_err(|e| e.to_string())?;
                let stale = session.reader().display().stale_pixels();
                if stale != 0 {
                    status = 2;
                }
                let mut step = match &outcome {
                    Outcome::Slept(r) => {
                        before_sleep = Some(page_anchor);
                        eprintln!("ink-preview: sleep screen: {}", r.source.describe());
                        let f = save(session.reader().display().panel(), &a.out, &stem, a.png)?;
                        let sf = save(
                            session.reader().display().panel(),
                            &a.out,
                            "sleep-frame",
                            a.png,
                        )?;
                        sleep_frames.push(json!({"step": i + 1, "file": f, "copy": sf}));
                        screen_json(op, r, &f, stale)
                    }
                    Outcome::Resumed(rep) => {
                        let f = save(session.reader().display().panel(), &a.out, &stem, a.png)?;
                        let mut v = step_json(op, rep, &f, stale, panel_area);
                        let (page, anchor) = before_sleep.take().unwrap_or(page_anchor);
                        let same = rep.page == page && rep.anchor == anchor;
                        let full = rep.updates.len() == 1 && rep.updates[0].full;
                        if !same || !full {
                            status = 2;
                        }
                        script_checks.push(json!({
                            "step": i + 1,
                            "op": op,
                            "page_before_sleep": page + 1,
                            "page_after_wake": rep.page + 1,
                            "same_page_and_anchor": same,
                            "full_gc16": full,
                        }));
                        if let Value::Object(m) = &mut v {
                            m.insert("screen".into(), json!("resume"));
                        }
                        v
                    }
                    Outcome::Page(_) | Outcome::Ignored => json!({
                        "op": op,
                        "skipped": match (op, session.state()) {
                            (_, SessionState::Sleeping) if idle.is_some() => "already asleep",
                            ("sleep", _) => "already asleep",
                            ("wake", _) => "not asleep",
                            _ => "idle timeout not reached",
                        },
                        "state": format!("{:?}", session.state()).to_lowercase(),
                    }),
                };
                if let (Some(w), Value::Object(m)) = (waited, &mut step) {
                    m.insert("idle_secs".into(), json!(idle));
                    m.insert("main_loop_wait_ms".into(), json!(w));
                }
                steps.push(step);
                continue;
            }

            now += 1000;
            let before = session.reader().current_anchor();
            let rep = match op {
                "next" => session.act(now, |r| r.next_page()),
                "prev" => session.act(now, |r| r.prev_page()),
                "font+" => session.act(now, |r| r.step_font(1).map(Some)),
                "font-" => session.act(now, |r| r.step_font(-1).map(Some)),
                "full" => session.act(now, |r| {
                    let p = r.page();
                    r.goto_page(p)?;
                    let u = r.display_mut().full_refresh()?;
                    Ok(Some(StepReport {
                        page: p,
                        pages: r.page_count(),
                        anchor: r.current_anchor(),
                        updates: vec![u],
                        layout_ms: 0.0,
                        render_ms: 0.0,
                        update_ms: 0.0,
                    }))
                }),
                g if g.starts_with("goto:") => {
                    let n: usize = g[5..].parse().map_err(|e| format!("{g}: {e}"))?;
                    session.act(now, |r| r.goto_page(n.saturating_sub(1)).map(Some))
                }
                other => return Err(format!("unknown script op {other:?}")),
            }
            .map_err(|e| e.to_string())?;
            let stale = session.reader().display().stale_pixels();
            if stale != 0 {
                status = 2;
            }
            let Some(rep) = rep else {
                let why = if session.state() == SessionState::Sleeping {
                    "asleep (only wake works)"
                } else {
                    "at the first/last page"
                };
                steps.push(json!({"op": op, "skipped": why}));
                continue;
            };
            if op.starts_with("font") {
                let page = &session.reader().pagination().pages[rep.page];
                let survived = page.chapter == before.chapter
                    && page.start <= before.offset
                    && (before.offset < page.end || page.start == page.end);
                if !survived {
                    status = 2;
                }
                script_checks.push(json!({
                    "step": i + 1,
                    "op": op,
                    "font_px": session.reader().style().font_px,
                    "anchor_before": {"chapter": before.chapter, "offset": before.offset},
                    "page_after": rep.page + 1,
                    "page_range": [page.start, page.end],
                    "anchor_survived": survived,
                }));
            }
            let f = save(session.reader().display().panel(), &a.out, &stem, a.png)?;
            steps.push(step_json(op, &rep, &f, stale, panel_area));
        }
    } else {
        for n in 2..=a.pages.min(session.reader().page_count()) {
            now += 1000;
            let Some(rep) = session
                .act(now, |r| r.next_page())
                .map_err(|e| e.to_string())?
            else {
                break;
            };
            let f = save(
                session.reader().display().panel(),
                &a.out,
                &format!("page-{n:04}"),
                a.png,
            )?;
            steps.push(step_json(
                "next",
                &rep,
                &f,
                session.reader().display().stale_pixels(),
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
    let reader = session.reader();
    let log = reader.display().log();
    let report = json!({
        "book": book_info,
        "screens": screens_info,
        "splash": splash_json,
        "sleep_frames": sleep_frames,
        "sleeps": session.sleeps(),
        "state": format!("{:?}", session.state()).to_lowercase(),
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
