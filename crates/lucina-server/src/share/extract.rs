use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context};
use scraper::{Html, Selector};
use tokio::process::Command;

use serde_json::Value;

use super::handlers::Extracted;
use super::{clip, Part};
use crate::AppState;

/// Characters of text kept from one part (stored; the turn sees less).
pub(super) const MAX_TEXT: usize = 200_000;

pub(super) fn is_location(s: &str) -> bool {
    if s.starts_with("geo:") {
        return true;
    }
    reqwest::Url::parse(s).ok().is_some_and(|u| {
        let host = u.host_str().unwrap_or("");
        host == "maps.apple.com"
            || host == "maps.google.com"
            || host == "maps.app.goo.gl"
            || ((host == "google.com" || host.ends_with(".google.com"))
                && u.path().starts_with("/maps"))
    })
}
fn location(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("geo:") {
        return format!("Location: {}\n{s}", rest.replace(['?', '&'], " "));
    }
    let mut out = format!("Location: {s}");
    if let Ok(url) = reqwest::Url::parse(s) {
        for (k, v) in url.query_pairs() {
            if ["ll", "q", "query", "address", "name"].contains(&k.as_ref()) {
                out.push_str(&format!("\n{k}: {v}"));
            }
        }
    }
    out
}

/// Conservatively refuse special-use networks, including IPv4-in-IPv6.
fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0)
        || (a == 192 && b == 2)
        || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
        || (a == 203 && b == 0 && c == 113)
        || a >= 224)
}
pub(super) fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_v4(v4);
            }
            let oct = ip.octets();
            // Global unicast only. Exclude documentation, 6to4 and Teredo
            // transition prefixes which can encode private IPv4 endpoints.
            (oct[0] & 0xe0) == 0x20
                && !(oct[0] == 0x20 && oct[1] == 0x02)
                && !(oct[..4] == [0x20, 0x01, 0, 0])
                && !(oct[..4] == [0x20, 0x01, 0x0d, 0xb8])
        }
    }
}
pub(crate) async fn download(st: &AppState, input: &str) -> anyhow::Result<(Vec<u8>, String)> {
    // `st` only carries the test-only loopback override.
    #[cfg(not(debug_assertions))]
    let _ = st;
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut url = reqwest::Url::parse(input)?;
        for redirects in 0..=5 {
            if !["http", "https"].contains(&url.scheme()) || !url.username().is_empty() || url.password().is_some() { bail!("unsupported URL"); }
            let host = url.host_str().context("URL has no host")?.trim_matches(['[', ']']).to_owned();
            let port = url.port_or_known_default().context("URL has no port")?;
            let addresses: Vec<SocketAddr> = match host.parse::<IpAddr>() { Ok(ip) => vec![SocketAddr::new(ip, port)], Err(_) => tokio::net::lookup_host((host.as_str(), port)).await?.collect() };
            if addresses.is_empty() { bail!("host has no addresses"); }
            for address in &addresses {
                let allowed = public_ip(address.ip());
                #[cfg(debug_assertions)]
                let allowed = allowed || (st.cfg.share.test_allow_loopback && address.ip().is_loopback());
                if !allowed { bail!("URL resolves to a forbidden address"); }
            }
            // Pin the checked DNS answer and disable environment proxies, so
            // resolution cannot change between validation and connection.
            let http = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none()).resolve_to_addrs(&host, &addresses).timeout(Duration::from_secs(15)).user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/130.0 Safari/537.36 LucinaShare/1").build()?;
            let mut response = http.get(url.clone()).send().await?;
            if response.status().is_redirection() {
                if redirects == 5 { bail!("too many redirects"); }
                let target = response.headers().get(reqwest::header::LOCATION).context("redirect missing location")?.to_str()?;
                url = url.join(target)?;
                continue;
            }
            response.error_for_status_ref()?;
            let mime = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|h| h.to_str().ok()).unwrap_or("text/html").to_string();
            let mut data = Vec::new();
            while let Some(chunk) = response.chunk().await? { if data.len() + chunk.len() > 3 * 1024 * 1024 { bail!("page exceeds 3 MB"); } data.extend_from_slice(&chunk); }
            return Ok((data, mime));
        }
        bail!("too many redirects")
    }).await.context("page fetch timed out")?
}

