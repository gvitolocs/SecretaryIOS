//! Zoom and pan of one picture in a viewport, as pure functions on [`View`].
//!
//! Coordinates are viewport points with the origin at the top left. `scale` is relative to the
//! fitted size (1.0 = the whole picture fits, letterboxed); `offset` is how far the picture's
//! centre is moved from the viewport's centre. The platforms feed gestures in and draw the
//! picture at [`View::rect`].

use serde::{Deserialize, Serialize};

pub const MAX_SCALE: f32 = 8.0;
/// Double tap on a fitted picture zooms to this.
pub const TAP_SCALE: f32 = 2.5;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub image_w: f32,
    pub image_h: f32,
    pub viewport_w: f32,
    pub viewport_h: f32,
    pub scale: f32,
    pub offset_x: f32,
    pub offset_y: f32,
}

/// Where to draw the picture: top-left corner and size, in viewport points.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl View {
    /// A picture fitted to the viewport.
    pub fn new(image_w: f32, image_h: f32, viewport_w: f32, viewport_h: f32) -> Self {
        Self { image_w, image_h, viewport_w, viewport_h, scale: 1.0, offset_x: 0.0, offset_y: 0.0 }
    }

    /// Points per image pixel when fitted.
    fn fit_factor(&self) -> f32 {
        if self.image_w <= 0.0 || self.image_h <= 0.0 {
            return 1.0;
        }
        (self.viewport_w / self.image_w).min(self.viewport_h / self.image_h)
    }

    /// The drawn size at the current scale.
    fn drawn(&self) -> (f32, f32) {
        let f = self.fit_factor() * self.scale;
        (self.image_w * f, self.image_h * f)
    }

    pub fn rect(&self) -> Rect {
        let (w, h) = self.drawn();
        Rect {
            x: (self.viewport_w - w) / 2.0 + self.offset_x,
            y: (self.viewport_h - h) / 2.0 + self.offset_y,
            w,
            h,
        }
    }

    /// Keep the picture covering as much of the viewport as it can: no panning past an edge, and
    /// centred along an axis where it is smaller than the viewport.
    fn clamp(mut self) -> Self {
        self.scale = self.scale.clamp(1.0, MAX_SCALE);
        let (w, h) = self.drawn();
        let max_x = ((w - self.viewport_w) / 2.0).max(0.0);
        let max_y = ((h - self.viewport_h) / 2.0).max(0.0);
        self.offset_x = self.offset_x.clamp(-max_x, max_x);
        self.offset_y = self.offset_y.clamp(-max_y, max_y);
        self
    }

    pub fn pan(self, dx: f32, dy: f32) -> Self {
        Self { offset_x: self.offset_x + dx, offset_y: self.offset_y + dy, ..self }.clamp()
    }

    /// Multiply the scale by `factor`, keeping the picture point under (`ax`, `ay`) in place.
    pub fn pinch(self, factor: f32, ax: f32, ay: f32) -> Self {
        let new_scale = (self.scale * factor).clamp(1.0, MAX_SCALE);
        let k = new_scale / self.scale;
        // The anchor relative to the picture's centre scales with the picture.
        let cx = self.viewport_w / 2.0 + self.offset_x;
        let cy = self.viewport_h / 2.0 + self.offset_y;
        let offset_x = self.offset_x + (ax - cx) * (1.0 - k);
        let offset_y = self.offset_y + (ay - cy) * (1.0 - k);
        Self { scale: new_scale, offset_x, offset_y, ..self }.clamp()
    }

    /// Fitted → zoom in on the tapped point; zoomed → back to fitted.
    pub fn double_tap(self, ax: f32, ay: f32) -> Self {
        if self.scale > 1.01 {
            Self { scale: 1.0, offset_x: 0.0, offset_y: 0.0, ..self }
        } else {
            self.pinch(TAP_SCALE / self.scale, ax, ay)
        }
    }

    /// The viewport or the picture changed size (rotation, a sharper frame with more pixels):
    /// keep the relative zoom and position.
    pub fn resize(self, image_w: f32, image_h: f32, viewport_w: f32, viewport_h: f32) -> Self {
        let rx = if self.viewport_w > 0.0 { viewport_w / self.viewport_w } else { 1.0 };
        let ry = if self.viewport_h > 0.0 { viewport_h / self.viewport_h } else { 1.0 };
        Self {
            image_w,
            image_h,
            viewport_w,
            viewport_h,
            offset_x: self.offset_x * rx,
            offset_y: self.offset_y * ry,
            ..self
        }
        .clamp()
    }

    /// The best rendition for the drawn size: `preview` while it has enough pixels, the original
    /// beyond (`preview_px` is the preview's long side).
    pub fn wants_original(&self, preview_px: f32, screen_scale: f32) -> bool {
        let (w, h) = self.drawn();
        w.max(h) * screen_scale > preview_px * 1.25
    }
}

