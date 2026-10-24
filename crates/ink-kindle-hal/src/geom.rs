//! Integer geometry and an owned 8-bit grayscale image (0 = black, 255 = white).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

impl Size {
    pub fn rect(self) -> Rect {
        Rect::new(0, 0, self.w, self.h)
    }
    pub fn area(self) -> u64 {
        u64::from(self.w) * u64::from(self.h)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// Axis-aligned rectangle; `w == 0 || h == 0` is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
    pub fn right(&self) -> u32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> u32 {
        self.y + self.h
    }
    pub fn area(&self) -> u64 {
        u64::from(self.w) * u64::from(self.h)
    }
    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect::new(
            x,
            y,
            self.right().max(o.right()) - x,
            self.bottom().max(o.bottom()) - y,
        )
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        if r <= x || b <= y {
            Rect::default()
        } else {
            Rect::new(x, y, r - x, b - y)
        }
    }
    pub fn contains_rect(&self, o: &Rect) -> bool {
        o.is_empty()
            || (o.x >= self.x
                && o.y >= self.y
                && o.right() <= self.right()
                && o.bottom() <= self.bottom())
    }
    pub fn contains(&self, p: Point) -> bool {
        p.x >= 0
            && p.y >= 0
            && (p.x as u32) >= self.x
            && (p.y as u32) >= self.y
            && (p.x as u32) < self.right()
            && (p.y as u32) < self.bottom()
    }
}

/// Owned 8-bit grayscale image with `stride == width`.
#[derive(Clone, PartialEq, Eq)]
pub struct GrayImage {
    pub size: Size,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for GrayImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GrayImage({}x{})", self.size.w, self.size.h)
    }
}

impl GrayImage {
    pub fn new(size: Size, fill: u8) -> Self {
        Self {
            size,
            data: vec![fill; size.area() as usize],
        }
    }
    pub fn width(&self) -> u32 {
        self.size.w
    }
    pub fn height(&self) -> u32 {
        self.size.h
    }
    pub fn fill(&mut self, v: u8) {
        self.data.fill(v);
    }
    pub fn row(&self, y: u32) -> &[u8] {
        let w = self.size.w as usize;
        &self.data[y as usize * w..(y as usize + 1) * w]
    }
    pub fn get(&self, x: u32, y: u32) -> u8 {
        self.data[(y * self.size.w + x) as usize]
    }
    pub fn set(&mut self, x: u32, y: u32, v: u8) {
        let i = (y * self.size.w + x) as usize;
        self.data[i] = v;
    }
    /// Fill a rectangle (clipped to the image).
    pub fn fill_rect(&mut self, r: Rect, v: u8) {
        let r = r.intersect(&self.size.rect());
        for y in r.y..r.bottom() {
            let s = (y * self.size.w + r.x) as usize;
            self.data[s..s + r.w as usize].fill(v);
        }
    }
    /// Darken a pixel by `coverage` (0..=255): black ink composited over the existing value.
    #[inline]
    pub fn darken(&mut self, x: i32, y: i32, coverage: u8) {
        if x < 0 || y < 0 || x as u32 >= self.size.w || y as u32 >= self.size.h || coverage == 0 {
            return;
        }
        let i = (y as u32 * self.size.w + x as u32) as usize;
        let d = u32::from(self.data[i]);
        self.data[i] = (d * (255 - u32::from(coverage)) / 255) as u8;
    }
}
