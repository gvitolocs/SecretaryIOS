//! The Instagram / Pixiv plugin tools against mocked services (wiremock):
//! request paths and headers, safe-only Pixiv, owner-only gating and clean
//! "not available" answers.

use std::sync::Arc;
use std::time::Duration;

use lucina_tools::agent;
use lucina_tools::{
    Ctx, Effect, Origin, Registry, ToolEnv, ToolSelection, NO_MCP_TOOLS, OWNER_ONLY_TOOLS,
};
use serde_json::{json, Value};
use wiremock::matchers::{body_json, header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const IG_SECRET: &str = "ig-gateway-secret-0123456789";
const PX_SECRET: &str = "px-gateway-secret-0123456789";
const PUBLIC: &str = "https://getsecretary.tech";

struct Fixture {
    ig: MockServer,
    px: MockServer,
    dir: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        Self {
            ig: MockServer::start().await,
            px: MockServer::start().await,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn env(&self) -> ToolEnv {
        let mut env = ToolEnv::new(self.dir.path(), "Europe/Rome".parse().unwrap());
        let ig = self.dir.path().join("ig.secret");
        let px = self.dir.path().join("px.secret");
        std::fs::write(&ig, format!("{IG_SECRET}\n")).unwrap();
        std::fs::write(&px, format!("{PX_SECRET}\n")).unwrap();
        env.plugins.instagram.url = self.ig.uri();
        env.plugins.instagram.secret_file = ig;
        env.plugins.pixiv.url = self.px.uri();
        env.plugins.pixiv.secret_file = px;
        env.plugins.public_url = PUBLIC.into();
        env
    }

    fn ctx(&self) -> Ctx {
        Ctx::new(Arc::new(self.env()), "web", "chat-1")
    }
}

fn registry() -> Registry {
    Registry::builtin(&ToolSelection::All)
}

async fn call(ctx: &Ctx, name: &str, args: Value) -> (bool, Value) {
    let (ok, out, _) = agent::execute(&registry(), name, args, ctx, Duration::from_secs(10)).await;
    (ok, out)
}

fn profile() -> Value {
    json!({
        "profile": {"username": "natgeo", "fullName": "National Geographic", "followers": 1},
        "posts": {"items": [
            {"id": "p1", "kind": "image", "url": "/v1/instagram/s/a/1/9999999999/sig1",
             "thumbUrl": "/v1/instagram/s/a/1t/9999999999/sig1t", "takenAt": "2026-10-27T10:00:00Z",
             "caption": "A fox"},
            {"id": "p2", "kind": "video", "url": "/v1/instagram/s/a/2/9999999999/sig2",
             "thumbUrl": "/v1/instagram/s/a/2t/9999999999/sig2t"},
        ], "nextCursor": null},
        "highlights": [],
    })
}

#[tokio::test]
async fn profile_calls_the_service_with_the_gateway_secret_and_absolutizes_urls() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/v1/instagram/profile/natgeo"))
        .and(header("x-lucina-gateway", IG_SECRET))
        .respond_with(ResponseTemplate::new(200).set_body_json(profile()))
        .expect(2)
        .mount(&f.ig)
        .await;
    let ctx = f.ctx();
    let (ok, out) = call(&ctx, "instagram_profile", json!({"username": "@natgeo"})).await;
    assert!(ok, "{out}");
    assert_eq!(
        out["posts"]["items"][0]["url"],
        "https://getsecretary.tech/v1/instagram/s/a/1/9999999999/sig1"
    );
    assert_eq!(
        out["posts"]["items"][1]["thumbUrl"],
        "https://getsecretary.tech/v1/instagram/s/a/2t/9999999999/sig2t"
    );
    assert_eq!(out["profile"]["username"], "natgeo");
    // marks the turn private and offers the plugin page
    let effects = ctx.effects();
    assert!(effects.iter().any(|e| matches!(e, Effect::PrivateTurn)));
    assert!(effects.iter().any(
        |e| matches!(e, Effect::Gallery { path, count } if path == "/instagram" && *count == 2)
    ));
    // the timeline line carries no username
    let (_, _, summary) = agent::execute(
        &registry(),
        "instagram_profile",
        json!({"username": "natgeo"}),
        &ctx,
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(summary, "instagram_profile");
}

#[tokio::test]
async fn stories_use_their_own_path_and_bad_usernames_never_reach_the_network() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/v1/instagram/stories/some.user_1"))
        .and(header("x-lucina-gateway", IG_SECRET))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .expect(1)
        .mount(&f.ig)
        .await;
    let ctx = f.ctx();
    let (ok, out) = call(
        &ctx,
        "instagram_stories",
        json!({"username": "some.user_1"}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(out["items"], json!([]));
    for bad in ["../admin", "a/b", "x y", "x?y=1", ""] {
        let (ok, out) = call(&ctx, "instagram_stories", json!({"username": bad})).await;
        assert!(!ok, "{bad}: {out}");
    }
    // `expect(1)` above: the bad names made no request
}

#[tokio::test]
async fn pixiv_search_is_always_safe_mode_and_drops_adult_works() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/v1/pixiv/search"))
        .and(header("x-lucina-gateway", PX_SECRET))
        .and(query_param("q", "フリーレン"))
        .and(query_param("mode", "safe"))
        .and(query_param("type", "illust"))
        .and(query_param_is_missing("adult"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                {"id": "1", "title": "ok", "xRestrict": 0, "thumbUrl": "/v1/pixiv/s/t/1/9999999999/sig"},
                {"id": "2", "title": "adult", "xRestrict": 1},
                {"id": "3", "title": "adult too", "adult": true},
            ],
            "page": 1,
        })))
        .expect(1)
        .mount(&f.px)
        .await;
    let ctx = f.ctx();
    // a model that tries to ask for adult content has no argument for it: unknown fields are ignored
    let (ok, out) = call(
        &ctx,
        "pixiv_search",
        json!({"query": "フリーレン", "type": "illust", "mode": "all", "adult": 1}),
    )
    .await;
    assert!(ok, "{out}");
    let items = out["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], "1");
    assert_eq!(
        items[0]["thumbUrl"],
        "https://getsecretary.tech/v1/pixiv/s/t/1/9999999999/sig"
    );
    assert_eq!(out["adultDropped"], 2);
}

