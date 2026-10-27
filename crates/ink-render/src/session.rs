//! The app-level session: splash at start-up, reading, the sleep screen and resume.
//!
//! ```text
//! Splash ──(book paginated, first page drawn)──▶ Reading ──(power button | idle timeout)──▶ Sleeping
//!                                                   ▲                                         │
//!                                                   └──────── Resuming ◀────────(wake)────────┘
//! ```
//!
//! * Every system-screen update (splash, sleep, resume) is one full-screen GC16 flash.
//! * Going to sleep writes the reading anchor to the [`AnchorStore`] **before** the sleep picture is drawn,
//!   so a suspend (or a crash, or a flat battery) right after the flash cannot lose the position.
//! * Resume redraws exactly the page that was shown (page and anchor unchanged) with a GC16 flash.
//! * The idle timeout is a deadline, not a poll: [`Session::wait_timeout_ms`] is the time left, and
//!   [`run`] blocks in [`Input::next_event`] for exactly that long.
//!
//! These screens apply only while Lucina Ink runs. The stock boot splash and the Amazon UI's own lock
//! screen are not touched (no firmware or system-partition changes).
use crate::layout::{Anchor, Style};
use crate::reader::{Reader, StepReport};
use crate::screens::{render_screen, ScreenKind, ScreenSource, SystemScreens};
use crate::{Error, Fonts, Result};
use ink_epub::Book;
use ink_kindle_hal::{Display, Input, InputEvent, PowerEvent, UpdateRecord, Wait};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Default idle time before the sleep screen: 10 minutes.
pub const DEFAULT_IDLE_MS: u64 = 10 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// The splash picture is up while the book is opened and paginated.
    Splash,
    Reading,
    /// The sleep picture is on the panel; touches are ignored until a wake event.
    Sleeping,
    /// Transient: the page is being redrawn after a wake.
    Resuming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepReason {
    PowerButton,
    Idle,
}

impl SleepReason {
    pub fn name(self) -> &'static str {
        match self {
            SleepReason::PowerButton => "power",
            SleepReason::Idle => "idle",
        }
    }
}

/// Where the reading position is persisted. Implementations must make `save` durable before returning.
pub trait AnchorStore {
    fn load(&mut self) -> io::Result<Option<Anchor>>;
    fn save(&mut self, anchor: Anchor) -> io::Result<()>;
}

/// Write `bytes` to `path` atomically: a temporary file in the same directory, `fsync`, `rename`, then a
/// best-effort `fsync` of the directory. Readers see the old or the new content, never a torn file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

const ANCHOR_MAGIC: &str = "lucina-ink-anchor 1";

/// The reading position of one book in a small text file (`lucina-ink-anchor 1\n<chapter> <offset>\n`),
/// written with [`write_atomic`]. Use one file per book.
#[derive(Debug, Clone)]
pub struct FileAnchorStore {
    path: PathBuf,
}

impl FileAnchorStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl AnchorStore for FileAnchorStore {
    fn load(&mut self) -> io::Result<Option<Anchor>> {
        let s = match std::fs::read_to_string(&self.path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let bad = || io::Error::new(io::ErrorKind::InvalidData, "not an anchor file");
        let mut lines = s.lines();
        if lines.next() != Some(ANCHOR_MAGIC) {
            return Err(bad());
        }
        let mut it = lines.next().ok_or_else(bad)?.split_whitespace();
        let mut num =
            || -> io::Result<u32> { it.next().ok_or_else(bad)?.parse().map_err(|_| bad()) };
        Ok(Some(Anchor {
            chapter: num()?,
            offset: num()?,
        }))
    }
    fn save(&mut self, a: Anchor) -> io::Result<()> {
        write_atomic(
            &self.path,
            format!("{ANCHOR_MAGIC}\n{} {}\n", a.chapter, a.offset).as_bytes(),
        )
    }
}

/// In-memory store (tests, emulator runs without a state file).
#[derive(Debug, Clone, Default)]
pub struct MemoryAnchorStore {
    pub initial: Option<Anchor>,
    /// Every anchor saved, in order.
    pub saved: Vec<Anchor>,
}

impl AnchorStore for MemoryAnchorStore {
    fn load(&mut self) -> io::Result<Option<Anchor>> {
        Ok(self.saved.last().copied().or(self.initial))
    }
    fn save(&mut self, a: Anchor) -> io::Result<()> {
        self.saved.push(a);
        Ok(())
    }
}

/// Milliseconds on a monotonic clock.
pub trait Clock {
    fn now_ms(&self) -> u64;
}

/// [`Instant`]-based clock (CLOCK_MONOTONIC on Linux), counting from its creation.
#[derive(Debug, Clone, Copy)]
pub struct SystemClock {
    start: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

/// One system screen as drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenReport {
    pub kind: ScreenKind,
    pub source: ScreenSource,
    /// The single full-screen GC16 update that showed it.
    pub update: UpdateRecord,
    /// Loading, fitting and quantising the picture, ms.
    pub render_ms: f64,
    /// Sleep only: why the session went to sleep.
    pub reason: Option<SleepReason>,
    /// Sleep only: the anchor written to the store before the screen was drawn.
    pub saved_anchor: Option<Anchor>,
    /// Sleep only: the store failed (the sleep screen is still shown).
    pub persist_error: Option<String>,
}

/// What [`Session::handle`] / [`Session::tick`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Nothing changed on the panel.
    Ignored,
    Page(StepReport),
    Slept(ScreenReport),
    Resumed(StepReport),
}

