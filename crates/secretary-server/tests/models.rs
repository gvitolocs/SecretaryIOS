//! `/v1/models*` on the real router and real files: Lucina (service token)
//! creates models and uploads files; devices read, stream and delete; files
//! are checked, private and never served without auth.

use reqwest::header::{CONTENT_TYPE, ETAG, IF_NONE_MATCH, RANGE};
use reqwest::{Client, Method, StatusCode};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

struct H {
    base: String,
    http: Client,
    token: String,
    service: String,
    data: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl H {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::new("http://127.0.0.1:9", None);
        cfg.data_dir = dir.path().join("state");
        cfg.rate_limit_per_minute = 10_000;
        let (_, token) = DeviceStore::new(&cfg.data_dir)
            .issue("test iPhone")
            .unwrap();
        let path = dir.path().join("service.token");
        secretary_server::service::write_new(&path, false).unwrap();
        cfg.service_token = Some(
            secretary_server::service::ServiceToken::read_file(path.to_str().unwrap()).unwrap(),
        );
        let service = std::fs::read_to_string(&path).unwrap().trim().to_owned();
        let state = AppState::new(&cfg).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            serve_on(listener, app(state), std::future::pending())
                .await
                .unwrap()
        });
        Self {
            base,
            http: Client::new(),
            token,
            service,
            data: cfg.data_dir,
            _dir: dir,
        }
    }

    fn req(&self, m: Method, p: &str, bearer: Option<&str>) -> reqwest::RequestBuilder {
        let rb = self.http.request(m, format!("{}{p}", self.base));
        match bearer {
            Some(b) => rb.bearer_auth(b),
            None => rb,
        }
    }

    async fn json(&self, rb: reqwest::RequestBuilder) -> (StatusCode, Value) {
        let r = rb.send().await.unwrap();
        let s = r.status();
        (s, r.json().await.unwrap_or(Value::Null))
    }
}

fn glb() -> Vec<u8> {
    let mut b = b"glTF\x02\0\0\0".to_vec();
    b.extend(std::iter::repeat_n(7u8, 4096));
    b
}

#[tokio::test]
async fn lucina_adds_devices_read_and_delete() {
    let h = H::new().await;
    let (svc, dev) = (h.service.clone(), h.token.clone());
    // Only Lucina creates.
    let body = json!({"title": "Knight", "prompt": "a knight in silver armor", "credits": 41, "animations": ["Casual Walk"],
                      "formats": ["glb", "usdz"], "meshy": {"taskId": "t-1", "taskKind": "text-to-3d", "rigTaskId": "r-1"}});
    let (s, _) = h
        .json(h.req(Method::POST, "/v1/models", Some(&dev)).json(&body))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = h
        .json(h.req(Method::POST, "/v1/models", None).json(&body))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, b) = h
        .json(
            h.req(Method::POST, "/v1/models", Some(&svc))
                .json(&json!({"title": ""})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
    let (s, b) = h
        .json(h.req(Method::POST, "/v1/models", Some(&svc)).json(&body))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let id = b["model"]["id"].as_str().unwrap().to_owned();
    assert_eq!(b["model"]["createdBy"], "lucina");

    // Uploads: checked by magic bytes, Lucina only.
    let up = |name: &str, bytes: Vec<u8>, who: &str| {
        h.req(
            Method::PUT,
            &format!("/v1/models/{id}/files/{name}"),
            Some(who),
        )
        .header(CONTENT_TYPE, "application/octet-stream")
        .body(bytes)
    };
    let (s, _) = h
        .json(up("model.glb", b"<html>not a glb</html>".to_vec(), &svc))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h.json(up("model.glb", glb(), &dev)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = h.json(up("evil.sh", b"#!/bin/sh".to_vec(), &svc)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, b) = h.json(up("model.glb", glb(), &svc)).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let (s, b) = h
        .json(up("thumb.png", b"\x89PNG\r\n\x1a\n0000".to_vec(), &svc))
        .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["model"]["files"].as_array().unwrap().len(), 2);
    // private files on disk
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(h.data.join("models").join(&id).join("model.glb")).unwrap();
        assert_eq!(m.permissions().mode() & 0o077, 0);
    }

    // Devices list, read and stream with ETag and ranges.
    let (s, b) = h
        .json(h.req(Method::GET, "/v1/models?q=walk", Some(&dev)))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["models"][0]["id"], json!(id));
    let (s, _) = h.json(h.req(Method::GET, "/v1/models", None)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let r = h
        .req(
            Method::GET,
            &format!("/v1/models/{id}/files/model.glb"),
            Some(&dev),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()[CONTENT_TYPE], "model/gltf-binary");
    assert!(r.headers()["cache-control"]
        .to_str()
        .unwrap()
        .starts_with("private"));
    let etag = r.headers()[ETAG].to_str().unwrap().to_owned();
    assert_eq!(r.bytes().await.unwrap().len(), glb().len());
    let r = h
        .req(
            Method::GET,
            &format!("/v1/models/{id}/files/model.glb"),
            Some(&dev),
        )
        .header(IF_NONE_MATCH, &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_MODIFIED);
    let r = h
        .req(
            Method::GET,
            &format!("/v1/models/{id}/files/model.glb"),
            Some(&dev),
        )
        .header(RANGE, "bytes=0-3")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(&r.bytes().await.unwrap()[..], b"glTF");
    for bad in ["preview.mp4", "../model.json", "model.json"] {
        let r = h
            .req(
                Method::GET,
                &format!("/v1/models/{id}/files/{bad}"),
                Some(&dev),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "{bad}");
    }
    let r = h
        .req(
            Method::GET,
            &format!("/v1/models/{id}/files/model.glb"),
            None,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    // Delete (device), then it is gone.
    let (s, b) = h
        .json(h.req(Method::DELETE, &format!("/v1/models/{id}"), Some(&dev)))
        .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let (s, _) = h
        .json(h.req(Method::GET, &format!("/v1/models/{id}"), Some(&dev)))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(!h.data.join("models").join(&id).exists());
}
