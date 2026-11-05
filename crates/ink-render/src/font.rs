//! Font loading (TTF/OTF via `fontdue`) with glyph and advance caches.
//!
//! Fonts are loaded from paths at runtime; none are embedded. Missing bold/italic faces fall back to the
//! regular face (bold is synthesised by double-striking one pixel to the right).
//!
//! fontdue reads every outline of a face when it parses it, the slow part of loading a family: the
//! regular face is parsed at once, the bold / italic ones the first time a glyph of theirs is needed
//! ([`Fonts::parse_all`] does it ahead). A face whose file does not parse then counts as missing.
use crate::{Error, Result};
use ink_epub::SpanStyle;
use std::cell::OnceCell;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Face {
    Regular = 0,
    Bold = 1,
    Italic = 2,
    BoldItalic = 3,
}

impl Face {
    pub fn from_style(s: SpanStyle) -> Face {
        match (s.bold, s.italic) {
            (false, false) => Face::Regular,
            (true, false) => Face::Bold,
            (false, true) => Face::Italic,
            (true, true) => Face::BoldItalic,
        }
    }
    pub fn bolder(self) -> Face {
        match self {
            Face::Regular | Face::Bold => Face::Bold,
            Face::Italic | Face::BoldItalic => Face::BoldItalic,
        }
    }
}

/// A rasterised glyph: coverage bitmap (0..=255) and its offset from the pen position / baseline.
#[derive(Debug, Clone)]
pub struct Glyph {
    pub xmin: i32,
    /// Bottom edge relative to the baseline, positive upwards (fontdue convention).
    pub ymin: i32,
    pub width: usize,
    pub height: usize,
    pub bitmap: Vec<u8>,
}

type Key = (u8, u16, char);

/// A bold / italic face: its file, parsed when first used (`None` there: it does not parse).
struct LazyFace {
    bytes: Vec<u8>,
    font: OnceCell<Option<fontdue::Font>>,
}

impl LazyFace {
    fn get(&self) -> Option<&fontdue::Font> {
        self.font
            .get_or_init(|| parse(&self.bytes[..]).ok())
            .as_ref()
    }
}

pub struct Fonts {
    regular: fontdue::Font,
    /// Bold, italic, bold italic (indices 1..=3 of [`Face`]).
    others: [Option<LazyFace>; 3],
    advances: HashMap<Key, f32>,
    glyphs: HashMap<Key, Glyph>,
    /// Display name (file name of the regular face).
    pub name: String,
}

impl std::fmt::Debug for Fonts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Fonts({})", self.name)
    }
}

fn parse(bytes: &[u8]) -> Result<fontdue::Font> {
    fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default())
        .map_err(|e| Error::Font(e.to_string()))
}

impl Fonts {
    pub fn from_bytes(regular: Vec<u8>, name: impl Into<String>) -> Result<Self> {
        Ok(Self {
            regular: parse(&regular)?,
            others: [None, None, None],
            advances: HashMap::new(),
            glyphs: HashMap::new(),
            name: name.into(),
        })
    }
    pub fn load(regular: &Path) -> Result<Self> {
        let bytes = std::fs::read(regular)
            .map_err(|e| Error::Font(format!("{}: {e}", regular.display())))?;
        let name = regular
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self::from_bytes(bytes, name)
    }
    /// Add a bold/italic face (parsed when first used).
    pub fn with_face(mut self, face: Face, path: &Path) -> Result<Self> {
        self.set_face(face, path)?;
        Ok(self)
    }
    /// Add a bold/italic face in place (parsed when first used); a file that cannot be read is an
    /// error and leaves the fonts as they were. `Face::Regular` is replaced at once.
    pub fn set_face(&mut self, face: Face, path: &Path) -> Result<()> {
        let bytes =
            std::fs::read(path).map_err(|e| Error::Font(format!("{}: {e}", path.display())))?;
        match face {
            Face::Regular => self.regular = parse(&bytes)?,
            f => {
                self.others[f as usize - 1] = Some(LazyFace {
                    bytes,
                    font: OnceCell::new(),
                })
            }
        }
        self.advances.clear();
        self.glyphs.clear();
        Ok(())
    }
    /// Whether `face` has a file of its own that parses (parsing it if it was not yet).
    pub fn has_face(&self, face: Face) -> bool {
        self.face(face as usize).is_some()
    }
    /// Parse every face now (a family loaded ahead, off the drawing thread).
    pub fn parse_all(&self) {
        for i in 1..4 {
            self.face(i);
        }
    }

