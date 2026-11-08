//! Desktop E Ink emulator.
//!
//! [`SimDisplay`] keeps two images: the framebuffer the application draws into and the *panel*, which only
//! changes inside regions that were actually sent as updates. Anything drawn but never updated stays
//! invisible, exactly like on the EPDC, so tests can assert `stale_pixels() == 0` after each step. Every
//! update is logged with its region and waveform, and the [`RefreshPolicy`] decides when a partial update
//! is promoted to a full GC16 flash.
//!
//! Not emulated: waveform timing, the flash itself, real ghosting (temperature, content history),
//! power use and the controller's update queue/collisions.
use crate::display::{check_regions, Display, FrameMut, RefreshPolicy, UpdateRecord, WaveformMode};
use crate::geom::{GrayImage, Rect, Size};
use crate::input::{
    Gesture, GestureConfig, GestureRecognizer, Input, InputEvent, PowerEvent, TouchEvent, Wait,
};
use crate::Result;
use std::collections::VecDeque;

pub struct SimDisplay {
    fb: GrayImage,
    panel: GrayImage,
    pub policy: RefreshPolicy,
    log: Vec<UpdateRecord>,
    seq: u32,
}

impl SimDisplay {
    /// A white panel of `size` with the given refresh policy.
    pub fn new(size: Size, policy: RefreshPolicy) -> Self {
        Self {
            fb: GrayImage::new(size, 0xff),
            panel: GrayImage::new(size, 0xff),
            policy,
            log: Vec::new(),
            seq: 0,
        }
    }
    /// What the panel currently shows.
    pub fn panel(&self) -> &GrayImage {
        &self.panel
    }
    /// What has been drawn (visible or not).
    pub fn framebuffer(&self) -> &GrayImage {
        &self.fb
    }
    pub fn log(&self) -> &[UpdateRecord] {
        &self.log
    }
    pub fn take_log(&mut self) -> Vec<UpdateRecord> {
        std::mem::take(&mut self.log)
    }
    /// Pixels drawn into the framebuffer that no update has made visible.
    pub fn stale_pixels(&self) -> usize {
        self.fb
            .data
            .iter()
            .zip(&self.panel.data)
            .filter(|(a, b)| a != b)
            .count()
    }

    fn show(
        &mut self,
        region: Rect,
        mode: WaveformMode,
        full: bool,
        promoted: bool,
    ) -> UpdateRecord {
        let w = self.fb.size.w as usize;
        for y in region.y..region.bottom() {
            let s = y as usize * w + region.x as usize;
            let e = s + region.w as usize;
            self.panel.data[s..e].copy_from_slice(&self.fb.data[s..e]);
        }
        self.seq += 1;
        let r = UpdateRecord {
            seq: self.seq,
            region,
            mode,
            full,
            promoted,
        };
        self.log.push(r);
        r
    }
}

impl Display for SimDisplay {
    fn size(&self) -> Size {
        self.fb.size
    }
    fn frame_mut(&mut self) -> FrameMut<'_> {
        let size = self.fb.size;
        FrameMut {
            data: &mut self.fb.data,
            size,
            stride: size.w as usize,
        }
    }
    fn update_regions(
        &mut self,
        regions: &[Rect],
        mode: WaveformMode,
    ) -> Result<Vec<UpdateRecord>> {
        check_regions(regions, self.fb.size)?;
        let d = self.policy.decide(mode);
        if d.full {
            return Ok(vec![self.show(
                self.fb.size.rect(),
                d.mode,
                true,
                d.promoted,
            )]);
        }
        Ok(regions
            .iter()
            .map(|r| self.show(*r, d.mode, false, false))
            .collect())
    }
    fn full_refresh(&mut self) -> Result<UpdateRecord> {
        self.policy.note_full();
        Ok(self.show(self.fb.size.rect(), WaveformMode::Gc16, true, false))
    }
}

/// One scripted input: a raw touch event or an emulated power event (power button / wake).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimEvent {
    Touch(TouchEvent),
    Power(PowerEvent),
}

