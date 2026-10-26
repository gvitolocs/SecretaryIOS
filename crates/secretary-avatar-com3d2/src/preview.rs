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
fn body(request: &mut tiny_http::Request) -> Result<Vec<u8>> {
    let content = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Content-Type"))
        .map(|h| h.value.as_str())
        .unwrap_or("");
    if !content.starts_with("application/json") {
        return Err(invalid("JSON content type required"));
    }
    let origin = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Origin"))
        .map(|h| h.value.as_str());
    if origin.is_some_and(|o| !matches!(o, "http://127.0.0.1:28983" | "http://localhost:28983")) {
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
pub fn serve(root: &Path, bundle: &Path) -> Result<()> {
    let root = root.canonicalize()?;
    let mut library = if root.join("library-config.json").is_file() {
        Some(Library::open(&root)?)
    } else {
        None
    };
    let server = tiny_http::Server::http("127.0.0.1:28983").map_err(|e| invalid(e.to_string()))?;
    for mut request in server.incoming_requests() {
        let host = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Host"))
            .map(|h| h.value.as_str())
            .unwrap_or("");
        if !["127.0.0.1:28983", "localhost:28983"].contains(&host) {
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
                let result = body(&mut request)
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
                let result = body(&mut request)
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
        let _ = request.respond(response);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
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
