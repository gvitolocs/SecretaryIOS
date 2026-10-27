//! The Drive end to end through the HTTP router: every file of a Drive folder indexed in place,
//! browsing with breadcrumbs, sorting and type filters, search by name, folder and content, text
//! and code previews with highlighting, archive listings, PDFs (and Office files converted when
//! LibreOffice is installed), downloads, path guards, the scope split and blob ownership.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use lucina_media::config::Config;
use lucina_media::ingest::{self, Library};
use lucina_media::{drive, http, store};
use serde_json::Value;
use tower::ServiceExt;

const KEY: &str = "0123456789abcdef0123456789abcdef";

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    lib: Library,
    app: Router,
}

fn write(p: &Path, body: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn tool(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
}

/// A one-page PDF with `text` in Helvetica, offsets computed so any reader accepts it.
fn pdf(text: &str) -> Vec<u8> {
    let stream = format!("BT /F1 18 Tf 72 720 Td ({text}) Tj ET");
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offsets {
        out.extend(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn docx(text: &str) -> Vec<u8> {
    use std::io::Write;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("[Content_Types].xml", o).unwrap();
        z.write_all(br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).unwrap();
        z.start_file("_rels/.rels", o).unwrap();
        z.write_all(br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#).unwrap();
        z.start_file("word/document.xml", o).unwrap();
        z.write_all(format!(r#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"#).as_bytes()).unwrap();
        z.finish().unwrap();
    }
    buf.into_inner()
}

fn setup(with_tools: bool) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("mybook/Drive/Lezioni");
    let mut cfg = Config::for_dirs(tmp.path().join("data"), tmp.path().join("store"));
    cfg.quiet_start = 0;
    cfg.quiet_end = 0;
    cfg.upstream_key = Some(KEY.into());
    cfg.folders = vec![
        ("Lezioni".into(), root.clone()),
        ("Film".into(), tmp.path().join("mybook/Film")),
    ];
    if with_tools {
        cfg.pdftotext = tool("pdftotext");
        cfg.pdftoppm = tool("pdftoppm");
        cfg.soffice = tool("soffice");
    }
    store::init_store(&cfg).unwrap();
    write(
        &root.join("Digital Forensics/L01 Introduction.pdf"),
        &pdf("Introduction to digital forensics"),
    );
    // An hour older than L05: written one after the other they often share a modification time
    // to the millisecond, and searches (newest first) then returned the two in either order.
    std::fs::File::options()
        .write(true)
        .open(root.join("Digital Forensics/L01 Introduction.pdf"))
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .unwrap();
    write(
        &root.join("Digital Forensics/L05 File carving.pdf"),
        &pdf("File carving with scalpel and foremost"),
    );
    write(
        &root.join("Digital Forensics/notes.docx"),
        &docx("Chain of custody notes"),
    );
    write(
        &root.join("Digital Forensics/lab/carve.py"),
        b"import os\n\ndef carve(path):\n    # find JPEG headers\n    return b'\\xff\\xd8'\n",
    );
    write(&root.join("Digital Forensics/lab/evidence.zip"), &{
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        z.start_file("disk.img", zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(&[0u8; 64]).unwrap();
        z.finish().unwrap();
        buf.into_inner()
    });
    write(&root.join("Networks/README.md"), b"# Networks\nTCP and UDP");
    write(&root.join("Networks/.git/config"), b"[core]");
    write(&root.join("Networks/node_modules/a/index.js"), b"1");
    let lib = Library::open(cfg).unwrap();
    let app = http::router(lib.clone(), None).merge(http::internal_router(lib.clone()));
    Env {
        _tmp: tmp,
        root,
        lib,
        app,
    }
}

async fn call(app: &Router, uri: &str) -> (StatusCode, Vec<u8>, axum::http::HeaderMap) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("x-media-upstream-key", KEY)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    (
        status,
        res.into_body().collect().await.unwrap().to_bytes().to_vec(),
        headers,
    )
}

async fn get_json(app: &Router, uri: &str) -> (StatusCode, Value) {
    let (s, b, _) = call(app, uri).await;
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

fn index(env: &Env) {
    let s = drive::index_drive_folder(&env.lib, "Lezioni", &env.root).unwrap();
    assert_eq!((s.added, s.errors), (6, 0), "{s:?}");
}

/// Run the Drive worker's jobs and the thumbnail worker until nothing is pending.
async fn settle(lib: &Library) {
    let d = tokio::spawn(drive::drive_worker(lib.clone()));
    let t = tokio::spawn(ingest::thumb_worker(lib.clone()));
    for _ in 0..1200 {
        let pending: i64 = lib
            .db
            .with(|c| {
                c.query_row(
                    "SELECT (SELECT COUNT(*) FROM drive_files WHERE text_state = 'pending') + (SELECT COUNT(*) FROM items WHERE thumb_state = 'pending')",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        if pending == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    d.abort();
    t.abort();
}

fn names(v: &Value, key: &str, field: &str) -> Vec<String> {
    v[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            i.pointer(field)
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn browse_folders_with_breadcrumbs_sorting_and_filters() {
    let env = setup(false);
    index(&env);
    let (s, roots) = get_json(&env.app, "/v1/media/drive").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        names(&roots, "folders", "/folder"),
        vec!["Lezioni"],
        "films are not a Drive folder"
    );
    assert_eq!(roots["folders"][0]["files"], 6);

    let (s, top) = get_json(&env.app, "/v1/media/drive/browse?folder=Lezioni").await;
    assert_eq!(s, StatusCode::OK, "{top}");
    assert_eq!(
        names(&top, "folders", "/name"),
        vec!["Digital Forensics", "Networks"]
    );
    assert_eq!(top["folders"][0]["files"], 5);
    assert_eq!(top["files"].as_array().unwrap().len(), 0);
    assert_eq!(top["breadcrumbs"][0]["name"], "Lezioni");

    let (_, df) = get_json(
        &env.app,
        "/v1/media/drive/browse?folder=Lezioni&path=Digital%20Forensics",
    )
    .await;
    assert_eq!(
        names(&df, "folders", "/path"),
        vec!["Digital Forensics/lab"]
    );
    assert_eq!(
        names(&df, "files", "/drive/name"),
        vec!["L01 Introduction.pdf", "L05 File carving.pdf", "notes.docx"]
    );
    let f = &df["files"][1];
    assert_eq!(f["drive"]["type"], "pdf");
    assert_eq!(f["drive"]["preview"], "pdf");
    assert_eq!(f["drive"]["dir"], "Digital Forensics");
    assert_eq!(f["kind"], "file");
    assert!(f["urls"]["pdf"].as_str().unwrap().ends_with("/pdf"));
    assert!(f["urls"]["download"]
        .as_str()
        .unwrap()
        .ends_with("/original?download=1"));
    assert_eq!(f["urls"]["archive"], Value::Null);
    assert_eq!(df["breadcrumbs"][1]["path"], "Digital Forensics");
    assert_eq!(df["total"], 3);

    let (_, by_type) = get_json(
        &env.app,
        "/v1/media/drive/browse?folder=Lezioni&path=Digital%20Forensics&sort=type&order=desc",
    )
    .await;
    assert_eq!(by_type["files"][0]["drive"]["type"], "pdf");
    assert_eq!(by_type["files"][2]["drive"]["type"], "document");
    let (_, pdfs) = get_json(
        &env.app,
        "/v1/media/drive/browse?folder=Lezioni&recursive=1&type=pdf&limit=1",
    )
    .await;
    assert_eq!(pdfs["total"], 2);
    assert_eq!(pdfs["next_offset"], 1);
    assert_eq!(pdfs["folders"].as_array().unwrap().len(), 0);

    for bad in [
        "..",
        "../..",
        "%2Fetc",
        "a%2F..%2F..",
        "Digital%20Forensics%2F",
    ] {
        let (s, _) = get_json(
            &env.app,
            &format!("/v1/media/drive/browse?folder=Lezioni&path={bad}"),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (s, _) = get_json(&env.app, "/v1/media/drive/browse?folder=Film").await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "the film folders are not browsable as the Drive"
    );
    let (s, _) = get_json(&env.app, "/v1/media/drive/browse?folder=Lezioni&sort=evil").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = get_json(&env.app, "/v1/media/items?scope=drive&type=exe").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Scopes: everything here is the Drive, nothing is in the Gallery or Films.
    let (_, gallery) = get_json(&env.app, "/v1/media/items?scope=gallery").await;
    assert_eq!(gallery["total"], 0);
    let (_, films) = get_json(&env.app, "/v1/media/items?scope=films").await;
    assert_eq!(films["total"], 0);
    let (_, drive) = get_json(&env.app, "/v1/media/items?scope=drive").await;
    assert_eq!(drive["total"], 6);
}

#[tokio::test]
async fn search_finds_lecture_slides_by_folder_name_and_content() {
    let env = setup(false);
    index(&env);
    // By folder and type, before any text is read: "the slides from the Digital Forensics lectures".
    let (_, r) = get_json(
        &env.app,
        "/v1/media/items?scope=drive&q=digital%20forensics&type=pdf",
    )
    .await;
    assert_eq!(
        names(&r, "items", "/drive/name"),
        vec!["L05 File carving.pdf", "L01 Introduction.pdf"]
    );
    settle(&env.lib).await;
    // By content (pure-Rust PDF and docx text here).
    let (_, r) = get_json(&env.app, "/v1/media/items?scope=drive&q=scalpel").await;
    assert_eq!(
        names(&r, "items", "/drive/name"),
        vec!["L05 File carving.pdf"]
    );
    assert!(
        r["items"][0]["snippet"]
            .as_str()
            .unwrap()
            .contains("scalpel"),
        "{r}"
    );
    let (_, r) = get_json(&env.app, "/v1/media/items?scope=drive&q=custody").await;
    assert_eq!(names(&r, "items", "/drive/name"), vec!["notes.docx"]);
    let (_, r) = get_json(
        &env.app,
        "/v1/media/drive/browse?folder=Lezioni&path=Digital%20Forensics&q=jpeg%20headers",
    )
    .await;
    assert_eq!(
        names(&r, "files", "/drive/path"),
        vec!["Digital Forensics/lab/carve.py"]
    );
    // Code is in the index, its type words too.
    let (_, r) = get_json(&env.app, "/v1/media/items?scope=drive&q=code&type=code").await;
    assert_eq!(r["total"], 1);
    // The worker read every text, and the archive has none.
    let states: Vec<(String, String)> = env
        .lib
        .db
        .with(|c| {
            let mut s = c.prepare("SELECT name, text_state FROM drive_files ORDER BY name")?;
            let r = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect();
            r
        })
        .unwrap();
    for (name, state) in &states {
        let want = if name == "evidence.zip" {
            "none"
        } else {
            "ready"
        };
        assert_eq!(state, want, "{name}");
    }
}

#[tokio::test]
async fn previews_text_code_archives_pdfs_and_downloads() {
    let env = setup(false);
    index(&env);
    let (_, lab) = get_json(
        &env.app,
        "/v1/media/drive/browse?folder=Lezioni&path=Digital%20Forensics%2Flab",
    )
    .await;
    let id_of = |name: &str| -> String {
        lab["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["drive"]["name"] == name)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let py = id_of("carve.py");
    let (s, t) = get_json(&env.app, &format!("/v1/media/items/{py}/text?highlight=1")).await;
    assert_eq!(s, StatusCode::OK, "{t}");
    assert_eq!(t["language"], "Python");
    assert_eq!(t["source"], "file");
    assert!(t["text"].as_str().unwrap().starts_with("import os"));
    let lines = t["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 5);
    assert_eq!(lines[0][0], serde_json::json!(["kw", "import"]));
    assert!(lines[3].as_array().unwrap().iter().any(|r| r[0] == "com"));

    let zip = id_of("evidence.zip");
    let (s, a) = get_json(&env.app, &format!("/v1/media/items/{zip}/archive")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(a["format"], "zip");
    assert_eq!(a["entries"][0]["path"], "disk.img");
    assert_eq!(a["entries"][0]["size"], 64);
    let (s, _) = get_json(&env.app, &format!("/v1/media/items/{zip}/text")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = get_json(&env.app, &format!("/v1/media/items/{py}/archive")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (_, df) = get_json(
        &env.app,
        "/v1/media/drive/browse?folder=Lezioni&path=Digital%20Forensics",
    )
    .await;
    let pdf_id = df["files"][1]["id"].as_str().unwrap().to_string();
    let (s, body, h) = call(&env.app, &format!("/v1/media/items/{pdf_id}/pdf")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h["content-type"], "application/pdf");
    assert!(body.starts_with(b"%PDF"));
    // Extracted on demand when the worker has not been there yet.
    let (s, t) = get_json(&env.app, &format!("/v1/media/items/{pdf_id}/text")).await;
    assert_eq!(s, StatusCode::OK, "{t}");
    assert_eq!(t["source"], "extracted");
    assert!(t["text"]
        .as_str()
        .unwrap()
        .contains("File carving with scalpel"));
    let (s, _, h) = call(
        &env.app,
        &format!("/v1/media/items/{pdf_id}/original?download=1"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        h["content-disposition"],
        "attachment; filename=\"L05 File carving.pdf\"; filename*=UTF-8''L05%20File%20carving.pdf"
    );
    let (_, _, h) = call(&env.app, &format!("/v1/media/items/{pdf_id}/original")).await;
    assert!(h["content-disposition"]
        .to_str()
        .unwrap()
        .starts_with("inline;"));

    // No LibreOffice configured: an Office file has no PDF, but its text.
    let docx_id = df["files"][2]["id"].as_str().unwrap().to_string();
    assert_eq!(df["files"][2]["urls"]["pdf"], Value::Null);
    let (s, _) = get_json(&env.app, &format!("/v1/media/items/{docx_id}/pdf")).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    let (_, t) = get_json(&env.app, &format!("/v1/media/items/{docx_id}/text")).await;
    assert_eq!(t["text"], "Chain of custody notes");
    for bad in ["..%2F..", "x%00"] {
        let (s, _) = get_json(&env.app, &format!("/v1/media/items/{bad}/text")).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn with_poppler_and_libreoffice_pdfs_get_pages_and_office_converts() {
    let env = setup(true);
    if env.lib.cfg.pdftoppm.is_none()
        || env.lib.cfg.soffice.is_none()
        || env.lib.cfg.pdftotext.is_none()
    {
        eprintln!("skipped: poppler-utils or LibreOffice not installed");
        return;
    }
    index(&env);
    settle(&env.lib).await;
    let (_, df) = get_json(
        &env.app,
        "/v1/media/drive/browse?folder=Lezioni&path=Digital%20Forensics",
    )
    .await;
    for f in df["files"].as_array().unwrap() {
        assert_eq!(f["thumb_state"], "ready", "{}", f["drive"]["name"]);
        assert!(f["urls"]["grid"].is_string());
        assert!(
            f["urls"]["pdf"].is_string(),
            "office files convert when LibreOffice is there"
        );
    }
    let docx_id = df["files"][2]["id"].as_str().unwrap();
    let (s, body, _) = call(&env.app, &format!("/v1/media/items/{docx_id}/pdf")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.starts_with(b"%PDF"));
    let (_, r) = get_json(&env.app, "/v1/media/items?scope=drive&q=foremost").await;
    assert_eq!(
        names(&r, "items", "/drive/name"),
        vec!["L05 File carving.pdf"]
    );
}

#[tokio::test]
async fn a_phone_copy_of_a_drive_file_is_owned_by_the_library() {
    let env = setup(false);
    index(&env);
    let bytes = std::fs::read(env.root.join("Networks/README.md")).unwrap();
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let loc: String = env
        .lib
        .db
        .with(|c| {
            c.query_row("SELECT location FROM blobs WHERE hash = ?1", [&hash], |r| {
                r.get(0)
            })
        })
        .unwrap();
    assert_eq!(loc, "external");
    // The same bytes arrive from a phone: they are a duplicate of the Drive item, but the
    // library now owns a copy, so deleting the Drive file never loses the phone's original.
    let src = env.root.parent().unwrap().join("upload.md");
    std::fs::write(&src, &bytes).unwrap();
    let client = lucina_media::meta::ClientMeta {
        filename: Some("README.md".into()),
        source: Some("phone-ios".into()),
        ..Default::default()
    };
    let r = ingest::ingest_file(&env.lib, &src, None, &client, None).unwrap();
    assert!(r.duplicate);
    let (loc, ext): (String, String) = env
        .lib
        .db
        .with(|c| {
            c.query_row(
                "SELECT location, ext FROM blobs WHERE hash = ?1",
                [&hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .unwrap();
    assert_eq!(loc, "store");
    assert!(env
        .lib
        .cfg
        .store_dir
        .join(store::object_rel(&hash, &ext))
        .is_file());
    // The Drive item still serves its own file.
    let id: String = env
        .lib
        .db
        .with(|c| {
            c.query_row(
                "SELECT item_id FROM drive_files WHERE name = 'README.md'",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
    let (p, _) = ingest::original_path(&env.lib, &id).unwrap().unwrap();
    assert_eq!(p, env.root.join("Networks/README.md"));
    // A second identical upload is dropped: the owned copy is already there.
    std::fs::write(&src, &bytes).unwrap();
    ingest::ingest_file(&env.lib, &src, None, &client, None).unwrap();
    assert!(!src.exists());
}

/// The SQL of each scope is the kernel's partition (DK2): every combination of phone / film
/// album lands in exactly the scope `drive_kernel::Scope::of` gives it.
#[tokio::test]
async fn scope_sql_matches_the_kernel() {
    use lucina_media::drive_kernel::Scope;
    let env = setup(false);
    let c = |sql: &str| env.lib.db.with(|c| c.execute(sql, [])).unwrap();
    c("INSERT INTO blobs (hash, bytes, ext, location, created_at) VALUES ('h', 1, 'bin', 'store', 'now')");
    c("INSERT INTO albums (id, title, created_at, updated_at) VALUES ('fa', 'Film', 'now', 'now')");
    let mut cases = Vec::new();
    for phone in [false, true] {
        for film in [false, true] {
            let id = format!("itm_{}{}", phone as u8, film as u8);
            let source = if phone { "phone-ios" } else { "folder" };
            c(&format!("INSERT INTO items (id, kind, title, hash, mime, bytes, sort_at, source, added_at, updated_at) VALUES ('{id}', 'file', '{id}', 'h', 'x/y', 1, 'now', '{source}', 'now', 'now')"));
            if film {
                c(&format!("INSERT INTO album_items (album_id, item_id, added_at) VALUES ('fa', '{id}', 'now')"));
            }
            cases.push((id, phone, film));
        }
    }
    for (scope, s) in [
        ("gallery", Scope::Gallery),
        ("films", Scope::Films),
        ("drive", Scope::Drive),
    ] {
        let (_, v) = get_json(&env.app, &format!("/v1/media/items?scope={scope}")).await;
        let mut got = names(&v, "items", "/id");
        got.sort();
        let want: Vec<String> = cases
            .iter()
            .filter(|(_, p, f)| Scope::of(*p, *f) == s)
            .map(|(id, _, _)| id.clone())
            .collect();
        assert_eq!(got, want, "{scope}");
    }
}
