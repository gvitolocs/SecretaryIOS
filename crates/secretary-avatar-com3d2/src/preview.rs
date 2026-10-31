//! Loopback review server. Native asset work is performed in Rust; no raw source
//! paths, archive data or configuration files are served to the browser.
use crate::{
    invalid,
    wardrobe::{Library, Settings},
    Result,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{fs::File, io::Read, path::Path};
fn cache_file(path: &str) -> Option<String> {
    let key = path.strip_prefix("/cache/view-")?.strip_suffix(".glb")?;
    if key.len() != 16 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("view-{key}.glb"))
}
fn json_response(request: tiny_http::Request, status: u16, value: Value) {
    let mut r = tiny_http::Response::from_string(value.to_string()).with_status_code(status);
    for (k, v) in [
        ("Content-Type", "application/json"),
        ("Cache-Control", "no-store"),
        ("X-Content-Type-Options", "nosniff"),
    ] {
        r.add_header(tiny_http::Header::from_bytes(k, v).unwrap());
    }
    let _ = request.respond(r);
}
/// Where the server listens and whom it answers. The defaults are the loopback review server; the
/// Secretary deployment sets them from the environment:
/// - `COM3D2_LISTEN`: address (default `127.0.0.1:28983`);
/// - `COM3D2_HOSTS` / `COM3D2_ORIGINS`: comma-separated `Host` and `Origin` values accepted;
/// - `COM3D2_GATEWAY_KEY_FILE`: every request must carry this key in `X-Com3d2-Gateway-Key`
///   (the p1 gateway adds it after the session check; the key never reaches a browser).
pub struct ServeOptions {
    pub listen: String,
    pub hosts: Vec<String>,
    pub origins: Vec<String>,
    pub gateway_key: Option<String>,
}
impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:28983".into(),
            hosts: vec!["127.0.0.1:28983".into(), "localhost:28983".into()],
            origins: vec!["http://127.0.0.1:28983".into(), "http://localhost:28983".into()],
            gateway_key: None,
        }
    }
}
impl ServeOptions {
    pub fn from_env() -> Result<Self> {
        let list = |v: String| v.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect();
        let mut o = Self::default();
        if let Ok(v) = std::env::var("COM3D2_LISTEN") {
            o.listen = v;
        }
        if let Ok(v) = std::env::var("COM3D2_HOSTS") {
            o.hosts = list(v);
        }
        if let Ok(v) = std::env::var("COM3D2_ORIGINS") {
            o.origins = list(v);
        }
        if let Ok(path) = std::env::var("COM3D2_GATEWAY_KEY_FILE") {
            let key = std::fs::read_to_string(&path)?.trim().to_owned();
            if key.len() < 32 {
                return Err(invalid("gateway key shorter than 32 characters"));
            }
            o.gateway_key = Some(key);
        }
        Ok(o)
    }
    /// The request's `Host` is served and, when a gateway key is set, the key matches.
    fn admits(&self, host: &str, key: Option<&str>) -> bool {
        self.hosts.iter().any(|h| h == host)
            && self.gateway_key.as_deref().is_none_or(|k| key.is_some_and(|g| same(g.as_bytes(), k.as_bytes())))
    }
}
/// Constant-time equality.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}
fn header<'a>(request: &'a tiny_http::Request, name: &'static str) -> Option<&'a str> {
    request.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str())
}
fn body(request: &mut tiny_http::Request, options: &ServeOptions) -> Result<Vec<u8>> {
    let content = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Content-Type"))
        .map(|h| h.value.as_str())
        .unwrap_or("");
    if !content.starts_with("application/json") {
        return Err(invalid("JSON content type required"));
    }
    let origin = header(request, "Origin");
    if origin.is_some_and(|o| !options.origins.iter().any(|a| a == o)) {
        return Err(invalid("cross-origin request refused"));
    }
    if request
        .headers()
        .iter()
        .any(|h| h.field.equiv("Sec-Fetch-Site") && h.value.as_str() == "cross-site")
    {
        return Err(invalid("cross-site request refused"));
    }
    let mut data = Vec::new();
    request.as_reader().take(65537).read_to_end(&mut data)?;
    if data.len() > 65536 {
        return Err(invalid("request body too large"));
    }
    Ok(data)
}
pub fn serve(root: &Path, bundle: &Path, options: &ServeOptions) -> Result<()> {
    let root = root.canonicalize()?;
    let mut library = if root.join("library-config.json").is_file() {
        Some(Library::open(&root)?)
    } else {
        None
    };
    let server = tiny_http::Server::http(&options.listen).map_err(|e| invalid(e.to_string()))?;
    for mut request in server.incoming_requests() {
        if !options.admits(header(&request, "Host").unwrap_or(""), header(&request, "X-Com3d2-Gateway-Key")) {
            let _ = request.respond(tiny_http::Response::empty(403));
            continue;
        }
        let path = request.url().split('?').next().unwrap_or("").to_owned();
        if let Some(library) = &mut library {
            if request.method() == &tiny_http::Method::Get && path == "/catalog.json" {
                json_response(request, 200, library.catalog());
                continue;
            }
            if request.method() == &tiny_http::Method::Get && path == "/settings" {
                match library.settings() {
                    Ok(s) => json_response(request, 200, json!(s)),
                    Err(e) => json_response(request, 500, json!({"error":e.to_string()})),
                }
                continue;
            }
            if request.method() == &tiny_http::Method::Post && path == "/compose" {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Selection {
                    items: Vec<String>,
                }
                let result = body(&mut request, options)
                    .and_then(|b| {
                        serde_json::from_slice::<Selection>(&b).map_err(|e| invalid(e.to_string()))
                    })
                    .and_then(|s| library.compose(&s.items));
                match result {
                    Ok(value) => json_response(request, 200, value),
                    Err(e) => json_response(request, 422, json!({"error":e.to_string()})),
                }
                continue;
            }
            if request.method() == &tiny_http::Method::Post && path == "/settings" {
                let result = body(&mut request, options)
                    .and_then(|b| {
                        serde_json::from_slice::<Settings>(&b).map_err(|e| invalid(e.to_string()))
                    })
                    .and_then(|s| {
                        library.save_settings(&s)?;
                        Ok(json!(s))
                    });
                match result {
                    Ok(value) => json_response(request, 200, value),
                    Err(e) => json_response(request, 422, json!({"error":e.to_string()})),
                }
                continue;
            }
        }
        if request.method() != &tiny_http::Method::Get
            && request.method() != &tiny_http::Method::Head
        {
            let _ = request.respond(tiny_http::Response::empty(405));
            continue;
        }
        let (file, mime) = match path.as_str() {
            "/" => (None, "text/html; charset=utf-8"),
            "/stage.js" => (Some(bundle.to_path_buf()), "text/javascript"),
            "/body.wasm" => (Some(root.join("body.wasm")), "application/wasm"),
            "/bindings.json" | "/camera.json" => (Some(root.join(&path[1..])), "application/json"),
            "/native-body.glb" | "/native-dressed.glb" | "/native-idle.glb" => {
                (Some(root.join(&path[1..])), "model/gltf-binary")
            }
            p => {
                if let Some(name) = cache_file(p) {
                    (Some(root.join("cache").join(name)), "model/gltf-binary")
                } else {
                    let _ = request.respond(tiny_http::Response::empty(404));
                    continue;
                }
            }
        };
        let mut response = if let Some(file) = file {
            let Ok(canonical) = file.canonicalize() else {
                let _ = request.respond(tiny_http::Response::empty(404));
                continue;
            };
            if path != "/stage.js" && !canonical.starts_with(&root) {
                let _ = request.respond(tiny_http::Response::empty(403));
                continue;
            }
            match File::open(canonical) {
                Ok(f) => tiny_http::Response::from_file(f).boxed(),
                Err(_) => {
                    let _ = request.respond(tiny_http::Response::empty(404));
                    continue;
                }
            }
        } else {
            tiny_http::Response::from_string(if library.is_some() {
                include_str!("../ui/wardrobe.html")
            } else {
                include_str!("../ui/index.html")
            })
            .boxed()
        };
        for (k, v) in [
            ("Content-Type", mime),
            ("Cache-Control", "no-store"),
            ("X-Content-Type-Options", "nosniff"),
            ("Referrer-Policy", "no-referrer"),
        ] {
            response.add_header(
                tiny_http::Header::from_bytes(k, v).map_err(|_| invalid("response header"))?,
            );
        }
        // Models are tens of MB: a slow download must not hold up the next request.
        std::thread::spawn(move || {
            let _ = request.respond(response);
        });
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gateway_mode_needs_the_served_host_and_the_exact_key() {
        let local = ServeOptions::default();
        assert!(local.admits("127.0.0.1:28983", None));
        assert!(!local.admits("getsecretary.tech", None));
        let key = "k".repeat(40);
        let gw = ServeOptions { hosts: vec!["com3d2.internal".into()], gateway_key: Some(key.clone()), ..ServeOptions::default() };
        assert!(gw.admits("com3d2.internal", Some(&key)));
        assert!(!gw.admits("com3d2.internal", None));
        assert!(!gw.admits("com3d2.internal", Some(&"k".repeat(39))));
        assert!(!gw.admits("com3d2.internal", Some(&format!("{}x", "k".repeat(39)))));
        assert!(!gw.admits("127.0.0.1:28983", Some(&key)));
    }
    #[test]
    fn cached_asset_routes_cannot_escape_or_expose_source_files() {
        assert_eq!(
            cache_file("/cache/view-0123456789abcdef.glb").as_deref(),
            Some("view-0123456789abcdef.glb")
        );
        for p in [
            "/cache/../library-config.json",
            "/cache/view-../body.glb",
            "/cache/view-0123456789abcdeg.glb",
            "/cache/view-0123456789abcdef.glb/../x",
        ] {
            assert!(cache_file(p).is_none());
        }
    }
}