/// A gesture, as the platforms send it through the FFI (`{"op":"pinch",...}`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Gesture {
    Pan { dx: f32, dy: f32 },
    Pinch { factor: f32, x: f32, y: f32 },
    DoubleTap { x: f32, y: f32 },
    Resize { image_w: f32, image_h: f32, viewport_w: f32, viewport_h: f32 },
}

impl View {
    pub fn apply(self, g: Gesture) -> Self {
        match g {
            Gesture::Pan { dx, dy } => self.pan(dx, dy),
            Gesture::Pinch { factor, x, y } => self.pinch(factor, x, y),
            Gesture::DoubleTap { x, y } => self.double_tap(x, y),
            Gesture::Resize { image_w, image_h, viewport_w, viewport_h } => {
                self.resize(image_w, image_h, viewport_w, viewport_h)
            }
        }
    }
}

/// A one-finger drag shorter than this (points) has no direction yet.
pub const AXIS_SLOP: f32 = 10.0;
/// Tangent of 40°: drags within this cone of vertical belong to the viewer (out or info).
const OUT_CONE: f32 = 0.839;
/// Tangent of 50°: downward drags in this band belong to the viewer unless velocity is horizontal-dominant.
const OUT_BAND: f32 = 1.192;
/// The picture shrinks by up to this while dragged down, as in Google Photos.
const SHRINK: f32 = 0.25;
/// Released past this share of the viewport height (or flung past [`FLING_OUT`]) → close.
const OUT_AT: f32 = 0.12;
const FLING_OUT: f32 = 0.3;
/// Released this far up (points), or flung past [`FLING_INFO`] of the height → the details sheet.
const INFO_AT: f32 = 80.0;
const FLING_INFO: f32 = 0.25;
/// Minimum travel in points for a velocity-triggered flick.
const FLICK_MIN: f32 = 16.0;
/// Minimum finger speed in points per second for a velocity-triggered flick.
const FLICK_V: f32 = 500.0;

/// Who owns a one-finger drag, decided once from its first movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DragAxis {
    /// Not far enough to tell.
    Undecided,
    /// Horizontal on a fitted picture: the next or previous one.
    Pager,
    /// Vertical on a fitted picture: down closes the viewer, up shows the details.
    Out,
    /// The picture is zoomed: the drag pans it.
    Pan,
}

/// How a picture being dragged out is drawn: moved by `offset`, shrunk to `scale` (about its
/// centre) and over a backdrop of this opacity (0 shows what is behind the viewer).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DragOut {
    pub offset_x: f32,
    pub offset_y: f32,
    pub scale: f32,
    pub backdrop: f32,
}

/// What a released drag-out does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DragEnd {
    /// Close the viewer.
    Dismiss,
    /// Spring back and show the details.
    Info,
    /// Spring back.
    Back,
}

impl DragOut {
    pub const REST: DragOut = DragOut { offset_x: 0.0, offset_y: 0.0, scale: 1.0, backdrop: 1.0 };
}

