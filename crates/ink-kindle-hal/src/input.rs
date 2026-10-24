//! Touch events and gesture recognition (tap, long-press, swipe) from raw single/multi-touch contacts.
use crate::geom::Point;
use crate::Result;

/// A raw contact event in panel coordinates (after [`crate::evdev::TouchTransform`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchEvent {
    Down { id: i32, pos: Point, t_ms: u64 },
    Move { id: i32, pos: Point, t_ms: u64 },
    Up { id: i32, t_ms: u64 },
}

impl TouchEvent {
    pub fn t_ms(&self) -> u64 {
        match *self {
            TouchEvent::Down { t_ms, .. }
            | TouchEvent::Move { t_ms, .. }
            | TouchEvent::Up { t_ms, .. } => t_ms,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    Tap {
        pos: Point,
    },
    LongPress {
        pos: Point,
    },
    Swipe {
        dir: Direction,
        from: Point,
        to: Point,
        duration_ms: u64,
    },
}

/// Thresholds in panel pixels / milliseconds. Defaults are for a 300 dpi panel
/// (40 px ≈ 3.4 mm, 120 px ≈ 10 mm).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GestureConfig {
    /// Maximum travel for a tap or long-press.
    pub tap_slop: u32,
    /// Minimum travel for a swipe.
    pub swipe_min: u32,
    /// Hold time for a long-press.
    pub long_press_ms: u64,
}

impl Default for GestureConfig {
    fn default() -> Self {
        Self {
            tap_slop: 40,
            swipe_min: 120,
            long_press_ms: 600,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Contact {
    id: i32,
    start: Point,
    start_t: u64,
    last: Point,
    last_t: u64,
}

/// Single-finger gesture recognizer. A second finger cancels the gesture in progress (pinch and other
/// multi-finger gestures are not recognised yet); recognition restarts once every finger is lifted.
#[derive(Debug, Clone, Default)]
pub struct GestureRecognizer {
    pub config: GestureConfig,
    contacts: Vec<Contact>,
    cancelled: bool,
    moved: bool,
    long_fired: bool,
}

fn dist2(a: Point, b: Point) -> i64 {
    let dx = i64::from(a.x - b.x);
    let dy = i64::from(a.y - b.y);
    dx * dx + dy * dy
}

impl GestureRecognizer {
    pub fn new(config: GestureConfig) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }

    /// Fire a long-press while the finger is still down. Call with the current time when no event arrived.
    pub fn tick(&mut self, now_ms: u64) -> Option<Gesture> {
        let c = match self.contacts.as_slice() {
            [c] => *c,
            _ => return None,
        };
        if self.cancelled || self.moved || self.long_fired {
            return None;
        }
        if now_ms.saturating_sub(c.start_t) >= self.config.long_press_ms {
            self.long_fired = true;
            return Some(Gesture::LongPress { pos: c.start });
        }
        None
    }

    pub fn feed(&mut self, ev: TouchEvent) -> Option<Gesture> {
        let fired = self.tick(ev.t_ms());
        if fired.is_some() {
            // Deliver the long-press first; the event still updates state below.
            let _ = self.apply(ev);
            return fired;
        }
        self.apply(ev)
    }

    fn apply(&mut self, ev: TouchEvent) -> Option<Gesture> {
        let slop2 = i64::from(self.config.tap_slop).pow(2);
        match ev {
            TouchEvent::Down { id, pos, t_ms } => {
                if self.contacts.is_empty() {
                    self.cancelled = false;
                    self.moved = false;
                    self.long_fired = false;
                }
                self.contacts.retain(|c| c.id != id);
                self.contacts.push(Contact {
                    id,
                    start: pos,
                    start_t: t_ms,
                    last: pos,
                    last_t: t_ms,
                });
                if self.contacts.len() > 1 {
                    self.cancelled = true;
                }
                None
            }
            TouchEvent::Move { id, pos, t_ms } => {
                if let Some(c) = self.contacts.iter_mut().find(|c| c.id == id) {
                    c.last = pos;
                    c.last_t = t_ms;
                    if dist2(c.start, pos) > slop2 {
                        self.moved = true;
                    }
                }
                None
            }
            TouchEvent::Up { id, t_ms } => {
                let idx = self.contacts.iter().position(|c| c.id == id)?;
                let mut c = self.contacts.remove(idx);
                c.last_t = c.last_t.max(t_ms);
                if !self.contacts.is_empty() || self.cancelled || self.long_fired {
                    return None;
                }
                self.classify(c)
            }
        }
    }

    fn classify(&self, c: Contact) -> Option<Gesture> {
        let d2 = dist2(c.start, c.last);
        let duration_ms = c.last_t.saturating_sub(c.start_t);
        if !self.moved && d2 <= i64::from(self.config.tap_slop).pow(2) {
            return Some(if duration_ms >= self.config.long_press_ms {
                Gesture::LongPress { pos: c.start }
            } else {
                Gesture::Tap { pos: c.start }
            });
        }
        if d2 < i64::from(self.config.swipe_min).pow(2) {
            return None; // a short drag: neither tap nor swipe
        }
        let dx = c.last.x - c.start.x;
        let dy = c.last.y - c.start.y;
        let dir = if dx.abs() >= dy.abs() {
            if dx < 0 {
                Direction::Left
            } else {
                Direction::Right
            }
        } else if dy < 0 {
            Direction::Up
        } else {
            Direction::Down
        };
        Some(Gesture::Swipe {
            dir,
            from: c.start,
            to: c.last,
            duration_ms,
        })
    }
}

/// A source of gestures (the Kindle touch screen, or a scripted sequence in the emulator).
pub trait Input {
    /// Wait up to `timeout_ms` (`None` = forever) for the next gesture; `Ok(None)` on timeout / end of input.
    fn next_gesture(&mut self, timeout_ms: Option<u32>) -> Result<Option<Gesture>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: i32, y: i32) -> Point {
        Point::new(x, y)
    }
    fn run(events: &[TouchEvent]) -> Vec<Gesture> {
        let mut r = GestureRecognizer::default();
        events.iter().filter_map(|e| r.feed(*e)).collect()
    }
    fn down(id: i32, x: i32, y: i32, t: u64) -> TouchEvent {
        TouchEvent::Down {
            id,
            pos: p(x, y),
            t_ms: t,
        }
    }
    fn mv(id: i32, x: i32, y: i32, t: u64) -> TouchEvent {
        TouchEvent::Move {
            id,
            pos: p(x, y),
            t_ms: t,
        }
    }
    fn up(id: i32, t: u64) -> TouchEvent {
        TouchEvent::Up { id, t_ms: t }
    }

    #[test]
    fn tap_with_small_jitter() {
        let g = run(&[
            down(1, 900, 700, 0),
            mv(1, 905, 703, 30),
            mv(1, 910, 690, 60),
            up(1, 90),
        ]);
        assert_eq!(g, vec![Gesture::Tap { pos: p(900, 700) }]);
    }

    #[test]
    fn long_press_on_release_and_on_tick() {
        assert_eq!(
            run(&[down(1, 10, 20, 0), up(1, 800)]),
            vec![Gesture::LongPress { pos: p(10, 20) }]
        );
        let mut r = GestureRecognizer::default();
        assert_eq!(r.feed(down(1, 10, 20, 1000)), None);
        assert_eq!(r.tick(1300), None);
        assert_eq!(r.tick(1600), Some(Gesture::LongPress { pos: p(10, 20) }));
        assert_eq!(r.tick(1700), None, "fires once");
        assert_eq!(r.feed(up(1, 2000)), None, "no tap after a long-press");
    }

    #[test]
    fn long_press_fires_before_a_late_event() {
        let g = run(&[down(1, 10, 20, 0), mv(1, 12, 21, 700), up(1, 710)]);
        assert_eq!(g, vec![Gesture::LongPress { pos: p(10, 20) }]);
    }

    #[test]
    fn swipes_in_four_directions() {
        let cases = [
            ((800, 700), (200, 720), Direction::Left),
            ((200, 700), (800, 680), Direction::Right),
            ((500, 1200), (520, 300), Direction::Up),
            ((500, 300), (480, 1200), Direction::Down),
        ];
        for ((x0, y0), (x1, y1), dir) in cases {
            let g = run(&[
                down(3, x0, y0, 0),
                mv(3, (x0 + x1) / 2, (y0 + y1) / 2, 50),
                mv(3, x1, y1, 100),
                up(3, 120),
            ]);
            assert_eq!(
                g,
                vec![Gesture::Swipe {
                    dir,
                    from: p(x0, y0),
                    to: p(x1, y1),
                    duration_ms: 120
                }]
            );
        }
    }

    #[test]
    fn slow_drag_out_and_back_is_not_a_tap() {
        let g = run(&[
            down(1, 500, 500, 0),
            mv(1, 600, 500, 50),
            mv(1, 505, 500, 100),
            up(1, 150),
        ]);
        assert!(g.is_empty(), "{g:?}");
    }

    #[test]
    fn short_drag_is_ignored() {
        assert!(run(&[down(1, 500, 500, 0), mv(1, 580, 500, 50), up(1, 100)]).is_empty());
    }

    #[test]
    fn second_finger_cancels_and_recognition_resumes() {
        let g = run(&[
            down(1, 100, 100, 0),
            down(2, 600, 600, 10),
            mv(2, 900, 900, 50),
            up(2, 80),
            up(1, 90),
            down(1, 300, 300, 500),
            up(1, 560),
        ]);
        assert_eq!(g, vec![Gesture::Tap { pos: p(300, 300) }]);
    }

    #[test]
    fn unknown_up_is_ignored() {
        assert!(run(&[up(7, 10)]).is_empty());
    }
}