#[tokio::test]
async fn pixiv_ranking_never_asks_for_r18() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/v1/pixiv/ranking"))
        .and(query_param("mode", "week"))
        .and(query_param_is_missing("adult"))
        .and(header("x-lucina-gateway", PX_SECRET))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .expect(1)
        .mount(&f.px)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/pixiv/ranking"))
        .and(query_param("mode", "day"))
        .and(query_param_is_missing("adult"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .expect(1)
        .mount(&f.px)
        .await;
    let ctx = f.ctx();
    assert!(call(&ctx, "pixiv_ranking", json!({"mode": "week"})).await.0);
    assert!(call(&ctx, "pixiv_ranking", json!({})).await.0);
    for mode in [
        "day_r18",
        "week_r18",
        "day_male_r18",
        "r18",
        "DAY_R18",
        "day&adult=1",
    ] {
        let (ok, out) = call(&ctx, "pixiv_ranking", json!({"mode": mode})).await;
        assert!(!ok, "{mode}: {out}");
        assert!(out["error"].as_str().unwrap().contains("invalid arguments"));
    }
    // the safe schema has no adult field to set
    let spec = registry()
        .specs()
        .into_iter()
        .find(|s| s.function.name == "pixiv_ranking")
        .unwrap();
    let p = spec.function.parameters.to_string();
    assert!(!p.contains("r18") && !p.contains("adult"), "{p}");
    let spec = registry()
        .specs()
        .into_iter()
        .find(|s| s.function.name == "pixiv_search")
        .unwrap();
    let p = spec.function.parameters.to_string();
    assert!(!p.contains("r18") && !p.contains("adult"), "{p}");
}

#[tokio::test]
async fn save_instagram_items_from_the_latest_result() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/v1/instagram/profile/natgeo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(profile()))
        .mount(&f.ig)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/instagram/save"))
        .and(header("x-lucina-gateway", IG_SECRET))
        .and(body_json(json!({"items": [
            {"url": "/v1/instagram/s/a/1/9999999999/sig1", "kind": "image", "username": "natgeo",
             "takenAt": "2026-10-27T10:00:00Z", "caption": "A fox"},
            {"url": "/v1/instagram/s/a/2/9999999999/sig2", "kind": "video", "username": "natgeo"},
        ]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "saved": [{"itemId": "itm_1", "duplicate": false}, {"itemId": "itm_2", "duplicate": true}],
            "failed": [],
        })))
        .expect(1)
        .mount(&f.ig)
        .await;
    let ctx = f.ctx();
    // nothing to save before any result
    let (ok, out) = call(
        &ctx,
        "save_to_gallery",
        json!({"source": "instagram", "all": true}),
    )
    .await;
    assert!(!ok, "{out}");
    call(&ctx, "instagram_profile", json!({"username": "natgeo"})).await;
    let (ok, out) = call(
        &ctx,
        "save_to_gallery",
        json!({"source": "instagram", "all": true}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(
        (out["saved"].as_u64(), out["alreadyThere"].as_u64()),
        (Some(1), Some(1))
    );
}

#[tokio::test]
async fn save_by_url_accepts_the_absolute_form_and_refuses_foreign_urls() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/v1/instagram/profile/natgeo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(profile()))
        .mount(&f.ig)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/instagram/save"))
        .and(body_json(json!({"items": [
            {"url": "/v1/instagram/s/a/2/9999999999/sig2", "kind": "video", "username": "natgeo"},
        ]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "saved": [{"itemId": "itm_9", "duplicate": false}], "failed": [],
        })))
        .expect(1)
        .mount(&f.ig)
        .await;
    let ctx = f.ctx();
    call(&ctx, "instagram_profile", json!({"username": "natgeo"})).await;
    let (ok, out) = call(
        &ctx,
        "save_to_gallery",
        json!({"source": "instagram",
               "url": "https://getsecretary.tech/v1/instagram/s/a/2/9999999999/sig2"}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(out["saved"], 1);
    for bad in [
        "https://evil.example/v1/instagram/s/a/2/9999999999/sig2",
        "http://127.0.0.1:8830/internal/v1/ingest",
        "/v1/pixiv/s/a/1",
        "https://scontent.cdninstagram.com/v/t51/x.jpg",
    ] {
        let (ok, out) = call(
            &ctx,
            "save_to_gallery",
            json!({"source": "instagram", "url": bad, "kind": "image", "username": "natgeo"}),
        )
        .await;
        assert!(!ok, "{bad}: {out}");
    }
    // an own signed url that was not in this chat's results needs its kind and username
    let (ok, _) = call(
        &ctx,
        "save_to_gallery",
        json!({"source": "instagram", "url": "/v1/instagram/s/x/9/9999999999/sigx"}),
    )
    .await;
    assert!(!ok);
}

#[tokio::test]
async fn save_pixiv_work_posts_id_and_pages() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path("/v1/pixiv/save"))
        .and(header("x-lucina-gateway", PX_SECRET))
        .and(body_json(json!({"id": 123456, "pages": [0, 2]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "saved": [{"itemId": "itm_a", "duplicate": false}, {"itemId": "itm_b", "duplicate": false}],
            "failed": [],
        })))
        .expect(1)
        .mount(&f.px)
        .await;
    let ctx = f.ctx();
    let (ok, out) = call(
        &ctx,
        "save_to_gallery",
        json!({"source": "pixiv", "id": "123456", "pages": [0, 2]}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(out["saved"], 2);
    assert!(ctx.effects().iter().any(
        |e| matches!(e, Effect::Gallery { path, count } if path == "/gallery" && *count == 2)
    ));
    let (ok, _) = call(
        &ctx,
        "save_to_gallery",
        json!({"source": "pixiv", "id": "abc"}),
    )
    .await;
    assert!(!ok);
    let (ok, _) = call(&ctx, "save_to_gallery", json!({"source": "pixiv"})).await;
    assert!(!ok);
}

#[tokio::test]
async fn an_r18_save_is_refused_by_the_service_and_said_plainly() {
    let f = Fixture::new().await;
    Mock::given(method("POST"))
        .and(path("/v1/pixiv/save"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({"error": "adult_off", "message": "adult content is off"})),
        )
        .mount(&f.px)
        .await;
    let (ok, out) = call(
        &f.ctx(),
        "save_to_gallery",
        json!({"source": "pixiv", "id": "777"}),
    )
    .await;
    assert!(!ok);
    assert!(out["error"]
        .as_str()
        .unwrap()
        .contains("adult content is switched off"));
}

#[tokio::test]
async fn rate_limit_and_missing_things_answer_cleanly() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/v1/pixiv/ranking"))
        .respond_with(
            ResponseTemplate::new(429)
                .set_body_json(json!({"error": "rate_limited", "retryAfter": 12})),
        )
        .mount(&f.px)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/instagram/profile/ghost"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "not_found"})))
        .mount(&f.ig)
        .await;
    let ctx = f.ctx();
    let (ok, out) = call(&ctx, "pixiv_ranking", json!({})).await;
    assert!(!ok);
    assert!(out["error"].as_str().unwrap().contains("12 s"), "{out}");
    let (ok, out) = call(&ctx, "instagram_profile", json!({"username": "ghost"})).await;
    assert!(!ok);
    assert_eq!(out["error"], "Instagram: not found");
}

