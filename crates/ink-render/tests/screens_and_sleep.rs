//! System screens (splash / sleep picture) and the sleep/resume session on the emulated panel.
//! Pictures are synthetic and generated here; the owner's picture is never used in tests.
use ink_epub::text::from_markdown;
use ink_epub::Book;
use ink_kindle_hal::sim::SimDisplay;
use ink_kindle_hal::{
    Display, FrameMut, Gesture, GrayImage, Input, InputEvent, Point, PowerEvent, Rect,
    RefreshPolicy, Size, UpdateRecord, Wait, WaveformMode, PW3_SIZE,
};
use ink_render::screens::{self, quantize16, render_screen};
use ink_render::session::{run, show_screen};
use ink_render::{
    Anchor, AnchorStore, Background, Clock, Fonts, MemoryAnchorStore, Outcome, ScreenKind,
    ScreenSource, Session, SessionConfig, SessionState, SleepReason, Style, SystemScreens,
    DEFAULT_IDLE_MS,
};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const FONT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/LiberationSerif-Regular.ttf"
);

fn fonts() -> Fonts {
    Fonts::load(Path::new(FONT)).expect("fixture font")
}

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-screens-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Write a synthetic PNG with the `png` crate.
fn write_png(path: &Path, w: u32, h: u32, color: png::ColorType, data: &[u8]) {
    let f = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    enc.set_color(color);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(data).unwrap();
}

/// A black-and-white "portrait": white paper, a black frame and a black disc, plus a gray ramp row block.
fn synthetic_gray(w: u32, h: u32) -> Vec<u8> {
    let mut v = vec![0xffu8; (w * h) as usize];
    let (cx, cy, r) = (w as i64 / 2, h as i64 / 2, w.min(h) as i64 / 4);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            let (dx, dy) = (x as i64 - cx, y as i64 - cy);
            if x < 4 || y < 4 || x >= w - 4 || y >= h - 4 || dx * dx + dy * dy <= r * r {
                v[i] = 0;
            } else if y < h / 8 {
                v[i] = (x * 255 / (w - 1)) as u8; // ramp: exercises quantisation
            }
        }
    }
    v
}

fn book() -> Book {
    let mut md = String::from("# Test\n\n");
    for ch in 1..=3 {
        md.push_str(&format!("# Chapter {ch}\n\n"));
        for p in 0..30 {
            md.push_str(&format!(
                "Paragraph {p} of chapter {ch} runs over several lines of the emulated panel so that \
                 pages fill up and the reader has to turn them, again and again and again.\n\n"
            ));
        }
    }
    from_markdown(md.as_bytes(), "Test")
}

