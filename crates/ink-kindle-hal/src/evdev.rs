//! Pure decoding of Linux evdev touch reports into [`TouchEvent`]s.
//!
//! Supports the multitouch **protocol B** (slots + tracking ids, as used by the Kindle touch controllers)
//! and falls back to single-touch `ABS_X`/`ABS_Y`/`BTN_TOUCH` for devices without MT axes. Codes are the
//! kernel ABI from `include/uapi/linux/input-event-codes.h`; protocol semantics from the kernel document
//! `Documentation/input/multi-touch-protocol.rst`. No device I/O happens here, so it is unit-tested with
//! synthetic event streams; reading the device is in `kindle::EvdevInput`.
use crate::geom::{Point, Size};
use crate::input::TouchEvent;

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_ABS: u16 = 0x03;
pub const SYN_REPORT: u16 = 0;
pub const SYN_DROPPED: u16 = 3;
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_MT_SLOT: u16 = 0x2f;
pub const ABS_MT_POSITION_X: u16 = 0x35;
pub const ABS_MT_POSITION_Y: u16 = 0x36;
pub const ABS_MT_TRACKING_ID: u16 = 0x39;
pub const BTN_TOUCH: u16 = 0x14a;

const MAX_SLOTS: usize = 16;

/// Maps raw controller coordinates to panel pixels. The PW3 orientation/ranges are **not measured yet**:
/// read them on the device with `EVIOCGABS` (done by `kindle::EvdevInput::open`) and adjust the flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchTransform {
    /// Raw axis maxima (inclusive), before swapping.
    pub raw_max_x: i32,
    pub raw_max_y: i32,
    pub panel: Size,
    pub swap_xy: bool,
    pub mirror_x: bool,
    pub mirror_y: bool,
}

impl TouchTransform {
    pub fn identity(panel: Size) -> Self {
        Self {
            raw_max_x: panel.w as i32 - 1,
            raw_max_y: panel.h as i32 - 1,
            panel,
            swap_xy: false,
            mirror_x: false,
            mirror_y: false,
        }
    }
    pub fn apply(&self, x: i32, y: i32) -> Point {
        let (mut x, mut y, mx, my) = if self.swap_xy {
            (y, x, self.raw_max_y, self.raw_max_x)
        } else {
            (x, y, self.raw_max_x, self.raw_max_y)
        };
        let pw = self.panel.w as i64 - 1;
        let ph = self.panel.h as i64 - 1;
        x = (i64::from(x.clamp(0, mx.max(1))) * pw / i64::from(mx.max(1))) as i32;
        y = (i64::from(y.clamp(0, my.max(1))) * ph / i64::from(my.max(1))) as i32;
        if self.mirror_x {
            x = pw as i32 - x;
        }
        if self.mirror_y {
            y = ph as i32 - y;
        }
        Point::new(x, y)
    }
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    id: i32,
    x: i32,
    y: i32,
    reported_id: i32,
    moved: bool,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            id: -1,
            x: 0,
            y: 0,
            reported_id: -1,
            moved: false,
        }
    }
}

/// Stateful decoder: feed every `(type, code, value)` and collect events emitted at each `SYN_REPORT`.
#[derive(Debug, Clone)]
pub struct MtDecoder {
    pub transform: TouchTransform,
    slots: [Slot; MAX_SLOTS],
    slot: usize,
    mt_seen: bool,
    dropping: bool,
    st_touch: bool,
    st_reported: bool,
    st_x: i32,
    st_y: i32,
    st_moved: bool,
}

impl MtDecoder {
    pub fn new(transform: TouchTransform) -> Self {
        Self {
            transform,
            slots: [Slot::default(); MAX_SLOTS],
            slot: 0,
            mt_seen: false,
            dropping: false,
            st_touch: false,
            st_reported: false,
            st_x: 0,
            st_y: 0,
            st_moved: false,
        }
    }

