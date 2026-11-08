//! The store against a mock Infisical: Universal Auth login, renewal,
//! re-login on 401, reads (v4 and v3), names without values, cache, policy
//! and audit. Fake values only.

use std::time::Duration;

use lucina_secrets::infisical::Credentials;
use lucina_secrets::{redactor, SecretError, SecretRef, SecretStore, SecretsConfig};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CLIENT_ID: &str = "fake-client-id-0001";
const CLIENT_SECRET: &str = "fake-client-secret-c0ffee-0002";
const TOKEN_1: &str = "fake.access.token-one-AAAA1111";
const TOKEN_2: &str = "fake.access.token-two-BBBB2222";
const VALUE: &str = "Wm9vLXRlc3QtdmFsdWUtNDI1Ng";

fn config(uri: &str, api: &str, ttl: u64, audit: &std::path::Path) -> SecretsConfig {
    SecretsConfig::from_toml(&format!(
        r#"
[infisical]
url = "{uri}"
api = "{api}"

[projects]
lucina = "proj-0001"

[scope]
project = "lucina"
environment = "prod"
path = "/lucina"

[cache]
ttl_secs = {ttl}

[audit]
path = "{}"

[refs]
"weather.api_key" = "secret://lucina/prod/lucina#OPEN_METEO_API_KEY"
"todoist.token" = "secret://lucina/prod/lucina#TODOIST_TOKEN"
"#,
        audit.display()
    ))
    .unwrap()
}

fn creds() -> Credentials {
    Credentials::new(CLIENT_ID.into(), CLIENT_SECRET.into())
}

async fn mount_login(s: &MockServer, token: &str, expires: u64) {
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/universal-auth/login"))
        .and(body_json(json!({"clientId": CLIENT_ID, "clientSecret": CLIENT_SECRET})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "accessToken": token, "expiresIn": expires, "accessTokenMaxTTL": 7200, "tokenType": "Bearer"
        })))
        .mount(s)
        .await;
}

fn secret_body(key: &str, value: &str) -> serde_json::Value {
    json!({"secret": {
        "id": "s1", "_id": "s1", "workspace": "proj-0001", "environment": "prod", "version": 3,
        "type": "shared", "secretKey": key, "secretValue": value, "secretComment": "",
        "createdAt": "2026-10-01T08:00:00.000Z", "updatedAt": "2026-10-20T09:00:00.000Z",
        "secretValueHidden": false, "secretPath": "/lucina"
    }})
}