#[tokio::test]
async fn a_missing_secret_or_a_service_that_is_down_is_not_available() {
    let f = Fixture::new().await;
    // no request may be made without the secret
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(0)
        .mount(&f.ig)
        .await;
    let mut env = f.env();
    env.plugins.instagram.secret_file = f.dir.path().join("nope.secret");
    let ctx = Ctx::new(Arc::new(env), "web", "s");
    let (ok, out) = call(&ctx, "instagram_profile", json!({"username": "natgeo"})).await;
    assert!(!ok);
    let e = out["error"].as_str().unwrap();
    assert!(e.starts_with("Instagram is not available"), "{e}");
    assert!(
        !e.contains(f.dir.path().to_str().unwrap()),
        "no path leak: {e}"
    );
    // an empty secret file is unusable too
    std::fs::write(f.dir.path().join("empty.secret"), "\n").unwrap();
    let mut env = f.env();
    env.plugins.pixiv.secret_file = f.dir.path().join("empty.secret");
    let ctx = Ctx::new(Arc::new(env), "web", "s");
    let (ok, out) = call(&ctx, "pixiv_search", json!({"query": "x"})).await;
    assert!(
        !ok && out["error"]
            .as_str()
            .unwrap()
            .starts_with("Pixiv is not available")
    );
    // nothing listens
    let mut env = f.env();
    env.plugins.pixiv.url = "http://127.0.0.1:1".into();
    let ctx = Ctx::new(Arc::new(env), "web", "s");
    let (ok, out) = call(&ctx, "pixiv_ranking", json!({})).await;
    assert!(!ok);
    assert_eq!(
        out["error"],
        "Pixiv is not available: the service is not running on this host"
    );
}