pub(crate) fn html_text(input: &str) -> (String, String) {
    let html = Html::parse_document(input);
    let title = html
        .select(&Selector::parse("title").expect("static selector"))
        .next()
        .map(|e| e.text().collect::<String>())
        .unwrap_or_default()
        .trim()
        .to_owned();
    let mut out = String::new();
    for meta in html.select(&Selector::parse("meta").expect("static selector")) {
        if meta.value().attr("name") == Some("description")
            || meta
                .value()
                .attr("property")
                .is_some_and(|p| p.starts_with("og:"))
        {
            if let Some(content) = meta.value().attr("content") {
                out.push_str(content);
                out.push('\n');
            }
        }
    }
    for node in html.tree.nodes() {
        if let scraper::node::Node::Text(text) = node.value() {
            let blocked = node.ancestors().any(|a| {
                a.value().as_element().is_some_and(|e| {
                    [
                        "script", "style", "nav", "footer", "head", "noscript", "svg",
                    ]
                    .contains(&e.name())
                })
            });
            if !blocked {
                out.push_str(text);
                out.push(' ');
            }
        }
        if out.len() > 100_000 {
            break;
        }
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    (title, out)
}

/// Office/ODF text with structure: headings and slide titles on their own
/// lines. Up to `MAX_TEXT` characters.
pub(crate) fn document(path: &Path) -> anyhow::Result<String> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    let mut names: Vec<String> = (0..archive.len().min(4096))
        .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_owned()))
        .filter(|name| {
            (name == "word/document.xml"
                || name.starts_with("word/header")
                || name.starts_with("word/footer")
                || name == "xl/sharedStrings.xml"
                || name.starts_with("ppt/slides/slide")
                || name == "content.xml")
                && name.ends_with(".xml")
        })
        .collect();
    // Slides in their order (slide2 before slide10).
    names.sort_by_key(|n| {
        let digits: String = n.chars().filter(char::is_ascii_digit).collect();
        let slides_first = if n.starts_with("ppt/slides/") { 0 } else { 1 };
        (slides_first, digits.parse::<u32>().unwrap_or(0))
    });
    let mut out = String::new();
    let mut budget = 16 * 1024 * 1024;
    for name in names {
        let file = archive.by_name(&name)?;
        let mut xml = String::new();
        let n = file.take(budget as u64).read_to_string(&mut xml)?;
        budget -= n;
        if let Some(slide) = name.strip_prefix("ppt/slides/slide") {
            out.push_str(&format!("\n## Slide {}\n", slide.trim_end_matches(".xml")));
        }
        out.push_str(&structured_xml_text(&xml));
        out.push('\n');
        if budget == 0 || out.len() > MAX_TEXT {
            break;
        }
    }
    if out.trim().is_empty() {
        bail!("document has no readable XML text (this format may be unsupported)");
    }
    Ok(clip(out.trim(), MAX_TEXT))
}

/// XML text where paragraphs (`w:p`, `a:p`, `text:p`, `text:h`) end lines
/// and headings (Word `w:pStyle` Heading/Title, ODF `text:h`) become `#` lines.
fn structured_xml_text(xml: &str) -> String {
    let mut out = String::new();
    let mut rest = xml;
    while let Some(start) = rest.find('<') {
        out.push_str(&unescape(&rest[..start]));
        let Some(end) = rest[start..].find('>') else {
            break;
        };
        let tag = &rest[start + 1..start + end];
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split([' ', '/'])
            .next()
            .unwrap_or("");
        match (closing, name) {
            (true, "w:p" | "a:p" | "text:p" | "text:h" | "si") => out.push('\n'),
            (false, "w:pStyle")
                if ["Heading", "Title", "Titolo", "Overskrift"]
                    .iter()
                    .any(|h| tag.contains(h)) =>
            {
                out.push_str("# ")
            }
            (false, "text:h") if !tag.ends_with('/') => out.push_str("# "),
            (false, "w:tab" | "text:tab") => out.push('\t'),
            (false, "w:br" | "a:br" | "text:line-break") => out.push('\n'),
            _ => {}
        }
        rest = &rest[start + end + 1..];
    }
    out.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty() && l != "#")
        .collect::<Vec<_>>()
        .join("\n")
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

