//! Font loading (TTF/OTF via `fontdue`) with glyph and advance caches.
//!
//! Fonts are loaded from paths at runtime; none are embedded. Missing bold/italic faces fall back to the
//! regular face (bold is synthesised by double-striking one pixel to the right).
use crate::{Error, Result};
use ink_epub::SpanStyle;
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

pub struct Fonts {
    faces: [Option<fontdue::Font>; 4],
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

fn parse(bytes: Vec<u8>) -> Result<fontdue::Font> {
    fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default())
        .map_err(|e| Error::Font(e.to_string()))
}

impl Fonts {
    pub fn from_bytes(regular: Vec<u8>, name: impl Into<String>) -> Result<Self> {
        Ok(Self {
            faces: [Some(parse(regular)?), None, None, None],
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
    /// Add a bold/italic face.
    pub fn with_face(mut self, face: Face, path: &Path) -> Result<Self> {
        let bytes =
            std::fs::read(path).map_err(|e| Error::Font(format!("{}: {e}", path.display())))?;
        self.faces[face as usize] = Some(parse(bytes)?);
        self.advances.clear();
        self.glyphs.clear();
        Ok(self)
    }
    pub fn has_face(&self, face: Face) -> bool {
        self.faces[face as usize].is_some()
    }

    /// The loaded face to use and whether bold must be synthesised.
    fn resolve(&self, face: Face) -> (usize, bool) {
        let have = |f: Face| self.faces[f as usize].is_some();
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
        // Index 0 always exists; resolve() only returns loaded indices.
        self.faces[idx].as_ref().unwrap_or_else(|| {
            self.faces[0]
                .as_ref()
                .expect("regular face is always loaded")
        })
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