#[tokio::test]
async fn owner_only_never_on_mcp_and_refused_on_an_mcp_context() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(0)
        .mount(&f.ig)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(0)
        .mount(&f.px)
        .await;
    let all = registry();
    let trusted = all.without(OWNER_ONLY_TOOLS);
    let guarded = all.without(NO_MCP_TOOLS);
    for n in lucina_tools::tools::plugins::NAMES {
        assert!(all.get(n).is_some(), "{n} is registered for the owner");
        assert!(trusted.get(n).is_none(), "{n} absent on a trusted MCP turn");
        assert!(guarded.get(n).is_none(), "{n} absent on a guarded MCP turn");
    }
    // belt and braces: even if one were registered, an MCP-origin call refuses
    let ctx = Ctx::new(Arc::new(f.env()), "web", "s").with_origin(Origin::Mcp);
    for (name, args) in [
        ("instagram_profile", json!({"username": "natgeo"})),
        ("instagram_stories", json!({"username": "natgeo"})),
        ("pixiv_search", json!({"query": "x"})),
        ("pixiv_ranking", json!({})),
        ("save_to_gallery", json!({"source": "pixiv", "id": "1"})),
    ] {
        let (ok, out) = call(&ctx, name, args).await;
        assert!(!ok, "{name}: {out}");
        assert!(
            out["error"].as_str().unwrap().contains("owner's own chat"),
            "{name}: {out}"
        );
    }
}