async fn mount_secret(s: &MockServer, token: &str, key: &str, value: &str, expect: u64) {
    Mock::given(method("GET"))
        .and(path(format!("/api/v4/secrets/{key}")))
        .and(query_param("projectId", "proj-0001"))
        .and(query_param("environment", "prod"))
        .and(query_param("secretPath", "/lucina"))
        .and(header("authorization", format!("Bearer {token}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(secret_body(key, value)))
        .expect(expect)
        .mount(s)
        .await;
}

fn r(key: &str) -> SecretRef {
    SecretRef::parse(&format!("secret://lucina/prod/lucina#{key}")).unwrap()
}

#[tokio::test]
async fn login_read_cache_and_audit() {
    let s = MockServer::start().await;
    mount_login(&s, TOKEN_1, 3600).await;
    mount_secret(&s, TOKEN_1, "OPEN_METEO_API_KEY", VALUE, 1).await;
    let dir = tempfile::tempdir().unwrap();
    let audit = dir.path().join("audit.jsonl");
    let store =
        SecretStore::with_credentials(config(&s.uri(), "v4", 600, &audit), creds()).unwrap();

    let v = store
        .resolve(&r("OPEN_METEO_API_KEY"), "weather")
        .await
        .unwrap();
    assert_eq!(v.expose_secret(), VALUE);
    assert_eq!(format!("{v:?}"), "SecretValue([redacted])");
    // second resolve: from the cache (the mock expects exactly one GET)
    let again = store
        .resolve_setting("weather.api_key", "weather")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.expose_secret(), VALUE);
    // the redactor learned it
    assert_eq!(
        redactor().redact(&format!("echo {VALUE}")),
        "echo [secret:OPEN_METEO_API_KEY]"
    );
    // the identity and the token are known too
    assert!(!redactor().redact(CLIENT_SECRET).contains(CLIENT_SECRET));
    assert!(!redactor().redact(TOKEN_1).contains(TOKEN_1));

    let entries = store.audit_entries(10, None, None);
    assert_eq!(entries.len(), 2);
    assert!(entries[0].ok && entries[0].cached);
    assert!(entries[1].ok && !entries[1].cached);
    assert_eq!(entries[1].tool, "weather");
    let file = std::fs::read_to_string(&audit).unwrap();
    assert_eq!(file.lines().count(), 2);
    for secret in [VALUE, CLIENT_SECRET, CLIENT_ID, TOKEN_1] {
        assert!(!file.contains(secret));
    }
    // no setting -> None, no call
    assert!(store
        .resolve_setting("github.token", "github")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn ttl_expiry_refetches() {
    let s = MockServer::start().await;
    mount_login(&s, TOKEN_1, 3600).await;
    mount_secret(&s, TOKEN_1, "TODOIST_TOKEN", "todoist-fake-token-abcdef", 2).await;
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::with_credentials(
        config(&s.uri(), "v4", 1, &dir.path().join("a.jsonl")),
        creds(),
    )
    .unwrap();
    store.resolve(&r("TODOIST_TOKEN"), "todoist").await.unwrap();
    store.resolve(&r("TODOIST_TOKEN"), "todoist").await.unwrap(); // cached
    tokio::time::sleep(Duration::from_millis(1100)).await;
    store.resolve(&r("TODOIST_TOKEN"), "todoist").await.unwrap(); // expired -> GET
    store.forget_cached();
}

#[tokio::test]
async fn relogin_on_401_and_renewal() {
    let s = MockServer::start().await;
    // the first login hands out a token that is already due for renewal (1 s)
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/universal-auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "accessToken": TOKEN_1, "expiresIn": 1, "accessTokenMaxTTL": 7200, "tokenType": "Bearer"
        })))
        .up_to_n_times(1)
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/token/renew"))
        .and(body_json(json!({"accessToken": TOKEN_1})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "accessToken": TOKEN_2, "expiresIn": 3600, "accessTokenMaxTTL": 7200, "tokenType": "Bearer"
        })))
        .expect(1)
        .mount(&s)
        .await;
    mount_secret(&s, TOKEN_1, "TODOIST_TOKEN", "todoist-fake-token-abcdef", 1).await;
    mount_secret(&s, TOKEN_2, "OPEN_METEO_API_KEY", VALUE, 1).await;
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::with_credentials(
        config(&s.uri(), "v4", 600, &dir.path().join("a.jsonl")),
        creds(),
    )
    .unwrap();
    store.resolve(&r("TODOIST_TOKEN"), "todoist").await.unwrap();
    // 0.8 s later the token is in its renewal window
    tokio::time::sleep(Duration::from_millis(850)).await;
    store
        .resolve(&r("OPEN_METEO_API_KEY"), "weather")
        .await
        .unwrap();

    // A server that rejects the cached token: one fresh login, then success.
    let s2 = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/universal-auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "accessToken": TOKEN_1, "expiresIn": 3600, "accessTokenMaxTTL": 7200, "tokenType": "Bearer"
        })))
        .expect(2)
        .mount(&s2)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v4/secrets/TODOIST_TOKEN"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"statusCode": 401, "message": "Token expired"})),
        )
        .up_to_n_times(1)
        .mount(&s2)
        .await;
    mount_secret(
        &s2,
        TOKEN_1,
        "TODOIST_TOKEN",
        "todoist-fake-token-abcdef",
        1,
    )
    .await;
    let store2 = SecretStore::with_credentials(
        config(&s2.uri(), "v4", 600, &dir.path().join("b.jsonl")),
        creds(),
    )
    .unwrap();
    store2
        .resolve(&r("TODOIST_TOKEN"), "todoist")
        .await
        .unwrap();
}