impl View {
    fn zoomed(&self) -> bool {
        self.scale > 1.01
    }

    /// Who owns a drag that has moved (`dx`, `dy`), with finger velocity in points per second.
    pub fn drag_axis(&self, dx: f32, dy: f32, vx: f32, vy: f32) -> DragAxis {
        if self.zoomed() {
            return DragAxis::Pan;
        }
        let (ax, ay) = (dx.abs(), dy.abs());
        if ax.max(ay) < AXIS_SLOP {
            DragAxis::Undecided
        } else if ax <= ay * OUT_CONE {
            DragAxis::Out
        } else if dy > 0.0 && ax <= dy * OUT_BAND && vy.abs() >= vx.abs() {
            DragAxis::Out
        } else {
            DragAxis::Pager
        }
    }

    /// Down: the picture follows the finger, shrinks and the backdrop fades (gone at half the
    /// viewport height). Up: it follows at half speed, as the details come up behind it.
    pub fn drag_out(&self, dx: f32, dy: f32) -> DragOut {
        if dy <= 0.0 {
            return DragOut { offset_y: dy * 0.5, ..DragOut::REST };
        }
        let progress = if self.viewport_h > 0.0 { (dy / (0.5 * self.viewport_h)).min(1.0) } else { 1.0 };
        DragOut { offset_x: dx, offset_y: dy, scale: 1.0 - SHRINK * progress, backdrop: 1.0 - progress }
    }

