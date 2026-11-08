//! Spreadsheets through the HTTP router (docs/xlsx.md): a Drive workbook opens as a sheet, a save
//! replaces the file in place and keeps the old bytes as a version, conflicts and refused cells
//! answer with codes, an earlier version restores, a rescan keeps the item; a workbook shared into
//! the library saves as a new blob.

use std::io::Write;
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use lucina_media::config::Config;
use lucina_media::ingest::{self, Library};
use lucina_media::{drive, http, store};
use serde_json::{json, Value};
use tower::ServiceExt;

const KEY: &str = "0123456789abcdef0123456789abcdef";

/// A protected sheet: A1 locked text, A2 unlocked text, B2 unlocked time, C2 a formula.
fn workbook() -> Vec<u8> {
    let parts: [(&str, &str); 6] = [
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
        ),
        (
            "xl/workbook.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Hours" sheetId="1" r:id="rId1"/></sheets><calcPr calcId="191028"/></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#,
        ),
        (
            "xl/styles.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font/></fonts><fills count="1"><fill><patternFill patternType="none"/></fill></fills><borders count="1"><border/></borders><cellXfs count="3"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/><xf numFmtId="0" fontId="0" fillId="0" borderId="0" applyProtection="1"><protection locked="0"/></xf><xf numFmtId="20" fontId="0" fillId="0" borderId="0" applyProtection="1"><protection locked="0"/></xf></cellXfs></styleSheet>"#,
        ),
        (
            "xl/worksheets/sheet1.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Day</t></is></c></row><row r="2"><c r="A2" s="1"/><c r="B2" s="2"><v>0</v></c><c r="C2"><f>B2*24</f><v>0</v></c></row></sheetData><sheetProtection sheet="1"/></worksheet>"#,
        ),
    ];
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    for (name, body) in parts {
        z.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(body.as_bytes()).unwrap();
    }
    z.finish().unwrap();
    buf.into_inner()
}

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    lib: Library,
    app: Router,
}

fn setup() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("work/admin");
    let mut cfg = Config::for_dirs(tmp.path().join("data"), tmp.path().join("store"));
    cfg.quiet_start = 0;
    cfg.quiet_end = 0;
    cfg.upstream_key = Some(KEY.into());
    // The scan pauses on a hot or busy box (DK5); nezopt often is both.
    cfg.prepare_max_load = u32::MAX;
    cfg.drive_max_temp_c = u32::MAX;
    cfg.folders = vec![("Admin".into(), root.clone())];
    store::init_store(&cfg).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("worktime.xlsx"), workbook()).unwrap();
    let lib = Library::open(cfg).unwrap();
    let app = http::router(lib.clone(), None);
    Env {
        _tmp: tmp,
        root,
        lib,
        app,
    }
}

