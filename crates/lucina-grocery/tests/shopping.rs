//! Shopping v2 against a throwaway Postgres with migrations 001, 002 and 003.
//!
//! Needs `LUCINA_TEST_DATABASE_URL` (e.g. a `postgres:17` container on
//! 127.0.0.1:55432); without it every test is skipped. The schema is
//! dropped and recreated, so never point this at a real database: the URL
//! must contain `55432` or `lucina_test`.

use lucina_grocery::{resolve_canonical_product, Catalog, GroceryError, GroceryRepo};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use tokio::sync::OnceCell;
use uuid::Uuid;

static MIGRATED: OnceCell<String> = OnceCell::const_new();

async fn repo() -> Option<GroceryRepo> {
    let url = std::env::var("LUCINA_TEST_DATABASE_URL").ok()?;
    assert!(
        url.contains("55432") || url.contains("lucina_test"),
        "refusing a non-test database"
    );
    let url = MIGRATED
        .get_or_init(|| async move {
            let pool = PgPoolOptions::new()
                .max_connections(1)
                .connect(&url)
                .await
                .unwrap();
            sqlx::raw_sql("DROP SCHEMA IF EXISTS grocery CASCADE")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::raw_sql(include_str!("../migrations/001_grocery.sql"))
                .execute(&pool)
                .await
                .unwrap();
            sqlx::raw_sql(include_str!("../migrations/002_shopping_v2.sql"))
                .execute(&pool)
                .await
                .unwrap();
            sqlx::raw_sql(include_str!("../migrations/003_shopping_items.sql"))
                .execute(&pool)
                .await
                .unwrap();
            sqlx::raw_sql(include_str!("../migrations/004_shopping_row_aliases.sql"))
                .execute(&pool)
                .await
                .unwrap();
            pool.close().await;
            url
        })
        .await
        .clone();
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    Some(GroceryRepo::with_pool(
        pool,
        Catalog::fixed(&json!({"version": 1, "items": [
            {"id": "banana", "names": {"it": "Banane", "en": "Bananas"}, "aliases": {"it": ["banana"]}, "section": "produce", "icon": "🍌"},
            {"id": "milk", "names": {"it": "Latte", "en": "Milk"}, "section": "dairy", "icon": "🥛"}
        ]})),
    ))
}

fn user() -> String {
    format!("test-{}", Uuid::new_v4())
}

fn op(kind: &str, extra: Value) -> Value {
    let mut v = json!({"opId": Uuid::new_v4().to_string(), "type": kind});
    v.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    v
}

fn code(e: GroceryError) -> &'static str {
    e.code()
}

#[tokio::test]
async fn lists_crud_and_default_list() {
    let Some(r) = repo().await else {
        return eprintln!("LUCINA_TEST_DATABASE_URL unset: skipped");
    };
    let u = user();
    let lists = r.list_lists(&u).await.unwrap();
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0]["name"], "Casa");
    assert_eq!(lists[0]["isDefault"], true);
    assert_eq!(lists[0]["needed"], 0);
    let office = r
        .create_list(&u, &json!({"name": " Ufficio ", "theme": "work"}))
        .await
        .unwrap();
    assert_eq!(office["name"], "Ufficio");
    assert_eq!(office["position"], 1);
    assert_eq!(
        code(
            r.create_list(&u, &json!({"name": "x", "theme": "space"}))
                .await
                .unwrap_err()
        ),
        "bad_request"
    );
    let id = office["id"].as_str().unwrap();
    let renamed = r
        .update_list(&u, id, &json!({"name": "Office"}))
        .await
        .unwrap();
    assert_eq!(renamed["revision"], 1);
    assert_eq!(renamed["theme"], "work");
    assert_eq!(
        code(r.delete_list(&u, "default").await.unwrap_err()),
        "default_list"
    );
    assert_eq!(
        r.delete_list(&u, id).await.unwrap(),
        json!({"id": id, "deleted": true})
    );
    assert_eq!(
        code(r.list_state(&u, id).await.unwrap_err()),
        "list_not_found"
    );
    assert_eq!(
        code(r.list_state(&user(), id).await.unwrap_err()),
        "list_not_found"
    );
    assert_eq!(r.list_lists(&u).await.unwrap().len(), 1);
}

