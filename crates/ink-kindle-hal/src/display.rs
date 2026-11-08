//! Display trait, waveform modes and the ghosting / full-refresh policy.
use crate::geom::{GrayImage, Rect, Size};
use crate::Result;

/// EPDC waveform modes the reader uses (names as in the i.MX/Lab126 mxcfb header).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaveformMode {
    /// Direct update: fast, black/white only (menus, highlights).
    Du,
    /// Full 16-level grayscale; flashes when issued as a full update. Clears ghosting.
    Gc16,
    /// 16-level grayscale for transitions from white; no flash. Page turns.
    Gl16,
    /// Fastest 1-bit mode (animation, pen); leaves the most ghosting.
    A2,
}

impl WaveformMode {
    pub fn name(self) -> &'static str {
        match self {
            WaveformMode::Du => "DU",
            WaveformMode::Gc16 => "GC16",
            WaveformMode::Gl16 => "GL16",
            WaveformMode::A2 => "A2",
        }
    }
}

/// One panel update as issued to the controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateRecord {
    /// 1-based sequence number (the mxcfb update marker on the device).
    pub seq: u32,
    pub region: Rect,
    pub mode: WaveformMode,
    /// A flashing full update (`UPDATE_MODE_FULL`).
    pub full: bool,
    /// The policy turned a requested partial update into a full GC16 flash.
    pub promoted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub mode: WaveformMode,
    pub full: bool,
    pub promoted: bool,
}

/// Approximate ghosting control: E Ink panels accumulate ghosting with every non-flashing refresh, so
/// after `full_every` partial refreshes the next partial request is promoted to a full-screen GC16 flash.
/// This is the policy stock Kindle firmware exposes as "page refresh every N pages"; `0` disables it.
/// It counts updates, not pixels: the real amount of ghosting depends on content and temperature,
/// which the emulator does not model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshPolicy {
    pub full_every: u32,
    partials: u32,
}

impl Default for RefreshPolicy {
    fn default() -> Self {
        Self::new(6)
    }
}

impl RefreshPolicy {
    pub const fn new(full_every: u32) -> Self {
        Self {
            full_every,
            partials: 0,
        }
    }
    /// Partial refreshes since the last full flash.
    pub fn partials_since_full(&self) -> u32 {
        self.partials
    }
    /// Decide how a requested partial refresh (one batch of regions) is issued.
    pub fn decide(&mut self, requested: WaveformMode) -> Decision {
        if self.full_every > 0 && self.partials >= self.full_every {
            self.partials = 0;
            return Decision {
                mode: WaveformMode::Gc16,
                full: true,
                promoted: true,
            };
        }
        self.partials += 1;
        Decision {
            mode: requested,
            full: false,
            promoted: false,
        }
    }
    /// An explicit full refresh happened; ghosting is cleared.
    pub fn note_full(&mut self) {
        self.partials = 0;
    }
}

/// Validate a batch of update regions against the panel.
pub fn check_regions(regions: &[Rect], size: Size) -> Result<()> {
    let full = size.rect();
    if regions.is_empty() {
        return Err(crate::Error::Device("empty update batch".into()));
    }
    for r in regions {
        if r.is_empty() || !full.contains_rect(r) {
            return Err(crate::Error::OutOfBounds(*r, size));
        }
    }
    Ok(())
}

/// Mutable view of a framebuffer (rows may be padded: `stride >= size.w`).
pub struct FrameMut<'a> {
    pub data: &'a mut [u8],
    pub size: Size,
    pub stride: usize,
}

impl FrameMut<'_> {
    /// Copy `region` of `src` (same panel size) into the framebuffer.
    pub fn blit(&mut self, src: &GrayImage, region: Rect) {
        let r = region
            .intersect(&self.size.rect())
            .intersect(&src.size.rect());
        let sw = src.size.w as usize;
        for y in r.y..r.bottom() {
            let s = y as usize * sw + r.x as usize;
            let d = y as usize * self.stride + r.x as usize;
            self.data[d..d + r.w as usize].copy_from_slice(&src.data[s..s + r.w as usize]);
        }
    }
}

/// An E Ink display: write pixels into [`Display::frame_mut`], then make them visible with an update.
pub trait Display {
    fn size(&self) -> Size;
    fn frame_mut(&mut self) -> FrameMut<'_>;
    /// Show `regions` with non-flashing updates of the requested mode, as **one** partial refresh for the
    /// [`RefreshPolicy`] (a page turn that changes the text block and the page number counts once). The
    /// policy may promote the whole batch to a single full-screen GC16 flash; the returned records say
    /// what was actually issued. Regions must be non-empty and inside the panel.
    fn update_regions(&mut self, regions: &[Rect], mode: WaveformMode)
        -> Result<Vec<UpdateRecord>>;
    /// Single-region [`Display::update_regions`].
    fn update(&mut self, region: Rect, mode: WaveformMode) -> Result<UpdateRecord> {
        let mut v = self.update_regions(&[region], mode)?;
        Ok(v.remove(0))
    }
    /// Full-screen flashing GC16 update; resets the ghosting counter.
    fn full_refresh(&mut self) -> Result<UpdateRecord>;
}
