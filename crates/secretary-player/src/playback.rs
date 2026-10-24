//! Playback plans: can a client play a file as it is, or does the server
//! remux it to HLS (codecs kept) or transcode it?
//!
//! The server probes each file once (ffprobe) and asks this module per
//! client profile, so every app and the server agree on what plays where.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Client {
    /// AVPlayer on iPhone/iPad (A12 or later).
    Ios,
    /// AVPlayer on macOS.
    Macos,
    /// Media3 ExoPlayer.
    Android,
    WebSafari,
    /// Chrome, Edge and other Chromium browsers (MSE via hls.js for HLS).
    WebChrome,
    WebFirefox,
}

/// What ffprobe found, normalised to ffprobe's codec names
/// (`h264`, `hevc`, `vp9`, `av1`, `aac`, `mp3`, `opus`, `ac3`, `eac3`, `flac`, `alac`, `pcm_s16le`...).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Probe {
    /// ffprobe `format_name`'s first entry or the file extension: `mov`, `mp4`, `matroska`, `webm`, `mp3`...
    pub container: String,
    #[serde(default)]
    pub video_codec: Option<String>,
    #[serde(default)]
    pub audio_codec: Option<String>,
    /// HEVC sample entry: `hvc1` plays on Apple players, `hev1` does not.
    #[serde(default)]
    pub codec_tag: Option<String>,
    /// PQ or HLG transfer.
    #[serde(default)]
    pub hdr: bool,
    /// 10-bit or more.
    #[serde(default)]
    pub bit_depth: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Serve the original bytes with HTTP range.
    Direct,
    /// HLS (fMP4 segments) with the streams copied.
    Remux,
    /// HLS with at least one stream re-encoded.
    Transcode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    None,
    Copy,
    H264,
    Aac,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub mode: Mode,
    pub video: Stream,
    pub audio: Stream,
    /// Copying HEVC: write the `hvc1` tag (ffmpeg `-tag:v hvc1`).
    pub hvc1_tag: bool,
    /// Re-encoding HDR to SDR H.264: tone-map.
    pub tonemap: bool,
}

fn direct_container(client: Client, c: &str, is_video: bool) -> bool {
    let apple = matches!(c, "mov" | "mp4" | "m4v" | "m4a" | "3gp" | "mov,mp4,m4a,3gp,3g2,mj2");
    let apple_audio = matches!(c, "mp3" | "aac" | "adts" | "wav" | "aiff" | "caf" | "flac");
    match client {
        Client::Ios | Client::Macos | Client::WebSafari => apple || (!is_video && apple_audio),
        Client::Android => apple || matches!(c, "matroska" | "mkv" | "webm" | "matroska,webm") || (!is_video && (apple_audio || matches!(c, "ogg"))),
        Client::WebChrome | Client::WebFirefox => {
            apple || matches!(c, "webm" | "matroska,webm") || (!is_video && (apple_audio || matches!(c, "ogg")))
        }
    }
}

fn video_ok(client: Client, codec: &str) -> bool {
    match client {
        Client::Ios | Client::Macos | Client::WebSafari => matches!(codec, "h264" | "hevc"),
        Client::Android => matches!(codec, "h264" | "hevc" | "vp9" | "av1"),
        Client::WebChrome => matches!(codec, "h264" | "vp9" | "av1"),
        Client::WebFirefox => matches!(codec, "h264" | "vp9" | "av1"),
    }
}

fn audio_ok(client: Client, codec: &str) -> bool {
    let pcm = codec.starts_with("pcm_");
    match client {
        Client::Ios | Client::Macos => pcm || matches!(codec, "aac" | "mp3" | "alac" | "flac" | "ac3" | "eac3" | "opus"),
        Client::WebSafari => pcm || matches!(codec, "aac" | "mp3" | "alac" | "flac" | "opus"),
        Client::Android => pcm || matches!(codec, "aac" | "mp3" | "flac" | "opus" | "vorbis"),
        Client::WebChrome | Client::WebFirefox => pcm || matches!(codec, "aac" | "mp3" | "flac" | "opus" | "vorbis"),
    }
}

/// Codecs the HLS path can carry to this client in fMP4 segments.
fn hls_video_ok(client: Client, codec: &str) -> bool {
    match client {
        // hls.js feeds MSE: H.264 is the safe choice; VP9/AV1 in fMP4 HLS are not.
        Client::WebChrome | Client::WebFirefox => codec == "h264",
        _ => video_ok(client, codec),
    }
}

fn hls_audio_ok(client: Client, codec: &str) -> bool {
    match client {
        Client::Ios | Client::Macos | Client::WebSafari => matches!(codec, "aac" | "mp3" | "ac3" | "eac3" | "alac" | "flac"),
        Client::Android => matches!(codec, "aac" | "mp3" | "ac3" | "eac3" | "opus" | "flac"),
        Client::WebChrome | Client::WebFirefox => matches!(codec, "aac" | "mp3"),
    }
}