#[tokio::test]
async fn ops_semantics() {
    let Some(r) = repo().await else { return };
    let u = user();
    // add by free text maps to the catalog; a second add reuses the row
    let a = op("add", json!({"name": "banane", "spec": "6"}));
    let out = r
        .apply_ops(&u, "default", &json!([a]), None, "ios")
        .await
        .unwrap();
    assert_eq!(out.revision, 1);
    let item = out.items[0].clone();
    assert_eq!(item["catalogId"], "banana");
    assert_eq!(
        (item["section"].clone(), item["icon"].clone()),
        (json!("produce"), json!("🍌"))
    );
    assert_eq!(item["updatedBy"], "ios");
    assert_eq!(item["spec"], "6");
    let id = item["id"].as_str().unwrap().to_owned();
    // replaying the same opId is a duplicate and changes nothing
    let replay = r
        .apply_ops(&u, "default", &json!([a]), None, "ios")
        .await
        .unwrap();
    assert_eq!(
        replay.duplicates,
        vec![a["opId"].as_str().unwrap().to_owned()]
    );
    assert_eq!(replay.revision, 1);
    // purchase -> recent; add again -> needed, same row
    let out = r
        .apply_ops(
            &u,
            "default",
            &json!([op("purchase", json!({"itemId": id}))]),
            Some(1),
            "web",
        )
        .await
        .unwrap();
    assert_eq!(out.items[0]["status"], "recent");
    assert!(out.items[0]["purchasedAt"].is_string());
    let out = r
        .apply_ops(
            &u,
            "default",
            &json!([op("add", json!({"catalogId": "banana", "quantity": "2"}))]),
            None,
            "web",
        )
        .await
        .unwrap();
    assert_eq!(out.items.len(), 1);
    assert_eq!(out.items[0]["id"], json!(id));
    assert_eq!(out.items[0]["status"], "needed");
    assert_eq!(out.items[0]["quantity"], 2);
    assert_eq!(out.revision, 3);
    // rejected ops get receipts; the rest of the batch applies
    let bad = op("purchase", json!({"itemId": Uuid::new_v4().to_string()}));
    let milk = op("add", json!({"name": "Latte"}));
    let out = r
        .apply_ops(
            &u,
            "default",
            &json!([
                bad,
                milk,
                op("set_quantity", json!({"itemId": id, "quantity": -1}))
            ]),
            None,
            "web",
        )
        .await
        .unwrap();
    assert_eq!(
        out.rejected,
        vec![
            json!({"opId": bad["opId"], "error": "item_not_found"}),
            json!({"opId": out.rejected[1]["opId"], "error": "invalid_quantity"}),
        ]
    );
    assert_eq!(out.applied.len(), 1);
    assert_eq!(out.revision, 4);
    let replay = r
        .apply_ops(&u, "default", &json!([bad]), None, "web")
        .await
        .unwrap();
    assert_eq!(replay.duplicates.len(), 1);
    // rename onto an existing name on the list
    let milk_id = out.items.iter().find(|i| i["catalogId"] == "milk").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = r
        .apply_ops(
            &u,
            "default",
            &json!([op("add", json!({"name": "pane"}))]),
            None,
            "web",
        )
        .await
        .unwrap();
    let bread_id = out.items.iter().find(|i| i["typedName"] == "pane").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = r
        .apply_ops(
            &u,
            "default",
            &json!([op("rename", json!({"itemId": bread_id, "name": "Banane"}))]),
            None,
            "web",
        )
        .await
        .unwrap();
    assert_eq!(
        out.rejected[0]["error"], "duplicate_item",
        "renaming onto another object's name (any language) that is already listed"
    );
    // a custom row: rename edits its object's name in the request language
    let out = r
        .apply_ops_in(
            &u,
            "default",
            &json!([op(
                "rename",
                json!({"itemId": bread_id, "name": "Pagnotta"})
            )]),
            None,
            "web",
            Some("it"),
        )
        .await
        .unwrap();
    let bread = out
        .items
        .iter()
        .find(|i| i["id"] == json!(bread_id))
        .unwrap();
    assert_eq!(bread["name"], "Pagnotta");
    assert_eq!(bread["item"]["kind"], "custom");
    assert_eq!(bread["item"]["names"]["en"], "pane");
    let dup = r
        .apply_ops(
            &u,
            "default",
            &json!([op("add", json!({"name": "acqua"}))]),
            None,
            "web",
        )
        .await
        .unwrap();
    let water_id = dup
        .items
        .iter()
        .find(|i| i["typedName"] == "acqua")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = r
        .apply_ops(
            &u,
            "default",
            &json!([op(
                "rename",
                json!({"itemId": water_id, "name": "  BANANE "})
            )]),
            None,
            "web",
        )
        .await
        .unwrap();
    assert_eq!(out.rejected[0]["error"], "duplicate_item");
    // reorder, remove, deleted stays deleted
    let out = r
        .apply_ops(
            &u,
            "default",
            &json!([op("reorder", json!({"itemId": milk_id, "position": 0}))]),
            None,
            "web",
        )
        .await
        .unwrap();
    assert_eq!(out.items[0]["id"], json!(milk_id));
    let out = r
        .apply_ops(
            &u,
            "default",
            &json!([
                op("remove", json!({"itemId": milk_id})),
                op("need", json!({"itemId": milk_id}))
            ]),
            None,
            "web",
        )
        .await
        .unwrap();
    assert_eq!(out.applied.len(), 2);
    assert!(out.items.iter().all(|i| i["id"] != json!(milk_id)));
    // bad batches
    assert_eq!(
        code(
            r.apply_ops(&u, "default", &json!([]), None, "web")
                .await
                .unwrap_err()
        ),
        "bad_request"
    );
    assert_eq!(
        code(
            r.apply_ops(
                &u,
                "default",
                &json!([{"opId": "x", "type": "add"}]),
                None,
                "web"
            )
            .await
            .unwrap_err()
        ),
        "bad_request"
    );
    assert_eq!(
        code(
            r.apply_ops(
                &u,
                "default",
                &json!([{"opId": Uuid::new_v4().to_string(), "type": "explode"}]),
                None,
                "web"
            )
            .await
            .unwrap_err()
        ),
        "bad_request"
    );
}