/// Replays raw touch events through a [`GestureRecognizer`] (emulated touch screen), interleaved with
/// emulated [`PowerEvent`]s. Timeouts are not emulated: the script is consumed in order and the input
/// reports [`Wait::Closed`] once it is exhausted.
pub struct ScriptedInput {
    events: VecDeque<SimEvent>,
    recognizer: GestureRecognizer,
}

impl ScriptedInput {
    pub fn new(events: impl IntoIterator<Item = TouchEvent>, config: GestureConfig) -> Self {
        Self::with_events(events.into_iter().map(SimEvent::Touch), config)
    }
    pub fn with_events(events: impl IntoIterator<Item = SimEvent>, config: GestureConfig) -> Self {
        Self {
            events: events.into_iter().collect(),
            recognizer: GestureRecognizer::new(config),
        }
    }
    /// Append an emulated power event (`sleep` / `wake` in a preview script).
    pub fn push_power(&mut self, ev: PowerEvent) {
        self.events.push_back(SimEvent::Power(ev));
    }
}

impl Input for ScriptedInput {
    /// Gestures only; power events in the script are skipped (use [`Input::next_event`] to see them).
    fn next_gesture(&mut self, _timeout_ms: Option<u32>) -> Result<Option<Gesture>> {
        while let Some(ev) = self.events.pop_front() {
            if let SimEvent::Touch(t) = ev {
                if let Some(g) = self.recognizer.feed(t) {
                    return Ok(Some(g));
                }
            }
        }
        Ok(None)
    }
    fn next_event(&mut self, _timeout_ms: Option<u32>) -> Result<Wait> {
        while let Some(ev) = self.events.pop_front() {
            match ev {
                SimEvent::Power(p) => return Ok(Wait::Event(InputEvent::Power(p))),
                SimEvent::Touch(t) => {
                    if let Some(g) = self.recognizer.feed(t) {
                        return Ok(Wait::Event(InputEvent::Gesture(g)));
                    }
                }
            }
        }
        Ok(Wait::Closed)
    }
}

/// Image dumps: binary PGM (P5) and a minimal grayscale PNG (zlib via `flate2`, no PNG crate).
pub mod dump {
    use crate::geom::GrayImage;
    use std::io::{self, Write};

    pub fn write_pgm<W: Write>(mut w: W, img: &GrayImage) -> io::Result<()> {
        write!(w, "P5\n{} {}\n255\n", img.size.w, img.size.h)?;
        w.write_all(&img.data)
    }

