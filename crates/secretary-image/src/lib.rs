//! The image viewer core shared by the Secretary apps (iOS and Mac through `secretary-ffi`,
//! Android through JNI, the web through wasm). Every platform shows exactly what the media
//! server sends, decoded here:
//!
//! 1. [`placeholder`]: the item's ThumbHash (about 18 bytes, inside the gallery JSON) becomes a
//!    small blurred RGBA picture, shown before any image byte arrives;
//! 2. [`Decoder`]: the rendition bytes are fed as they arrive. A progressive JPEG XL file
//!    (the server's 1440 px preview) renders a coarse picture after its first passes and refines
//!    with every pass; JPEG and PNG (posters, older renditions) appear once complete;
//! 3. [`view`]: zoom and pan as pure functions, so pinch, double tap and clamping behave the same
//!    on every platform.

pub mod view;

use std::fmt;

/// A decoded picture: 8-bit RGBA, rows top to bottom, no padding.
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// The whole file was decoded (not a progressive pass).
    pub complete: bool,
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Frame({}x{}, complete: {})", self.width, self.height, self.complete)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn err(e: impl fmt::Display) -> Error {
    Error(e.to_string())
}

/// ThumbHash → a small RGBA picture (at most 32 px on its long side).
pub fn placeholder(hash: &[u8]) -> Result<Frame, Error> {
    let (w, h, rgba) = thumbhash::thumb_hash_to_rgba(hash).map_err(|_| Error("invalid ThumbHash".into()))?;
    Ok(Frame { width: w as u32, height: h as u32, rgba, complete: true })
}

/// The aspect ratio (width / height) a ThumbHash encodes, for laying out before decoding.
pub fn placeholder_aspect(hash: &[u8]) -> Option<f32> {
    thumbhash::thumb_hash_to_approximate_aspect_ratio(hash).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Unknown,
    Jxl,
    Other,
}

fn sniff(head: &[u8]) -> Kind {
    const JXL_CONTAINER: &[u8] = &[0, 0, 0, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A, 0x87, 0x0A];
    match head {
        [] | [0xFF] => Kind::Unknown,
        // Bare JPEG XL codestream; any other 0xFF lead (JPEG 0xFF 0xD8) is not.
        [0xFF, 0x0A, ..] => Kind::Jxl,
        [0xFF, ..] => Kind::Other,
        // A box-based file: the JPEG XL container, or something else (HEIF, MP4 poster) once
        // the signature is in.
        [0, ..] if head.len() < JXL_CONTAINER.len() => Kind::Unknown,
        [0, ..] if head.starts_with(JXL_CONTAINER) => Kind::Jxl,
        _ => Kind::Other,
    }
}

enum Jxl {
    Uninit(jxl_oxide::UninitializedJxlImage),
    Ready(jxl_oxide::JxlImage),
    Gone,
}

/// Feed it the bytes of one picture as they arrive; it hands out a better [`Frame`] whenever one
/// can be rendered.
pub struct Decoder {
    kind: Kind,
    /// Every byte so far (needed for JPEG/PNG, which decode once complete).
    bytes: Vec<u8>,
    jxl: Jxl,
    /// Bytes fed since the last progressive render.
    since_render: usize,
    frame: Option<Frame>,
    /// Re-render a partial JPEG XL after at least this many new bytes.
    pub render_step: usize,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            kind: Kind::Unknown,
            bytes: Vec::new(),
            jxl: Jxl::Uninit(jxl_oxide::JxlImage::builder().build_uninit()),
            since_render: 0,
            frame: None,
            render_step: 12 * 1024,
        }
    }

    /// The best frame so far.
    pub fn frame(&self) -> Option<&Frame> {
        self.frame.as_ref()
    }

    /// Size of the picture as soon as the header is in (JPEG XL), else of the last frame.
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        match &self.jxl {
            Jxl::Ready(img) => Some((img.width(), img.height())),
            _ => self.frame.as_ref().map(|f| (f.width, f.height)),
        }
    }

    /// Add `chunk`; `done` marks the last chunk. Returns `true` when [`Self::frame`] got better.
    pub fn feed(&mut self, chunk: &[u8], done: bool) -> Result<bool, Error> {
        self.bytes.extend_from_slice(chunk);
        if self.kind == Kind::Unknown {
            self.kind = sniff(&self.bytes);
            if self.kind == Kind::Unknown && !done {
                return Ok(false);
            }
        }
        match self.kind {
            Kind::Jxl => self.feed_jxl(chunk, done),
            _ => {
                if !done {
                    return Ok(false);
                }
                let img = image::load_from_memory(&self.bytes).map_err(err)?.to_rgba8();
                self.frame = Some(Frame { width: img.width(), height: img.height(), rgba: img.into_raw(), complete: true });
                Ok(true)
            }
        }
    }

    fn feed_jxl(&mut self, chunk: &[u8], done: bool) -> Result<bool, Error> {
        self.since_render += chunk.len();
        self.jxl = match std::mem::replace(&mut self.jxl, Jxl::Gone) {
            Jxl::Uninit(mut u) => {
                // The uninitialised image keeps every byte; it moves them into the image itself.
                u.feed_bytes(chunk).map_err(err)?;
                match u.try_init().map_err(err)? {
                    jxl_oxide::InitializeResult::NeedMoreData(u) => Jxl::Uninit(u),
                    jxl_oxide::InitializeResult::Initialized(img) => Jxl::Ready(img),
                }
            }
            Jxl::Ready(mut img) => {
                img.feed_bytes(chunk).map_err(err)?;
                Jxl::Ready(img)
            }
            Jxl::Gone => return Err(Error("decoder used after an error".into())),
        };
        let Jxl::Ready(img) = &mut self.jxl else { return Ok(false) };
        let finished = done || img.is_loading_done();
        if !finished && self.since_render < self.render_step {
            return Ok(false);
        }
        let render = if img.num_loaded_keyframes() > 0 {
            img.render_frame(0)
        } else {
            match img.render_loading_frame() {
                Ok(r) => Ok(r),
                // Not even the first pass is in yet.
                Err(_) if !finished => return Ok(false),
                Err(e) => Err(e),
            }
        }
        .map_err(err)?;
        self.since_render = 0;
        let complete = img.num_loaded_keyframes() > 0;
        let (w, h) = (img.width(), img.height());
        let mut stream = render.stream();
        let channels = stream.channels() as usize;
        let mut buf = vec![0u8; w as usize * h as usize * channels];
        stream.write_to_buffer::<u8>(&mut buf);
        let rgba = match channels {
            4 => buf,
            3 => buf.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
            1 => buf.iter().flat_map(|&v| [v, v, v, 255]).collect(),
            2 => buf.as_chunks::<2>().0.iter().flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
            n => return Err(Error(format!("{n} channels"))),
        };
        self.frame = Some(Frame { width: w, height: h, rgba, complete });
        Ok(true)
    }
}

