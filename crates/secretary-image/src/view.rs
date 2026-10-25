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
}