/// Draw system screen `kind` (the `n`-th sleep) full-screen with one GC16 flash.
pub fn show_screen<D: Display>(
    display: &mut D,
    fonts: &mut Fonts,
    screens: &SystemScreens,
    kind: ScreenKind,
    n: u64,
) -> Result<ScreenReport> {
    let t = Instant::now();
    let (img, source) = render_screen(screens, kind, n, display.size(), fonts);
    let render_ms = t.elapsed().as_secs_f64() * 1000.0;
    display.frame_mut().blit(&img, img.size.rect());
    let update = display.full_refresh()?;
    Ok(ScreenReport {
        kind,
        source,
        update,
        render_ms,
        reason: None,
        saved_anchor: None,
        persist_error: None,
    })
}

/// What a [`Session`] is started with.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionConfig {
    pub style: Style,
    pub screens: SystemScreens,
    /// Idle time before the sleep screen, ms; `0` disables the timeout. Default [`DEFAULT_IDLE_MS`].
    pub idle_ms: u64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            style: Style::default(),
            screens: SystemScreens::default(),
            idle_ms: DEFAULT_IDLE_MS,
        }
    }
}

pub struct Session<D: Display, S: AnchorStore> {
    reader: Reader<D>,
    pub screens: SystemScreens,
    store: S,
    state: SessionState,
    /// Idle time before the sleep screen, ms; `0` disables the timeout.
    pub idle_ms: u64,
    last_activity_ms: u64,
    sleeps: u64,
    history: Vec<SessionState>,
}

impl<D: Display, S: AnchorStore> Session<D, S> {
    /// Show the splash, then load the book (`load_book` runs while the splash is up), paginate it and show
    /// the saved position from `store` (or the start) with a full refresh.
    pub fn start(
        mut display: D,
        mut fonts: Fonts,
        config: SessionConfig,
        store: S,
        now_ms: u64,
        load_book: impl FnOnce() -> std::result::Result<Book, String>,
    ) -> Result<(Self, ScreenReport, StepReport)> {
        let splash = show_screen(
            &mut display,
            &mut fonts,
            &config.screens,
            ScreenKind::Splash,
            0,
        )?;
        let book = load_book().map_err(Error::Book)?;
        let (s, first) = Self::open_after_splash(display, fonts, config, store, now_ms, book)?;
        Ok((s, splash, first))
    }

    /// The second half of [`Session::start`], for callers that drew the splash themselves with
    /// [`show_screen`] (the emulator saves the splash frame in between).
    pub fn open_after_splash(
        display: D,
        fonts: Fonts,
        config: SessionConfig,
        mut store: S,
        now_ms: u64,
        book: Book,
    ) -> Result<(Self, StepReport)> {
        // An unreadable position file is not fatal: start at the beginning.
        let anchor = store.load().ok().flatten();
        let (reader, first) = Reader::open_at(book, fonts, config.style, display, anchor)?;
        let s = Self {
            reader,
            screens: config.screens,
            store,
            state: SessionState::Reading,
            idle_ms: config.idle_ms,
            last_activity_ms: now_ms,
            sleeps: 0,
            history: vec![SessionState::Splash, SessionState::Reading],
        };
        Ok((s, first))
    }

    pub fn state(&self) -> SessionState {
        self.state
    }
    /// Every state entered, in order (starts with `Splash, Reading`).
    pub fn history(&self) -> &[SessionState] {
        &self.history
    }
    pub fn reader(&self) -> &Reader<D> {
        &self.reader
    }
    pub fn store(&self) -> &S {
        &self.store
    }
    /// Sleep screens shown so far.
    pub fn sleeps(&self) -> u64 {
        self.sleeps
    }

    fn enter(&mut self, s: SessionState) {
        self.state = s;
        self.history.push(s);
    }

    /// When the session goes to sleep if nothing happens (reading only).
    pub fn idle_deadline_ms(&self) -> Option<u64> {
        (self.state == SessionState::Reading && self.idle_ms > 0)
            .then(|| self.last_activity_ms.saturating_add(self.idle_ms))
    }

    /// How long the main loop may block waiting for input: the time left until the idle deadline while
    /// reading, `None` (forever) while asleep or with the timeout disabled.
    pub fn wait_timeout_ms(&self, now_ms: u64) -> Option<u32> {
        self.idle_deadline_ms()
            .map(|d| d.saturating_sub(now_ms).min(u64::from(u32::MAX)) as u32)
    }