    fn crc_table() -> [u32; 256] {
        let mut t = [0u32; 256];
        for (n, slot) in t.iter_mut().enumerate() {
            let mut c = n as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        t
    }

    /// CRC-32 (ISO-HDLC) as used by PNG chunks.
    pub fn crc32(parts: &[&[u8]]) -> u32 {
        let t = crc_table();
        let mut c = 0xffff_ffffu32;
        for p in parts {
            for &b in *p {
                c = t[((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8);
            }
        }
        c ^ 0xffff_ffff
    }

    fn chunk<W: Write>(w: &mut W, ty: &[u8; 4], data: &[u8]) -> io::Result<()> {
        w.write_all(&(data.len() as u32).to_be_bytes())?;
        w.write_all(ty)?;
        w.write_all(data)?;
        w.write_all(&crc32(&[ty, data]).to_be_bytes())
    }

    pub fn write_png<W: Write>(mut w: W, img: &GrayImage) -> io::Result<()> {
        w.write_all(b"\x89PNG\r\n\x1a\n")?;
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&img.size.w.to_be_bytes());
        ihdr.extend_from_slice(&img.size.h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 0, 0, 0, 0]); // 8-bit, grayscale, deflate, adaptive filter, no interlace
        chunk(&mut w, b"IHDR", &ihdr)?;
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
        let width = img.size.w as usize;
        let mut prev = vec![0u8; width];
        let mut line = vec![0u8; width + 1];
        for y in 0..img.size.h {
            // Filter type 2 ("Up") compresses text pages well and is trivial to apply.
            let row = img.row(y);
            line[0] = 2;
            for x in 0..width {
                line[x + 1] = row[x].wrapping_sub(prev[x]);
            }
            z.write_all(&line)?;
            prev.copy_from_slice(row);
        }
        chunk(&mut w, b"IDAT", &z.finish()?)?;
        chunk(&mut w, b"IEND", &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;
    use std::io::Read;

    fn draw(d: &mut SimDisplay, r: Rect, v: u8) {
        let size = d.size();
        let mut img = GrayImage::new(size, 0);
        img.data.copy_from_slice(&d.framebuffer().data);
        img.fill_rect(r, v);
        d.frame_mut().blit(&img, r);
    }

    #[test]
    fn updates_make_only_their_region_visible() {
        let mut d = SimDisplay::new(Size { w: 100, h: 80 }, RefreshPolicy::default());
        draw(&mut d, Rect::new(10, 10, 20, 5), 0);
        draw(&mut d, Rect::new(50, 50, 10, 10), 0);
        assert_eq!(d.stale_pixels(), 100 + 100);
        let r = d
            .update(Rect::new(10, 10, 20, 5), WaveformMode::Gl16)
            .unwrap();
        assert_eq!((r.seq, r.full, r.mode), (1, false, WaveformMode::Gl16));
        assert_eq!(d.stale_pixels(), 100);
        assert_eq!(d.panel().get(15, 12), 0);
        assert_eq!(d.panel().get(55, 55), 0xff);
        assert!(d.update(Rect::new(95, 0, 10, 1), WaveformMode::Du).is_err());
        assert!(d.update(Rect::default(), WaveformMode::Du).is_err());
    }

    #[test]
    fn policy_promotes_every_nth_partial_to_a_full_flash() {
        let mut d = SimDisplay::new(Size { w: 64, h: 64 }, RefreshPolicy::default());
        let small = Rect::new(0, 0, 8, 8);
        let kinds: Vec<(bool, bool)> = (0..14)
            .map(|_| {
                let r = d.update(small, WaveformMode::Gl16).unwrap();
                (r.full, r.promoted)
            })
            .collect();
        // six partials, then a promoted full flash, six partials, then another
        let expect: Vec<(bool, bool)> = (0..14)
            .map(|i| {
                if i % 7 == 6 {
                    (true, true)
                } else {
                    (false, false)
                }
            })
            .collect();
        assert_eq!(kinds, expect);
        let promoted = d.log()[6];
        assert_eq!(promoted.region, Rect::new(0, 0, 64, 64));
        assert_eq!(promoted.mode, WaveformMode::Gc16);
    }

    #[test]
    fn a_batch_counts_as_one_partial_refresh() {
        let mut d = SimDisplay::new(Size { w: 64, h: 64 }, RefreshPolicy::new(2));
        let rs = [Rect::new(0, 0, 8, 8), Rect::new(0, 50, 8, 8)];
        assert_eq!(d.update_regions(&rs, WaveformMode::Gl16).unwrap().len(), 2);
        assert_eq!(d.update_regions(&rs, WaveformMode::Gl16).unwrap().len(), 2);
        let third = d.update_regions(&rs, WaveformMode::Gl16).unwrap();
        assert_eq!(third.len(), 1);
        assert!(third[0].full && third[0].promoted);
        assert!(d.update_regions(&[], WaveformMode::Gl16).is_err());
    }

    #[test]
    fn explicit_full_refresh_resets_and_policy_is_configurable() {
        let mut p = RefreshPolicy::new(2);
        assert!(!p.decide(WaveformMode::Du).full);
        assert!(!p.decide(WaveformMode::A2).full);
        p.note_full();
        assert_eq!(p.partials_since_full(), 0);
        assert!(!p.decide(WaveformMode::Gl16).full);
        assert!(!p.decide(WaveformMode::Gl16).full);
        assert!(p.decide(WaveformMode::Gl16).full);
        let mut never = RefreshPolicy::new(0);
        assert!((0..100).all(|_| !never.decide(WaveformMode::Gl16).full));
    }

    #[test]
    fn pgm_and_png_dumps_round_trip() {
        let mut img = GrayImage::new(Size { w: 7, h: 5 }, 0xff);
        img.set(3, 2, 0);
        img.set(6, 4, 0x80);
        let mut pgm = Vec::new();
        dump::write_pgm(&mut pgm, &img).unwrap();
        assert!(pgm.starts_with(b"P5\n7 5\n255\n"));
        assert_eq!(&pgm[pgm.len() - 35..], img.data.as_slice());

        let mut png = Vec::new();
        dump::write_png(&mut png, &img).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        // walk chunks, check CRCs, inflate IDAT and undo the Up filter
        let mut i = 8;
        let mut idat = Vec::new();
        while i < png.len() {
            let len = u32::from_be_bytes(png[i..i + 4].try_into().unwrap()) as usize;
            let ty = &png[i + 4..i + 8];
            let data = &png[i + 8..i + 8 + len];
            let crc = u32::from_be_bytes(png[i + 8 + len..i + 12 + len].try_into().unwrap());
            assert_eq!(crc, dump::crc32(&[ty, data]));
            if ty == b"IDAT" {
                idat.extend_from_slice(data);
            }
            i += 12 + len;
        }
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(idat.as_slice())
            .read_to_end(&mut raw)
            .unwrap();
        let mut prev = vec![0u8; 7];
        let mut out = Vec::new();
        for row in raw.chunks(8) {
            assert_eq!(row[0], 2);
            let cur: Vec<u8> = row[1..]
                .iter()
                .zip(&prev)
                .map(|(a, b)| a.wrapping_add(*b))
                .collect();
            out.extend_from_slice(&cur);
            prev = cur;
        }
        assert_eq!(out, img.data);
        assert_eq!(dump::crc32(&[b"123456789"]), 0xcbf4_3926);
    }

    #[test]
    fn scripted_input_yields_gestures() {
        let mut inp = ScriptedInput::new(
            [
                TouchEvent::Down {
                    id: 0,
                    pos: Point::new(1000, 700),
                    t_ms: 0,
                },
                TouchEvent::Up { id: 0, t_ms: 60 },
                TouchEvent::Down {
                    id: 0,
                    pos: Point::new(100, 700),
                    t_ms: 500,
                },
                TouchEvent::Up { id: 0, t_ms: 560 },
            ],
            GestureConfig::default(),
        );
        assert_eq!(
            inp.next_gesture(None).unwrap(),
            Some(Gesture::Tap {
                pos: Point::new(1000, 700)
            })
        );
        assert!(inp.next_gesture(None).unwrap().is_some());
        assert_eq!(inp.next_gesture(None).unwrap(), None);
    }

    #[test]
    fn scripted_power_events_are_interleaved_with_gestures() {
        let tap = |t: u64| {
            [
                SimEvent::Touch(TouchEvent::Down {
                    id: 0,
                    pos: Point::new(900, 700),
                    t_ms: t,
                }),
                SimEvent::Touch(TouchEvent::Up {
                    id: 0,
                    t_ms: t + 50,
                }),
            ]
        };
        let mut events = tap(0).to_vec();
        events.push(SimEvent::Power(PowerEvent::SleepRequested));
        events.extend(tap(1000));
        let mut inp = ScriptedInput::with_events(events, GestureConfig::default());
        inp.push_power(PowerEvent::Wake);
        let g = Gesture::Tap {
            pos: Point::new(900, 700),
        };
        let seen: Vec<Wait> = (0..5).map(|_| inp.next_event(Some(10)).unwrap()).collect();
        assert_eq!(
            seen,
            vec![
                Wait::Event(InputEvent::Gesture(g)),
                Wait::Event(InputEvent::Power(PowerEvent::SleepRequested)),
                Wait::Event(InputEvent::Gesture(g)),
                Wait::Event(InputEvent::Power(PowerEvent::Wake)),
                Wait::Closed,
            ]
        );
    }
}