#[test]
fn half_size_picture_is_doubled_exactly_and_quantised_to_16_levels() {
    let d = tmpdir("fit");
    let (w, h) = (PW3_SIZE.w / 2, PW3_SIZE.h / 2);
    let gray = synthetic_gray(w, h);
    let p = d.join("half.png");
    write_png(&p, w, h, png::ColorType::Grayscale, &gray);
    let s = SystemScreens {
        splash: Some(p.clone()),
        sleep: vec![p.clone()],
        ..SystemScreens::default()
    };
    let (img, src) = render_screen(&s, ScreenKind::Sleep, 0, PW3_SIZE, &mut fonts());
    assert_eq!(src, ScreenSource::Image(p));
    assert_eq!(img.size, PW3_SIZE);
    for y in (0..PW3_SIZE.h).step_by(7) {
        for x in (0..PW3_SIZE.w).step_by(5) {
            let want = quantize16(gray[((y / 2) * w + x / 2) as usize]);
            assert_eq!(img.get(x, y), want, "({x},{y}) nearest x2 then quantised");
        }
    }
    assert!(
        img.data.iter().all(|&v| v % 17 == 0),
        "only the 16 EPDC levels"
    );
    let mut levels: Vec<u8> = img.data.clone();
    levels.sort_unstable();
    levels.dedup();
    assert_eq!(levels.len(), 16, "the ramp uses every level");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn large_rgba_landscape_is_letterboxed_on_black_with_alpha_composited() {
    let d = tmpdir("rgba");
    let (w, h) = (2144u32, 1072u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for _x in 0..w {
            // top half opaque pure red (luma 76), bottom half fully transparent
            if y < h / 2 {
                rgba.extend_from_slice(&[255, 0, 0, 255]);
            } else {
                rgba.extend_from_slice(&[255, 255, 255, 0]);
            }
        }
    }
    let p = d.join("wide.png");
    write_png(&p, w, h, png::ColorType::Rgba, &rgba);
    let s = SystemScreens {
        sleep: vec![p],
        background: Background::Black,
        ..SystemScreens::default()
    };
    let (img, src) = render_screen(&s, ScreenKind::Sleep, 0, PW3_SIZE, &mut fonts());
    assert!(matches!(src, ScreenSource::Image(_)));
    let r = screens::fit_rect(Size { w, h }, PW3_SIZE);
    assert_eq!(
        r,
        Rect::new(0, 456, 1072, 536),
        "full width, centred vertically"
    );
    assert_eq!(img.get(500, 10), 0, "black bar above");
    assert_eq!(img.get(500, 1440), 0, "black bar below");
    assert_eq!(
        img.get(500, 456 + 10),
        quantize16(76),
        "red -> Rec.601 luma"
    );
    assert_eq!(img.get(500, 456 + 500), 0, "transparent -> background");
    std::fs::remove_dir_all(&d).unwrap();
}

fn has_ink(img: &GrayImage) -> bool {
    img.data.iter().any(|&v| v < 0x80)
}

#[test]
fn missing_or_corrupt_pictures_fall_back_to_the_text_screen() {
    let d = tmpdir("corrupt");
    let good = d.join("good.png");
    write_png(
        &good,
        64,
        64,
        png::ColorType::Grayscale,
        &synthetic_gray(64, 64),
    );
    let bytes = std::fs::read(&good).unwrap();
    let truncated = d.join("truncated.png");
    std::fs::write(&truncated, &bytes[..bytes.len() / 2]).unwrap();
    let garbage = d.join("garbage.png");
    std::fs::write(&garbage, b"\x89PNG\r\n\x1a\nthis is not a png at all").unwrap();
    let mut flipped = bytes.clone();
    let n = flipped.len();
    flipped[n - 30] ^= 0xff; // corrupt the compressed data / CRC
    let bad_crc = d.join("crc.png");
    std::fs::write(&bad_crc, &flipped).unwrap();
    let mut f = fonts();
    for p in [
        d.join("missing.png"),
        truncated,
        garbage,
        bad_crc,
        d.clone(), // a directory
    ] {
        let s = SystemScreens {
            sleep: vec![p.clone()],
            ..SystemScreens::default()
        };
        let (img, src) = render_screen(&s, ScreenKind::Sleep, 0, PW3_SIZE, &mut f);
        match &src {
            ScreenSource::Fallback { tried, reason } => {
                assert_eq!(tried.as_deref(), Some(p.as_path()));
                assert!(!reason.is_empty());
            }
            other => panic!("{}: expected fallback, got {other:?}", p.display()),
        }
        assert!(has_ink(&img), "text screen has text");
        assert!(img.data.iter().all(|&v| v % 17 == 0));
    }
    // no picture configured at all, black background: inverted text screen
    let s = SystemScreens {
        splash: None,
        background: Background::Black,
        ..SystemScreens::default()
    };
    let (img, src) = render_screen(&s, ScreenKind::Splash, 0, PW3_SIZE, &mut f);
    assert!(matches!(src, ScreenSource::Fallback { tried: None, .. }));
    let dark = img.data.iter().filter(|&&v| v == 0).count();
    assert!(dark > img.data.len() * 9 / 10, "mostly black");
    assert!(img.data.contains(&0xff), "white text");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn show_text_adds_a_caption_band() {
    let d = tmpdir("caption");
    let p = d.join("white.png");
    write_png(&p, 100, 100, png::ColorType::Grayscale, &[0xff; 100 * 100]);
    let mut s = SystemScreens {
        sleep: vec![p],
        ..SystemScreens::default()
    };
    let (plain, _) = render_screen(&s, ScreenKind::Sleep, 0, PW3_SIZE, &mut fonts());
    assert!(!has_ink(&plain), "picture alone by default");
    s.show_text = true;
    let (cap, _) = render_screen(&s, ScreenKind::Sleep, 0, PW3_SIZE, &mut fonts());
    let bottom = Rect::new(0, PW3_SIZE.h - 100, PW3_SIZE.w, 100);
    let ink_in =
        |r: Rect| (r.y..r.bottom()).any(|y| (r.x..r.right()).any(|x| cap.get(x, y) < 0x80));
    assert!(ink_in(bottom));
    assert!(!ink_in(Rect::new(0, 0, PW3_SIZE.w, PW3_SIZE.h - 100)));
    std::fs::remove_dir_all(&d).unwrap();
}

// ---------------------------------------------------------------------------------------------------
// Session: a display and a store that write into one shared event log, so ordering can be checked.

#[derive(Debug, Clone, PartialEq)]
enum Ev {
    Draw,
    Update { full: bool, mode: WaveformMode },
    Save(Anchor),
}

type Log = Rc<RefCell<Vec<Ev>>>;

struct LoggedDisplay {
    sim: SimDisplay,
    log: Log,
}

impl Display for LoggedDisplay {
    fn size(&self) -> Size {
        self.sim.size()
    }
    fn frame_mut(&mut self) -> FrameMut<'_> {
        self.log.borrow_mut().push(Ev::Draw);
        self.sim.frame_mut()
    }
    fn update_regions(
        &mut self,
        regions: &[Rect],
        mode: WaveformMode,
    ) -> ink_kindle_hal::Result<Vec<UpdateRecord>> {
        let v = self.sim.update_regions(regions, mode)?;
        for u in &v {
            self.log.borrow_mut().push(Ev::Update {
                full: u.full,
                mode: u.mode,
            });
        }
        Ok(v)
    }
    fn full_refresh(&mut self) -> ink_kindle_hal::Result<UpdateRecord> {
        let u = self.sim.full_refresh()?;
        self.log.borrow_mut().push(Ev::Update {
            full: true,
            mode: u.mode,
        });
        Ok(u)
    }
}

struct LoggedStore {
    log: Log,
    initial: Option<Anchor>,
    fail: bool,
}

impl AnchorStore for LoggedStore {
    fn load(&mut self) -> std::io::Result<Option<Anchor>> {
        Ok(self.initial)
    }
    fn save(&mut self, a: Anchor) -> std::io::Result<()> {
        if self.fail {
            return Err(std::io::Error::other("disk full"));
        }
        self.log.borrow_mut().push(Ev::Save(a));
        Ok(())
    }
}

fn pictures(d: &Path, n: usize) -> Vec<PathBuf> {
    (0..n)
        .map(|i| {
            let p = d.join(format!("pic{i}.png"));
            let mut g = synthetic_gray(268, 362);
            // a black band of a different height makes each picture different
            for v in g.iter_mut().take(268 * (10 + 20 * i)) {
                *v = 0;
            }
            write_png(&p, 268, 362, png::ColorType::Grayscale, &g);
            p
        })
        .collect()
}

fn start(
    screens: SystemScreens,
    initial: Option<Anchor>,
    fail: bool,
) -> (Session<LoggedDisplay, LoggedStore>, Log) {
    let log: Log = Rc::default();
    let display = LoggedDisplay {
        sim: SimDisplay::new(PW3_SIZE, RefreshPolicy::new(6)),
        log: log.clone(),
    };
    let store = LoggedStore {
        log: log.clone(),
        initial,
        fail,
    };
    let config = SessionConfig {
        style: Style::default(),
        screens,
        idle_ms: DEFAULT_IDLE_MS,
    };
    let (s, splash, first) =
        Session::start(display, fonts(), config, store, 0, || Ok(book())).unwrap();
    assert_eq!(splash.kind, ScreenKind::Splash);
    assert!(splash.update.full && splash.update.mode == WaveformMode::Gc16);
    assert!(first.updates.len() == 1 && first.updates[0].full);
    (s, log)
}

fn tap_right() -> InputEvent {
    InputEvent::Gesture(Gesture::Tap {
        pos: Point::new(1000, 700),
    })
}

fn full_gc16(e: &Ev) -> bool {
    *e == Ev::Update {
        full: true,
        mode: WaveformMode::Gc16,
    }
}

#[test]
fn splash_then_page_then_sleep_persists_before_drawing_and_resumes_on_the_same_page() {
    let d = tmpdir("session");
    let pics = pictures(&d, 2);
    let screens = SystemScreens {
        splash: Some(pics[0].clone()),
        sleep: pics.clone(),
        ..SystemScreens::default()
    };
    let (mut s, log) = start(screens, None, false);
    assert_eq!(s.history(), [SessionState::Splash, SessionState::Reading]);
    {
        let l = log.borrow();
        // splash: draw + GC16 flash, then the first page: draw + GC16 flash
        assert_eq!(l.len(), 4, "{l:?}");
        assert!(l[0] == Ev::Draw && full_gc16(&l[1]) && l[2] == Ev::Draw && full_gc16(&l[3]));
    }
    for t in 1..=3 {
        assert!(matches!(
            s.handle(tap_right(), t * 1000).unwrap(),
            Outcome::Page(_)
        ));
    }
    let page = s.reader().page();
    let anchor = s.reader().current_anchor();
    let panel_before = s.reader().display().sim.panel().clone();
    assert_eq!(page, 3);

    let mark = log.borrow().len();
    let Outcome::Slept(rep) = s
        .handle(InputEvent::Power(PowerEvent::SleepRequested), 5000)
        .unwrap()
    else {
        panic!("expected sleep")
    };
    assert_eq!(rep.reason, Some(SleepReason::PowerButton));
    assert_eq!(rep.saved_anchor, Some(anchor));
    assert_eq!(rep.source, ScreenSource::Image(pics[0].clone()));
    assert!(rep.update.full && rep.update.mode == WaveformMode::Gc16);
    assert_eq!(
        log.borrow()[mark..],
        [
            Ev::Save(anchor),
            Ev::Draw,
            Ev::Update {
                full: true,
                mode: WaveformMode::Gc16
            }
        ],
        "anchor saved before the sleep frame is drawn; one full GC16 flash"
    );
    assert_eq!(s.state(), SessionState::Sleeping);
    assert_eq!(s.reader().display().sim.stale_pixels(), 0);
    assert_ne!(s.reader().display().sim.panel(), &panel_before);

    // asleep: touches and repeated sleep requests do nothing, no idle deadline, wait forever
    let mark = log.borrow().len();
    assert_eq!(s.handle(tap_right(), 6000).unwrap(), Outcome::Ignored);
    assert_eq!(
        s.handle(InputEvent::Power(PowerEvent::SleepRequested), 6000)
            .unwrap(),
        Outcome::Ignored
    );
    assert_eq!(s.wait_timeout_ms(6000), None);
    assert_eq!(s.tick(u64::MAX / 2).unwrap(), Outcome::Ignored);
    assert_eq!(log.borrow().len(), mark, "nothing drawn while asleep");

    let Outcome::Resumed(rep) = s.handle(InputEvent::Power(PowerEvent::Wake), 9000).unwrap() else {
        panic!("expected resume")
    };
    assert_eq!(rep.page, page);
    assert_eq!(rep.anchor, anchor);
    assert!(
        rep.updates.len() == 1
            && full_gc16(&Ev::Update {
                full: rep.updates[0].full,
                mode: rep.updates[0].mode
            })
    );
    assert_eq!(
        s.reader().display().sim.panel(),
        &panel_before,
        "exact same page"
    );
    assert_eq!(s.reader().display().sim.stale_pixels(), 0);
    assert_eq!(
        s.history(),
        [
            SessionState::Splash,
            SessionState::Reading,
            SessionState::Sleeping,
            SessionState::Resuming,
            SessionState::Reading
        ]
    );
    // the next page turn after resume is an ordinary partial update again
    let Outcome::Page(p) = s.handle(tap_right(), 10_000).unwrap() else {
        panic!()
    };
    assert!(p
        .updates
        .iter()
        .all(|u| !u.full && u.mode == WaveformMode::Gl16));

    // second sleep rotates to the second picture
    let Outcome::Slept(rep2) = s
        .handle(InputEvent::Power(PowerEvent::SleepRequested), 11_000)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(rep2.source, ScreenSource::Image(pics[1].clone()));
    assert_eq!(s.sleeps(), 2);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn every_system_screen_update_is_one_full_gc16_flash() {
    let (mut s, log) = start(
        SystemScreens {
            splash: None,
            sleep: vec![],
            ..SystemScreens::default()
        },
        None,
        false,
    );
    for i in 0..5u64 {
        let t = i * 100_000;
        s.handle(tap_right(), t + 1).unwrap();
        let mark = log.borrow().len();
        let Outcome::Slept(r) = s.sleep(SleepReason::PowerButton).unwrap() else {
            panic!()
        };
        assert!(matches!(
            r.source,
            ScreenSource::Fallback { tried: None, .. }
        ));
        let mid = log.borrow().len();
        let Outcome::Resumed(_) = s.wake(t + 2).unwrap() else {
            panic!()
        };
        let l = log.borrow();
        let sleep_updates: Vec<&Ev> = l[mark..mid]
            .iter()
            .filter(|e| matches!(e, Ev::Update { .. }))
            .collect();
        let wake_updates: Vec<&Ev> = l[mid..]
            .iter()
            .filter(|e| matches!(e, Ev::Update { .. }))
            .collect();
        assert!(sleep_updates.len() == 1 && full_gc16(sleep_updates[0]));
        assert!(wake_updates.len() == 1 && full_gc16(wake_updates[0]));
    }
}

#[test]
fn saved_position_is_restored_at_start_and_store_failure_still_sleeps() {
    let anchor = Anchor {
        chapter: 2,
        offset: 0,
    };
    let (mut s, _log) = start(
        SystemScreens {
            splash: None,
            sleep: vec![],
            ..SystemScreens::default()
        },
        Some(anchor),
        true,
    );
    let p = s.reader().page();
    assert!(p > 0);
    assert_eq!(s.reader().pagination().pages[p].chapter, 2);
    assert_eq!(s.reader().current_anchor(), anchor);
    let Outcome::Slept(r) = s.sleep(SleepReason::Idle).unwrap() else {
        panic!()
    };
    assert_eq!(r.saved_anchor, None);
    assert!(r.persist_error.as_deref().unwrap().contains("disk full"));
    assert_eq!(s.state(), SessionState::Sleeping);
}

#[test]
fn idle_deadline_follows_activity_on_a_fake_clock() {
    let (mut s, _log) = start(
        SystemScreens {
            splash: None,
            sleep: vec![],
            ..SystemScreens::default()
        },
        None,
        false,
    );
    assert_eq!(s.idle_deadline_ms(), Some(DEFAULT_IDLE_MS));
    assert_eq!(s.wait_timeout_ms(0), Some(600_000));
    assert_eq!(s.wait_timeout_ms(1_000), Some(599_000));
    s.handle(tap_right(), 100_000).unwrap();
    assert_eq!(
        s.idle_deadline_ms(),
        Some(700_000),
        "activity moves the deadline"
    );
    assert_eq!(s.wait_timeout_ms(400_000), Some(300_000));
    assert_eq!(s.tick(699_999).unwrap(), Outcome::Ignored);
    // a wake while awake counts as activity too
    s.handle(InputEvent::Power(PowerEvent::Wake), 650_000)
        .unwrap();
    assert_eq!(s.idle_deadline_ms(), Some(1_250_000));
    assert_eq!(s.tick(1_249_999).unwrap(), Outcome::Ignored);
    let Outcome::Slept(r) = s.tick(1_250_000).unwrap() else {
        panic!("deadline reached")
    };
    assert_eq!(r.reason, Some(SleepReason::Idle));
    assert_eq!(s.wait_timeout_ms(1_250_000), None);
    // overdue deadline: zero wait, never negative
    s.wake(2_000_000).unwrap();
    assert_eq!(s.wait_timeout_ms(9_000_000), Some(0));
    // disabled timeout
    s.idle_ms = 0;
    assert_eq!(s.idle_deadline_ms(), None);
    assert_eq!(s.wait_timeout_ms(9_000_000), None);
    assert_eq!(s.tick(u64::MAX / 2).unwrap(), Outcome::Ignored);
}

/// Input on a fake clock: an event arrives at its time if that is within the timeout; otherwise the wait
/// times out and the clock moves by exactly the timeout. Records every timeout it was asked to wait.
struct FakeInput {
    clock: Rc<Cell<u64>>,
    events: VecDeque<(u64, InputEvent)>,
    waits: Vec<Option<u32>>,
}

impl Input for FakeInput {
    fn next_gesture(&mut self, _t: Option<u32>) -> ink_kindle_hal::Result<Option<Gesture>> {
        unreachable!("the session loop uses next_event")
    }
    fn next_event(&mut self, timeout_ms: Option<u32>) -> ink_kindle_hal::Result<Wait> {
        self.waits.push(timeout_ms);
        let now = self.clock.get();
        match (self.events.front().copied(), timeout_ms) {
            (Some((t, ev)), to) if to.is_none_or(|to| t <= now + u64::from(to)) => {
                self.events.pop_front();
                self.clock.set(now.max(t));
                Ok(Wait::Event(ev))
            }
            (_, Some(to)) => {
                self.clock.set(now + u64::from(to));
                Ok(Wait::TimedOut)
            }
            (None, None) => Ok(Wait::Closed),
            (Some(_), None) => unreachable!(),
        }
    }
}

struct FakeClock(Rc<Cell<u64>>);
impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }
}