#[tokio::test]
async fn policy_refusals_make_no_call_and_are_audited() {
    let s = MockServer::start().await;
    // any request fails the test
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&s)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::with_credentials(
        config(&s.uri(), "v4", 600, &dir.path().join("a.jsonl")),
        creds(),
    )
    .unwrap();
    for (raw, want) in [
        ("secret://lucina/prod/lucina#SUDO_PASSWORD", "denied"),
        ("secret://lucina/prod/lucina#NEZ_LOGIN_PASSWORD", "denied"),
        ("secret://lucina/prod/lucina#ROOT_PW", "denied"),
        ("secret://lucina/prod/lucina#GMAIL_PASSWORD", "denied"),
        ("secret://lucina/prod/owner#GITHUB_TOKEN", "out_of_scope"),
        ("secret://lucina/dev/lucina#GITHUB_TOKEN", "out_of_scope"),
    ] {
        let err = store
            .resolve(&SecretRef::parse(raw).unwrap(), "test")
            .await
            .unwrap_err();
        assert_eq!(err.code(), want, "{raw}: {err}");
    }
    let entries = store.audit_entries(50, None, None);
    assert_eq!(entries.len(), 6);
    assert!(entries.iter().all(|e| !e.ok));
}

#[tokio::test]
async fn not_found_short_values_and_errors() {
    let s = MockServer::start().await;
    mount_login(&s, TOKEN_1, 3600).await;
    Mock::given(method("GET"))
        .and(path("/api/v4/secrets/MISSING_KEY"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"statusCode": 404, "message": "Secret not found"})),
        )
        .mount(&s)
        .await;
    mount_secret(&s, TOKEN_1, "SHORT_KEY", "abc1234", 1).await;
    Mock::given(method("GET"))
        .and(path("/api/v4/secrets/BROKEN_KEY"))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_json(json!({"message": format!("boom near {VALUE}")})),
        )
        .mount(&s)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::with_credentials(
        config(&s.uri(), "v4", 600, &dir.path().join("a.jsonl")),
        creds(),
    )
    .unwrap();
    redactor().register("OPEN_METEO_API_KEY", VALUE);
    assert!(matches!(
        store.resolve(&r("MISSING_KEY"), "t").await,
        Err(SecretError::NotFound(_))
    ));
    assert!(matches!(
        store.resolve(&r("SHORT_KEY"), "t").await,
        Err(SecretError::TooShort(..))
    ));
    let err = store.resolve(&r("BROKEN_KEY"), "t").await.unwrap_err();
    let text = err.to_string();
    assert!(text.contains("HTTP 500"), "{text}");
    assert!(!text.contains(VALUE), "{text}");
}

