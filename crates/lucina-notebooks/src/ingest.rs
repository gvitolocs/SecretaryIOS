//! Getting a source's text. The crate does plain text and PDFs (page by page, so passages
//! keep their page); everything else (web pages, YouTube, audio and video through Whisper,
//! Office files, OCR, Lucina's computer) comes from lucina-server's own pipelines through
//! [`Extractor`].

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::chunk::Part;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Extracted {
    pub title: Option<String>,
    /// `pdf`, `youtube`, `web`, `audio`, `file`, `text`: what the source turned out to be.
    pub kind: Option<String>,
    pub parts: Vec<Part>,
    pub pages: Option<i64>,
    pub duration_s: Option<f64>,
    /// A file worth keeping as the source's original (a PDF behind a link, say).
    pub file: Option<PathBuf>,
}

impl Extracted {
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .map(|p| p.text.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

#[async_trait]
pub trait Extractor: Send + Sync {
    /// A web page, a link to a PDF, or a YouTube video. `work` is scratch space for this source.
    async fn url(&self, url: &str, work: &Path) -> anyhow::Result<Extracted>;
    /// A stored file (`name` is its original name, for the type).
    async fn file(&self, path: &Path, name: &str, work: &Path) -> anyhow::Result<Extracted>;
    /// Copies a file from Lucina's computer (`/home/cua/…`) to `dest_dir`; returns its path.
    async fn sandbox(&self, path: &str, dest_dir: &Path) -> anyhow::Result<PathBuf>;
}

pub fn extension(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub fn is_pdf(name: &str, head: &[u8]) -> bool {
    extension(name) == "pdf" || head.starts_with(b"%PDF")
}

pub const TEXT_EXT: [&str; 8] = ["txt", "md", "markdown", "csv", "json", "html", "htm", "rst"];

/// The text of every page, in order (empty pages kept, so numbering stays right).
pub async fn pdf_pages(path: &Path) -> anyhow::Result<Vec<Part>> {
    let p = path.to_owned();
    let pages = tokio::task::spawn_blocking(move || pdf_extract::extract_text_by_pages(&p))
        .await
        .map_err(|e| anyhow::anyhow!("the PDF reader crashed: {e}"))??;
    Ok(pages
        .into_iter()
        .enumerate()
        .map(|(i, t)| Part {
            text: clean_pdf_text(&t),
            page: Some(i as i64 + 1),
            start_s: None,
        })
        .collect())
}

/// pdf-extract leaves hyphenated line ends and single line breaks inside paragraphs.
pub fn clean_pdf_text(t: &str) -> String {
    let t = t.replace("-\n", "").replace('\u{0}', "");
    let mut out = String::with_capacity(t.len());
    for para in t.split("\n\n") {
        let p = para.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
        if !p.is_empty() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&p);
        }
    }
    out
}

/// What the crate can read without lucina-server: PDFs (text layer) and text files.
pub async fn basic_file(path: &Path, name: &str) -> anyhow::Result<Option<Extracted>> {
    let head = {
        use tokio::io::AsyncReadExt;
        let mut f = tokio::fs::File::open(path).await?;
        let mut b = [0u8; 8];
        let n = f.read(&mut b).await?;
        b[..n].to_vec()
    };
    if is_pdf(name, &head) {
        let parts = pdf_pages(path).await?;
        let pages = parts.len() as i64;
        return Ok(Some(Extracted {
            kind: Some("pdf".into()),
            pages: Some(pages),
            parts,
            ..Default::default()
        }));
    }
    if TEXT_EXT.contains(&extension(name).as_str()) {
        let bytes = tokio::fs::read(path).await?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        return Ok(Some(Extracted {
            kind: Some("text".into()),
            parts: vec![Part { text, ..Default::default() }],
            ..Default::default()
        }));
    }
    Ok(None)
}

/// The YouTube video id in a link, if it is one.
pub fn youtube_id(url: &str) -> Option<String> {
    let u = reqwest::Url::parse(url.trim()).ok()?;
    let host = u.host_str()?.trim_start_matches("www.").trim_start_matches("m.");
    let id = match host {
        "youtu.be" => u.path_segments()?.next()?.to_owned(),
        "youtube.com" | "music.youtube.com" => {
            if let Some(v) = u.query_pairs().find(|(k, _)| k == "v") {
                v.1.into_owned()
            } else {
                let mut seg = u.path_segments()?;
                match seg.next()? {
                    "shorts" | "live" | "embed" => seg.next()?.to_owned(),
                    _ => return None,
                }
            }
        }
        _ => return None,
    };
    (id.len() == 11 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')).then_some(id)
}

/// For tests and as a fallback: text and PDFs only.
pub struct Basic;

#[async_trait]
impl Extractor for Basic {
    async fn url(&self, _url: &str, _work: &Path) -> anyhow::Result<Extracted> {
        anyhow::bail!("links are not supported here")
    }
    async fn file(&self, path: &Path, name: &str, _work: &Path) -> anyhow::Result<Extracted> {
        basic_file(path, name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("this kind of file is not supported yet"))
    }
    async fn sandbox(&self, _path: &str, _dest: &Path) -> anyhow::Result<PathBuf> {
        anyhow::bail!("Lucina's computer is not reachable here")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn youtube_links() {
        assert_eq!(youtube_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=1"), Some("dQw4w9WgXcQ".into()));
        assert_eq!(youtube_id("https://youtu.be/dQw4w9WgXcQ"), Some("dQw4w9WgXcQ".into()));
        assert_eq!(youtube_id("https://youtube.com/shorts/dQw4w9WgXcQ"), Some("dQw4w9WgXcQ".into()));
        assert_eq!(youtube_id("https://example.com/watch?v=dQw4w9WgXcQ"), None);
        assert_eq!(youtube_id("https://youtube.com/feed"), None);
    }

    #[test]
    fn pdf_text_is_reflowed() {
        assert_eq!(clean_pdf_text("Snoop-\ning is\nuseful.\n\nNext para"), "Snooping is useful.\n\nNext para");
    }

    #[tokio::test]
    async fn text_files_are_read() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.md");
        std::fs::write(&p, "# Title\n\nBody").unwrap();
        let x = basic_file(&p, "a.md").await.unwrap().unwrap();
        assert_eq!(x.kind.as_deref(), Some("text"));
        assert!(x.text().contains("Body"));
        let p = d.path().join("a.bin");
        std::fs::write(&p, [0u8, 1, 2]).unwrap();
        assert!(basic_file(&p, "a.bin").await.unwrap().is_none());
    }
}