    /// The release at `dy`, with the predicted end and finger velocity in points per second.
    pub fn drag_end(&self, dy: f32, predicted_dy: f32, vy: f32) -> DragEnd {
        let h = self.viewport_h.max(1.0);
        if dy > 0.0 && (dy > OUT_AT * h || predicted_dy > FLING_OUT * h || (dy >= FLICK_MIN && vy >= FLICK_V)) {
            DragEnd::Dismiss
        } else if dy < 0.0 && (-dy > INFO_AT || -predicted_dy > FLING_INFO * h || (-dy >= FLICK_MIN && -vy >= FLICK_V)) {
            DragEnd::Info
        } else {
            DragEnd::Back
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn fits_letterboxed() {
        // 3:2 landscape in a portrait phone viewport.
        let v = View::new(1440.0, 960.0, 390.0, 844.0);
        let r = v.rect();
        assert!(close(r.w, 390.0) && close(r.h, 260.0));
        assert!(close(r.x, 0.0) && close(r.y, (844.0 - 260.0) / 2.0));
    }

    #[test]
    fn pinch_keeps_the_anchor_still() {
        let v = View::new(1000.0, 1000.0, 400.0, 400.0);
        // The picture point under (300, 100) before and after.
        let before = v.rect();
        let px = (300.0 - before.x) / before.w;
        let py = (100.0 - before.y) / before.h;
        let z = v.pinch(2.0, 300.0, 100.0);
        let r = z.rect();
        assert!(close(z.scale, 2.0));
        assert!(close(r.x + px * r.w, 300.0) && close(r.y + py * r.h, 100.0), "{r:?}");
    }

    #[test]
    fn pan_stops_at_the_edges_and_fitted_pictures_stay_centred() {
        let v = View::new(1000.0, 1000.0, 400.0, 400.0);
        assert_eq!(v.pan(50.0, -30.0), v, "fitted: no pan");
        let z = v.pinch(2.0, 200.0, 200.0).pan(1000.0, 1000.0);
        let r = z.rect();
        assert!(close(r.x, 0.0) && close(r.y, 0.0), "left/top edge at the viewport edge: {r:?}");
        assert!(v.pinch(0.2, 0.0, 0.0).scale >= 1.0 && v.pinch(100.0, 0.0, 0.0).scale <= MAX_SCALE);
    }

    #[test]
    fn double_tap_toggles() {
        let v = View::new(1000.0, 1000.0, 400.0, 400.0);
        let z = v.double_tap(100.0, 100.0);
        assert!(close(z.scale, TAP_SCALE));
        assert_eq!(z.double_tap(0.0, 0.0), v);
    }

    #[test]
    fn gestures_from_json() {
        let g: Gesture = serde_json::from_str(r#"{"op":"pinch","factor":2,"x":10,"y":20}"#).unwrap();
        assert_eq!(g, Gesture::Pinch { factor: 2.0, x: 10.0, y: 20.0 });
        let v = View::new(1440.0, 960.0, 390.0, 844.0).apply(Gesture::DoubleTap { x: 195.0, y: 422.0 });
        assert!(close(v.scale, TAP_SCALE));
        assert!(!View::new(1440.0, 960.0, 390.0, 844.0).wants_original(1440.0, 3.0));
        assert!(v.pinch(3.0, 195.0, 422.0).wants_original(1440.0, 3.0));
    }

    #[test]
    fn drag_axis_waits_for_slop() {
        let v = View::new(1440.0, 960.0, 390.0, 844.0);
        assert_eq!(v.drag_axis(3.0, 4.0, 0.0, 900.0), DragAxis::Undecided, "velocity does not bypass slop");
        assert_eq!(v.drag_axis(0.0, AXIS_SLOP, 0.0, 0.0), DragAxis::Out, "slop boundary decides direction");
    }

    #[test]
    fn straight_down_is_out() {
        let v = View::new(1440.0, 960.0, 390.0, 844.0);
        for (dx, dy) in [(0.0, 40.0), (5.0, 40.0), (-5.0, -40.0)] {
            assert_eq!(v.drag_axis(dx, dy, 0.0, 0.0), DragAxis::Out, "within the vertical cone: ({dx}, {dy})");
        }
    }

    #[test]
    fn diagonal_down_is_out() {
        let v = View::new(1440.0, 960.0, 390.0, 844.0);
        assert_eq!(v.drag_axis(28.0, 40.0, 0.0, 0.0), DragAxis::Out, "35 degrees is within the cone");
        assert_eq!(v.drag_axis(30.0, 30.0, 0.0, 0.0), DragAxis::Out, "45 degrees with zero velocity uses the band");
        assert_eq!(v.drag_axis(30.0, 30.0, 400.0, 600.0), DragAxis::Out, "vertical-dominant velocity uses the band");
        assert_eq!(v.drag_axis(30.0, 30.0, 900.0, 300.0), DragAxis::Pager, "horizontal-dominant velocity pages in the band");
    }

    #[test]
    fn diagonal_up_in_band_pages() {
        let v = View::new(1440.0, 960.0, 390.0, 844.0);
        assert_eq!(v.drag_axis(30.0, -30.0, 0.0, -900.0), DragAxis::Pager, "upward band drags page even with vertical velocity");
        assert_eq!(v.drag_axis(20.0, -40.0, 0.0, 0.0), DragAxis::Out, "26.6 degrees up is within the cone");
    }

    #[test]
    fn horizontal_swipe_pages() {
        let v = View::new(1440.0, 960.0, 390.0, 844.0);
        for (dx, dy) in [(40.0, 10.0), (-40.0, 12.0), (35.0, 20.0)] {
            assert_eq!(v.drag_axis(dx, dy, 0.0, 0.0), DragAxis::Pager, "flatter than 50 degrees: ({dx}, {dy})");
        }
    }

    #[test]
    fn zoomed_drag_always_pans() {
        let v = View::new(1440.0, 960.0, 390.0, 844.0).double_tap(195.0, 422.0);
        for (dx, dy, vx, vy) in [
            (0.0, 100.0, 0.0, 900.0),
            (30.0, 30.0, 900.0, 300.0),
            (30.0, -30.0, 0.0, -900.0),
            (40.0, 10.0, -900.0, 0.0),
            (0.0, 0.0, 0.0, 0.0),
        ] {
            assert_eq!(v.drag_axis(dx, dy, vx, vy), DragAxis::Pan, "zoomed drag pans at ({dx}, {dy}), velocity ({vx}, {vy})");
        }
    }

    #[test]
    fn fast_short_flick_closes() {
        let v = View::new(1440.0, 960.0, 390.0, 844.0);
        assert_eq!(v.drag_end(24.0, 30.0, 900.0), DragEnd::Dismiss, "short fast downward flick closes");
        assert_eq!(v.drag_end(20.0, 20.0, -300.0), DragEnd::Back, "pulled back up before release");
        assert_eq!(v.drag_end(120.0, 120.0, -300.0), DragEnd::Dismiss, "distance still closes after reversing");
        assert_eq!(v.drag_end(10.0, 10.0, 2000.0), DragEnd::Back, "velocity needs minimum travel");
        assert_eq!(v.drag_end(10.0, 400.0, 2000.0), DragEnd::Dismiss, "predicted end can close below minimum flick travel");
        assert_eq!(v.drag_end(16.0, 16.0, 500.0), DragEnd::Dismiss, "flick thresholds are inclusive");
        assert_eq!(v.drag_end(16.0, 16.0, 499.0), DragEnd::Back, "speed below the flick threshold springs back");
    }

    #[test]
    fn slow_short_drag_springs_back() {
        let v = View::new(1440.0, 960.0, 390.0, 844.0);
        assert_eq!(v.drag_end(60.0, 70.0, 40.0), DragEnd::Back, "slow short drag springs back");
    }

    #[test]
    fn fast_short_flick_up_shows_info() {
        let v = View::new(1440.0, 960.0, 390.0, 844.0);
        assert_eq!(v.drag_end(-20.0, -20.0, -800.0), DragEnd::Info, "short fast upward flick shows info");
    }

    #[test]
    fn dragging_down_follows_shrinks_and_fades() {
        let v = View::new(1440.0, 960.0, 390.0, 844.0);
        assert_eq!(v.drag_out(0.0, 0.0), DragOut::REST);
        let d = v.drag_out(30.0, 211.0); // a quarter of the height = half way
        assert!(close(d.offset_x, 30.0) && close(d.offset_y, 211.0));
        assert!(close(d.scale, 0.875) && close(d.backdrop, 0.5), "{d:?}");
        let far = v.drag_out(0.0, 2000.0);
        assert!(close(far.scale, 0.75) && close(far.backdrop, 0.0), "clamped: {far:?}");
        let up = v.drag_out(50.0, -100.0);
        assert!(close(up.offset_y, -50.0) && close(up.offset_x, 0.0) && close(up.scale, 1.0) && close(up.backdrop, 1.0));
    }

    #[test]
    fn release_closes_far_or_fast_and_springs_back_otherwise() {
        let v = View::new(1440.0, 960.0, 390.0, 844.0);
        assert_eq!(v.drag_end(120.0, 130.0, 0.0), DragEnd::Dismiss, "past 12% of 844");
        assert_eq!(v.drag_end(40.0, 400.0, 0.0), DragEnd::Dismiss, "a quick flick down");
        assert_eq!(v.drag_end(60.0, 70.0, 0.0), DragEnd::Back, "short travel springs back");
        assert_eq!(v.drag_end(60.0, -50.0, 0.0), DragEnd::Back, "pulled back up before letting go");
        assert_eq!(v.drag_end(-90.0, -90.0, 0.0), DragEnd::Info, "past info distance");
        assert_eq!(v.drag_end(-30.0, -300.0, 0.0), DragEnd::Info, "a quick flick up");
        assert_eq!(v.drag_end(-30.0, -40.0, 0.0), DragEnd::Back, "short travel springs back");
        assert_eq!(v.drag_end(0.0, 0.0, 0.0), DragEnd::Back, "short travel springs back");
    }
}