pub(super) async fn command(bin: &str, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new(bin)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        bail!(
            "{bin}: {}",
            clip(&String::from_utf8_lossy(&output.stderr), 400)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
pub(crate) async fn ocr(path: &Path) -> anyhow::Result<String> {
    let langs = command("tesseract", &["--list-langs"])
        .await
        .unwrap_or_default();
    let mut selected = vec!["eng"];
    for lang in ["ita", "dan"] {
        if langs.lines().any(|s| s.trim() == lang) {
            selected.push(lang);
        }
    }
    command(
        "tesseract",
        &[
            path.to_str().context("invalid image path")?,
            "stdout",
            "-l",
            &selected.join("+"),
        ],
    )
    .await
}
/// A ≤1536 px JPEG copy for vision and OCR.
async fn vision_jpeg(path: &Path) -> anyhow::Result<std::path::PathBuf> {
    let jpg = path.with_extension("vision.jpg");
    let input = path.to_str().context("invalid image path")?;
    let output = jpg.to_str().context("invalid image path")?;
    if command(
        "convert",
        &[
            &format!("{input}[0]"),
            "-auto-orient",
            "-thumbnail",
            "1536x1536>",
            "-background",
            "white",
            "-alpha",
            "remove",
            "-quality",
            "85",
            output,
        ],
    )
    .await
    .is_err()
    {
        command(
            "ffmpeg",
            &[
                "-nostdin",
                "-y",
                "-i",
                input,
                "-vf",
                "scale=w='min(1536,iw)':h='min(1536,ih)':force_original_aspect_ratio=decrease",
                "-frames:v",
                "1",
                output,
            ],
        )
        .await?;
    }
    Ok(jpg)
}
/// Vision description (with the model's category) plus OCR text.
async fn image(st: &AppState, path: &Path) -> anyhow::Result<Extracted> {
    let jpg = vision_jpeg(path).await?;
    let mut x = Extracted::new("image");
    let description = match super::vision::describe(st, &jpg).await {
        Ok(s) => {
            let (category, description) = super::vision::split_category(&s);
            x.subtype = category;
            description
        }
        Err(e) => {
            x.warnings
                .push(format!("vision could not read the image: {e}"));
            String::new()
        }
    };
    let text = match ocr(&jpg).await {
        Ok(s) => s.trim().to_owned(),
        Err(e) => {
            x.warnings.push(format!("OCR failed: {e}"));
            String::new()
        }
    };
    x.text = format!("Vision: {description}\nOCR text:\n{text}");
    let model = st.model.as_ref();
    match x.subtype.as_deref() {
        Some("receipt") => {
            x.structured = super::structured::receipt(model, &x.text)
                .await
                .unwrap_or(Value::Null)
        }
        Some("document_photo") if super::structured::looks_like_invoice(&text) => {
            x.structured = super::structured::invoice(model, &text)
                .await
                .unwrap_or(Value::Null);
            if !x.structured.is_null() {
                x.subtype = Some("invoice".into());
            }
        }
        Some("screenshot_error") => {
            x.structured = serde_json::json!({ "error": clip(text.trim(), 2000) });
        }
        _ => {}
    }
    Ok(x)
}
pub(crate) async fn pdf(st: &AppState, path: &Path) -> anyhow::Result<Extracted> {
    let path_owned = path.to_owned();
    let (text, pages, images) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let doc = lopdf::Document::load(&path_owned)?;
        let pages = doc.get_pages();
        let text = pdf_extract::extract_text(&path_owned).unwrap_or_default();
        let mut images = Vec::new();
        if text.trim().chars().count() < 200 {
            for (_, id) in pages.iter().take(5) {
                // Resources can be inherited from the page tree.
                let (resource, ancestors) = doc.get_page_resources(*id)?;
                let resources = resource.into_iter().chain(
                    ancestors
                        .iter()
                        .filter_map(|id| doc.get_dictionary(*id).ok()),
                );
                for resources in resources {
                    let Ok(xobjects) = resources.get(b"XObject").and_then(|o| {
                        o.as_dict()
                            .or_else(|_| o.as_reference().and_then(|id| doc.get_dictionary(id)))
                    }) else {
                        continue;
                    };
                    for (_, object) in xobjects.iter() {
                        let Ok(stream) = object
                            .as_reference()
                            .and_then(|id| doc.get_object(id))
                            .and_then(|o| o.as_stream())
                        else {
                            continue;
                        };
                        if stream.dict.get(b"Subtype").and_then(|o| o.as_name()).ok()
                            != Some(b"Image")
                        {
                            continue;
                        }
                        let dct = stream.dict.get(b"Filter").ok().is_some_and(|f| {
                            f.as_name().is_ok_and(|n| n == b"DCTDecode")
                                || f.as_array().is_ok_and(|a| {
                                    a.iter()
                                        .any(|o| o.as_name().is_ok_and(|n| n == b"DCTDecode"))
                                })
                        });
                        if dct && stream.content.len() <= 20 * 1024 * 1024 {
                            images.push(stream.content.clone());
                            break;
                        }
                    }
                    if images.len() >= 5 {
                        break;
                    }
                }
            }
        }
        Ok((clip(&text, MAX_TEXT), pages.len(), images))
    })
    .await??;
    let mut x = Extracted::new("pdf");
    x.text = format!("PDF, {pages} pages\n{}", text.trim());
    x.title = text
        .lines()
        .map(str::trim)
        .find(|l| l.chars().count() >= 3)
        .map(|l| clip(l, 120));
    if !images.is_empty() {
        x.warnings.push("scanned PDF: text read with OCR".into());
    }
    for (i, bytes) in images.iter().take(5).enumerate() {
        let img = path.with_extension(format!("page-{i}.jpg"));
        tokio::fs::write(&img, bytes).await?;
        let page = if i == 0 {
            image(st, &img).await.map(|p| p.text)
        } else {
            ocr(&img).await
        };
        x.text.push_str(&format!(
            "\n## Page {} (scanned)\n{}",
            i + 1,
            page.unwrap_or_else(|e| format!("could not be read: {e}"))
        ));
    }
    if super::structured::looks_like_invoice(&x.text) {
        let fields = super::structured::invoice(st.model.as_ref(), &x.text)
            .await
            .unwrap_or(Value::Null);
        if !fields.is_null() {
            x.subtype = Some("invoice".into());
            x.structured = fields;
        }
    }
    Ok(x)
}
/// 16 kHz mono WAV of the first ten minutes for voice notes, otherwise 30, then Whisper.
pub(super) async fn transcribe(
    st: &AppState,
    path: &Path,
    voice: bool,
) -> anyhow::Result<(String, f64)> {
    let wav = path.with_extension("share.wav");
    command(
        "ffmpeg",
        &[
            "-nostdin",
            "-y",
            "-i",
            path.to_str().context("invalid path")?,
            "-t",
            if voice { "600" } else { "1800" },
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "pcm_s16le",
            wav.to_str().context("invalid path")?,
        ],
    )
    .await?;
    let cfg = &st.cfg.share.whisper;
    let duration = lucina_memo::transcribe::duration(cfg, &wav).await?;
    let work = path.with_extension("whisper-work");
    tokio::fs::create_dir_all(&work).await?;
    let transcript =
        lucina_memo::transcribe::transcribe(cfg, &wav, &work, duration, cfg.gpu_wait).await?;
    Ok((transcript.text(), duration))
}
async fn audio(
    st: &AppState,
    path: &Path,
    on_device: Option<&str>,
    voice: bool,
) -> anyhow::Result<Extracted> {
    let mut x = Extracted::new("audio");
    let (transcript, source) = match on_device.filter(|t| !t.trim().is_empty()) {
        Some(t) => (t.trim().to_owned(), "on-device"),
        None => match transcribe(st, path, voice).await {
            Ok((t, _)) => (t, "whisper"),
            Err(e) => {
                x.warnings.push(format!("transcription failed: {e}"));
                (String::new(), "none")
            }
        },
    };
    let duration = lucina_memo::transcribe::duration(&st.cfg.share.whisper, path)
        .await
        .ok();
    x.structured = serde_json::json!({ "duration_s": duration, "transcript_source": source });
    if voice {
        x.subtype = Some("voice".into());
        x.text = transcript.trim().to_owned();
    } else {
        x.text = format!("Transcript ({source}):\n{transcript}");
    }
    Ok(x)
}
/// Up to six key frames: scene changes, else evenly spaced.
async fn key_frames(path: &Path, duration: f64) -> Vec<std::path::PathBuf> {
    let Some(input) = path.to_str() else {
        return Vec::new();
    };
    let pattern = path.with_extension("scene-%02d.jpg");
    let _ = command(
        "ffmpeg",
        &[
            "-nostdin",
            "-y",
            "-i",
            input,
            "-vf",
            "select='gt(scene,0.4)',scale='min(1280,iw)':-2",
            "-vsync",
            "vfr",
            "-frames:v",
            "6",
            pattern.to_str().unwrap_or_default(),
        ],
    )
    .await;
    let mut frames: Vec<_> = (1..=6)
        .map(|i| path.with_extension(format!("scene-{i:02}.jpg")))
        .filter(|p| p.exists())
        .collect();
    if frames.len() < 3 {
        frames.clear();
        for (i, at) in [0.1, 0.3, 0.5, 0.7, 0.9].iter().enumerate() {
            let out = path.with_extension(format!("even-{i}.jpg"));
            let ok = command(
                "ffmpeg",
                &[
                    "-nostdin",
                    "-y",
                    "-ss",
                    &format!("{:.2}", duration * at),
                    "-i",
                    input,
                    "-frames:v",
                    "1",
                    "-vf",
                    "scale='min(1280,iw)':-2",
                    out.to_str().unwrap_or_default(),
                ],
            )
            .await
            .is_ok();
            if ok && out.exists() {
                frames.push(out);
            }
        }
    }
    frames
}
async fn video(st: &AppState, path: &Path, on_device: Option<&str>) -> anyhow::Result<Extracted> {
    let mut x = audio(st, path, on_device, false).await?;
    x.kind = "video";
    let duration = x.structured["duration_s"].as_f64().unwrap_or(0.0);
    let frames = key_frames(path, duration).await;
    if frames.is_empty() {
        x.warnings
            .push("no frames could be taken from the video".into());
    }
    for (i, frame) in frames.iter().enumerate() {
        let described = match super::vision::describe(st, frame).await {
            Ok(s) => super::vision::split_category(&s).1,
            Err(e) => format!("could not be read: {e}"),
        };
        x.text
            .push_str(&format!("\n## Frame {}\n{described}", i + 1));
    }
    x.structured["frames"] = serde_json::json!(frames.len());
    Ok(x)
}
/// YouTube: the watch page's title, channel, description and captions.
async fn youtube(st: &AppState, url: &str, lang: &str) -> anyhow::Result<Extracted> {
    use super::handlers::youtube as yt;
    let (page, _) = download(st, url).await?;
    let page = String::from_utf8_lossy(&page);
    let (title, channel, description) = yt::page_meta(&page);
    let tracks = yt::caption_tracks(&page);
    let transcript = match yt::pick_track(&tracks, lang) {
        Some(track) => match download(st, &yt::timedtext_url(track)).await {
            Ok((body, _)) => Some(yt::parse_timedtext(&String::from_utf8_lossy(&body)))
                .filter(|t| !t.trim().is_empty()),
            Err(_) => None,
        },
        None => None,
    };
    let mut x = yt::to_extracted(title, channel, description, transcript);
    if let Some(id) = yt::video_id(url) {
        x.structured["video_id"] = Value::String(id);
    }
    if x.structured["has_transcript"] != Value::Bool(true) {
        x.warnings
            .push("no captions were available for this video".into());
    }
    Ok(x)
}
async fn read_text_file(path: &Path, max: u64) -> anyhow::Result<(String, bool)> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path).await?;
    let len = file.metadata().await?.len();
    let mut bytes = Vec::new();
    file.take(max).read_to_end(&mut bytes).await?;
    Ok((String::from_utf8_lossy(&bytes).into_owned(), len > max))
}

