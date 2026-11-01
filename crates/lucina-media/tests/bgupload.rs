//! Background uploads (`bgupload.rs`) over a real socket, as PhotoKit's upload extension sends
//! them through p1: the preflight, the 104 before the final answer, a resumed upload, offset
//! conflicts, the capability check, and the ingest into the library.

use base64::Engine;
use lucina_media::config::Config;
use lucina_media::ingest::Library;
use lucina_media::meta::ClientMeta;
use lucina_media::{bgupload, store};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const KEY: &str = "0123456789abcdef0123456789abcdef";

async fn setup() -> (tempfile::TempDir, Library, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = Config::for_dirs(tmp.path().join("data"), tmp.path().join("store"));
    cfg.quiet_start = 0;
    cfg.quiet_end = 0;
    cfg.upstream_key = Some(KEY.into());
    store::init_store(&cfg).unwrap();
    let lib = Library::open(cfg).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(bgupload::serve_listener(lib.clone(), listener));
    (tmp, lib, addr)
}

fn png() -> Vec<u8> {
    let img = image::RgbImage::from_fn(64, 48, |x, y| image::Rgb([x as u8 * 4, y as u8 * 5, 90]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn meta_header(asset: &str) -> String {
    let meta = ClientMeta {
        filename: Some("IMG_0001.PNG".into()),
        mime: Some("image/png".into()),
        taken_at: Some("2026-11-01T08:12:11.593Z".into()),
        device: Some("ios-test".into()),
        asset_id: Some(asset.into()),
        source: Some("phone-ios".into()),
        ..Default::default()
    };
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&meta).unwrap())
}

/// One request on its own connection; the raw response bytes (informational ones included).
async fn raw(addr: std::net::SocketAddr, head: &str, body: &[u8]) -> String {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(head.as_bytes()).await.unwrap();
    s.write_all(body).await.unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).await.unwrap();
    String::from_utf8_lossy(&out).to_string()
}

fn request(method: &str, path: &str, headers: &[(&str, String)], len: usize) -> String {
    let mut h = format!(
        "{method} {path} HTTP/1.1\r\nhost: 100.107.164.33:8831\r\nx-forwarded-host: getsecretary.tech\r\nx-forwarded-proto: https\r\nx-media-upstream-key: {KEY}\r\nconnection: close\r\ncontent-length: {len}\r\n"
    );
    for (k, v) in headers {
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    h.push_str("\r\n");
    h
}

fn header<'a>(resp: &'a str, name: &str) -> Option<&'a str> {
    // The final response's headers (after any 1xx).
    let last = resp.rfind("HTTP/1.1 ").unwrap();
    resp[last..]
        .split("\r\n\r\n")
        .next()
        .unwrap()
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim())
        })
}

fn path_of(location: &str) -> &str {
    &location[location.find("/v1/").unwrap()..]
}

#[tokio::test]
async fn the_watchdog_sees_photos_no_background_upload_delivered() {
    let (_t, lib, addr) = setup().await;
    let later = chrono::Utc::now() + chrono::Duration::hours(1);
    assert!(bgupload::missed_photos(&lib, later).unwrap().is_empty());

    // Came in through the app's own backup, and the extension never sent it.
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("IMG_0002.PNG");
    std::fs::write(&file, png()).unwrap();
    let meta = ClientMeta {
        filename: Some("IMG_0002.PNG".into()),
        taken_at: Some(now),
        device: Some("ios-test".into()),
        asset_id: Some("ASSET-5/L0/001".into()),
        source: Some("phone-ios".into()),
        ..Default::default()
    };
    let l = lib.clone();
    tokio::task::spawn_blocking(move || {
        lucina_media::ingest::ingest_file(&l, &file, None, &meta, None)
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        bgupload::missed_photos(&lib, later).unwrap(),
        ["IMG_0002.PNG"]
    );
    // Still within the time the extension has to send it.
    assert!(bgupload::missed_photos(&lib, chrono::Utc::now())
        .unwrap()
        .is_empty());

    // The extension delivers the same photo (the app was first): nothing is missing any more.
    let body = png();
    let enc = {
        let meta = ClientMeta {
            filename: Some("IMG_0002.PNG".into()),
            device: Some("ios-test".into()),
            asset_id: Some("ASSET-5/L0/001".into()),
            source: Some("phone-ios".into()),
            ..Default::default()
        };
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&meta).unwrap())
    };
    let headers = [("x-lucina-meta", enc), ("upload-complete", "?1".into())];
    let r = raw(
        addr,
        &request("POST", "/v1/media/bgupload", &headers, body.len()),
        &body,
    )
    .await;
    assert!(r.contains("HTTP/1.1 201"), "{r}");
    assert!(bgupload::missed_photos(&lib, later).unwrap().is_empty());
}

#[tokio::test]
async fn preflight_announces_the_limit() {
    let (_t, _lib, addr) = setup().await;
    let r = raw(addr, &request("OPTIONS", "/v1/media/bgupload", &[], 0), b"").await;
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    assert_eq!(header(&r, "upload-limit"), Some("20000000000"));
}

#[tokio::test]
async fn the_upstream_key_is_required() {
    let (_t, _lib, addr) = setup().await;
    let head = request("OPTIONS", "/v1/media/bgupload", &[], 0).replace(KEY, "wrong");
    let r = raw(addr, &head, b"").await;
    assert!(r.starts_with("HTTP/1.1 401"), "{r}");
}

