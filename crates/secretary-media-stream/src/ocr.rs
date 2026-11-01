//! Image subtitles (DVD VobSub, Blu-ray PGS) to text, so they show like any
//! other track and can be turned on and off without re-encoding the video.
//!
//! ffmpeg draws the subtitle track over a black canvas at `FPS` frames a
//! second (it decodes only the subtitles, not the video). Each run of
//! identical frames is one cue; its picture is cropped, binarised to dark
//! text on white and read by tesseract in one batch.
use secretary_player::subtitles::Cue;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Frames per second sampled: cue times are accurate to 1/FPS s.
pub const FPS: u64 = 5;
/// Pixels brighter than this are subtitle text (DVD text is white or light).
const INK: u8 = 100;
/// White margin around each crop: tesseract reads poorly at the edge.
const PAD: usize = 12;

#[derive(Debug, Clone)]
pub struct OcrConfig {
    pub tesseract: PathBuf,
    /// Folder with `<lang>.traineddata` (tessdata_fast); None = tesseract's default.
    pub tessdata: Option<PathBuf>,
}

/// A picture shown from `start_ms` to `end_ms`, as a binarised crop.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub start_ms: u64,
    pub end_ms: u64,
    pub width: usize,
    pub height: usize,
    /// 0 = ink, 255 = paper.
    pub pixels: Vec<u8>,
}

/// Splits a stream of grey frames into events.
pub struct Segmenter {
    width: usize,
    height: usize,
    current: Option<(u64, Event)>,
    pub events: Vec<Event>,
}

impl Segmenter {
    pub fn new(width: usize, height: usize) -> Self {
        Self { width, height, current: None, events: Vec::new() }
    }

