//! The refusals and the delivery of `lucina-updates`, over a real socket with a raw HTTP/1.1
//! client (so `..`, `//` and `%2e` reach the server as written). Every test but the atomic
//! publish one runs with the whole store frozen (directories 0555, files 0444) and ends by
//! checking that nothing in it was created, changed or removed. Test data lives in a temporary
//! directory that a drop guard thaws and removes, also when an assertion fails.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const GIB4: u64 = 4 * 1024 * 1024 * 1024;
const MARK: &[u8] = b"HIGH-OFFSET-DATA";

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn sha(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}
/// 1000 bytes that differ from one position to the next.
fn blob() -> Vec<u8> {
    (0..1000u32).map(|i| (i * 7 + 3) as u8).collect()
}

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}
impl Resp {
    fn h(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

async fn raw(addr: &str, method: &str, path: &str, extra: &[(&str, &str)]) -> Resp {
    raw_bytes(addr, method, path.as_bytes(), extra).await
}

async fn raw_bytes(addr: &str, method: &str, path: &[u8], extra: &[(&str, &str)]) -> Resp {
    let mut req = format!("{method} ").into_bytes();
    req.extend_from_slice(path);
    req.extend_from_slice(b" HTTP/1.1\r\nHost: updates.test\r\nConnection: close\r\n");
    for (k, v) in extra {
        req.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    req.extend_from_slice(b"Content-Length: 0\r\n\r\n");
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(&req).await.unwrap();
    let mut all = Vec::new();
    s.read_to_end(&mut all).await.unwrap();
    let split = all
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("no header end in {:?}", String::from_utf8_lossy(&all)));
    let head = String::from_utf8_lossy(&all[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .map(|l| {
            let (k, v) = l.split_once(':').unwrap();
            (k.trim().to_ascii_lowercase(), v.trim().to_owned())
        })
        .collect();
    Resp {
        status,
        headers,
        body: all[split + 4..].to_vec(),
    }
}

/// path, kind, size, mtime, mode of everything under `root` (symlinks not followed).
fn snapshot(root: &Path) -> BTreeMap<String, (char, u64, i64, i64, u32)> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, (char, u64, i64, i64, u32)>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let md = fs::symlink_metadata(e.path()).unwrap();
            let kind = if md.is_symlink() {
                'l'
            } else if md.is_dir() {
                'd'
            } else {
                'f'
            };
            let rel = e.path().strip_prefix(root).unwrap().display().to_string();
            out.insert(
                rel,
                (kind, md.len(), md.mtime(), md.mtime_nsec(), md.mode()),
            );
            if kind == 'd' {
                walk(&e.path(), root, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn chmod_tree(dir: &Path, dmode: u32, fmode: u32) {
    // Directories open first when thawing; the files inside are chmod-ed by their own path.
    let set = |p: &Path, m: u32| {
        let _ = fs::set_permissions(p, fs::Permissions::from_mode(m));
    };
    if dmode >= 0o700 {
        set(dir, dmode);
    }
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let md = fs::symlink_metadata(e.path()).unwrap();
            if md.is_dir() {
                chmod_tree(&e.path(), dmode, fmode);
            } else if md.is_file() {
                set(&e.path(), fmode);
            }
        }
    }
    if dmode < 0o700 {
        set(dir, dmode);
    }
}

struct Ctx {
    tmp: Option<tempfile::TempDir>,
    store: PathBuf,
    outside: PathBuf,
    addr: String,
    before: BTreeMap<String, (char, u64, i64, i64, u32)>,
    frozen: bool,
    blob_sha: String,
    sparse_sha: String,
}

impl Drop for Ctx {
    fn drop(&mut self) {
        // thaw so the TempDir can remove everything, also after a failed assertion
        if let Some(t) = &self.tmp {
            chmod_tree(t.path(), 0o755, 0o644);
        }
    }
}

const SYM_SHA: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const INSTORE_SHA: &str = "2222222222222222222222222222222222222222222222222222222222222222";

impl Ctx {
    async fn new(freeze: bool) -> Ctx {
        let tmp = tempfile::Builder::new()
            .prefix("lucina-updates-test-")
            .tempdir()
            .unwrap();
        let store = tmp.path().join("store");
        let outside = tmp.path().join("outside");
        let blob = blob();
        let blob_sha = sha(&blob);
        let sparse_sha = sha(b"sparse");
        let stable = store.join("lucina/stable");
        fs::create_dir_all(stable.join("artefacts")).unwrap();
        fs::write(
            stable.join("manifest"),
            b"fake manifest bytes, not signed\n",
        )
        .unwrap();
        fs::write(stable.join(format!("artefacts/{blob_sha}.bin")), &blob).unwrap();
        // a sparse 5 GiB file with a marker above 4 GiB
        {
            let mut f =
                fs::File::create(stable.join(format!("artefacts/{sparse_sha}.tar.gz"))).unwrap();
            f.set_len(GIB4 + (1 << 30)).unwrap();
            f.seek(SeekFrom::Start(GIB4 + 100)).unwrap();
            f.write_all(MARK).unwrap();
        }
        // outside the store: a secret file and a whole channel
        fs::create_dir_all(outside.join("chan/artefacts")).unwrap();
        fs::write(outside.join("secret"), b"SECRET OUTSIDE THE STORE").unwrap();
        fs::write(outside.join("chan/manifest"), b"SECRET MANIFEST").unwrap();
        fs::write(
            outside.join(format!("chan/artefacts/{blob_sha}.bin")),
            b"SECRET ARTEFACT",
        )
        .unwrap();
        // symlinks: file out of the store, file inside the store, manifest, directory component
        symlink(
            outside.join("secret"),
            stable.join(format!("artefacts/{SYM_SHA}.bin")),
        )
        .unwrap();
        symlink(
            stable.join(format!("artefacts/{blob_sha}.bin")),
            stable.join(format!("artefacts/{INSTORE_SHA}.bin")),
        )
        .unwrap();
        fs::create_dir_all(store.join("lucina/sym")).unwrap();
        symlink(outside.join("secret"), store.join("lucina/sym/manifest")).unwrap();
        symlink(outside.join("chan"), store.join("lucina/linked")).unwrap();
        // size cap: exactly 16 KiB is served, one byte more is not
        fs::create_dir_all(store.join("lucina/exact")).unwrap();
        fs::write(store.join("lucina/exact/manifest"), vec![b'x'; 16 * 1024]).unwrap();
        fs::create_dir_all(store.join("lucina/big")).unwrap();
        fs::write(store.join("lucina/big/manifest"), vec![b'x'; 16 * 1024 + 1]).unwrap();
        // a directory where the manifest should be
        fs::create_dir_all(store.join("lucina/dirm/manifest")).unwrap();
        fs::create_dir_all(store.join("lucina/atomic")).unwrap();

        if freeze {
            chmod_tree(&store, 0o555, 0o444);
        }
        let before = snapshot(&store);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let app = lucina_updates::app(&store).unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Ctx {
            tmp: Some(tmp),
            store,
            outside,
            addr,
            before,
            frozen: freeze,
            blob_sha,
            sparse_sha,
        }
    }

    async fn get(&self, path: &str, extra: &[(&str, &str)]) -> Resp {
        raw(&self.addr, "GET", path, extra).await
    }
    fn art(&self) -> String {
        format!("/v1/updates/lucina/stable/artefacts/{}.bin", self.blob_sha)
    }

    /// Nothing under the store was created, changed or removed, and a frozen store really is.
    fn done(self) {
        let after = snapshot(&self.store);
        assert_eq!(self.before, after, "the store changed");
        if self.frozen {
            assert!(
                fs::File::create(self.store.join("lucina/stable/new")).is_err(),
                "the store was not read-only"
            );
        }
        let _ = &self.outside;
    }
}

#[tokio::test]
async fn path_traversal_is_refused() {
    let cx = Ctx::new(true).await;
    let sha = &cx.blob_sha;
    let paths = [
        "/v1/updates/../outside/secret".to_string(),
        "/v1/updates/lucina/stable/../../../outside/secret".to_string(),
        "/v1/updates/lucina/stable/artefacts/../manifest".to_string(),
        "/v1/updates/%2e%2e/outside/secret".to_string(),
        "/v1/updates/lucina/stable/%2E%2E/manifest".to_string(),
        "/v1/updates/lucina%2fstable/manifest".to_string(),
        "/v1/updates/lucina/stable%2Fmanifest".to_string(),
        "/v1/updates/lucina/stable/artefacts%5c..%5cmanifest".to_string(),
        "/v1/updates//lucina/stable/manifest".to_string(),
        "/v1/updates/lucina//stable/manifest".to_string(),
        format!("/v1/updates/lucina/stable/artefacts//{sha}.bin"),
        "/v1/updates/lucina/stable/manifest%00".to_string(),
        "/etc/passwd".to_string(),
        "/v1/updates/lucina/stable/manifest/..".to_string(),
        format!("{}/etc/passwd", cx.outside.display()),
        format!("/v1/updates{}", cx.outside.join("secret").display()),
    ];
    for p in &paths {
        let r = cx.get(p, &[]).await;
        assert_eq!(r.status, 404, "{p}");
        assert!(!String::from_utf8_lossy(&r.body).contains("SECRET"), "{p}");
    }
    // a backslash and a NUL: the HTTP layer may answer 400 before the handler; never a file
    for p in [
        b"/v1/updates/lucina\\stable/manifest".as_slice(),
        b"/v1/updates/lucina/stable/manifest\0",
        b"/v1/updates/lucina/stable\\..\\manifest",
    ] {
        let r = raw_bytes(&cx.addr, "GET", p, &[]).await;
        assert!(r.status == 400 || r.status == 404, "{p:?} -> {}", r.status);
    }
    // the control: the same store, a clean path
    assert_eq!(
        cx.get("/v1/updates/lucina/stable/manifest", &[])
            .await
            .status,
        200
    );
    cx.done();
}

#[tokio::test]
async fn unknown_product_and_bad_channel_are_404() {
    let cx = Ctx::new(true).await;
    for p in [
        "/v1/updates/nosuch/stable/manifest",
        "/v1/updates/lucina/nosuch/manifest",
        "/v1/updates/Lucina/stable/manifest",
        "/v1/updates/lucina/Stable/manifest",
        "/v1/updates/lucina/-stable/manifest",
        "/v1/updates/lucina/stable_1/manifest",
        "/v1/updates/lucina/stable.x/manifest",
        "/v1/updates/lucina/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/manifest",
        "/v1/updates//stable/manifest",
        "/v1/updates/lucina/manifest",
        "/v1/other/lucina/stable/manifest",
        "/v1/updates/nosuch/stable/artefacts/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bin",
    ] {
        assert_eq!(cx.get(p, &[]).await.status, 404, "{p}");
    }
    cx.done();
}

#[tokio::test]
async fn bad_artefact_names_are_404() {
    let cx = Ctx::new(true).await;
    let sha = cx.blob_sha.clone();
    let d = "/v1/updates/lucina/stable/artefacts";
    for name in [
        format!("{}.bin", &sha[..63]),         // 63 digits
        format!("{sha}0.bin"),                 // 65 digits
        format!("{}.bin", sha.to_uppercase()), // uppercase hex
        format!("{sha}.BIN"),                  // uppercase extension
        format!("{sha}.exe!"),                 // bad extension character
        format!("{sha}.verylongext"),          // extension over 8
        sha.to_string(),                      // no extension
        format!("{sha}."),                     // empty extension
        format!("{sha}.bin.gz.extra"),         // extra suffix
        format!("{sha}.bin/"),                 // trailing slash
        format!("{sha}.bin/x"),                // a path under a file
        format!("x{sha}.bin"),                 // prefix
        format!("{}g.bin", &sha[..63]),        // not hex
    ] {
        assert_eq!(
            cx.get(&format!("{d}/{name}"), &[]).await.status,
            404,
            "{name}"
        );
    }
    // a well-formed name for a file that does not exist
    assert_eq!(
        cx.get(&format!("{d}/{}.bin", "0".repeat(64)), &[])
            .await
            .status,
        404
    );
    assert_eq!(cx.get(&cx.art(), &[]).await.status, 200);
    cx.done();
}

#[tokio::test]
async fn symlinks_are_never_followed() {
    let cx = Ctx::new(true).await;
    let d = "/v1/updates/lucina/stable/artefacts";
    for p in [
        format!("{d}/{SYM_SHA}.bin"), // file symlink to a file outside the store
        format!("{d}/{INSTORE_SHA}.bin"), // file symlink to a file inside the store
        "/v1/updates/lucina/sym/manifest".to_string(), // manifest symlink to outside
        "/v1/updates/lucina/linked/manifest".to_string(), // directory component symlink
        format!("/v1/updates/lucina/linked/artefacts/{}.bin", cx.blob_sha),
    ] {
        let r = cx.get(&p, &[]).await;
        assert_eq!(r.status, 404, "{p}");
        assert!(!String::from_utf8_lossy(&r.body).contains("SECRET"), "{p}");
        assert_eq!(raw(&cx.addr, "HEAD", &p, &[]).await.status, 404, "HEAD {p}");
    }
    cx.done();
}

#[tokio::test]
async fn directories_are_404_with_no_listing() {
    let cx = Ctx::new(true).await;
    for p in [
        "/v1/updates",
        "/v1/updates/",
        "/v1/updates/lucina",
        "/v1/updates/lucina/",
        "/v1/updates/lucina/stable",
        "/v1/updates/lucina/stable/",
        "/v1/updates/lucina/stable/artefacts",
        "/v1/updates/lucina/stable/artefacts/",
        "/v1/updates/lucina/stable/manifest/",
        "/v1/updates/lucina/dirm/manifest", // a directory named manifest
        "/",
    ] {
        let r = cx.get(p, &[]).await;
        assert_eq!(r.status, 404, "{p}");
        let body = String::from_utf8_lossy(&r.body).into_owned();
        assert!(
            !body.contains(&cx.blob_sha) && !body.contains("manifest"),
            "{p}: {body}"
        );
    }
    cx.done();
}

#[tokio::test]
async fn only_get_and_head_are_served() {
    let cx = Ctx::new(true).await;
    for m in ["POST", "PUT", "DELETE", "PATCH"] {
        for p in [cx.art(), "/v1/updates/lucina/stable/manifest".to_string()] {
            let r = raw(&cx.addr, m, &p, &[]).await;
            assert_eq!(r.status, 405, "{m} {p}");
            assert_eq!(r.h("allow"), Some("GET, HEAD"));
        }
    }
    cx.done();
}

#[tokio::test]
async fn range_returns_exactly_the_requested_bytes() {
    let cx = Ctx::new(true).await;
    let blob = blob();
    let r = cx.get(&cx.art(), &[("Range", "bytes=10-19")]).await;
    assert_eq!(r.status, 206);
    assert_eq!(r.body, &blob[10..20]);
    assert_eq!(r.h("content-range"), Some("bytes 10-19/1000"));
    assert_eq!(r.h("content-length"), Some("10"));
    let r = cx.get(&cx.art(), &[("Range", "bytes=-5")]).await;
    assert_eq!((r.status, &r.body[..]), (206, &blob[995..]));
    let r = cx.get(&cx.art(), &[("Range", "bytes=990-")]).await;
    assert_eq!((r.status, &r.body[..]), (206, &blob[990..]));
    let r = cx.get(&cx.art(), &[("Range", "bytes=990-5000")]).await;
    assert_eq!((r.status, &r.body[..]), (206, &blob[990..]));
    assert_eq!(r.h("content-range"), Some("bytes 990-999/1000"));
    // the full body
    let r = cx.get(&cx.art(), &[]).await;
    assert_eq!((r.status, r.body), (200, blob));
    cx.done();
}

#[tokio::test]
async fn unsatisfiable_range_is_416() {
    let cx = Ctx::new(true).await;
    for rg in ["bytes=1000-", "bytes=5000-6000", "bytes=-0"] {
        let r = cx.get(&cx.art(), &[("Range", rg)]).await;
        assert_eq!(r.status, 416, "{rg}");
        assert_eq!(r.h("content-range"), Some("bytes */1000"));
    }
    cx.done();
}

#[tokio::test]
async fn stale_if_range_gets_the_full_file() {
    let cx = Ctx::new(true).await;
    let blob = blob();
    let stale = format!("\"{}\"", "0".repeat(64));
    for ir in [
        stale.as_str(),
        "Wed, 21 Oct 2015 07:28:00 GMT",
        &format!("W/\"{}\"", cx.blob_sha),
    ] {
        let r = cx
            .get(&cx.art(), &[("Range", "bytes=0-9"), ("If-Range", ir)])
            .await;
        assert_eq!(r.status, 200, "{ir}");
        assert_eq!(r.body, blob);
        assert!(r.h("content-range").is_none());
    }
    // the current strong tag keeps the range
    let cur = format!("\"{}\"", cx.blob_sha);
    let r = cx
        .get(&cx.art(), &[("Range", "bytes=0-9"), ("If-Range", &cur)])
        .await;
    assert_eq!((r.status, &r.body[..]), (206, &blob[..10]));
    cx.done();
}

#[tokio::test]
async fn if_none_match_is_304() {
    let cx = Ctx::new(true).await;
    let cur = format!("\"{}\"", cx.blob_sha);
    let r = cx.get(&cx.art(), &[("If-None-Match", &cur)]).await;
    assert_eq!(r.status, 304);
    assert!(r.body.is_empty());
    assert_eq!(r.h("etag"), Some(cur.as_str()));
    cx.done();
}

#[tokio::test]
async fn head_matches_get_with_no_body() {
    let cx = Ctx::new(true).await;
    for (p, extra) in [
        (cx.art(), vec![]),
        (cx.art(), vec![("Range", "bytes=10-19")]),
        ("/v1/updates/lucina/stable/manifest".to_string(), vec![]),
    ] {
        let g = raw(&cx.addr, "GET", &p, &extra).await;
        let h = raw(&cx.addr, "HEAD", &p, &extra).await;
        assert_eq!(g.status, h.status, "{p}");
        assert!(h.body.is_empty(), "{p}");
        assert!(!g.body.is_empty());
        let strip = |r: &Resp| -> Vec<(String, String)> {
            let mut v: Vec<_> = r
                .headers
                .iter()
                .filter(|(k, _)| k != "date")
                .cloned()
                .collect();
            v.sort();
            v
        };
        assert_eq!(strip(&g), strip(&h), "{p}");
        assert_eq!(
            h.h("content-length").map(str::to_owned),
            Some(g.body.len().to_string())
        );
    }
    cx.done();
}

#[tokio::test]
async fn etag_is_the_strong_sha256_of_the_name() {
    let cx = Ctx::new(true).await;
    let r = cx.get(&cx.art(), &[]).await;
    assert_eq!(r.h("etag"), Some(format!("\"{}\"", cx.blob_sha).as_str()));
    assert!(!r.h("etag").unwrap().starts_with("W/"));
    // and the name really is the hash of the bytes the test stored
    assert_eq!(sha(&r.body), cx.blob_sha);
    cx.done();
}

#[tokio::test]
async fn cache_and_safety_headers() {
    let cx = Ctx::new(true).await;
    let m = cx.get("/v1/updates/lucina/stable/manifest", &[]).await;
    assert_eq!(m.status, 200);
    assert_eq!(m.h("cache-control"), Some("no-cache"));
    let a = cx.get(&cx.art(), &[]).await;
    assert_eq!(a.h("cache-control"), Some("private, immutable"));
    for r in [&m, &a] {
        assert_eq!(r.h("accept-ranges"), Some("bytes"));
        assert_eq!(r.h("content-type"), Some("application/octet-stream"));
        assert_eq!(r.h("x-content-type-options"), Some("nosniff"));
    }
    cx.done();
}

#[tokio::test]
async fn a_manifest_over_16_kib_is_not_served() {
    let cx = Ctx::new(true).await;
    let ok = cx.get("/v1/updates/lucina/exact/manifest", &[]).await;
    assert_eq!((ok.status, ok.body.len()), (200, 16 * 1024));
    let big = cx.get("/v1/updates/lucina/big/manifest", &[]).await;
    assert_eq!(big.status, 500);
    assert!(big.body.len() < 100, "no manifest bytes in the error");
    assert_eq!(
        raw(&cx.addr, "HEAD", "/v1/updates/lucina/big/manifest", &[])
            .await
            .status,
        500
    );
    cx.done();
}

#[tokio::test]
async fn range_above_4_gib_is_not_truncated() {
    let cx = Ctx::new(true).await;
    let p = format!(
        "/v1/updates/lucina/stable/artefacts/{}.tar.gz",
        cx.sparse_sha
    );
    let total = GIB4 + (1 << 30);
    let (a, b) = (GIB4 + 100, GIB4 + 100 + MARK.len() as u64 - 1);
    let r = cx.get(&p, &[("Range", &format!("bytes={a}-{b}"))]).await;
    assert_eq!(r.status, 206);
    assert_eq!(r.body, MARK);
    let cr = format!("bytes {a}-{b}/{total}");
    assert_eq!(r.h("content-range"), Some(cr.as_str()));
    // the 32-bit wrap of that offset holds zeros, not the marker
    let w = cx.get(&p, &[("Range", "bytes=100-115")]).await;
    assert_eq!(w.body, vec![0u8; 16]);
    // a suffix range of a file over 4 GiB
    let s = cx.get(&p, &[("Range", "bytes=-4")]).await;
    assert_eq!((s.status, s.body), (206, vec![0u8; 4]));
    cx.done();
}

#[tokio::test]
async fn the_whole_store_is_read_only() {
    let cx = Ctx::new(true).await;
    // every kind of request, then the same tree
    let art = cx.art();
    let sparse = format!(
        "/v1/updates/lucina/stable/artefacts/{}.tar.gz",
        cx.sparse_sha
    );
    for p in [
        art.as_str(),
        sparse.as_str(),
        "/v1/updates/lucina/stable/manifest",
        "/v1/updates/lucina/big/manifest",
        "/v1/updates/lucina/linked/manifest",
        "/v1/updates/lucina/nosuch/manifest",
        "/v1/updates/lucina/stable/artefacts/",
        "/v1/updates/lucina/stable/../..",
    ] {
        for m in ["GET", "HEAD", "POST", "PUT", "DELETE", "PATCH"] {
            let _ = raw(&cx.addr, m, p, &[("Range", "bytes=0-3")]).await;
        }
    }
    cx.done();
}

#[tokio::test]
async fn readers_only_ever_see_one_complete_manifest_while_it_is_replaced_by_rename() {
    let cx = Ctx::new(false).await;
    let dir = cx.store.join("lucina/atomic");
    let version = |c: u8| -> Vec<u8> {
        let mut v = format!("manifest version {}\n", c as char).into_bytes();
        v.resize(8000, c);
        v
    };
    let (a, b) = (version(b'A'), version(b'B'));
    fs::write(dir.join("manifest"), &a).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (stop, dir, a, b) = (stop.clone(), dir.clone(), a.clone(), b.clone());
        std::thread::spawn(move || {
            let mut i = 0u32;
            let mut n = 0u32;
            while !stop.load(Ordering::Relaxed) {
                let tmp = dir.join("manifest.tmp");
                fs::write(&tmp, if i.is_multiple_of(2) { &b } else { &a }).unwrap();
                fs::rename(&tmp, dir.join("manifest")).unwrap();
                i += 1;
                n += 1;
            }
            n
        })
    };
    let mut readers = Vec::new();
    for _ in 0..4 {
        let (addr, a, b) = (cx.addr.clone(), a.clone(), b.clone());
        readers.push(tokio::spawn(async move {
            let mut seen = [0u32; 2];
            for _ in 0..150 {
                let r = raw(&addr, "GET", "/v1/updates/lucina/atomic/manifest", &[]).await;
                assert_eq!(r.status, 200);
                if r.body == a {
                    seen[0] += 1;
                } else if r.body == b {
                    seen[1] += 1;
                } else {
                    panic!("a torn or foreign manifest: {} bytes", r.body.len());
                }
            }
            seen
        }));
    }
    let mut total = [0u32; 2];
    for r in readers {
        let s = r.await.unwrap();
        total[0] += s[0];
        total[1] += s[1];
    }
    stop.store(true, Ordering::Relaxed);
    let swaps = writer.join().unwrap();
    assert!(swaps > 10, "the writer barely ran ({swaps} swaps)");
    assert_eq!(total[0] + total[1], 600);
}