#[tokio::test]
async fn revision_conflicts() {
    let Some(r) = repo().await else { return };
    let u = user();
    let out = r
        .apply_ops(
            &u,
            "default",
            &json!([
                op("add", json!({"name": "kiwi"})),
                op("add", json!({"name": "pere"}))
            ]),
            None,
            "web",
        )
        .await
        .unwrap();
    let kiwi = out.items.iter().find(|i| i["name"] == "kiwi").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let pere = out.items.iter().find(|i| i["name"] == "pere").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(out.revision, 1);
    // another client changes kiwi (revision 2)
    r.apply_ops(
        &u,
        "default",
        &json!([op("set_spec", json!({"itemId": kiwi, "spec": "bio"}))]),
        Some(1),
        "ios",
    )
    .await
    .unwrap();
    // a stale client touching kiwi: 409, nothing applied, no receipts
    let stale = op("purchase", json!({"itemId": kiwi}));
    let other = op("purchase", json!({"itemId": pere}));
    match r
        .apply_ops(&u, "default", &json!([stale, other]), Some(1), "web")
        .await
        .unwrap_err()
    {
        GroceryError::Shopping {
            code,
            revision,
            conflicts,
            ..
        } => {
            assert_eq!(code, "revision_conflict");
            assert_eq!(revision, Some(2));
            assert_eq!(conflicts, vec![stale["opId"].as_str().unwrap().to_owned()]);
        }
        e => panic!("{e:?}"),
    }
    let state = r.list_state(&u, "default").await.unwrap();
    assert_eq!(state["revision"], 2);
    assert!(state["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["status"] == "needed"));
    // the refused opIds were not used: resending the non-conflicting one applies
    let out = r
        .apply_ops(&u, "default", &json!([other]), Some(1), "web")
        .await
        .unwrap();
    assert_eq!(out.applied.len(), 1);
    assert_eq!(out.revision, 3);
    // add with spec onto a changed item conflicts; add without overwrite does not
    let out = r
        .apply_ops(
            &u,
            "default",
            &json!([op("add", json!({"name": "kiwi"}))]),
            Some(1),
            "web",
        )
        .await
        .unwrap();
    assert_eq!(out.applied.len(), 1);
    let err = r
        .apply_ops(
            &u,
            "default",
            &json!([op("add", json!({"name": "kiwi", "spec": "3"}))]),
            Some(1),
            "web",
        )
        .await
        .unwrap_err();
    assert_eq!(code(err), "revision_conflict");
}

#[tokio::test]
async fn move_merge_and_clear_recent() {
    let Some(r) = repo().await else { return };
    let u = user();
    let work = r
        .create_list(&u, &json!({"name": "Ufficio", "theme": "work"}))
        .await
        .unwrap();
    let work_id = work["id"].as_str().unwrap().to_owned();
    let home = r
        .apply_ops(
            &u,
            "default",
            &json!([
                op("add", json!({"name": "caffè"})),
                op("add", json!({"name": "zucchero"}))
            ]),
            None,
            "web",
        )
        .await
        .unwrap();
    let coffee = home.items.iter().find(|i| i["name"] == "caffè").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let sugar = home.items.iter().find(|i| i["name"] == "zucchero").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    r.apply_ops(
        &u,
        &work_id,
        &json!([op("add", json!({"name": "Caffè"}))]),
        None,
        "web",
    )
    .await
    .unwrap();
    // coffee already on the work list: merged; sugar moves
    let out = r
        .apply_ops(
            &u,
            "default",
            &json!([
                op("move", json!({"itemId": coffee, "toListId": work_id})),
                op("move", json!({"itemId": sugar, "toListId": work_id})),
                op("move", json!({"itemId": sugar, "toListId": "default"}))
            ]),
            None,
            "web",
        )
        .await
        .unwrap();
    assert_eq!(out.applied.len(), 2);
    assert_eq!(out.rejected[0]["error"], "item_not_found");
    assert!(out.items.is_empty());
    let w = r.list_state(&u, &work_id).await.unwrap();
    assert_eq!(w["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        w["revision"], 2,
        "one bump from the add, one from the moves"
    );
    let sid = w["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == "zucchero")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = r
        .apply_ops(
            &u,
            &work_id,
            &json!([op("move", json!({"itemId": sid, "toListId": work_id}))]),
            None,
            "web",
        )
        .await
        .unwrap();
    assert_eq!(out.rejected[0]["error"], "same_list");
    let out = r
        .apply_ops(
            &u,
            &work_id,
            &json!([
                op("purchase", json!({"itemId": sid})),
                op("clear_recent", json!({}))
            ]),
            None,
            "web",
        )
        .await
        .unwrap();
    assert_eq!(out.items.len(), 1);
}

#[tokio::test]
async fn legacy_routes() {
    let Some(r) = repo().await else { return };
    let u = user();
    let item = r
        .add_item(&u, &resolve_canonical_product("milk"), "latte", "ios")
        .await
        .unwrap();
    assert_eq!(
        item["product"], "Latte",
        "the object's name in the owner language"
    );
    assert_eq!(item["itemId"], "milk");
    assert_eq!(item["rawName"], "latte");
    assert_eq!(item["merged"], false);
    assert_eq!(item["status"], "needed");
    let again = r
        .add_item(&u, &resolve_canonical_product("latte"), "latte", "web")
        .await
        .unwrap();
    assert_eq!(again["id"], item["id"]);
    assert_eq!(again["merged"], true);
    let id = item["id"].as_str().unwrap();
    let patched = r
        .legacy_set_status(&u, id, "purchased", "web")
        .await
        .unwrap();
    assert_eq!(
        patched,
        json!({"id": id, "product": "Latte", "name": "Latte", "itemId": "milk", "status": "purchased"})
    );
    let list = r.legacy_list(&u).await.unwrap();
    assert_eq!(list[0]["status"], "purchased");
    assert_eq!(list[0]["product"], "Latte");
    assert_eq!(
        r.legacy_list_in(&u, Some("en")).await.unwrap()[0]["product"],
        "Milk"
    );
    assert_eq!(
        code(
            r.legacy_set_status(&u, &Uuid::new_v4().to_string(), "needed", "web")
                .await
                .unwrap_err()
        ),
        "item_not_found"
    );
    assert_eq!(
        code(
            r.legacy_set_status(&user(), id, "needed", "web")
                .await
                .unwrap_err()
        ),
        "item_not_found"
    );
    assert_eq!(
        code(
            r.legacy_set_status(&u, id, "eaten", "web")
                .await
                .unwrap_err()
        ),
        "invalid_status"
    );
    let (removed, _) = r.remove_items_named(&u, "Latte", "telegram").await.unwrap();
    assert_eq!(removed, 1);
    assert!(r.legacy_list(&u).await.unwrap().is_empty());
}

#[tokio::test]
async fn item_objects() {
    let Some(r) = repo().await else { return };
    let u = user();
    // any language finds the same object; names follow the requested language
    let out = r
        .apply_ops(
            &u,
            "default",
            &json!([op("add", json!({"text": "milk"}))]),
            None,
            "web",
        )
        .await
        .unwrap();
    let milk = out.items[0].clone();
    assert_eq!(
        (
            milk["itemId"].clone(),
            milk["item"]["kind"].clone(),
            milk["name"].clone(),
            milk["typedName"].clone()
        ),
        (
            json!("milk"),
            json!("catalog"),
            json!("Latte"),
            json!("milk")
        )
    );
    let again = r
        .apply_ops_in(
            &u,
            "default",
            &json!([
                op("add", json!({"text": "LATTE"})),
                op("add", json!({"text": "the milk"})),
                op("add", json!({"itemId": "milk"}))
            ]),
            None,
            "web",
            Some("en"),
        )
        .await
        .unwrap();
    assert_eq!(again.items.len(), 1);
    assert_eq!(again.merged.len(), 3);
    assert_eq!(again.items[0]["name"], "Milk");
    assert_eq!(again.lang, "en");
    // an old web outbox: its own row UUID in itemId next to catalogId
    let legacy = r
        .apply_ops(&u, "default", &json!([op("add", json!({"itemId": Uuid::new_v4().to_string(), "catalogId": "banana", "name": "Banane"}))]), None, "web")
        .await
        .unwrap();
    assert!(legacy.rejected.is_empty());
    let unknown = r
        .apply_ops(
            &u,
            "default",
            &json!([op("add", json!({"itemId": "no-such-thing"}))]),
            None,
            "web",
        )
        .await
        .unwrap();
    assert_eq!(unknown.rejected[0]["error"], "unknown_item");

    // new text: a custom object, typed text everywhere without a model
    let out = r
        .apply_ops(
            &u,
            "default",
            &json!([
                op("add", json!({"text": "Detersivo piatti"})),
                op("add", json!({"text": "detersivo  PIATTI"}))
            ]),
            None,
            "telegram",
        )
        .await
        .unwrap();
    let soap = out
        .items
        .iter()
        .find(|i| i["typedName"] == "Detersivo piatti")
        .unwrap()
        .clone();
    assert_eq!(out.merged.len(), 1);
    let soap_id = soap["itemId"].as_str().unwrap().to_owned();
    assert!(
        lucina_grocery::items::is_custom_item_id(&soap_id),
        "{soap_id}"
    );
    assert_eq!(soap["item"]["names"]["da"], "Detersivo piatti");
    assert_eq!(soap["item"]["needsTranslation"], true);
    assert_eq!(soap["section"], "other");
    let customs = r.custom_items(&u).await.unwrap();
    assert_eq!(customs.len(), 1);
    assert_eq!(customs[0]["aliases"], json!(["Detersivo piatti"]));

    // edit_item: names, icon; catalog objects and taken names are refused
    let out = r
        .apply_ops_in(
            &u,
            "default",
            &json!([
                op("edit_item", json!({"itemId": soap_id, "names": {"en": "Dish soap", "da": "Opvaskemiddel"}, "icon": "🧴"})),
                op("edit_item", json!({"itemId": "milk", "names": {"en": "x"}})),
                op("edit_item", json!({"itemId": soap_id, "names": {"en": "Milk"}})),
                op("edit_item", json!({"itemId": soap_id, "icon": "soap"})),
            ]),
            None,
            "web",
            Some("en"),
        )
        .await
        .unwrap();
    let errors: Vec<&str> = out
        .rejected
        .iter()
        .filter_map(|x| x["error"].as_str())
        .collect();
    assert_eq!(errors, vec!["not_custom", "name_taken", "invalid_icon"]);
    let soap = out
        .items
        .iter()
        .find(|i| i["itemId"] == json!(soap_id))
        .unwrap();
    assert_eq!(
        (soap["name"].clone(), soap["icon"].clone()),
        (json!("Dish soap"), json!("🧴"))
    );
    assert_eq!(soap["item"]["needsTranslation"], false);
    // the other languages now find it
    let out = r
        .apply_ops(
            &u,
            "default",
            &json!([op("add", json!({"text": "opvaskemiddel"}))]),
            None,
            "web",
        )
        .await
        .unwrap();
    assert_eq!(out.merged.len(), 1);
    // remove by a name in another language
    let (n, _, names) = r
        .remove_items_named_in(&u, "dish soap", "telegram", Some("it"))
        .await
        .unwrap();
    assert_eq!((n, names), (1, vec!["Detersivo piatti".to_owned()]));
}

#[tokio::test]
async fn new_items_are_named_by_one_model_call() {
    let Some(r) = repo().await else { return };
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    let r = r.with_namer(std::sync::Arc::new(move |_system: String, user: String| {
        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert!(user.contains("spugne"), "{user}");
        Box::pin(async {
            Some("```json\n{\"names\":{\"it\":\"Spugne\",\"en\":\"Sponges\",\"da\":\"Svampe\"},\"section\":\"produce\",\"icon\":\"🧽\"}\n```".to_owned())
        })
    }));
    std::env::remove_var("LUCINA_SHOPPING_TRANSLATE");
    let u = user();
    let out = r
        .apply_ops_in(
            &u,
            "default",
            &json!([
                op("add", json!({"text": "spugne"})),
                op("add", json!({"text": "Spugne"}))
            ]),
            None,
            "web",
            Some("da"),
        )
        .await
        .unwrap();
    assert_eq!(out.items.len(), 1);
    assert_eq!(out.items[0]["name"], "Svampe");
    assert_eq!(out.items[0]["icon"], "🧽");
    assert_eq!(out.items[0]["item"]["needsTranslation"], false);
    let again = r
        .apply_ops(
            &u,
            "default",
            &json!([op("add", json!({"text": "sponges"}))]),
            None,
            "web",
        )
        .await
        .unwrap();
    assert_eq!(again.merged.len(), 1);
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one call; known names never call again"
    );
}

/// S1 (docs/verification.md): the response to a batch was lost after the
/// server applied it; the client resends the same ops (same op ids) with its
/// old revision while another client changed the item. The 409 names the
/// ops that were already applied, so the client drops them instead of
/// rebasing them into new ops that would apply twice.
#[tokio::test]
async fn a_conflict_names_the_batchs_duplicates() {
    let Some(r) = repo().await else { return };
    let u = user();
    let add = op("add", json!({"name": "kiwi", "spec": "bio"}));
    let out = r
        .apply_ops(&u, "default", &json!([add]), Some(0), "web")
        .await
        .unwrap();
    let kiwi = out.item_ids[add["opId"].as_str().unwrap()].clone();
    // (the ack is lost) another client buys the kiwi: revision 2
    r.apply_ops(
        &u,
        "default",
        &json!([op("purchase", json!({"itemId": kiwi}))]),
        Some(1),
        "ios",
    )
    .await
    .unwrap();
    // the retry: the add again (a duplicate) and a new op on the kiwi
    let spec = op("set_spec", json!({"itemId": kiwi, "spec": "2"}));
    match r
        .apply_ops(&u, "default", &json!([add, spec]), Some(0), "web")
        .await
        .unwrap_err()
    {
        GroceryError::Shopping {
            code,
            conflicts,
            duplicates,
            ..
        } => {
            assert_eq!(code, "revision_conflict");
            assert_eq!(conflicts, vec![spec["opId"].as_str().unwrap().to_owned()]);
            assert_eq!(duplicates, vec![add["opId"].as_str().unwrap().to_owned()]);
        }
        e => panic!("{e:?}"),
    }
    let state = r.list_state(&u, "default").await.unwrap();
    assert_eq!(state["revision"], 2);
    assert_eq!(
        state["items"][0]["status"], "recent",
        "the add did not apply again"
    );
}

/// S2: offline "add, then undo" goes out as one batch. The server used to
/// create its own row id, so the remove (on the client's optimistic row id)
/// was rejected `item_not_found` and dropped: the item stayed on the list.
#[tokio::test]
async fn ops_on_the_clients_optimistic_row_land() {
    let Some(r) = repo().await else { return };
    let u = user();
    let row = Uuid::new_v4().to_string();
    let out = r
        .apply_ops(
            &u,
            "default",
            &json!([
                op("add", json!({"name": "pane", "itemId": row})),
                op("remove", json!({"itemId": row}))
            ]),
            None,
            "web",
        )
        .await
        .unwrap();
    assert!(out.rejected.is_empty(), "{:?}", out.rejected);
    assert_eq!(out.applied.len(), 2);
    assert_eq!(
        out.item_ids.values().next().unwrap(),
        &row,
        "the client's id"
    );
    let state = r.list_state(&u, "default").await.unwrap();
    assert!(
        state["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["id"] != row.as_str()),
        "removed: {state}"
    );

    // An add that merges into the object's existing row: the client's row id
    // becomes an alias of that row, in this batch and in later ones.
    let add_milk = op("add", json!({"name": "latte"}));
    let first = r
        .apply_ops(&u, "default", &json!([add_milk]), None, "ios")
        .await
        .unwrap();
    let milk = first.item_ids[add_milk["opId"].as_str().unwrap()].clone();
    let mine = Uuid::new_v4().to_string();
    let out = r
        .apply_ops(
            &u,
            "default",
            &json!([
                op("add", json!({"name": "latte", "rowId": mine})),
                op("set_spec", json!({"itemId": mine, "spec": "intero"}))
            ]),
            None,
            "web",
        )
        .await
        .unwrap();
    assert!(out.rejected.is_empty(), "{:?}", out.rejected);
    let later = r
        .apply_ops(
            &u,
            "default",
            &json!([op("purchase", json!({"itemId": mine}))]),
            Some(out.revision),
            "web",
        )
        .await
        .unwrap();
    assert!(later.rejected.is_empty(), "{:?}", later.rejected);
    let item = later
        .items
        .iter()
        .find(|i| i["id"] == milk.as_str())
        .unwrap();
    assert_eq!(item["status"], "recent", "{item}");
    // A stale op through the alias conflicts like one on the row itself.
    r.apply_ops(
        &u,
        "default",
        &json!([op("need", json!({"itemId": milk}))]),
        None,
        "ios",
    )
    .await
    .unwrap();
    let err = r
        .apply_ops(
            &u,
            "default",
            &json!([op("purchase", json!({"itemId": mine}))]),
            Some(later.revision),
            "web",
        )
        .await
        .unwrap_err();
    assert_eq!(code(err), "revision_conflict");
}
