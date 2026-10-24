//! End-to-end through the router with real ffmpeg output (skipped when
//! ffmpeg is not installed).
use super::*;
use crate::dir::{DirSource, Root};
use axum::http::{Method, Request as HttpRequest};
use std::process::Command as StdCommand;

fn have_ffmpeg() -> bool {
    StdCommand::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

fn ffmpeg(args: &[&str]) {
    let st = StdCommand::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).status().unwrap();
    assert!(st.success(), "ffmpeg {args:?}");
}

async fn call(app: &Router, method: Method, uri: &str, body: Option<serde_json::Value>, range: Option<&str>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut req = HttpRequest::builder().method(method).uri(uri);
    if let Some(r) = range {
        req = req.header(header::RANGE, r);
    }
    let req = match body {
        Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())).unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec();
    (status, headers, bytes)
}

fn json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap_or_else(|_| panic!("not json: {}", String::from_utf8_lossy(bytes)))
}

#[tokio::test]
async fn library_play_range_hls_subs_poster_progress() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg missing: skipped");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let media = tmp.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    let mkv = media.join("Film.mkv");
    let mp4 = media.join("Clip.mp4");
    let mp3 = media.join("Memo.mp3");
    ffmpeg(&["-f", "lavfi", "-i", "testsrc=size=320x240:rate=25:duration=14", "-f", "lavfi", "-i", "sine=frequency=440:duration=14",
        "-c:v", "libx264", "-g", "25", "-c:a", "ac3", mkv.to_str().unwrap()]);
    ffmpeg(&["-f", "lavfi", "-i", "testsrc=size=320x240:rate=25:duration=3", "-f", "lavfi", "-i", "sine=duration=3",
        "-c:v", "libx264", "-c:a", "aac", "-movflags", "+faststart", mp4.to_str().unwrap()]);
    ffmpeg(&["-f", "lavfi", "-i", "sine=duration=5", "-c:a", "libmp3lame", mp3.to_str().unwrap()]);
    std::fs::write(media.join("Film.da.srt"), b"1\n00:00:01,000 --> 00:00:02,000\nHej p\xe5 dig\n").unwrap();

    let mut cfg = StreamConfig::new(vec![7; 32], tmp.path().join("cache"), tmp.path().join("progress.json"));
    cfg.max_jobs = 1;
    let src = Arc::new(DirSource::new(vec![Root { label: "Test".into(), path: media.clone() }]));
    let app = router(Arc::clone(&src), cfg.clone()).unwrap().merge(dir::items_router(src, cfg));

    let (st, _, body) = call(&app, Method::GET, "/v1/media/items", None, None).await;
    assert_eq!(st, StatusCode::OK);
    let list = json(&body);
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    let id_of = |title: &str| items.iter().find(|i| i["title"] == title).unwrap()["id"].as_str().unwrap().to_string();
    let (film, clip, memo) = (id_of("Film"), id_of("Clip"), id_of("Memo"));

    // Direct play with Range.
    let (st, _, body) = call(&app, Method::POST, &format!("/v1/media/items/{clip}/play"), Some(serde_json::json!({"client": "ios"})), None).await;
    assert_eq!(st, StatusCode::OK);
    let play = json(&body);
    assert_eq!(play["mode"], "direct");
    assert_eq!(play["url"], play["file_url"]);
    let url = play["url"].as_str().unwrap().to_string();
    let (st, headers, body) = call(&app, Method::GET, &url, None, Some("bytes=0-99")).await;
    assert_eq!(st, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body.len(), 100);
    assert!(headers[header::CONTENT_RANGE].to_str().unwrap().starts_with("bytes 0-99/"));
    assert_eq!(headers[header::CONTENT_TYPE], "video/mp4");

    // A forged or foreign token is refused.
    let forged = url.replace(&clip, &memo);
    assert_eq!(call(&app, Method::GET, &forged, None, None).await.0, StatusCode::FORBIDDEN);

    // MKV with AC-3 remuxes to HLS for iOS; transcodes the audio for Chrome.
    let (_, _, body) = call(&app, Method::POST, &format!("/v1/media/items/{film}/play"), Some(serde_json::json!({"client": "ios"})), None).await;
    let play = json(&body);
    assert_eq!(play["mode"], "remux");
    assert_eq!(play["duration_ms"], 14_000);
    let hls = play["hls_url"].as_str().unwrap().to_string();
    assert!(hls.ends_with("/hls/copy-copy/index.m3u8"), "{hls}");
    let (st, headers, body) = call(&app, Method::GET, &hls, None, None).await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(headers[header::CONTENT_TYPE], "application/vnd.apple.mpegurl");
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("#EXTINF") && text.contains("init.mp4"), "{text}");
    let init = hls.replace("index.m3u8", "init.mp4");
    let (st, _, body) = call(&app, Method::GET, &init, None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(!body.is_empty());
    assert_eq!(call(&app, Method::GET, &hls.replace("index.m3u8", "..%2Fx"), None, None).await.0, StatusCode::NOT_FOUND);
    let (_, _, body) = call(&app, Method::POST, &format!("/v1/media/items/{film}/play"), Some(serde_json::json!({"client": "web_chrome"})), None).await;
    assert_eq!(json(&body)["mode"], "transcode");

    // Wait for the remux to finish: the playlist turns VOD with every segment.
    let mut done = false;
    for _ in 0..120 {
        let (_, _, body) = call(&app, Method::GET, &hls, None, None).await;
        let text = String::from_utf8(body).unwrap();
        if text.contains("#EXT-X-ENDLIST") {
            assert!(text.contains("#EXT-X-PLAYLIST-TYPE:VOD"));
            assert!(!text.contains("EXT-X-START"));
            assert!(text.matches("#EXTINF").count() >= 2);
            done = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(done, "remux did not finish");

    // External Windows-1252 .srt → UTF-8 WebVTT.
    let sub = &play["subtitles"][0];
    assert_eq!(sub["lang"], "da");
    let (st, headers, body) = call(&app, Method::GET, sub["url"].as_str().unwrap(), None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/vtt"));
    assert_eq!(String::from_utf8(body).unwrap(), "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHej på dig\n");

    // Poster (a frame) and audio (no art: 404).
    let (st, headers, body) = call(&app, Method::GET, play["poster_url"].as_str().unwrap(), None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/jpeg");
    assert_eq!(&body[..2], &[0xff, 0xd8]);
    let (_, _, body) = call(&app, Method::POST, &format!("/v1/media/items/{memo}/play"), Some(serde_json::json!({"client": "ios"})), None).await;
    let audio = json(&body);
    assert_eq!((audio["mode"].as_str(), audio["kind"].as_str()), (Some("direct"), Some("audio")));
    assert_eq!(call(&app, Method::GET, audio["poster_url"].as_str().unwrap(), None, None).await.0, StatusCode::NOT_FOUND);

    // Progress: stored, resumed by /play, older writes acknowledged but not stored.
    let entry = serde_json::json!({"id": film, "position_ms": 9_000, "duration_ms": 14_000, "finished": false, "updated_at": 2_000, "device": "iphone"});
    let (_, _, body) = call(&app, Method::POST, "/v1/media/progress", Some(serde_json::json!({"entries": [entry]})), None).await;
    assert_eq!(json(&body)["acks"][0]["updated_at"], 2_000);
    let (_, _, body) = call(&app, Method::PUT, &format!("/v1/media/progress/{film}"),
        Some(serde_json::json!({"id": "", "position_ms": 1, "duration_ms": 14_000, "updated_at": 1_000})), None).await;
    assert_eq!(json(&body)["entries"][0]["position_ms"], 9_000);
    let (_, _, body) = call(&app, Method::GET, "/v1/media/progress?since=0", None, None).await;
    assert_eq!(json(&body)["entries"].as_array().unwrap().len(), 1);
    let (_, _, body) = call(&app, Method::POST, &format!("/v1/media/items/{film}/play"), Some(serde_json::json!({"client": "ios"})), None).await;
    assert_eq!(json(&body)["resume_ms"], 7_000);

    // Unknown items.
    assert_eq!(call(&app, Method::POST, "/v1/media/items/nope/play", Some(serde_json::json!({"client": "ios"})), None).await.0, StatusCode::NOT_FOUND);
}