// ---------------------------------------------------------------- watchlist

#[tokio::test]
async fn the_watchlist_is_listed_watched_unwatched_and_checked_for_new_items() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/v1/instagram/watch"))
        .and(header("x-lucina-gateway", IG_SECRET))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"username": "natgeo", "autoSave": false, "lastCheck": "2026-10-28T08:00:00Z",
             "newStories": 2, "newPosts": 0, "private": false},
        ])))
        .expect(1)
        .mount(&f.ig)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/instagram/watch/natgeo"))
        .and(header("x-lucina-gateway", IG_SECRET))
        .and(body_json(json!({"autoSave": true})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"username": "natgeo", "autoSave": true})),
        )
        .expect(1)
        .mount(&f.ig)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/instagram/watch/other"))
        .and(body_json(json!({})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"username": "other"})))
        .expect(1)
        .mount(&f.ig)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/instagram/watch/natgeo"))
        .and(header("x-lucina-gateway", IG_SECRET))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&f.ig)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/instagram/watch/natgeo/new"))
        .and(header("x-lucina-gateway", IG_SECRET))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "stories": [{"id": "s1", "kind": "image", "url": "/v1/instagram/s/a/s1/9999999999/sig",
                         "thumbUrl": "/v1/instagram/s/a/s1t/9999999999/sigt", "takenAt": "2026-10-28T07:00:00Z"}],
            "posts": [],
        })))
        .expect(1)
        .mount(&f.ig)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/instagram/save"))
        .and(body_json(json!({"items": [
            {"url": "/v1/instagram/s/a/s1/9999999999/sig", "kind": "image", "username": "natgeo",
             "takenAt": "2026-10-28T07:00:00Z"},
        ]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "saved": [{"itemId": "itm_1", "duplicate": false}], "failed": [],
        })))
        .expect(1)
        .mount(&f.ig)
        .await;
    let ctx = f.ctx();
    let (ok, out) = call(&ctx, "instagram_watchlist", json!({})).await;
    assert!(ok, "{out}");
    assert_eq!(out["watching"][0]["username"], "natgeo");
    assert_eq!(out["watching"][0]["newStories"], 2);
    let (ok, out) = call(
        &ctx,
        "instagram_watch",
        json!({"username": "@natgeo", "autoSave": true}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(out["watching"], true);
    assert!(
        call(&ctx, "instagram_watch", json!({"username": "other"}))
            .await
            .0
    );
    let (ok, out) = call(&ctx, "instagram_new", json!({"username": "natgeo"})).await;
    assert!(ok, "{out}");
    assert_eq!(
        out["stories"][0]["url"],
        "https://getsecretary.tech/v1/instagram/s/a/s1/9999999999/sig"
    );
    // what is new can be saved in one go
    let (ok, out) = call(
        &ctx,
        "save_to_gallery",
        json!({"source": "instagram", "all": true}),
    )
    .await;
    assert!(ok, "{out}");
    assert_eq!(out["saved"], 1);
    let (ok, out) = call(&ctx, "instagram_unwatch", json!({"username": "natgeo"})).await;
    assert!(ok, "{out}");
    assert_eq!(out["watching"], false);
    // all of them mark the turn private and none puts a name in the timeline line
    assert!(ctx
        .effects()
        .iter()
        .any(|e| matches!(e, Effect::PrivateTurn)));
    let all = registry();
    for n in [
        "instagram_watchlist",
        "instagram_watch",
        "instagram_unwatch",
        "instagram_new",
    ] {
        let summary = all
            .get(n)
            .unwrap()
            .summarize(&json!({"username": "natgeo"}));
        assert_eq!(summary, n);
    }
}

#[tokio::test]
async fn watch_tools_reject_bad_usernames_without_a_request() {
    let f = Fixture::new().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(0)
        .mount(&f.ig)
        .await;
    let ctx = f.ctx();
    for n in [
        "instagram_watch",
        "instagram_unwatch",
        "instagram_new",
        "instagram_save_latest",
    ] {
        for bad in ["../x", "a/b", "x y", ""] {
            let (ok, out) = call(&ctx, n, json!({"username": bad})).await;
            assert!(!ok, "{n} {bad}: {out}");
        }
    }
}

#[tokio::test]
async fn save_latest_post_takes_the_newest_post_not_a_highlight() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/v1/instagram/profile/natgeo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "profile": {"username": "natgeo"},
            "highlights": [{"id": "h1", "items": [
                {"id": "hi", "kind": "image", "url": "/v1/instagram/s/a/hi/9999999999/sigh", "takenAt": "2026-10-30T00:00:00Z"}]}],
            "posts": {"items": [
                {"id": "old", "kind": "image", "url": "/v1/instagram/s/a/old/9999999999/sigo", "takenAt": "2026-10-01T10:00:00Z"},
                {"id": "new", "kind": "video", "url": "/v1/instagram/s/a/new/9999999999/sign", "takenAt": "2026-10-27T10:00:00Z", "caption": "latest"},
            ]},
        })))
        .expect(1)
        .mount(&f.ig)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/instagram/save"))
        .and(header("x-lucina-gateway", IG_SECRET))
        .and(body_json(json!({"items": [
            {"url": "/v1/instagram/s/a/new/9999999999/sign", "kind": "video", "username": "natgeo",
             "takenAt": "2026-10-27T10:00:00Z", "caption": "latest"},
        ]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "saved": [{"itemId": "itm_7", "duplicate": true}], "failed": [],
        })))
        .expect(1)
        .mount(&f.ig)
        .await;
    let ctx = f.ctx();
    let (ok, out) = call(&ctx, "instagram_save_latest", json!({"username": "natgeo"})).await;
    assert!(ok, "{out}");
    assert_eq!(
        (out["saved"].as_u64(), out["alreadyThere"].as_u64()),
        (Some(0), Some(1))
    );
    assert_eq!(out["kind"], "video");
}