    pub fn push(&mut self, ty: u16, code: u16, value: i32, t_ms: u64, out: &mut Vec<TouchEvent>) {
        match (ty, code) {
            (EV_SYN, SYN_DROPPED) => {
                // Buffer overrun: lift every contact so the recogniser cannot get stuck and ignore events
                // until the next SYN_REPORT. Contacts still down are picked up again when they get a new
                // tracking id (a full resync would query EVIOCGMTSLOTS on the device).
                self.release_all(t_ms, out);
                self.dropping = true;
            }
            (EV_SYN, SYN_REPORT) => {
                if self.dropping {
                    self.dropping = false;
                    return;
                }
                self.report(t_ms, out);
            }
            _ if self.dropping => {}
            (EV_ABS, ABS_MT_SLOT) => {
                self.mt_seen = true;
                self.slot = (value.max(0) as usize).min(MAX_SLOTS - 1);
            }
            (EV_ABS, ABS_MT_TRACKING_ID) => {
                self.mt_seen = true;
                self.slots[self.slot].id = value;
            }
            (EV_ABS, ABS_MT_POSITION_X) => {
                self.mt_seen = true;
                let s = &mut self.slots[self.slot];
                s.x = value;
                s.moved = true;
            }
            (EV_ABS, ABS_MT_POSITION_Y) => {
                self.mt_seen = true;
                let s = &mut self.slots[self.slot];
                s.y = value;
                s.moved = true;
            }
            (EV_ABS, ABS_X) => {
                self.st_x = value;
                self.st_moved = true;
            }
            (EV_ABS, ABS_Y) => {
                self.st_y = value;
                self.st_moved = true;
            }
            (EV_KEY, BTN_TOUCH) => self.st_touch = value != 0,
            _ => {}
        }
    }

    fn release_all(&mut self, t_ms: u64, out: &mut Vec<TouchEvent>) {
        for s in self.slots.iter_mut() {
            if s.reported_id >= 0 {
                out.push(TouchEvent::Up {
                    id: s.reported_id,
                    t_ms,
                });
            }
            *s = Slot::default();
        }
        if self.st_reported {
            out.push(TouchEvent::Up { id: 0, t_ms });
            self.st_reported = false;
        }
        self.st_touch = false;
    }