/// Read one part with the handler its kind routes to. `extract_files: false`
/// (library saves) still reads texts, links and locations, but leaves stored
/// files unread. `on_device` is a transcript the phone already made.
/// `summarize: false` skips the long-document summary (a chat turn reading a
/// Drive file has no time for it; it gets the passages it needs instead).
pub(super) async fn read(
    st: &AppState,
    dir: &Path,
    part: &mut Part,
    extract_files: bool,
    on_device: Option<&str>,
    lang: &str,
    summarize: bool,
) -> anyhow::Result<Extracted> {
    if let Some(text) = &part.text {
        let mut x = Extracted::new("text");
        x.text = text.clone();
        return Ok(x);
    }
    if part.attachment.kind == "location" {
        let mut x = Extracted::new("location");
        x.text = location(part.attachment.url.as_deref().unwrap_or(""));
        x.title = part.attachment.name.clone();
        return Ok(x);
    }
    if part.attachment.kind == "link" && part.path.is_none() {
        let url = part.attachment.url.clone().context("missing URL")?;
        if super::handlers::youtube::video_id(&url).is_some() {
            let x = youtube(st, &url, lang).await?;
            if let Some(t) = &x.title {
                part.attachment.name = Some(clip(t, 200));
            }
            return Ok(x);
        }
        let (bytes, mime) = download(st, &url).await?;
        if mime.starts_with("image/") || mime.starts_with("application/pdf") {
            let ext = if mime.starts_with("image/") {
                "jpg"
            } else {
                "pdf"
            };
            let path = dir.join(format!("fetched-{}.{}", uuid::Uuid::new_v4(), ext));
            tokio::fs::write(&path, &bytes).await?;
            part.path = Some(path);
            part.attachment.mime = Some(mime);
            part.attachment.bytes = Some(bytes.len() as u64);
        } else {
            let input = String::from_utf8_lossy(&bytes);
            let (title, text) = html_text(&input);
            let mut x = Extracted::new("link");
            if !title.is_empty() {
                part.attachment.name = Some(clip(&title, 200));
                x.title = Some(title.clone());
            }
            x.text = clip(&format!("{title}\n{text}"), MAX_TEXT);
            x.structured = serde_json::json!({
                "url": url,
                "host": reqwest::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_owned)),
            });
            return Ok(x);
        }
    }
    let path = part.path.clone().context("missing file")?;
    let name = part.attachment.name.clone().unwrap_or_default();
    let mime = part.attachment.mime.clone().unwrap_or_default();
    let routed = {
        let (path, name, mime) = (path.clone(), name.clone(), mime.clone());
        let voice = part.attachment.kind == "voice";
        tokio::task::spawn_blocking(move || super::router::route_for(&path, &name, &mime, voice))
            .await?
    };
    let ext = Path::new(&name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !extract_files {
        let mut x = Extracted::new(routed);
        x.text = "Original saved in the Shared album.".into();
        return Ok(x);
    }
    let blocking = |f: fn(&Path, &str) -> anyhow::Result<Extracted>| {
        let (path, arg) = (
            path.clone(),
            if routed == "code" {
                name.clone()
            } else {
                ext.clone()
            },
        );
        tokio::task::spawn_blocking(move || f(&path, &arg))
    };
    let mut x = match routed {
        "image" => image(st, &path).await?,
        "pdf" => pdf(st, &path).await?,
        "document" | "slides" => {
            let p = path.clone();
            let mut x = Extracted::new(routed);
            x.text = tokio::task::spawn_blocking(move || document(&p)).await??;
            x.title = x
                .text
                .lines()
                .map(|l| l.trim_start_matches(['#', ' ']))
                .find(|l| l.chars().count() >= 3)
                .map(|l| clip(l, 120));
            if super::structured::looks_like_invoice(&x.text) {
                let fields = super::structured::invoice(st.model.as_ref(), &x.text)
                    .await
                    .unwrap_or(Value::Null);
                if !fields.is_null() {
                    x.subtype = Some("invoice".into());
                    x.structured = fields;
                }
            }
            x
        }
        "sheet" => blocking(super::handlers::sheet::read_sheet).await??,
        "code" => blocking(super::handlers::code::read_code).await??,
        "archive" => blocking(super::handlers::archive::list_archive).await??,
        "calendar" | "contact" | "text" => {
            let (text, cut) = read_text_file(&path, 2 * 1024 * 1024).await?;
            let mut x = match routed {
                "calendar" => super::handlers::ics::parse_ics(&text),
                "contact" => super::handlers::vcf::parse_vcf(&text),
                _ => {
                    let mut x = Extracted::new("text");
                    x.title = text
                        .lines()
                        .map(|l| l.trim_start_matches(['#', ' ']))
                        .find(|l| l.chars().count() >= 3)
                        .map(|l| clip(l, 120));
                    x.text = text;
                    x
                }
            };
            if cut {
                x.warnings.push("only the first 2 MB were read".into());
            }
            x
        }
        "audio" => audio(st, &path, on_device, part.attachment.kind == "voice").await?,
        "video" => video(st, &path, on_device).await?,
        _ => {
            let mut x = Extracted::new("other");
            let size = tokio::fs::metadata(&path)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            let mut head = vec![0u8; 16];
            if let Ok(mut f) = std::fs::File::open(&path) {
                let n = f.read(&mut head).unwrap_or(0);
                head.truncate(n);
            }
            let magic: String = head.iter().map(|b| format!("{b:02x}")).collect();
            x.text = format!(
                "A file Lucina has no reader for: name {name}, type {}, {size} bytes, first bytes {magic}.",
                if mime.is_empty() { "unknown" } else { &mime }
            );
            x.structured = serde_json::json!({ "mime": mime, "bytes": size, "magic": magic, "extension": ext });
            x
        }
    };
    if x.title.is_none() {
        x.title = Some(name.clone());
    }
    if summarize
        && x.subtype.as_deref() != Some("voice")
        && x.text.chars().count() > super::structured::SUMMARY_FROM_CHARS
        && x.summary.is_none()
    {
        match super::structured::summarize(st.model.as_ref(), &x.text, lang).await {
            Ok(s) => x.summary = Some(s),
            Err(e) => x.warnings.push(format!("summary failed: {e}")),
        }
    }
    Ok(x)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn locations() {
        assert!(is_location("geo:55.6,12.5?q=Home"));
        assert!(is_location("https://maps.apple.com/?ll=55.6,12.5&q=Home"));
        assert!(is_location("https://www.google.com/maps?q=Rome"));
        assert!(!is_location("https://evilgoogle.com/maps"));
        assert!(location("https://maps.apple.com/?ll=55.6,12.5&q=Home").contains("q: Home"));
    }
    #[test]
    fn ssrf_addresses() {
        for ip in [
            "0.0.0.0",
            "10.0.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.1.1",
            "172.31.255.255",
            "192.168.1.1",
            "100.64.0.1",
            "100.107.164.33",
            "100.127.255.255",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:192.168.1.2",
            "::ffff:100.107.164.33",
            "::127.0.0.1",
            "2002:7f00:1::",
            "64:ff9b::7f00:1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "100.63.255.255",
            "100.128.0.1",
            "172.32.0.1",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(public_ip(ip.parse().unwrap()), "{ip}");
        }
    }
    #[test]
    fn readable_html() {
        let (title, text) = html_text("<html><head><title>Test &amp; page</title><meta name='description' content='A description'><meta property='og:title' content='Social title'><style>hide me</style></head><body><nav>nav garbage</nav><main><h1>Hello</h1><p>World &amp; friends</p></main><script>bad code</script><footer>footer garbage</footer></body></html>");
        assert_eq!(title, "Test & page");
        assert!(
            text.contains("A description")
                && text.contains("Hello")
                && text.contains("World & friends")
                && text.contains("Social title")
        );
        for s in ["hide me", "nav garbage", "bad code", "footer garbage"] {
            assert!(!text.contains(s));
        }
    }
    #[test]
    fn office_xml() {
        for (name, contents, expected) in [
            (
                "word/document.xml",
                "<w:document><w:p><w:r><w:t>Hello &amp; document</w:t></w:r></w:p></w:document>",
                "Hello & document",
            ),
            (
                "xl/sharedStrings.xml",
                "<sst><si><t>Apples</t></si><si><t>Milk</t></si></sst>",
                "Apples\nMilk",
            ),
        ] {
            let temp = tempfile::NamedTempFile::new().unwrap();
            let mut zip = zip::ZipWriter::new(temp.reopen().unwrap());
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(contents.as_bytes()).unwrap();
            zip.finish().unwrap();
            assert_eq!(document(temp.path()).unwrap().trim(), expected);
        }
    }
    #[test]
    fn word_headings_and_slides() {
        let xml = r#"<w:document><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Contratto</w:t></w:r></w:p><w:p><w:r><w:t>Canone &amp; spese</w:t></w:r><w:r><w:tab/><w:t>€ 900</w:t></w:r></w:p></w:body></w:document>"#;
        assert_eq!(
            structured_xml_text(xml),
            "# Contratto\nCanone & spese € 900"
        );
        let temp = tempfile::NamedTempFile::new().unwrap();
        let mut zip = zip::ZipWriter::new(temp.reopen().unwrap());
        for (name, title) in [
            ("ppt/slides/slide10.xml", "Ten"),
            ("ppt/slides/slide2.xml", "Two"),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(
                format!("<p:sld><a:p><a:r><a:t>{title}</a:t></a:r></a:p></p:sld>").as_bytes(),
            )
            .unwrap();
        }
        zip.finish().unwrap();
        let text = document(temp.path()).unwrap();
        assert!(
            text.find("## Slide 2").unwrap() < text.find("## Slide 10").unwrap(),
            "{text}"
        );
        assert!(text.contains("Two") && text.contains("Ten"));
    }
    pub(super) fn small_pdf(path: &Path) {
        use lopdf::content::{Content, Operation};
        use lopdf::{dictionary, Object, Stream};
        let mut doc = lopdf::Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(
            dictionary! {"Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica"},
        );
        let resources = doc.add_object(dictionary! {"Font" => dictionary! {"F1" => font}});
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![20.into(), 100.into()]),
                Operation::new("Tj", vec![Object::string_literal("Hello PDF document")]),
                Operation::new("ET", vec![]),
            ],
        };
        let content = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page = doc.add_object(dictionary! {"Type" => "Page", "Parent" => pages_id, "Contents" => content, "Resources" => resources, "MediaBox" => vec![0.into(),0.into(),300.into(),300.into()]});
        doc.objects.insert(
            pages_id,
            Object::Dictionary(
                dictionary! {"Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1},
            ),
        );
        let catalog = doc.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages_id});
        doc.trailer.set("Root", catalog);
        doc.save(path).unwrap();
    }
    #[tokio::test]
    async fn pdf_text_layer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pdf");
        small_pdf(&path);
        let st = crate::AppState::build(crate::Config::for_data_dir(dir.path()), None).unwrap();
        let x = pdf(&st, &path).await.unwrap();
        assert!(x.text.contains("PDF, 1 pages"), "{}", x.text);
        assert!(x.text.contains("Hello PDF document"), "{}", x.text);
        assert_eq!(x.title.as_deref(), Some("Hello PDF document"));
        assert!(x.subtype.is_none() && x.structured.is_null());
    }
}
