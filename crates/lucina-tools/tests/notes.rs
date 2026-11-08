//! The `notes` tool against a mocked secretary-server, and its local fallback.

use std::sync::Arc;
use std::time::Duration;

use lucina_tools::agent;
use lucina_tools::{Ctx, Effect, Registry, ToolEnv, ToolSelection};
use serde_json::{json, Value};
use wiremock::matchers::{body_json, body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "svc_test_service_token_0123456789abcdef";

fn ctx(dir: &std::path::Path, url: Option<String>) -> Ctx {
    let mut env = ToolEnv::new(dir, "Europe/Rome".parse().unwrap());
    if let Some(u) = url {
        let file = dir.join("service.token");
        std::fs::write(&file, format!("{TOKEN}\n")).unwrap();
        env.secretary_url = Some(u);
        env.secretary_service_token_file = Some(file);
    }
    Ctx::new(Arc::new(env), "web", "app")
}

async fn call(ctx: &Ctx, args: Value) -> (bool, Value) {
    let registry = Registry::builtin(&ToolSelection::parse("notes"));
    let (ok, out, _) = agent::execute(&registry, "notes", args, ctx, Duration::from_secs(10)).await;
    (ok, out)
}

fn note(slug: &str, title: &str, content: &str) -> Value {
    json!({"id": "n_00000000000000000001", "slug": slug, "title": title, "content": content,
           "tags": ["work"], "source": "chatgpt", "pinned": true,
           "createdAt": "2026-10-22T11:30:00.000Z", "updatedAt": "2026-10-22T11:31:00.000Z"})
}

#[tokio::test]
async fn reads_lists_and_searches_secretary_notes() {
    let server = MockServer::start().await;
    let auth = format!("Bearer {TOKEN}");
    Mock::given(method("GET"))
        .and(path("/v1/notes/craftit-meeting"))
        .and(header("authorization", auth.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(note(
            "craftit-meeting",
            "CRAFT-IT4SD — meeting with Marianne & Jonas",
            "# Briefing\n\nMarianne offers paid hours.",
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/notes/craft-it4sd-meeting"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"error": {"code": "note_not_found"}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/notes"))
        .and(query_param("q", "CRAFT-IT4SD meeting"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"notes": [
            {"slug": "craftit-meeting", "title": "CRAFT-IT4SD — meeting with Marianne & Jonas", "excerpt": "Marianne offers"}
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/notes"))
        .and(query_param("q", "marianne"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"notes": [
            {"slug": "craftit-meeting", "title": "CRAFT-IT4SD", "excerpt": "Marianne offers paid hours."}
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/notes"))
        .and(query_param("limit", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"notes": [
            {"slug": "craftit-meeting", "title": "CRAFT-IT4SD", "updatedAt": "2026-10-22T11:31:00.000Z", "source": "chatgpt", "pinned": true, "tags": []}
        ]})))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(format!("{}/", server.uri())));

    let (ok, out) = call(&c, json!({"action": "read", "slug": "craftit-meeting"})).await;
    assert!(ok, "{out}");
    assert_eq!(out["content"], "# Briefing\n\nMarianne offers paid hours.");
    assert_eq!(
        (out["source"].as_str(), out["stored"].as_str()),
        (Some("chatgpt"), Some("secretary"))
    );
    // By title: not a slug, so the best search hit.
    let (ok, out) = call(
        &c,
        json!({"action": "read", "title": "CRAFT-IT4SD meeting"}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(out["slug"], "craftit-meeting");
    let (ok, out) = call(&c, json!({"action": "search", "query": "marianne"})).await;
    assert!(ok, "{out}");
    assert_eq!(out["results"][0]["slug"], "craftit-meeting");
    assert_eq!(out["results"][0]["lines"][0], "Marianne offers paid hours.");
    let (ok, out) = call(&c, json!({"action": "list"})).await;
    assert!(ok, "{out}");
    assert_eq!(out["notes"][0]["slug"], "craftit-meeting");
    assert_eq!(out["notes"][0]["pinned"], true);
    assert!(
        !dir.path().join("notes").exists(),
        "nothing written locally"
    );
}

#[tokio::test]
async fn creates_and_appends_on_the_server() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/notes"))
        .and(body_json(json!({"title": "Idee regalo", "content": "- libro", "source": "lucina", "tags": ["casa"]})))
        .respond_with(ResponseTemplate::new(201).set_body_json(note("idee-regalo", "Idee regalo", "- libro")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/notes/idee-regalo/append"))
        .and(body_json(
            json!({"content": "- sciarpa", "separator": "\n", "source": "lucina"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(note(
            "idee-regalo",
            "Idee regalo",
            "- libro\n- sciarpa",
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/notes/viaggio/append"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"error": {"code": "note_not_found"}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/notes"))
        .and(body_json(
            json!({"title": "Viaggio", "content": "Roma", "source": "lucina", "slug": "viaggio"}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(note("viaggio", "Viaggio", "Roma")))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(server.uri()));
    let (ok, out) = call(
        &c,
        json!({"action": "create", "title": "Idee regalo", "content": "- libro", "tags": ["casa"]}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(
        (out["slug"].as_str(), out["stored"].as_str()),
        (Some("idee-regalo"), Some("secretary"))
    );
    let (ok, out) = call(
        &c,
        json!({"action": "append", "title": "Idee regalo", "content": "- sciarpa"}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(out["chars"], 17);
    // Appending to a missing note starts it.
    let (ok, out) = call(
        &c,
        json!({"action": "append", "title": "Viaggio", "content": "Roma"}),
    )
    .await;
    assert!(ok, "{out}");
    let saved: Vec<String> = c
        .effects()
        .into_iter()
        .filter_map(|e| match e {
            Effect::Note { title, .. } => Some(title),
            _ => None,
        })
        .collect();
    assert_eq!(saved, ["Idee regalo", "Idee regalo", "Viaggio"]);
    assert!(!dir.path().join("notes").exists());
}

#[tokio::test]
async fn refusals_are_reported_and_a_dead_server_falls_back() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/notes"))
        .respond_with(
            ResponseTemplate::new(409)
                .set_body_json(json!({"error": {"code": "slug_taken", "message": "exists"}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/notes/missing"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"error": {"code": "note_not_found"}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/notes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"notes": []})))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(server.uri()));
    let (ok, out) = call(
        &c,
        json!({"action": "create", "title": "Dup", "content": "x"}),
    )
    .await;
    assert!(!ok);
    assert!(
        out["error"]
            .as_str()
            .unwrap()
            .contains("exists; use append"),
        "{out}"
    );
    let (ok, out) = call(&c, json!({"action": "read", "slug": "missing"})).await;
    assert!(!ok);
    assert!(out["error"].as_str().unwrap().contains("no note"), "{out}");
    assert!(
        !dir.path().join("notes").exists(),
        "a refusal never writes locally"
    );

    // Nothing listening: local files.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(dead));
    let (ok, out) = call(
        &c,
        json!({"action": "create", "title": "Offline", "content": "kept here"}),
    )
    .await;
    assert!(ok, "{out}");
    assert!(out.get("stored").is_none());
    assert!(dir.path().join("notes/offline.md").exists());
    let (ok, out) = call(&c, json!({"action": "read", "title": "Offline"})).await;
    assert!(ok);
    assert_eq!(out["content"], "# Offline\n\nkept here\n");
}

// ------------------------------------------------------------------ Keep

fn list_note(updated: &str, items: Value) -> Value {
    let n = items.as_array().unwrap().len();
    json!({"id": "n_00000000000000000009", "slug": "shopping", "title": "Shopping", "content": "…",
           "tags": ["godlevel", "fisica 2"], "source": "google-keep", "pinned": false, "color": "blue",
           "archived": false, "items": items, "position": n,
           "reminder": {"at": "2026-11-03T09:00:00+01:00", "recurrence": "weekly", "lucinaId": "r1"},
           "createdAt": "2026-10-22T11:30:00.000Z", "updatedAt": updated})
}

fn shopping_items() -> Value {
    json!([
        {"id": "i_1", "text": "Milk", "checked": false, "indent": 0},
        {"id": "i_2", "text": "Bread", "checked": true, "indent": 0},
        {"id": "i_3", "text": "Eggs", "checked": false, "indent": 1},
    ])
}

async fn mount_shopping(server: &MockServer, note: Value) {
    Mock::given(method("GET"))
        .and(path("/v1/notes/shopping"))
        .respond_with(ResponseTemplate::new(200).set_body_json(note))
        .mount(server)
        .await;
}

fn effect_slugs(c: &Ctx) -> Vec<Option<String>> {
    c.effects()
        .into_iter()
        .filter_map(|e| match e {
            Effect::Note { slug, .. } => Some(slug),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn reads_a_keep_checklist_with_labels_colour_and_reminder() {
    let server = MockServer::start().await;
    mount_shopping(
        &server,
        list_note("2026-10-22T11:31:00.000Z", shopping_items()),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/v1/notes/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"labels": [
            {"tag": "godlevel", "name": "GodLevel", "count": 4}, {"tag": "fisica 2", "name": "Fisica 2", "count": 1}]})))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(server.uri()));
    let (ok, out) = call(&c, json!({"action": "read", "slug": "shopping"})).await;
    assert!(ok, "{out}");
    assert_eq!(out["labels"], json!(["GodLevel", "Fisica 2"]));
    assert_eq!(out["color"], "blue");
    assert_eq!(out["checklist"], "1/3 done");
    assert_eq!(out["reminder"], "2026-11-03T09:00:00+01:00 (weekly)");
    assert_eq!(out["cite"], "[note:shopping]");
    assert_eq!(
        out["items"],
        json!([
            {"n": 1, "text": "Milk", "checked": false},
            {"n": 2, "text": "Bread", "checked": true},
            {"n": 3, "text": "Eggs", "checked": false, "indent": 1},
        ])
    );
    assert!(out.get("content").is_none(), "a checklist reads as items");
}

#[tokio::test]
async fn read_shows_attachments_links_and_collaborators() {
    let server = MockServer::start().await;
    let mut n = note("trip", "Trip", "Ideas");
    n["attachments"] = json!([{"id": "a", "file": "a.jpg", "mimetype": "image/jpeg", "size": 1234, "name": "map.jpg"}]);
    n["annotations"] = json!([{"url": "https://example.org", "title": "Site"}]);
    n["collaborators"] = json!(["anna@example.com"]);
    n["archived"] = json!(true);
    Mock::given(method("GET"))
        .and(path("/v1/notes/trip"))
        .respond_with(ResponseTemplate::new(200).set_body_json(n))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(server.uri()));
    let (ok, out) = call(&c, json!({"action": "read", "slug": "trip"})).await;
    assert!(ok, "{out}");
    assert_eq!(
        out["attachments"][0],
        json!({"name": "map.jpg", "type": "image/jpeg", "size": 1234})
    );
    assert_eq!(out["links"][0]["url"], "https://example.org");
    assert_eq!(out["shared_with"], json!(["anna@example.com"]));
    assert_eq!(out["archived"], true);
    assert_eq!(out["content"], "Ideas");
    assert_eq!(out["tags"], json!(["work"]), "no registry: the stored tags");
}

#[tokio::test]
async fn creates_a_keep_checklist_with_labels_colour_and_pin() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/notes"))
        .and(body_json(json!({"title": "Shopping", "source": "lucina",
            "items": [{"text": "Milk", "checked": false, "indent": 0}, {"text": "Bread", "checked": true, "indent": 0}],
            "labels": ["GodLevel"], "color": "blue", "pinned": true, "archived": false})))
        .respond_with(ResponseTemplate::new(201).set_body_json(list_note(
            "2026-10-22T11:31:00.000Z",
            json!([{"id": "i_1", "text": "Milk", "checked": false, "indent": 0}, {"id": "i_2", "text": "Bread", "checked": true, "indent": 0}]),
        )))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(server.uri()));
    let (ok, out) = call(
        &c,
        json!({"action": "create", "title": "Shopping", "items": ["Milk", {"text": "Bread", "checked": true}],
               "labels": ["#GodLevel"], "color": "Blue", "pinned": true, "archived": false}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(out["checklist"], "1/2 done");
    assert_eq!(out["cite"], "[note:shopping]");
    assert_eq!(effect_slugs(&c), [Some("shopping".to_owned())]);

    let (ok, out) = call(
        &c,
        json!({"action": "create", "title": "x", "content": "y", "color": "mauve"}),
    )
    .await;
    assert!(
        !ok && out["error"].as_str().unwrap().contains("cerulean"),
        "{out}"
    );
    let (ok, out) = call(&c, json!({"action": "create", "title": "x"})).await;
    assert!(
        !ok && out["error"]
            .as_str()
            .unwrap()
            .contains("content is required"),
        "{out}"
    );
}

#[tokio::test]
async fn list_and_search_filter_by_label_colour_type_and_archive() {
    let server = MockServer::start().await;
    let hit = json!({"slug": "shopping", "title": "Shopping", "excerpt": "- [ ] Milk", "updatedAt": "t", "source": "google-keep",
                     "pinned": false, "tags": ["godlevel"], "labels": ["GodLevel"], "color": "blue", "archived": true,
                     "isList": true, "itemsTotal": 3, "itemsDone": 1, "reminderAt": "2026-11-03T09:00:00+01:00"});
    Mock::given(method("GET"))
        .and(path("/v1/notes"))
        .and(query_param("tag", "godlevel"))
        .and(query_param("color", "blue"))
        .and(query_param("type", "list"))
        .and(query_param("archived", "only"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"notes": [hit.clone()]})))
        .expect(1)
        .mount(&server)
        .await;
    // list hides the archive by default.
    Mock::given(method("GET"))
        .and(path("/v1/notes"))
        .and(query_param("limit", "100"))
        .and(query_param("archived", "exclude"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"notes": []})))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(server.uri()));
    // A label-only search: no query needed.
    let (ok, out) = call(
        &c,
        json!({"action": "search", "label": "#GodLevel", "color": "Blue", "type": "list", "archive": "only"}),
    )
    .await;
    assert!(ok, "{out}");
    let r = &out["results"][0];
    assert_eq!(r["slug"], "shopping");
    assert_eq!(r["labels"], json!(["GodLevel"]));
    assert_eq!(r["color"], "blue");
    assert_eq!(r["archived"], true);
    assert_eq!(r["checklist"], "1/3 done");
    assert_eq!(r["reminder"], "2026-11-03T09:00:00+01:00");
    assert_eq!(r["cite"], "[note:shopping]");
    assert_eq!(r["lines"][0], "- [ ] Milk");
    let (ok, out) = call(&c, json!({"action": "list"})).await;
    assert!(ok && out["notes"].as_array().unwrap().is_empty(), "{out}");
    let (ok, out) = call(&c, json!({"action": "search"})).await;
    assert!(
        !ok && out["error"].as_str().unwrap().contains("query or a filter"),
        "{out}"
    );
}

#[tokio::test]
async fn labels_action_lists_names_with_counts() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/notes/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"labels": [
            {"tag": "godlevel", "name": "GodLevel", "count": 4}, {"tag": "fisica 2", "name": "Fisica 2", "count": 1}]})))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(server.uri()));
    let (ok, out) = call(&c, json!({"action": "labels"})).await;
    assert!(ok, "{out}");
    assert_eq!(
        out["labels"],
        json!([{"name": "GodLevel", "notes": 4}, {"name": "Fisica 2", "notes": 1}])
    );
}

#[tokio::test]
async fn update_checks_items_by_number_and_text() {
    let server = MockServer::start().await;
    mount_shopping(
        &server,
        list_note("2026-10-22T11:31:00.000Z", shopping_items()),
    )
    .await;
    // Check Milk (1) and Bread by text? Bread is already checked: uncheck it by text; remove Eggs (3); add two.
    let after = json!([
        {"id": "i_1", "text": "Milk", "checked": true, "indent": 0},
        {"id": "i_2", "text": "Bread", "checked": false, "indent": 0},
        {"text": "Butter", "checked": false, "indent": 0},
        {"text": "Jam", "checked": true, "indent": 0},
    ]);
    Mock::given(method("PATCH"))
        .and(path("/v1/notes/shopping"))
        .and(body_json(
            json!({"items": after, "baseUpdatedAt": "2026-10-22T11:31:00.000Z"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(list_note(
            "2026-10-22T11:32:00.000Z",
            json!([
                {"id": "i_1", "text": "Milk", "checked": true, "indent": 0},
                {"id": "i_2", "text": "Bread", "checked": false, "indent": 0},
                {"id": "i_4", "text": "Butter", "checked": false, "indent": 0},
                {"id": "i_5", "text": "Jam", "checked": true, "indent": 0},
            ]),
        )))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(server.uri()));
    let (ok, out) = call(
        &c,
        json!({"action": "update", "slug": "shopping", "check": [1], "uncheck": ["bread"],
               "remove_items": [3], "add_items": ["Butter", {"text": "Jam", "checked": true}]}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(out["updated"], true);
    assert_eq!(out["cite"], "[note:shopping]");
    assert_eq!(out["checklist"], "2/4 done");
    assert_eq!(
        out["changes"],
        json!([
            "checked 1 item",
            "unchecked 1 item",
            "removed 1 item",
            "added 2 items"
        ])
    );
    assert_eq!(
        out["items"][3],
        json!({"n": 4, "text": "Jam", "checked": true})
    );
    assert_eq!(effect_slugs(&c), [Some("shopping".to_owned())]);

    // Errors name what the list holds; nothing is sent.
    let (ok, out) = call(
        &c,
        json!({"action": "update", "slug": "shopping", "check": [9]}),
    )
    .await;
    assert!(
        !ok && out["error"].as_str().unwrap().contains("no item number 9")
            && out["error"].as_str().unwrap().contains("1. Milk"),
        "{out}"
    );
    let (ok, out) = call(
        &c,
        json!({"action": "update", "slug": "shopping", "check": ["nope"]}),
    )
    .await;
    assert!(
        !ok && out["error"]
            .as_str()
            .unwrap()
            .contains("no item with the text"),
        "{out}"
    );
    let (ok, out) = call(&c, json!({"action": "update", "slug": "shopping"})).await;
    assert!(
        !ok && out["error"].as_str().unwrap().contains("nothing to change"),
        "{out}"
    );
    let (ok, out) = call(
        &c,
        json!({"action": "update", "slug": "shopping", "content": "x", "check": [1]}),
    )
    .await;
    assert!(
        !ok && out["error"].as_str().unwrap().contains("not both"),
        "{out}"
    );
}

#[tokio::test]
async fn update_retries_once_on_a_conflict_and_keeps_the_other_edit() {
    let server = MockServer::start().await;
    // First GET: the version we edit; second GET: somebody added an item meanwhile.
    Mock::given(method("GET"))
        .and(path("/v1/notes/shopping"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list_note("v1", shopping_items())))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let mut newer = shopping_items();
    newer
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "i_9", "text": "Added elsewhere", "checked": false, "indent": 0}));
    Mock::given(method("GET"))
        .and(path("/v1/notes/shopping"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list_note("v2", newer.clone())))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/v1/notes/shopping"))
        .and(body_partial_json(json!({"baseUpdatedAt": "v1"})))
        .respond_with(
            ResponseTemplate::new(409)
                .set_body_json(json!({"error": {"code": "conflict", "message": "changed"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut done = newer.clone();
    done[0]["checked"] = json!(true);
    Mock::given(method("PATCH"))
        .and(path("/v1/notes/shopping"))
        .and(body_json(json!({"items": done, "baseUpdatedAt": "v2"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(list_note("v3", done.clone())))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(server.uri()));
    let (ok, out) = call(
        &c,
        json!({"action": "update", "slug": "shopping", "check": ["Milk"]}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(
        out["items"].as_array().unwrap().len(),
        4,
        "the concurrent addition survives"
    );

    // Conflict twice: reported, not retried forever.
    let server = MockServer::start().await;
    mount_shopping(&server, list_note("v1", shopping_items())).await;
    Mock::given(method("PATCH"))
        .and(path("/v1/notes/shopping"))
        .respond_with(
            ResponseTemplate::new(409).set_body_json(json!({"error": {"code": "conflict"}})),
        )
        .expect(2)
        .mount(&server)
        .await;
    let c = ctx(dir.path(), Some(server.uri()));
    let (ok, out) = call(
        &c,
        json!({"action": "update", "slug": "shopping", "pinned": true}),
    )
    .await;
    assert!(
        !ok && out["error"]
            .as_str()
            .unwrap()
            .contains("changed by someone else"),
        "{out}"
    );
}

#[tokio::test]
async fn update_labels_colour_archive_reminder_and_convert() {
    let server = MockServer::start().await;
    mount_shopping(&server, list_note("v1", shopping_items())).await;
    Mock::given(method("PATCH"))
        .and(path("/v1/notes/shopping"))
        .and(body_json(json!({"labels": ["godlevel", "Work"], "color": "teal", "archived": true, "title": "Groceries",
                              "reminder": {"at": "2026-11-10T09:00:00+01:00", "recurrence": "weekly"},
                              "baseUpdatedAt": "v1"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(list_note("v2", shopping_items())))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/v1/notes/shopping"))
        .and(body_json(
            json!({"reminder": null, "convert": "text", "baseUpdatedAt": "v1"}),
        ))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(server.uri()));
    let (ok, out) = call(
        &c,
        json!({"action": "update", "slug": "shopping", "add_labels": ["#Work", "godlevel"], "remove_labels": ["Fisica  2"],
               "color": "teal", "archived": true, "new_title": "Groceries", "reminder_at": "2026-11-10T09:00:00+01:00"}),
    )
    .await;
    assert!(ok, "{out}");
    assert!(
        out["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "label +Work, +godlevel"),
        "{out}"
    );
    let (ok, out) = call(
        &c,
        json!({"action": "update", "slug": "shopping", "reminder_at": "tomorrow"}),
    )
    .await;
    assert!(
        !ok && out["error"].as_str().unwrap().contains("RFC 3339"),
        "{out}"
    );
    let (ok, out) = call(
        &c,
        json!({"action": "update", "slug": "shopping", "color": "mauve"}),
    )
    .await;
    assert!(
        !ok && out["error"].as_str().unwrap().contains("cerulean"),
        "{out}"
    );
}

#[tokio::test]
async fn update_clears_a_reminder_converts_and_reports_a_missing_note() {
    let server = MockServer::start().await;
    mount_shopping(&server, list_note("v1", shopping_items())).await;
    // convert first (its own PATCH), then the reminder clear on the converted note.
    let mut text_note = list_note("v2", shopping_items());
    text_note.as_object_mut().unwrap().remove("items");
    Mock::given(method("PATCH"))
        .and(path("/v1/notes/shopping"))
        .and(body_json(json!({"convert": "text", "baseUpdatedAt": "v1"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(text_note))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/v1/notes/shopping"))
        .and(body_json(json!({"reminder": null, "baseUpdatedAt": "v2"})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"slug": "shopping", "title": "Shopping", "updatedAt": "v3"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/notes/ghost"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"error": {"code": "note_not_found"}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/notes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"notes": []})))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(server.uri()));
    let (ok, out) = call(
        &c,
        json!({"action": "update", "slug": "shopping", "convert": "text", "reminder_at": null}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(
        out["changes"],
        json!(["converted to text", "reminder cleared"])
    );
    assert!(out.get("items").is_none());
    let (ok, out) = call(
        &c,
        json!({"action": "update", "slug": "ghost", "pinned": true}),
    )
    .await;
    assert!(
        !ok && out["error"].as_str().unwrap().contains("no note titled"),
        "{out}"
    );
    // A text note is not a checklist.
    Mock::given(method("GET"))
        .and(path("/v1/notes/plain"))
        .respond_with(ResponseTemplate::new(200).set_body_json(note("plain", "Plain", "a\nb")))
        .mount(&server)
        .await;
    let (ok, out) = call(
        &c,
        json!({"action": "update", "slug": "plain", "check": [1]}),
    )
    .await;
    assert!(
        !ok && out["error"].as_str().unwrap().contains("text note"),
        "{out}"
    );
}

#[tokio::test]
async fn keep_features_are_refused_without_the_server_but_plain_notes_work() {
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(dead));
    for args in [
        json!({"action": "update", "slug": "x", "pinned": true}),
        json!({"action": "labels"}),
        json!({"action": "create", "title": "T", "content": "x", "items": ["a"]}),
        json!({"action": "create", "title": "T", "content": "x", "color": "red"}),
        json!({"action": "search", "query": "x", "label": "work"}),
        json!({"action": "list", "archive": "only"}),
    ] {
        let (ok, out) = call(&c, args.clone()).await;
        assert!(!ok, "{args}");
        assert!(
            out["error"].as_str().unwrap().contains("Secretary server"),
            "{args}: {out}"
        );
    }
    assert!(
        !dir.path().join("notes").exists(),
        "a refusal writes nothing"
    );
    let (ok, out) = call(
        &c,
        json!({"action": "create", "title": "Plain", "content": "ok", "tags": ["a"]}),
    )
    .await;
    assert!(ok, "{out}");
    assert!(dir.path().join("notes/plain.md").exists());
}