#[test]
fn main_loop_blocks_for_exactly_the_remaining_idle_time() {
    let (mut s, log) = start(
        SystemScreens {
            splash: None,
            sleep: vec![],
            ..SystemScreens::default()
        },
        None,
        false,
    );
    let clock = Rc::new(Cell::new(0));
    let mut input = FakeInput {
        clock: clock.clone(),
        events: VecDeque::from([
            (1_000, tap_right()),
            (61_000, tap_right()),
            // nothing for > 10 min: idle sleep at 661_000
            (700_000, InputEvent::Power(PowerEvent::Wake)),
            (750_000, tap_right()),
            (760_000, InputEvent::Power(PowerEvent::SleepRequested)),
            (800_000, InputEvent::Power(PowerEvent::Wake)),
        ]),
        waits: vec![],
    };
    let mut outcomes = Vec::new();
    run(&mut s, &mut input, &FakeClock(clock.clone()), |o| {
        outcomes.push(match o {
            Outcome::Page(_) => "page".to_string(),
            Outcome::Slept(r) => format!("sleep:{}", r.reason.unwrap().name()),
            Outcome::Resumed(_) => "resume".into(),
            Outcome::Ignored => "ignored".into(),
        })
    })
    .unwrap();
    assert_eq!(
        outcomes,
        [
            "page",
            "page",
            "sleep:idle",
            "resume",
            "page",
            "sleep:power",
            "resume",
            "sleep:idle"
        ]
    );
    // One wait per event or timeout, each exactly the time left until the idle deadline (no polling),
    // and an unbounded wait while asleep.
    assert_eq!(
        input.waits,
        [
            Some(600_000), // t=0      deadline 600_000
            Some(600_000), // t=1_000  deadline 601_000
            Some(600_000), // t=61_000 deadline 661_000 -> times out
            None,          // asleep: wake at 700_000
            Some(600_000), // t=700_000 -> tap at 750_000
            Some(600_000), // t=750_000 -> sleep at 760_000
            None,          // asleep: wake at 800_000
            Some(600_000), // t=800_000 -> times out at 1_400_000, idle sleep
            None,          // asleep, input closed
        ]
    );
    assert_eq!(clock.get(), 1_400_000);
    let saves: Vec<Anchor> = log
        .borrow()
        .iter()
        .filter_map(|e| match e {
            Ev::Save(a) => Some(*a),
            _ => None,
        })
        .collect();
    assert_eq!(saves.len(), 3, "one save per sleep");
}

#[test]
fn memory_store_and_show_screen_work_on_a_plain_sim_display() {
    let mut d = SimDisplay::new(PW3_SIZE, RefreshPolicy::default());
    let rep = show_screen(
        &mut d,
        &mut fonts(),
        &SystemScreens {
            splash: Some("/nonexistent/lucina.png".into()),
            ..SystemScreens::default()
        },
        ScreenKind::Splash,
        0,
    )
    .unwrap();
    assert!(rep.source.describe().contains("/nonexistent/lucina.png"));
    assert_eq!(d.log().len(), 1);
    assert!(d.log()[0].full && d.log()[0].region == PW3_SIZE.rect());
    assert_eq!(d.stale_pixels(), 0);
    let mut m = MemoryAnchorStore::default();
    assert_eq!(m.load().unwrap(), None);
}