#[tokio::test]
async fn a_whole_upload_gets_a_104_then_is_ingested() {
    let (_t, lib, addr) = setup().await;
    let body = png();
    let headers = [
        ("x-lucina-meta", meta_header("ASSET-1/L0/001")),
        ("upload-incomplete", "?0".into()),
        ("upload-draft-interop-version", "3".into()),
    ];
    let r = raw(
        addr,
        &request("POST", "/v1/media/bgupload", &headers, body.len()),
        &body,
    )
    .await;
    assert!(
        r.starts_with("HTTP/1.1 104 Upload Resumption Supported\r\n"),
        "{r}"
    );
    let first = r.split("\r\n\r\n").next().unwrap();
    assert!(
        first.contains("location: https://getsecretary.tech/v1/media/bgupload/bgu_"),
        "{first}"
    );
    assert!(first.contains("upload-draft-interop-version: 3"), "{first}");
    assert!(r.contains("HTTP/1.1 201 Created"), "{r}");
    assert_eq!(header(&r, "upload-incomplete"), Some("?0"));
    assert_eq!(
        header(&r, "upload-offset"),
        Some(body.len().to_string().as_str())
    );
    let item = header(&r, "x-lucina-item").unwrap().to_string();
    let (source, asset): (String, String) = lib
        .db
        .with(|c| {
            c.query_row(
                "SELECT source, asset_id FROM items WHERE id = ?1",
                [&item],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .unwrap();
    assert_eq!(
        (source.as_str(), asset.as_str()),
        ("phone-ios", "ASSET-1/L0/001")
    );

    // The same photo again (the app's own backup raced the extension): the same item.
    let r2 = raw(
        addr,
        &request("POST", "/v1/media/bgupload", &headers, body.len()),
        &body,
    )
    .await;
    assert_eq!(header(&r2, "x-lucina-item"), Some(item.as_str()));
}

#[tokio::test]
async fn a_cut_upload_resumes_with_head_and_patch() {
    let (_t, lib, addr) = setup().await;
    let body = png();
    let (a, b) = body.split_at(body.len() / 2);
    let headers = [
        ("x-lucina-meta", meta_header("ASSET-2/L0/001")),
        ("upload-complete", "?0".into()),
        ("upload-length", body.len().to_string()),
    ];
    let r = raw(
        addr,
        &request("POST", "/v1/media/bgupload", &headers, a.len()),
        a,
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 104"), "{r}");
    assert!(r.contains("HTTP/1.1 201 Created"), "{r}");
    assert_eq!(header(&r, "upload-complete"), Some("?0"));
    let url = path_of(header(&r, "location").unwrap()).to_string();

    let h = raw(addr, &request("HEAD", &url, &[], 0), b"").await;
    assert!(h.starts_with("HTTP/1.1 204"), "{h}");
    assert_eq!(
        header(&h, "upload-offset"),
        Some(a.len().to_string().as_str())
    );
    assert_eq!(header(&h, "cache-control"), Some("no-store"));

    // A wrong offset: 409 and where to resume.
    let wrong = [
        ("upload-offset", "0".to_string()),
        ("upload-complete", "?1".into()),
    ];
    let c = raw(addr, &request("PATCH", &url, &wrong, b.len()), b).await;
    assert!(c.starts_with("HTTP/1.1 409"), "{c}");
    assert_eq!(
        header(&c, "upload-offset"),
        Some(a.len().to_string().as_str())
    );

    // Another token for the same upload: not found.
    let forged = format!("{}{}", &url[..url.len() - 32], "0".repeat(32));
    let f = raw(addr, &request("HEAD", &forged, &[], 0), b"").await;
    assert!(f.starts_with("HTTP/1.1 404"), "{f}");

    let right = [
        ("upload-offset", a.len().to_string()),
        ("upload-complete", "?1".into()),
        ("content-type", "application/partial-upload".into()),
    ];
    let p = raw(addr, &request("PATCH", &url, &right, b.len()), b).await;
    assert!(p.starts_with("HTTP/1.1 200"), "{p}");
    assert_eq!(header(&p, "upload-complete"), Some("?1"));
    let item = header(&p, "x-lucina-item").unwrap().to_string();
    let bytes: i64 = lib
        .db
        .with(|c| {
            c.query_row("SELECT bytes FROM items WHERE id = ?1", [&item], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert_eq!(bytes as usize, body.len());

    // Done: more bytes are refused.
    let again = raw(addr, &request("PATCH", &url, &right, 1), b"x").await;
    assert!(again.starts_with("HTTP/1.1 400"), "{again}");
}

#[tokio::test]
async fn a_plain_upload_gets_no_104_and_metadata_is_required() {
    let (_t, _lib, addr) = setup().await;
    let body = png();
    let headers = [("x-lucina-meta", meta_header("ASSET-3/L0/001"))];
    let r = raw(
        addr,
        &request("POST", "/v1/media/bgupload", &headers, body.len()),
        &body,
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 201"), "{r}");
    let r = raw(
        addr,
        &request("POST", "/v1/media/bgupload", &[], body.len()),
        &body,
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 400"), "{r}");
}

#[tokio::test]
async fn an_unfinished_upload_can_be_cancelled() {
    let (_t, _lib, addr) = setup().await;
    let headers = [
        ("x-lucina-meta", meta_header("ASSET-4/L0/001")),
        ("upload-complete", "?0".into()),
    ];
    let r = raw(
        addr,
        &request("POST", "/v1/media/bgupload", &headers, 3),
        b"abc",
    )
    .await;
    let url = path_of(header(&r, "location").unwrap()).to_string();
    let d = raw(addr, &request("DELETE", &url, &[], 0), b"").await;
    assert!(d.starts_with("HTTP/1.1 204"), "{d}");
    let h = raw(addr, &request("HEAD", &url, &[], 0), b"").await;
    assert!(h.starts_with("HTTP/1.1 404"), "{h}");
}