pub fn plan(probe: &Probe, client: Client) -> Plan {
    let container = probe.container.to_ascii_lowercase();
    let video = probe.video_codec.as_deref().map(str::to_ascii_lowercase);
    let audio = probe.audio_codec.as_deref().map(str::to_ascii_lowercase);
    let apple = matches!(client, Client::Ios | Client::Macos | Client::WebSafari);
    let bad_hevc_tag = apple && video.as_deref() == Some("hevc") && probe.codec_tag.as_deref().is_some_and(|t| t != "hvc1");
    // A 10-bit H.264 (Hi10P) has no hardware decoder anywhere.
    let hi10 = video.as_deref() == Some("h264") && probe.bit_depth.is_some_and(|b| b > 8);

    let v_direct = video.as_deref().is_none_or(|v| video_ok(client, v) && !hi10);
    let a_direct = audio.as_deref().is_none_or(|a| audio_ok(client, a));
    if v_direct && a_direct && !bad_hevc_tag && direct_container(client, &container, video.is_some()) {
        let copy = |s: &Option<String>| if s.is_some() { Stream::Copy } else { Stream::None };
        return Plan { mode: Mode::Direct, video: copy(&video), audio: copy(&audio), hvc1_tag: false, tonemap: false };
    }

    let v = match video.as_deref() {
        None => Stream::None,
        Some(v) if hls_video_ok(client, v) && !hi10 => Stream::Copy,
        Some(_) => Stream::H264,
    };
    let a = match audio.as_deref() {
        None => Stream::None,
        Some(a) if hls_audio_ok(client, a) => Stream::Copy,
        Some(_) => Stream::Aac,
    };
    let mode = if v == Stream::H264 || a == Stream::Aac { Mode::Transcode } else { Mode::Remux };
    Plan {
        mode,
        video: v,
        audio: a,
        hvc1_tag: v == Stream::Copy && video.as_deref() == Some("hevc"),
        tonemap: v == Stream::H264 && probe.hdr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(container: &str, v: Option<&str>, a: Option<&str>) -> Probe {
        Probe {
            container: container.into(),
            video_codec: v.map(Into::into),
            audio_codec: a.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn iphone_movies_play_direct() {
        let plan = plan(&p("mov", Some("hevc"), Some("aac")), Client::Ios);
        assert_eq!(plan.mode, Mode::Direct);
        assert_eq!(plan.video, Stream::Copy);
    }

    #[test]
    fn mkv_h264_remuxes_for_apple_and_plays_on_android() {
        let probe = p("matroska", Some("h264"), Some("aac"));
        assert_eq!(plan(&probe, Client::Ios).mode, Mode::Remux);
        assert_eq!(plan(&probe, Client::Android).mode, Mode::Direct);
    }

    #[test]
    fn dts_audio_is_transcoded_video_copied() {
        let plan = plan(&p("matroska", Some("hevc"), Some("dts")), Client::Ios);
        assert_eq!((plan.mode, plan.video, plan.audio, plan.hvc1_tag), (Mode::Transcode, Stream::Copy, Stream::Aac, true));
    }

    #[test]
    fn hev1_tagged_mp4_needs_remux_on_apple() {
        let mut probe = p("mp4", Some("hevc"), Some("aac"));
        probe.codec_tag = Some("hev1".into());
        let plan = plan(&probe, Client::Ios);
        assert_eq!((plan.mode, plan.hvc1_tag), (Mode::Remux, true));
    }

    #[test]
    fn hevc_hdr_for_chrome_transcodes_with_tonemap() {
        let mut probe = p("mp4", Some("hevc"), Some("eac3"));
        probe.hdr = true;
        let plan = plan(&probe, Client::WebChrome);
        assert_eq!((plan.mode, plan.video, plan.audio, plan.tonemap), (Mode::Transcode, Stream::H264, Stream::Aac, true));
    }

    #[test]
    fn audio_files() {
        assert_eq!(plan(&p("mp3", None, Some("mp3")), Client::Ios).mode, Mode::Direct);
        assert_eq!(plan(&p("ogg", None, Some("opus")), Client::WebChrome).mode, Mode::Direct);
        let ogg_ios = plan(&p("ogg", None, Some("opus")), Client::Ios);
        assert_eq!((ogg_ios.mode, ogg_ios.audio), (Mode::Transcode, Stream::Aac));
        assert_eq!(plan(&p("flac", None, Some("flac")), Client::Ios).mode, Mode::Direct);
    }

    #[test]
    fn hi10p_is_transcoded() {
        let mut probe = p("matroska", Some("h264"), Some("aac"));
        probe.bit_depth = Some(10);
        assert_eq!(plan(&probe, Client::Android).video, Stream::H264);
    }
}