async fn send(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Vec<u8>) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-media-upstream-key", KEY);
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let status = res.status();
    (
        status,
        res.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

async fn json_of(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (s, b) = send(app, method, uri, body).await;
    (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

fn item_id(lib: &Library, name: &str) -> String {
    lib.db
        .with(|c| {
            c.query_row(
                "SELECT item_id FROM drive_files WHERE name = ?1",
                [name],
                |r| r.get(0),
            )
        })
        .unwrap()
}

fn edit(cell: &str, v: &str) -> Value {
    json!({ "sheet": "Hours", "cell": cell, "value": { "t": "input", "v": v } })
}

fn hidden_files(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with('.'))
        .collect()
}

#[tokio::test]
async fn drive_workbook_saves_in_place_with_versions() {
    let env = setup();
    drive::index_drive_folder(&env.lib, "Admin", &env.root).unwrap();
    let id = item_id(&env.lib, "worktime.xlsx");
    let original = std::fs::read(env.root.join("worktime.xlsx")).unwrap();

    let (s, v) = json_of(
        &env.app,
        "GET",
        &format!("/v1/media/items/{id}/sheet"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["name"], "worktime.xlsx");
    assert_eq!(v["writable"], true);
    assert_eq!(v["version"], 1);
    assert_eq!(v["location"]["kind"], "drive");
    assert_eq!(v["hash"], blake3::hash(&original).to_hex().as_str());
    let cells = v["sheets"][0]["cells"].as_array().unwrap();
    assert!(cells.iter().any(|c| c["ref"] == "B2" && c["input"] == true));
    let base = v["hash"].as_str().unwrap().to_string();
    let url = format!("/v1/media/items/{id}/sheet");

    // Someone else's bytes: conflict, nothing written.
    let (s, v) = json_of(
        &env.app,
        "POST",
        &url,
        Some(json!({ "base_hash": "0".repeat(64), "edits": [edit("B2", "9:00")] })),
    )
    .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (StatusCode::CONFLICT, Some("conflict"))
    );
    assert_eq!(v["hash"], base.as_str());
    // A locked cell and a formula: refused with the cell named.
    let (s, v) = json_of(
        &env.app,
        "POST",
        &url,
        Some(json!({ "base_hash": base, "edits": [edit("A1", "x")] })),
    )
    .await;
    assert_eq!(
        (s, v["error"].as_str(), v["cell"].as_str()),
        (StatusCode::BAD_REQUEST, Some("locked_cell"), Some("A1"))
    );
    let (s, v) = json_of(
        &env.app,
        "POST",
        &url,
        Some(json!({ "base_hash": base, "edits": [edit("C2", "1")] })),
    )
    .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("formula_cell"))
    );
    assert_eq!(
        std::fs::read(env.root.join("worktime.xlsx")).unwrap(),
        original
    );

    // A real save.
    let (s, v) = json_of(
        &env.app,
        "POST",
        &url,
        Some(json!({ "base_hash": base, "edits": [edit("B2", "7:30"), edit("A2", "Monday")] })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["version"], 2);
    assert_eq!(v["changed"].as_array().unwrap().len(), 2);
    assert_eq!(v["full_calc_on_load"], true);
    let saved = std::fs::read(env.root.join("worktime.xlsx")).unwrap();
    assert_ne!(saved, original);
    assert_eq!(v["hash"], blake3::hash(&saved).to_hex().as_str());
    assert!(
        hidden_files(&env.root).is_empty(),
        "temp files left: {:?}",
        hidden_files(&env.root)
    );
    let wb = secretary_xlsx::read(&saved).unwrap();
    assert_eq!(wb.display(&wb.sheets[0].cells[&(2, 2)]), "7:30");
    // The catalog follows at once: same item, new hash and size.
    let (hash, bytes, version): (String, i64, i64) = env
        .lib
        .db
        .with(|c| {
            c.query_row(
                "SELECT hash, bytes, version FROM items WHERE id = ?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
        })
        .unwrap();
    assert_eq!(
        (hash.as_str(), bytes, version),
        (v["hash"].as_str().unwrap(), saved.len() as i64, 2)
    );

    // The old bytes are version 1, downloadable.
    let (s, vs) = json_of(
        &env.app,
        "GET",
        &format!("/v1/media/items/{id}/sheet/versions"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(vs["versions"][0]["version"], 1);
    let (s, old) = send(
        &env.app,
        "GET",
        vs["versions"][0]["download_url"].as_str().unwrap(),
        None,
    )
    .await;
    assert_eq!((s, old == original), (StatusCode::OK, true));

    // A rescan finds the same file: same item, nothing trashed.
    let st = drive::index_drive_folder(&env.lib, "Admin", &env.root).unwrap();
    assert_eq!((st.pruned, st.errors), (0, 0), "{st:?}");
    assert_eq!(item_id(&env.lib, "worktime.xlsx"), id);

    // Restore version 1: the file is the original again, and the edit became version 2's blob.
    let now = v["hash"].as_str().unwrap().to_string();
    let (s, r) = json_of(
        &env.app,
        "POST",
        &format!("/v1/media/items/{id}/sheet/restore"),
        Some(json!({ "version": 1, "base_hash": now })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["version"], 3);
    assert_eq!(
        std::fs::read(env.root.join("worktime.xlsx")).unwrap(),
        original
    );
    let (_, vs) = json_of(
        &env.app,
        "GET",
        &format!("/v1/media/items/{id}/sheet/versions"),
        None,
    )
    .await;
    assert_eq!(vs["versions"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn shared_workbook_saves_as_new_blob() {
    let env = setup();
    let src = env.root.parent().unwrap().join("incoming.xlsx");
    std::fs::write(&src, workbook()).unwrap();
    let client = lucina_media::meta::ClientMeta {
        filename: Some("Timesheet.xlsx".into()),
        source: Some("ios-share".into()),
        ..Default::default()
    };
    let res = ingest::ingest_file(&env.lib, &src, None, &client, None).unwrap();
    let url = format!("/v1/media/items/{}/sheet", res.item_id);
    let (s, v) = json_of(&env.app, "GET", &url, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["location"]["kind"], "library");
    let base = v["hash"].as_str().unwrap().to_string();
    let (s, v) = json_of(
        &env.app,
        "POST",
        &url,
        Some(json!({ "base_hash": base, "edits": [edit("B2", "8:15")] })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["version"], 2);
    assert_ne!(v["hash"], base.as_str());
    let (s, old) = send(
        &env.app,
        "GET",
        &format!("/v1/media/items/{}/original?version=1", res.item_id),
        None,
    )
    .await;
    assert_eq!(
        (s, blake3::hash(&old).to_hex().to_string()),
        (StatusCode::OK, base)
    );
}

#[tokio::test]
async fn other_documents_do_not_open_as_sheets() {
    let env = setup();
    std::fs::write(env.root.join("notes.txt"), b"hello").unwrap();
    std::fs::write(env.root.join("old.xls"), b"\xD0\xCF\x11\xE0").unwrap();
    drive::index_drive_folder(&env.lib, "Admin", &env.root).unwrap();
    for name in ["notes.txt", "old.xls"] {
        let id = item_id(&env.lib, name);
        let (s, v) = json_of(
            &env.app,
            "GET",
            &format!("/v1/media/items/{id}/sheet"),
            None,
        )
        .await;
        assert_eq!(
            (s, v["error"].as_str()),
            (StatusCode::UNSUPPORTED_MEDIA_TYPE, Some("unsupported")),
            "{name}"
        );
    }
}