    fn report(&mut self, t_ms: u64, out: &mut Vec<TouchEvent>) {
        if self.mt_seen {
            for s in self.slots.iter_mut() {
                let pos = self.transform.apply(s.x, s.y);
                if s.reported_id >= 0 && s.id != s.reported_id {
                    out.push(TouchEvent::Up {
                        id: s.reported_id,
                        t_ms,
                    });
                    s.reported_id = -1;
                }
                if s.id >= 0 && s.reported_id < 0 {
                    out.push(TouchEvent::Down {
                        id: s.id,
                        pos,
                        t_ms,
                    });
                    s.reported_id = s.id;
                } else if s.id >= 0 && s.moved {
                    out.push(TouchEvent::Move {
                        id: s.id,
                        pos,
                        t_ms,
                    });
                }
                s.moved = false;
            }
            return;
        }
        let pos = self.transform.apply(self.st_x, self.st_y);
        match (self.st_touch, self.st_reported) {
            (true, false) => {
                out.push(TouchEvent::Down { id: 0, pos, t_ms });
                self.st_reported = true;
            }
            (true, true) if self.st_moved => out.push(TouchEvent::Move { id: 0, pos, t_ms }),
            (false, true) => {
                out.push(TouchEvent::Up { id: 0, t_ms });
                self.st_reported = false;
            }
            _ => {}
        }
        self.st_moved = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{Direction, Gesture, GestureRecognizer};

    const PANEL: Size = crate::PW3_SIZE;

    fn feed(d: &mut MtDecoder, evs: &[(u16, u16, i32)], t: u64) -> Vec<TouchEvent> {
        let mut out = Vec::new();
        for &(ty, code, v) in evs {
            d.push(ty, code, v, t, &mut out);
        }
        out
    }

    #[test]
    fn protocol_b_single_finger_swipe() {
        let mut d = MtDecoder::new(TouchTransform::identity(PANEL));
        let mut all = Vec::new();
        all.extend(feed(
            &mut d,
            &[
                (EV_ABS, ABS_MT_SLOT, 0),
                (EV_ABS, ABS_MT_TRACKING_ID, 42),
                (EV_ABS, ABS_MT_POSITION_X, 900),
                (EV_ABS, ABS_MT_POSITION_Y, 700),
                (EV_SYN, SYN_REPORT, 0),
            ],
            0,
        ));
        all.extend(feed(
            &mut d,
            &[(EV_ABS, ABS_MT_POSITION_X, 500), (EV_SYN, SYN_REPORT, 0)],
            40,
        ));
        all.extend(feed(
            &mut d,
            &[(EV_ABS, ABS_MT_POSITION_X, 150), (EV_SYN, SYN_REPORT, 0)],
            80,
        ));
        all.extend(feed(
            &mut d,
            &[(EV_ABS, ABS_MT_TRACKING_ID, -1), (EV_SYN, SYN_REPORT, 0)],
            100,
        ));
        assert_eq!(
            all,
            vec![
                TouchEvent::Down {
                    id: 42,
                    pos: Point::new(900, 700),
                    t_ms: 0
                },
                TouchEvent::Move {
                    id: 42,
                    pos: Point::new(500, 700),
                    t_ms: 40
                },
                TouchEvent::Move {
                    id: 42,
                    pos: Point::new(150, 700),
                    t_ms: 80
                },
                TouchEvent::Up { id: 42, t_ms: 100 },
            ]
        );
        let mut r = GestureRecognizer::default();
        let g: Vec<_> = all.into_iter().filter_map(|e| r.feed(e)).collect();
        assert!(matches!(
            g.as_slice(),
            [Gesture::Swipe {
                dir: Direction::Left,
                ..
            }]
        ));
    }

    #[test]
    fn protocol_b_two_slots_and_syn_dropped() {
        let mut d = MtDecoder::new(TouchTransform::identity(PANEL));
        let ev = feed(
            &mut d,
            &[
                (EV_ABS, ABS_MT_SLOT, 0),
                (EV_ABS, ABS_MT_TRACKING_ID, 1),
                (EV_ABS, ABS_MT_POSITION_X, 10),
                (EV_ABS, ABS_MT_POSITION_Y, 10),
                (EV_ABS, ABS_MT_SLOT, 1),
                (EV_ABS, ABS_MT_TRACKING_ID, 2),
                (EV_ABS, ABS_MT_POSITION_X, 20),
                (EV_ABS, ABS_MT_POSITION_Y, 20),
                (EV_SYN, SYN_REPORT, 0),
            ],
            5,
        );
        assert_eq!(ev.len(), 2);
        assert!(matches!(ev[1], TouchEvent::Down { id: 2, .. }));
        let ev = feed(
            &mut d,
            &[
                (EV_SYN, SYN_DROPPED, 0),
                (EV_ABS, ABS_MT_POSITION_X, 99),
                (EV_SYN, SYN_REPORT, 0),
            ],
            9,
        );
        assert_eq!(
            ev,
            vec![
                TouchEvent::Up { id: 1, t_ms: 9 },
                TouchEvent::Up { id: 2, t_ms: 9 }
            ]
        );
    }

    #[test]
    fn single_touch_fallback_tap() {
        let mut d = MtDecoder::new(TouchTransform::identity(PANEL));
        let mut ev = feed(
            &mut d,
            &[
                (EV_ABS, ABS_X, 300),
                (EV_ABS, ABS_Y, 400),
                (EV_KEY, BTN_TOUCH, 1),
                (EV_SYN, SYN_REPORT, 0),
            ],
            0,
        );
        ev.extend(feed(
            &mut d,
            &[(EV_KEY, BTN_TOUCH, 0), (EV_SYN, SYN_REPORT, 0)],
            50,
        ));
        let mut r = GestureRecognizer::default();
        let g: Vec<_> = ev.into_iter().filter_map(|e| r.feed(e)).collect();
        assert_eq!(
            g,
            vec![Gesture::Tap {
                pos: Point::new(300, 400)
            }]
        );
    }

    #[test]
    fn transform_scales_swaps_and_mirrors() {
        let t = TouchTransform {
            raw_max_x: 4095,
            raw_max_y: 4095,
            panel: PANEL,
            swap_xy: true,
            mirror_x: true,
            mirror_y: false,
        };
        // raw y -> panel x (mirrored), raw x -> panel y
        assert_eq!(t.apply(0, 0), Point::new(1071, 0));
        assert_eq!(t.apply(4095, 4095), Point::new(0, 1447));
        assert_eq!(
            TouchTransform::identity(PANEL).apply(5000, -3),
            Point::new(1071, 0)
        );
    }
}
