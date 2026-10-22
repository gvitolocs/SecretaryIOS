//! The `avatar_outfit` tool against a mocked secretary-server.

use std::sync::Arc;
use std::time::Duration;

use lucina_tools::agent;
use lucina_tools::{Ctx, Effect, Registry, ToolEnv, ToolSelection};
use serde_json::{json, Value};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "svc_test_service_token_0123456789abcdef";

fn ctx(dir: &std::path::Path, server: Option<&MockServer>) -> Ctx {
    let mut env = ToolEnv::new(dir, "Europe/Rome".parse().unwrap());
    if let Some(s) = server {
        let file = dir.join("service.token");
        std::fs::write(&file, format!("{TOKEN}\n")).unwrap();
        env.secretary_url = Some(format!("{}/", s.uri()));
        env.secretary_service_token_file = Some(file);
    }
    Ctx::new(Arc::new(env), "web", "app")
}

async fn call(ctx: &Ctx, args: Value) -> (bool, Value) {
    let registry = Registry::builtin(&ToolSelection::parse("avatar_outfit"));
    let (ok, out, _) = agent::execute(
        &registry,
        "avatar_outfit",
        args,
        ctx,
        Duration::from_secs(10),
    )
    .await;
    (ok, out)
}

fn outfits(ctx: &Ctx) -> Vec<Option<String>> {
    ctx.effects()
        .into_iter()
        .filter_map(|e| match e {
            Effect::AvatarOutfit { outfit } => Some(outfit),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn puts_an_outfit_on_with_the_service_token() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/avatar"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .and(body_json(json!({"outfit": "maid"})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"outfit": "maid", "auto": true})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/avatar"))
        .and(body_json(json!({"outfit": null, "auto": true})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"outfit": null, "auto": true})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(&server));
    let (ok, out) = call(&c, json!({"outfit": "Maid"})).await;
    assert!(ok, "{out}");
    assert_eq!(out["outfit"], "maid");
    let (ok, out) = call(&c, json!({"outfit": "auto"})).await;
    assert!(ok, "{out}");
    assert_eq!(out["auto"], true);
    assert_eq!(outfits(&c), vec![Some("maid".to_owned()), None]);
}

#[tokio::test]
async fn unknown_outfits_and_bad_ids_are_errors_for_the_model() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/avatar"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({"error": {
            "code": "unknown_outfit", "message": "no outfit \"pirate\" in the wardrobe; known: uniform, gym, maid"}})))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(&server));
    let (ok, out) = call(&c, json!({"outfit": "pirate"})).await;
    assert!(!ok);
    assert!(
        out.to_string().contains("known: uniform, gym, maid"),
        "{out}"
    );
    let (ok, out) = call(&c, json!({"outfit": "../etc"})).await;
    assert!(!ok, "{out}");
    assert!(outfits(&c).is_empty(), "no effect when nothing changed");
}

#[tokio::test]
async fn lists_the_wardrobe_without_an_outfit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/avatar/wardrobe"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "uniform", "name": {"it": "Divisa"}, "glb": "lucina.glb", "tags": ["uniform"]},
            {"id": "gym", "name": {"en": "Gym"}, "glb": "lucina-gym.glb"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/avatar"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"outfit": null, "auto": true,
            "rules": {"gym": "gym", "shopping": "chef", "default": "uniform"}})),
        )
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), Some(&server));
    let (ok, out) = call(&c, json!({})).await;
    assert!(ok, "{out}");
    assert_eq!(out["outfits"][1]["id"], "gym");
    assert!(
        out["outfits"][0].get("glb").is_none(),
        "asset names are not the model's business"
    );
    assert_eq!(out["current"]["rules"]["default"], "uniform");
    assert!(outfits(&c).is_empty());
}

#[tokio::test]
async fn says_so_when_not_configured() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path(), None);
    let (ok, out) = call(&c, json!({"outfit": "gym"})).await;
    assert!(!ok);
    assert!(
        out.to_string().contains("LUCINA_SECRETARY_SERVER_URL"),
        "{out}"
    );
}