#[tokio::test]
async fn status_lists_names_without_values_and_preload() {
    let s = MockServer::start().await;
    mount_login(&s, TOKEN_1, 3600).await;
    let listed = |with_values: bool| {
        json!({"secrets": [
            {"id": "a", "_id": "a", "workspace": "proj-0001", "environment": "prod", "version": 3, "type": "shared",
             "secretKey": "OPEN_METEO_API_KEY", "secretValue": if with_values { VALUE } else { "<hidden-by-infisical>" },
             "secretComment": "", "createdAt": "2026-10-01T08:00:00Z", "updatedAt": "2026-10-20T09:00:00Z",
             "secretValueHidden": !with_values, "secretPath": "/lucina"},
            {"id": "b", "_id": "b", "workspace": "proj-0001", "environment": "prod", "version": 1, "type": "shared",
             "secretKey": "GITHUB_TOKEN", "secretValue": if with_values { "ghp_fakeFAKEfake0123456789abcdEFGH" } else { "<hidden-by-infisical>" },
             "secretComment": "", "createdAt": "2026-10-05T08:00:00Z", "updatedAt": "2026-10-05T08:00:00Z",
             "secretValueHidden": !with_values, "secretPath": "/lucina/github"}
        ]})
    };
    Mock::given(method("GET"))
        .and(path("/api/v4/secrets"))
        .and(query_param("viewSecretValue", "false"))
        .and(query_param("recursive", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(listed(false)))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v4/secrets"))
        .and(query_param("viewSecretValue", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(listed(true)))
        .mount(&s)
        .await;
    mount_secret(&s, TOKEN_1, "OPEN_METEO_API_KEY", VALUE, 1).await;
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::with_credentials(
        config(&s.uri(), "v4", 600, &dir.path().join("a.jsonl")),
        creds(),
    )
    .unwrap();
    store
        .resolve(&r("OPEN_METEO_API_KEY"), "weather")
        .await
        .unwrap();
    let st = store.status().await.unwrap();
    let text = serde_json::to_string(&st).unwrap();
    assert!(!text.contains(VALUE), "{text}");
    assert!(!text.contains("hidden-by-infisical"), "{text}");
    let weather = st.iter().find(|x| x.name == "OPEN_METEO_API_KEY").unwrap();
    assert!(weather.set);
    assert_eq!(weather.used_by, vec!["weather.api_key".to_string()]);
    assert_eq!(weather.last_used_by.as_deref(), Some("weather"));
    assert_eq!(
        weather.last_rotated.as_deref(),
        Some("2026-10-20T09:00:00Z")
    );
    let gh = st.iter().find(|x| x.name == "GITHUB_TOKEN").unwrap();
    assert_eq!(
        gh.reference,
        "secret://lucina/prod/lucina/github#GITHUB_TOKEN"
    );
    let todo = st.iter().find(|x| x.name == "TODOIST_TOKEN").unwrap();
    assert!(!todo.set, "configured but not in Infisical");

    assert_eq!(store.preload_redaction().await.unwrap(), 2);
    let out = redactor().redact("leaked ghp_fakeFAKEfake0123456789abcdEFGH here");
    assert_eq!(out, "leaked [secret:GITHUB_TOKEN] here");
}

#[tokio::test]
async fn v3_raw_endpoints() {
    let s = MockServer::start().await;
    mount_login(&s, TOKEN_1, 3600).await;
    Mock::given(method("GET"))
        .and(path("/api/v3/secrets/raw/OPEN_METEO_API_KEY"))
        .and(query_param("workspaceId", "proj-0001"))
        .and(query_param("environment", "prod"))
        .and(query_param("secretPath", "/lucina"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"secret": {
            "_id": "a", "version": 2, "workspace": "proj-0001", "type": "shared", "environment": "prod",
            "secretKey": "OPEN_METEO_API_KEY", "secretValue": VALUE, "secretComment": ""
        }})))
        .expect(1)
        .mount(&s)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::with_credentials(
        config(&s.uri(), "v3", 600, &dir.path().join("a.jsonl")),
        creds(),
    )
    .unwrap();
    let v = store
        .resolve(&r("OPEN_METEO_API_KEY"), "weather")
        .await
        .unwrap();
    assert_eq!(v.expose_secret(), VALUE);
}

#[test]
fn credentials_from_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("lucina-infisical-client-id"),
        "file-client-id-123\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("lucina-infisical-client-secret"),
        "file-client-secret-456\n",
    )
    .unwrap();
    let c = Credentials::from_dir(
        dir.path(),
        "lucina-infisical-client-id",
        "lucina-infisical-client-secret",
    )
    .unwrap();
    assert_eq!(format!("{c:?}"), "Credentials([redacted])");
    assert!(!redactor()
        .redact("x file-client-secret-456 y")
        .contains("file-client-secret-456"));
    assert!(Credentials::from_dir(dir.path(), "nope", "lucina-infisical-client-secret").is_err());
}