    /// Run a reader action as user activity (resets the idle deadline). Does nothing unless reading.
    pub fn act(
        &mut self,
        now_ms: u64,
        f: impl FnOnce(&mut Reader<D>) -> Result<Option<StepReport>>,
    ) -> Result<Option<StepReport>> {
        if self.state != SessionState::Reading {
            return Ok(None);
        }
        self.last_activity_ms = now_ms;
        f(&mut self.reader)
    }

    pub fn handle(&mut self, ev: InputEvent, now_ms: u64) -> Result<Outcome> {
        match ev {
            InputEvent::Gesture(g) => Ok(self
                .act(now_ms, |r| r.handle_gesture(g))?
                .map_or(Outcome::Ignored, Outcome::Page)),
            InputEvent::Power(PowerEvent::SleepRequested) => self.sleep(SleepReason::PowerButton),
            InputEvent::Power(PowerEvent::Wake) => self.wake(now_ms),
        }
    }

    /// Go to sleep if the idle deadline has passed.
    pub fn tick(&mut self, now_ms: u64) -> Result<Outcome> {
        match self.idle_deadline_ms() {
            Some(d) if now_ms >= d => self.sleep(SleepReason::Idle),
            _ => Ok(Outcome::Ignored),
        }
    }

    /// Persist the anchor, then show the next sleep picture with a GC16 flash.
    pub fn sleep(&mut self, reason: SleepReason) -> Result<Outcome> {
        if self.state != SessionState::Reading {
            return Ok(Outcome::Ignored);
        }
        let anchor = self.reader.current_anchor();
        let persist_error = self.store.save(anchor).err().map(|e| e.to_string());
        self.enter(SessionState::Sleeping);
        let n = self.sleeps;
        self.sleeps += 1;
        let (fonts, display) = self.reader.fonts_and_display();
        let mut rep = show_screen(display, fonts, &self.screens, ScreenKind::Sleep, n)?;
        rep.reason = Some(reason);
        rep.saved_anchor = persist_error.is_none().then_some(anchor);
        rep.persist_error = persist_error;
        Ok(Outcome::Slept(rep))
    }

    /// Redraw the page that was shown before the sleep (same page, same anchor) with a GC16 flash.
    pub fn wake(&mut self, now_ms: u64) -> Result<Outcome> {
        match self.state {
            SessionState::Sleeping => {}
            SessionState::Reading => {
                self.last_activity_ms = now_ms;
                return Ok(Outcome::Ignored);
            }
            _ => return Ok(Outcome::Ignored),
        }
        self.enter(SessionState::Resuming);
        let rep = self.reader.redraw_full()?;
        self.enter(SessionState::Reading);
        self.last_activity_ms = now_ms;
        Ok(Outcome::Resumed(rep))
    }
}

/// The main loop: block on input until the idle deadline, handle events, sleep on timeout. Returns when
/// the input is closed (end of a script; a device input never closes). `on` sees every non-ignored outcome.
pub fn run<D: Display, S: AnchorStore, I: Input, C: Clock>(
    session: &mut Session<D, S>,
    input: &mut I,
    clock: &C,
    mut on: impl FnMut(&Outcome),
) -> Result<()> {
    loop {
        let now = clock.now_ms();
        let o = session.tick(now)?;
        if o != Outcome::Ignored {
            on(&o);
        }
        let timeout = session.wait_timeout_ms(now);
        match input.next_event(timeout)? {
            Wait::Event(ev) => {
                let o = session.handle(ev, clock.now_ms())?;
                if o != Outcome::Ignored {
                    on(&o);
                }
            }
            Wait::TimedOut => {} // the next iteration's tick sleeps
            Wait::Closed => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ink-session-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn file_store_round_trips_atomically() {
        let d = tmpdir("store");
        let mut s = FileAnchorStore::new(d.join("pos"));
        assert_eq!(s.load().unwrap(), None, "missing file = no position");
        let a = Anchor {
            chapter: 3,
            offset: 1234,
        };
        s.save(a).unwrap();
        assert_eq!(s.load().unwrap(), Some(a));
        s.save(Anchor {
            chapter: 4,
            offset: 0,
        })
        .unwrap();
        assert_eq!(s.load().unwrap().unwrap().chapter, 4);
        let names: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            names,
            vec![std::ffi::OsString::from("pos")],
            "no temp file left"
        );
        std::fs::write(d.join("pos"), "garbage").unwrap();
        assert!(s.load().is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn memory_store_returns_the_last_save() {
        let mut m = MemoryAnchorStore {
            initial: Some(Anchor::default()),
            saved: vec![],
        };
        assert_eq!(m.load().unwrap(), Some(Anchor::default()));
        let a = Anchor {
            chapter: 1,
            offset: 2,
        };
        m.save(a).unwrap();
        assert_eq!(m.load().unwrap(), Some(a));
    }
}
