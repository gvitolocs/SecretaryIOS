//! Turntable preview MP4 (Telegram/Discord cannot spin a 3D model).
//!
//! Runs `tools/meshy-preview/render-turntable.mjs`: headless Chrome with
//! SwiftShader (CPU only; the page refuses to draw on a GPU because Qwen
//! shares nezopt's), three.js from `packages/lucina-stage`, frames piped to
//! ffmpeg/libx264. Default 5 s, 1280×720, 30 fps, at most 8 MB (Discord's
//! limit without Nitro); an animated model plays its first clip.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::AsyncReadExt;

#[derive(Debug, Clone)]
pub struct PreviewConfig {
    /// `LUCINA_MESHY_PREVIEW` (default true).
    pub enabled: bool,
    pub node: PathBuf,
    /// `LUCINA_MESHY_PREVIEW_SCRIPT`.
    pub script: PathBuf,
    /// Extra environment for the script (`MESHY_PREVIEW_THREE`,
    /// `MESHY_PREVIEW_PLAYWRIGHT`, `MESHY_PREVIEW_CHROME`, `FFMPEG_PATH`).
    pub env: Vec<(String, String)>,
    pub seconds: f32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub max_mb: u32,
    pub timeout: Duration,
}

impl Default for PreviewConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            node: PathBuf::from("node"),
            script: PathBuf::from(
                "/home/nez/Projects/Lucina/SecretaryIOS/tools/meshy-preview/render-turntable.mjs",
            ),
            env: Vec::new(),
            seconds: 5.0,
            width: 1280,
            height: 720,
            fps: 30,
            max_mb: 8,
            timeout: Duration::from_secs(420),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewInfo {
    pub path: PathBuf,
    pub bytes: u64,
    pub seconds: f32,
    pub clip: Option<String>,
    pub renderer: String,
    pub render_s: f64,
}

/// An MP4 starts with an `ftyp` box.
pub fn looks_like_mp4(bytes: &[u8]) -> bool {
    bytes.len() > 12 && &bytes[4..8] == b"ftyp"
}

pub async fn render(
    cfg: &PreviewConfig,
    glb: &Path,
    out: &Path,
    label: &str,
) -> Result<PreviewInfo, String> {
    if !cfg.enabled {
        return Err("previews are off (LUCINA_MESHY_PREVIEW=false)".into());
    }
    let label: String = label.chars().filter(|c| !c.is_control()).take(60).collect();
    let mut cmd = tokio::process::Command::new(&cfg.node);
    cmd.arg(&cfg.script)
        .arg("--glb")
        .arg(glb)
        .arg("--out")
        .arg(out)
        .args(["--seconds", &cfg.seconds.to_string()])
        .args(["--width", &cfg.width.to_string()])
        .args(["--height", &cfg.height.to_string()])
        .args(["--fps", &cfg.fps.to_string()])
        .args(["--max-mb", &cfg.max_mb.to_string()])
        .args(["--label", &label])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for (k, v) in &cfg.env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("preview renderer did not start: {}", e.kind()))?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let run = async {
        let mut s = String::new();
        stdout
            .read_to_string(&mut s)
            .await
            .map_err(|e| e.to_string())?;
        let status = child.wait().await.map_err(|e| e.to_string())?;
        Ok::<_, String>((s, status))
    };
    let (text, status) = tokio::time::timeout(cfg.timeout, run)
        .await
        .map_err(|_| format!("preview render took longer than {}s", cfg.timeout.as_secs()))??;
    let line = text
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or("");
    let v: Value = serde_json::from_str(line).unwrap_or(Value::Null);
    if !status.success() || v["ok"] != Value::Bool(true) {
        let err = v["error"].as_str().unwrap_or("renderer failed");
        return Err(format!(
            "preview render failed: {}",
            err.chars().take(300).collect::<String>()
        ));
    }
    let renderer = v["renderer"].as_str().unwrap_or("").to_owned();
    if !renderer.to_ascii_lowercase().contains("swiftshader") {
        let _ = std::fs::remove_file(out);
        return Err("preview refused: not rendered on SwiftShader (CPU)".into());
    }
    let bytes = std::fs::read(out).map_err(|e| format!("preview missing: {}", e.kind()))?;
    if !looks_like_mp4(&bytes) || bytes.len() as u64 > u64::from(cfg.max_mb) * 1024 * 1024 + 1024 {
        return Err("preview is not a valid MP4 within the size limit".into());
    }
    Ok(PreviewInfo {
        path: out.to_owned(),
        bytes: bytes.len() as u64,
        seconds: v["seconds"].as_f64().unwrap_or(0.0) as f32,
        clip: v["clip"].as_str().map(str::to_owned),
        renderer,
        render_s: v["renderS"].as_f64().unwrap_or(0.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mp4_magic() {
        let mut b = vec![0, 0, 0, 0x20];
        b.extend_from_slice(b"ftypisom0000");
        assert!(looks_like_mp4(&b));
        assert!(!looks_like_mp4(b"GIF89a......."));
    }

    #[tokio::test]
    async fn refuses_a_renderer_that_is_not_swiftshader() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake.mjs");
        let out = dir.path().join("out.mp4");
        std::fs::write(
            &script,
            format!(
                "import {{ writeFileSync }} from 'node:fs';\nwriteFileSync({:?}, Buffer.from('000000206674797069736f6d00000000', 'hex'));\nconsole.log(JSON.stringify({{ok: true, renderer: 'NVIDIA GeForce', seconds: 5}}));\n",
                out.display().to_string()
            ),
        )
        .unwrap();
        let cfg = PreviewConfig {
            script,
            ..PreviewConfig::default()
        };
        if std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
        {
            return; // no node on this machine
        }
        let err = render(&cfg, Path::new("/nonexistent.glb"), &out, "x")
            .await
            .unwrap_err();
        assert!(err.contains("SwiftShader"), "{err}");
        assert!(!out.exists(), "a GPU render is deleted");
    }
}