    /// The bounding box of ink and a hash of the binarised picture inside it.
    fn signature(&self, frame: &[u8]) -> Option<(u64, (usize, usize, usize, usize))> {
        let (w, h) = (self.width, self.height);
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
        for y in 0..h {
            let row = &frame[y * w..(y + 1) * w];
            if let Some(first) = row.iter().position(|&p| p > INK) {
                let last = row.iter().rposition(|&p| p > INK).unwrap_or(first);
                x0 = x0.min(first);
                x1 = x1.max(last);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
        if x1 < x0 || y1 < y0 {
            return None;
        }
        // FNV-1a over the ink bits and the box: identical pictures hash alike.
        let mut hash: u64 = 0xcbf29ce484222325;
        for v in [x0, y0, x1, y1] {
            hash = (hash ^ v as u64).wrapping_mul(0x100000001b3);
        }
        for y in y0..=y1 {
            for x in x0..=x1 {
                hash = (hash ^ u64::from(frame[y * w + x] > INK)).wrapping_mul(0x100000001b3);
            }
        }
        Some((hash, (x0, y0, x1, y1)))
    }

    fn crop(&self, frame: &[u8], (x0, y0, x1, y1): (usize, usize, usize, usize), t_ms: u64) -> Event {
        let (cw, ch) = (x1 - x0 + 1 + 2 * PAD, y1 - y0 + 1 + 2 * PAD);
        let mut pixels = vec![255u8; cw * ch];
        for y in y0..=y1 {
            for x in x0..=x1 {
                if frame[y * self.width + x] > INK {
                    pixels[(y - y0 + PAD) * cw + (x - x0 + PAD)] = 0;
                }
            }
        }
        Event { start_ms: t_ms, end_ms: t_ms, width: cw, height: ch, pixels }
    }

    pub fn push(&mut self, frame: &[u8], t_ms: u64) {
        let sig = self.signature(frame);
        let same = match (&self.current, &sig) {
            (Some((a, _)), Some((b, _))) => a == b,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.close(t_ms);
        if let Some((hash, bbox)) = sig {
            self.current = Some((hash, self.crop(frame, bbox, t_ms)));
        }
    }

    pub fn close(&mut self, t_ms: u64) {
        if let Some((_, mut e)) = self.current.take() {
            e.end_ms = t_ms;
            if e.end_ms > e.start_ms {
                self.events.push(e);
            }
        }
    }
}

/// tesseract's name for a subtitle language (ISO 639-1 or -2), if its data is installed.
pub fn tesseract_lang(cfg: &OcrConfig, lang: Option<&str>) -> String {
    let code = match lang.map(str::to_ascii_lowercase).as_deref() {
        Some("da") => "dan".to_string(),
        Some("en") => "eng".into(),
        Some("it") => "ita".into(),
        Some("de") | Some("ger") => "deu".into(),
        Some("fr") | Some("fre") => "fra".into(),
        Some("es") => "spa".into(),
        Some("sv") => "swe".into(),
        Some("no") | Some("nob") => "nor".into(),
        Some("nl") | Some("dut") => "nld".into(),
        Some(other) => other.to_string(),
        None => "eng".into(),
    };
    let installed = |c: &str| cfg.tessdata.as_ref().is_none_or(|d| d.join(format!("{c}.traineddata")).exists());
    if installed(&code) { code } else { "eng".into() }
}

/// One image subtitle stream to read.
pub struct Source<'a> {
    /// The video (stream `spec` such as `0:3`) or a `.idx`/`.sup` file (`0:s:0`).
    pub input: &'a Path,
    pub spec: &'a str,
    /// The subtitle canvas.
    pub size: (u32, u32),
    pub duration_ms: u64,
    pub lang: Option<&'a str>,
}

/// Pictures OCR'd per tesseract run: a small first batch so the opening
/// lines are ready within seconds, then larger ones.
const FIRST_BATCH: usize = 8;
const BATCH: usize = 60;

/// Reads one image subtitle stream. `progress` gets every cue read so far
/// after each batch (the film plays from the start while the rest is read).
pub fn read_track(
    ffmpeg: &Path,
    cfg: &OcrConfig,
    src: &Source,
    workdir: &Path,
    progress: &mut dyn FnMut(&[Cue]),
) -> Result<Vec<Cue>, String> {
    let (w, h) = src.size;
    let (input, spec, duration_ms, lang) = (src.input, src.spec, src.duration_ms, src.lang);
    // Blu-ray canvases are halved: still ~25 px text, a quarter of the pixels.
    let half = w > 1000;
    let (ow, oh) = if half { ((w / 2) & !1, (h / 2) & !1) } else { (w, h) };
    let secs = duration_ms.div_ceil(1000).max(1);
    let scale = if half { format!(",scale={ow}:{oh}") } else { String::new() };
    let graph = format!("color=c=black:s={w}x{h}:r={FPS}:d={secs}[bg];[bg][{spec}]overlay=eof_action=pass{scale},format=gray");
    let mut child = Command::new(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-i"])
        .arg(input)
        .args(["-filter_complex", &graph, "-an", "-f", "rawvideo", "-pix_fmt", "gray", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    let mut out = child.stdout.take().expect("piped");
    let (fw, fh) = (ow as usize, oh as usize);
    let mut seg = Segmenter::new(fw, fh);
    let mut frame = vec![0u8; fw * fh];
    let mut n: u64 = 0;
    let mut cues: Vec<Cue> = Vec::new();
    let mut done = 0;
    loop {
        match out.read_exact(&mut frame) {
            Ok(()) => {
                seg.push(&frame, n * 1000 / FPS);
                n += 1;
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(format!("reading frames: {e}")),
        }
        let batch = if done == 0 { FIRST_BATCH } else { BATCH };
        if seg.events.len() - done >= batch {
            cues.extend(recognise(cfg, &seg.events[done..], lang, &workdir.join(format!("b{done}")))?);
            done = seg.events.len();
            progress(&cues);
        }
    }
    seg.close(n * 1000 / FPS);
    let status = child.wait_with_output().map_err(|e| e.to_string())?;
    if !status.status.success() {
        return Err(format!("ffmpeg: {}", String::from_utf8_lossy(&status.stderr).trim()));
    }
    cues.extend(recognise(cfg, &seg.events[done..], lang, &workdir.join(format!("b{done}")))?);
    let _ = std::fs::remove_dir_all(workdir);
    Ok(cues)
}

/// Writes each event as a PGM, runs one tesseract over the list, and pairs
/// the pages (form-feed separated) with the events.
pub fn recognise(cfg: &OcrConfig, events: &[Event], lang: Option<&str>, workdir: &Path) -> Result<Vec<Cue>, String> {
    if events.is_empty() {
        return Ok(vec![]);
    }
    std::fs::create_dir_all(workdir).map_err(|e| e.to_string())?;
    let mut list = String::new();
    for (i, e) in events.iter().enumerate() {
        let path = workdir.join(format!("ev{i:05}.pgm"));
        let mut f = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        write!(f, "P5\n{} {}\n255\n", e.width, e.height).map_err(|e| e.to_string())?;
        f.write_all(&e.pixels).map_err(|e| e.to_string())?;
        list.push_str(&path.display().to_string());
        list.push('\n');
    }
    let list_path = workdir.join("list.txt");
    std::fs::write(&list_path, list).map_err(|e| e.to_string())?;
    let mut cmd = Command::new(&cfg.tesseract);
    cmd.arg(&list_path).arg(workdir.join("out")).args(["-l", &tesseract_lang(cfg, lang), "--psm", "6"]);
    if let Some(d) = &cfg.tessdata {
        cmd.arg("--tessdata-dir").arg(d);
    }
    let run = cmd.output().map_err(|e| format!("tesseract: {e}"))?;
    if !run.status.success() {
        return Err(format!("tesseract: {}", String::from_utf8_lossy(&run.stderr).trim()));
    }
    let text = std::fs::read_to_string(workdir.join("out.txt")).map_err(|e| e.to_string())?;
    let pages: Vec<&str> = text.split('\u{c}').collect();
    if pages.len() < events.len() {
        return Err(format!("tesseract returned {} pages for {} pictures", pages.len(), events.len()));
    }
    let cues = events
        .iter()
        .zip(pages)
        .filter_map(|(e, page)| {
            let lines: Vec<&str> = page.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
            (!lines.is_empty()).then(|| Cue { start_ms: e.start_ms, end_ms: e.end_ms, text: lines.join("\n") })
        })
        .collect();
    let _ = std::fs::remove_dir_all(workdir);
    Ok(cues)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, boxes: &[(usize, usize, usize, usize)]) -> Vec<u8> {
        let mut f = vec![0u8; w * h];
        for &(x0, y0, x1, y1) in boxes {
            for y in y0..y1 {
                for x in x0..x1 {
                    f[y * w + x] = 230;
                }
            }
        }
        f
    }

    #[test]
    fn segments_runs_of_identical_pictures() {
        let (w, h) = (64, 48);
        let blank = frame(w, h, &[]);
        let a = frame(w, h, &[(10, 30, 40, 36)]);
        let b = frame(w, h, &[(5, 30, 50, 36), (20, 38, 30, 44)]);
        let mut s = Segmenter::new(w, h);
        let t = |n: u64| n * 1000 / FPS;
        for (n, f) in [&blank, &a, &a, &a, &b, &b, &blank, &blank, &a].iter().enumerate() {
            s.push(f, t(n as u64));
        }
        s.close(t(10));
        let spans: Vec<(u64, u64)> = s.events.iter().map(|e| (e.start_ms, e.end_ms)).collect();
        assert_eq!(spans, vec![(200, 800), (800, 1200), (1600, 2000)]);
        let first = &s.events[0];
        assert_eq!((first.width, first.height), (30 + 2 * PAD, 6 + 2 * PAD));
        assert_eq!(first.pixels[0], 255);
        assert_eq!(first.pixels[PAD * first.width + PAD], 0);
    }

    #[test]
    fn languages_fall_back_to_english() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dan.traineddata"), b"x").unwrap();
        std::fs::write(dir.path().join("eng.traineddata"), b"x").unwrap();
        let cfg = OcrConfig { tesseract: "tesseract".into(), tessdata: Some(dir.path().to_path_buf()) };
        assert_eq!(tesseract_lang(&cfg, Some("dan")), "dan");
        assert_eq!(tesseract_lang(&cfg, Some("da")), "dan");
        assert_eq!(tesseract_lang(&cfg, Some("jpn")), "eng");
        assert_eq!(tesseract_lang(&cfg, None), "eng");
    }

    /// Draws text with ffmpeg, then reads it back (skipped without ffmpeg's drawtext or tesseract).
    #[test]
    fn recognises_rendered_text() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("t.pgm");
        let ok = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=black:s=720x120",
                "-vf", "drawtext=text='Hello there':fontcolor=white:fontsize=40:x=60:y=40,format=gray",
                "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray"])
            .arg(&png)
            .status()
            .is_ok_and(|s| s.success());
        let tess = Command::new("tesseract").arg("--version").output().is_ok_and(|o| o.status.success());
        if !ok || !tess {
            eprintln!("ffmpeg drawtext or tesseract missing: skipped");
            return;
        }
        let raw = std::fs::read(&png).unwrap();
        let mut s = Segmenter::new(720, 120);
        s.push(&raw, 1000);
        s.close(3000);
        let cfg = OcrConfig { tesseract: "tesseract".into(), tessdata: None };
        let cues = recognise(&cfg, &s.events, Some("en"), &dir.path().join("work")).unwrap();
        assert_eq!(cues.len(), 1);
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (1000, 3000));
        assert_eq!(cues[0].text, "Hello there");
    }
}