    /// Face `idx` of [`Face`], parsed; `None` when it has no file or it does not parse.
    fn face(&self, idx: usize) -> Option<&fontdue::Font> {
        match idx {
            0 => Some(&self.regular),
            i => self.others[i - 1].as_ref()?.get(),
        }
    }

    /// The loaded face to use and whether bold must be synthesised.
    fn resolve(&self, face: Face) -> (usize, bool) {
        let have = |f: Face| self.face(f as usize).is_some();
        match face {
            Face::Regular => (0, false),
            Face::Bold if have(Face::Bold) => (1, false),
            Face::Bold => (0, true),
            Face::Italic if have(Face::Italic) => (2, false),
            Face::Italic => (0, false),
            Face::BoldItalic if have(Face::BoldItalic) => (3, false),
            Face::BoldItalic if have(Face::Bold) => (1, false),
            Face::BoldItalic if have(Face::Italic) => (2, true),
            Face::BoldItalic => (0, true),
        }
    }

    fn font(&self, idx: usize) -> &fontdue::Font {
        // resolve() only returns faces that parsed.
        self.face(idx).unwrap_or(&self.regular)
    }

    /// Whether glyphs of `face` are drawn twice (synthetic bold).
    pub fn synthetic_bold(&self, face: Face) -> bool {
        self.resolve(face).1
    }

    /// Horizontal advance in pixels (synthetic bold is one pixel wider per glyph).
    pub fn advance(&mut self, face: Face, px: u16, ch: char) -> f32 {
        let key = (face as u8, px, ch);
        if let Some(a) = self.advances.get(&key) {
            return *a;
        }
        let (idx, fake) = self.resolve(face);
        let a =
            self.font(idx).metrics(ch, f32::from(px)).advance_width + if fake { 1.0 } else { 0.0 };
        self.advances.insert(key, a);
        a
    }

    pub fn kern(&self, face: Face, px: u16, a: char, b: char) -> f32 {
        let (idx, _) = self.resolve(face);
        self.font(idx)
            .horizontal_kern(a, b, f32::from(px))
            .unwrap_or(0.0)
    }

    /// (ascent, descent) in pixels, both positive, from the regular face.
    pub fn line_metrics(&self, px: u16) -> (f32, f32) {
        match self.font(0).horizontal_line_metrics(f32::from(px)) {
            Some(m) => (m.ascent, -m.descent),
            None => (f32::from(px) * 0.8, f32::from(px) * 0.2),
        }
    }

    pub fn glyph(&mut self, face: Face, px: u16, ch: char) -> &Glyph {
        let key = (face as u8, px, ch);
        if !self.glyphs.contains_key(&key) {
            let (idx, _) = self.resolve(face);
            let (m, bitmap) = self.font(idx).rasterize(ch, f32::from(px));
            self.glyphs.insert(
                key,
                Glyph {
                    xmin: m.xmin,
                    ymin: m.ymin,
                    width: m.width,
                    height: m.height,
                    bitmap,
                },
            );
        }
        &self.glyphs[&key]
    }

    /// Width of `text` in one face, including kerning (same arithmetic as layout and drawing).
    pub fn measure(&mut self, face: Face, px: u16, text: &str) -> f32 {
        let mut w = 0.0;
        let mut prev = None;
        for c in text.chars() {
            if let Some(p) = prev {
                w += self.kern(face, px, p, c);
            }
            w += self.advance(face, px, c);
            prev = Some(c);
        }
        w
    }

    /// Number of cached glyph bitmaps (memory diagnostics).
    pub fn cached_glyphs(&self) -> usize {
        self.glyphs.len()
    }
}