#[tokio::test]
async fn save_latest_story_uses_stories_and_says_when_there_are_none() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/v1/instagram/stories/natgeo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .expect(1)
        .mount(&f.ig)
        .await;
    let ctx = f.ctx();
    let (ok, out) = call(
        &ctx,
        "instagram_save_latest",
        json!({"username": "natgeo", "kind": "story"}),
    )
    .await;
    assert!(!ok);
    assert!(
        out["error"].as_str().unwrap().contains("no stories"),
        "{out}"
    );
    let (ok, _) = call(
        &ctx,
        "instagram_save_latest",
        json!({"username": "natgeo", "kind": "reel"}),
    )
    .await;
    assert!(!ok);
}

#[tokio::test]
async fn the_watch_tools_are_owner_only_too() {
    let f = Fixture::new().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(0)
        .mount(&f.ig)
        .await;
    let all = registry();
    for n in [
        "instagram_watchlist",
        "instagram_watch",
        "instagram_unwatch",
        "instagram_new",
        "instagram_save_latest",
    ] {
        assert!(all.get(n).is_some(), "{n}");
        assert!(all.without(OWNER_ONLY_TOOLS).get(n).is_none(), "{n}");
        assert!(all.without(NO_MCP_TOOLS).get(n).is_none(), "{n}");
        assert!(lucina_tools::tools::plugins::NAMES.contains(&n), "{n}");
    }
    let ctx = Ctx::new(Arc::new(f.env()), "web", "s").with_origin(Origin::Mcp);
    for (n, a) in [
        ("instagram_watchlist", json!({})),
        ("instagram_watch", json!({"username": "natgeo"})),
        ("instagram_unwatch", json!({"username": "natgeo"})),
        ("instagram_new", json!({"username": "natgeo"})),
        ("instagram_save_latest", json!({"username": "natgeo"})),
    ] {
        let (ok, out) = call(&ctx, n, a).await;
        assert!(
            !ok && out["error"].as_str().unwrap().contains("owner's own chat"),
            "{n}: {out}"
        );
    }
}