/// Decode a whole file at once (tests, tools).
pub fn decode(bytes: &[u8]) -> Result<Frame, Error> {
    let mut d = Decoder::new();
    d.feed(bytes, true)?;
    d.frame.ok_or_else(|| Error("nothing to show".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32) -> Vec<u8> {
        let mut px = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                px.extend_from_slice(&[(x * 255 / w) as u8, (y * 255 / h) as u8, ((x + y) % 256) as u8]);
            }
        }
        px
    }

    fn jxl(w: u32, h: u32, progressive: bool) -> Vec<u8> {
        use jpegxl_sys::encoder::encode::JxlEncoderFrameSettingId as Opt;
        let mut enc = jpegxl_rs::encoder_builder().quality(2.5).build().unwrap();
        if progressive {
            enc.set_frame_option(Opt::Responsive, 1);
            enc.set_frame_option(Opt::ProgressiveAc, 1);
            enc.set_frame_option(Opt::ProgressiveDc, 1);
        }
        enc.encode::<u8>(&gradient(w, h), w, h).unwrap()
    }

    #[test]
    fn progressive_jxl_refines_as_bytes_arrive() {
        let bytes = jxl(1440, 960, true);
        let mut d = Decoder::new();
        d.render_step = 4 * 1024;
        let mut partial_frames = 0;
        let mut first_partial_at = None;
        for (i, chunk) in bytes.chunks(2048).enumerate() {
            let last = (i + 1) * 2048 >= bytes.len();
            if d.feed(chunk, last).unwrap() {
                let f = d.frame().unwrap();
                assert_eq!((f.width, f.height), (1440, 960));
                assert_eq!(f.rgba.len(), 1440 * 960 * 4);
                if !f.complete {
                    partial_frames += 1;
                    first_partial_at.get_or_insert((i + 1) * 2048);
                }
            }
        }
        let f = d.frame().unwrap();
        assert!(f.complete, "the last chunk gives the full picture");
        assert!(partial_frames >= 1, "at least one picture before the end ({} bytes)", bytes.len());
        assert!(first_partial_at.unwrap() < bytes.len(), "a first picture before all bytes were in");
        // The full picture matches the gradient closely.
        let px = &f.rgba[(480 * 1440 + 720) * 4..][..3];
        assert!((px[0] as i32 - 127).abs() < 12 && (px[1] as i32 - 127).abs() < 12, "{px:?}");
    }

    #[test]
    fn jpeg_and_png_appear_when_complete() {
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_raw(64, 32, gradient(64, 32)).unwrap())
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let mut d = Decoder::new();
        assert!(!d.feed(&png[..png.len() / 2], false).unwrap());
        assert!(d.feed(&png[png.len() / 2..], true).unwrap());
        assert_eq!(d.frame().unwrap().width, 64);
        assert!(decode(b"not an image").is_err());
    }

    #[test]
    fn thumbhash_placeholder() {
        let rgba: Vec<u8> = gradient(100, 75).as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        let hash = thumbhash::rgba_to_thumb_hash(100, 75, &rgba);
        let f = placeholder(&hash).unwrap();
        assert!(f.width <= 32 && f.height <= 32 && f.width > f.height);
        assert!((placeholder_aspect(&hash).unwrap() - 100.0 / 75.0).abs() < 0.2);
        assert!(placeholder(&[1, 2]).is_err());
    }

    #[test]
    fn sniffing() {
        assert_eq!(sniff(&[0xFF, 0x0A]), Kind::Jxl);
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF]), Kind::Other);
        assert_eq!(sniff(&[0x89, b'P']), Kind::Other);
        assert_eq!(sniff(&[0]), Kind::Unknown);
    }
}
