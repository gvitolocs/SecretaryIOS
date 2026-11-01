//! The OAuth 2.1 flow ChatGPT uses: discovery, registration (DCR and CIMD),
//! login + consent, PKCE, tokens, refresh rotation.

mod common;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use common::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const REDIRECT: &str = "https://chatgpt.com/connector_platform_oauth_redirect";
const RESOURCE: &str = "https://getsecretary.tech/mcp";
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk-extra-entropy";

fn challenge(v: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(v.as_bytes()))
}

async fn register(h: &Harness, body: Value) -> (u16, Value) {
    let r = h
        .http
        .post(format!("{}/oauth/register", h.base))
        .json(&body)
        .send()
        .await
        .unwrap();
    (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
}

async fn public_client(h: &Harness) -> String {
    let (s, v) = register(
        h,
        json!({"client_name": "ChatGPT", "redirect_uris": [REDIRECT], "token_endpoint_auth_method": "none",
               "grant_types": ["authorization_code", "refresh_token"], "response_types": ["code"]}),
    )
    .await;
    assert_eq!(s, 201, "{v}");
    assert!(v.get("client_secret").is_none());
    v["client_id"].as_str().unwrap().to_owned()
}

/// GET /oauth/authorize; returns (status, body, location).
async fn authorize(h: &Harness, params: &[(&str, &str)]) -> (u16, String, Option<String>) {
    let r = h
        .http
        .get(format!("{}/oauth/authorize", h.base))
        .query(params)
        .send()
        .await
        .unwrap();
    let loc = r
        .headers()
        .get("location")
        .map(|v| v.to_str().unwrap().to_owned());
    (r.status().as_u16(), r.text().await.unwrap(), loc)
}

fn request_id(html: &str) -> String {
    let i = html
        .find("name=\"request_id\" value=\"")
        .expect("request_id in page")
        + 25;
    html[i..].split('"').next().unwrap().to_owned()
}

async fn submit(h: &Harness, form: &[(&str, &str)]) -> (u16, String, Option<String>) {
    let r = h
        .http
        .post(format!("{}/oauth/authorize", h.base))
        .form(form)
        .send()
        .await
        .unwrap();
    let loc = r
        .headers()
        .get("location")
        .map(|v| v.to_str().unwrap().to_owned());
    (r.status().as_u16(), r.text().await.unwrap(), loc)
}

async fn token(h: &Harness, form: &[(&str, &str)]) -> (u16, Value) {
    let r = h
        .http
        .post(format!("{}/oauth/token", h.base))
        .form(form)
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["cache-control"], "no-store");
    (r.status().as_u16(), r.json().await.unwrap())
}

/// Register-free part of the flow: authorize, log in, get a code.
async fn get_code(h: &Harness, client_id: &str, scope: &str, uncheck_write: bool) -> String {
    let ch = challenge(VERIFIER);
    let (s, html, _) = authorize(
        h,
        &[
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", REDIRECT),
            ("state", "st-123"),
            ("scope", scope),
            ("code_challenge", &ch),
            ("code_challenge_method", "S256"),
            ("resource", RESOURCE),
        ],
    )
    .await;
    assert_eq!(s, 200, "{html}");
    assert!(html.contains("Connect ChatGPT to Lucina"), "{html}");
    let rid = request_id(&html);
    let mut form = vec![
        ("request_id", rid.as_str()),
        ("email", OWNER_EMAIL),
        ("password", OWNER_PASSWORD),
        ("decision", "approve"),
        ("scope_read", "1"),
        ("scope_chat", "1"),
    ];
    if !uncheck_write {
        form.push(("scope_write", "1"));
    }
    let (s, body, loc) = submit(h, &form).await;
    assert_eq!(s, 303, "{body}");
    let loc = loc.unwrap();
    assert!(loc.starts_with(REDIRECT), "{loc}");
    assert_eq!(query_param(&loc, "state").as_deref(), Some("st-123"));
    assert_eq!(
        query_param(&loc, "iss").as_deref(),
        Some("https://getsecretary.tech")
    );
    query_param(&loc, "code").unwrap()
}

#[tokio::test]
async fn discovery_metadata() {
    let h = start().await;
    for path in [
        "/.well-known/oauth-protected-resource/mcp",
        "/.well-known/oauth-protected-resource",
    ] {
        let v: Value = h
            .http
            .get(format!("{}{path}", h.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(v["resource"], RESOURCE);
        assert_eq!(
            v["authorization_servers"],
            json!(["https://getsecretary.tech"])
        );
        assert_eq!(
            v["scopes_supported"],
            json!(["read", "chat", "write", "computer", "agents", "notebooks"])
        );
    }
    let v: Value = h
        .http
        .get(format!("{}/.well-known/oauth-authorization-server", h.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["issuer"], "https://getsecretary.tech");
    assert_eq!(
        v["authorization_endpoint"],
        "https://getsecretary.tech/oauth/authorize"
    );
    assert_eq!(v["token_endpoint"], "https://getsecretary.tech/oauth/token");
    assert_eq!(
        v["registration_endpoint"],
        "https://getsecretary.tech/oauth/register"
    );
    assert_eq!(v["code_challenge_methods_supported"], json!(["S256"]));
    assert_eq!(v["authorization_response_iss_parameter_supported"], true);
    assert_eq!(v["client_id_metadata_document_supported"], true);
    assert!(v["token_endpoint_auth_methods_supported"]
        .as_array()
        .unwrap()
        .contains(&json!("none")));
}

#[tokio::test]
async fn full_flow_register_login_token_call_refresh() {
    let h = start().await;
    let client_id = public_client(&h).await;

    // a wrong password re-renders the form and keeps the request
    let ch = challenge(VERIFIER);
    let (_, html, _) = authorize(
        &h,
        &[
            ("response_type", "code"),
            ("client_id", &client_id),
            ("redirect_uri", REDIRECT),
            ("state", "s1"),
            ("code_challenge", &ch),
            ("code_challenge_method", "S256"),
        ],
    )
    .await;
    let rid = request_id(&html);
    let (s, body, loc) = submit(
        &h,
        &[
            ("request_id", &rid),
            ("email", OWNER_EMAIL),
            ("password", "nope"),
            ("decision", "approve"),
            ("scope_read", "1"),
        ],
    )
    .await;
    assert_eq!(s, 400);
    assert!(loc.is_none());
    assert!(body.contains("Wrong email or password"), "{body}");
    let logins = h.mock.requests("/v1/session");
    assert_eq!(logins.len(), 1);
    assert_eq!(logins[0].body["email"], OWNER_EMAIL);
    assert!(logins[0].headers.get("x-forwarded-for").is_some());

    // the real login: secretary-server checks it, the session is ended at once
    let code = get_code(&h, &client_id, "read chat write", false).await;
    let sessions = h.mock.requests("/v1/session");
    let del: Vec<_> = sessions.iter().filter(|r| r.method == "DELETE").collect();
    assert_eq!(del.len(), 1);
    assert_eq!(del[0].headers["cookie"], "sec_session=sess123");
    assert_eq!(del[0].headers["x-secretary-csrf"], "1");

    let (s, tok) = token(
        &h,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", REDIRECT),
            ("client_id", &client_id),
            ("code_verifier", VERIFIER),
            ("resource", RESOURCE),
        ],
    )
    .await;
    assert_eq!(s, 200, "{tok}");
    assert_eq!(tok["token_type"], "Bearer");
    assert_eq!(tok["expires_in"], 3600);
    assert_eq!(tok["scope"], "read chat write");
    let access = tok["access_token"].as_str().unwrap().to_owned();
    let refresh = tok["refresh_token"].as_str().unwrap().to_owned();
    assert!(access.starts_with("mcpa_") && refresh.starts_with("mcpr_"));

    // call a tool with the access token: Lucina sees "chatgpt"
    assert_eq!(h.tool_names(&access).await.len(), 48);
    let (err, text) = h
        .call(&access, "ask_lucina", json!({"message": "ciao"}))
        .await;
    assert!(!err, "{text}");
    assert!(
        text.contains("[via Lucina MCP, from chatgpt] ciao"),
        "{text}"
    );
    assert_eq!(
        h.mock.requests("/v1/chat")[0].body["sessionId"],
        "mcp-chatgpt"
    );

    // the code is single use; a replay revokes the grant
    let (s, v) = token(
        &h,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &client_id),
            ("code_verifier", VERIFIER),
        ],
    )
    .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("invalid_grant")));
    let (status, _, _) = h.rpc_raw(Some(&access), "tools/list", json!({})).await;
    assert_eq!(status, 401, "code replay must revoke the grant");

    // a fresh grant, then refresh rotation
    let code = get_code(&h, &client_id, "read chat write", false).await;
    let (_, tok) = token(
        &h,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &client_id),
            ("code_verifier", VERIFIER),
        ],
    )
    .await;
    let refresh = tok["refresh_token"].as_str().unwrap().to_owned();
    let (s, tok2) = token(
        &h,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh),
            ("client_id", &client_id),
        ],
    )
    .await;
    assert_eq!(s, 200, "{tok2}");
    let access2 = tok2["access_token"].as_str().unwrap().to_owned();
    let refresh2 = tok2["refresh_token"].as_str().unwrap().to_owned();
    assert_ne!(refresh, refresh2);
    let (err, _) = h.call(&access2, "lucina_status", json!({})).await;
    assert!(!err);
    // narrowing on refresh
    let (s, tok3) = token(
        &h,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh2),
            ("client_id", &client_id),
            ("scope", "read"),
        ],
    )
    .await;
    assert_eq!(s, 200, "{tok3}");
    assert_eq!(tok3["scope"], "read");
    let access3 = tok3["access_token"].as_str().unwrap().to_owned();
    assert_eq!(h.tool_names(&access3).await.len(), 28);
    // replaying an old refresh token revokes the whole grant
    let (s, v) = token(
        &h,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh),
            ("client_id", &client_id),
        ],
    )
    .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("invalid_grant")));
    let (status, _, _) = h.rpc_raw(Some(&access3), "tools/list", json!({})).await;
    assert_eq!(status, 401);
    // a token is bound to its client
    let other = public_client(&h).await;
    let code = get_code(&h, &client_id, "read", false).await;
    let (s, v) = token(
        &h,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &other),
            ("code_verifier", VERIFIER),
        ],
    )
    .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("invalid_grant")));
    // the CLI view of the store
    let data = std::fs::read_to_string(h.data_dir().join("oauth.json")).unwrap();
    assert!(
        !data.contains(&access2[5..]) && !data.contains(&refresh2[5..]),
        "tokens are stored hashed"
    );
}

#[tokio::test]
async fn consent_can_drop_write() {
    let h = start().await;
    let client_id = public_client(&h).await;
    let code = get_code(&h, &client_id, "read chat write", true).await;
    let (_, tok) = token(
        &h,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &client_id),
            ("code_verifier", VERIFIER),
        ],
    )
    .await;
    assert_eq!(tok["scope"], "read chat");
    let access = tok["access_token"].as_str().unwrap();
    let names = h.tool_names(access).await;
    assert!(names.contains(&"ask_lucina".into()) && !names.contains(&"shopping_add".into()));
    let (err, text) = h
        .call(
            access,
            "reminder_create",
            json!({"text": "x", "when": "2099-01-01T00:00:00Z"}),
        )
        .await;
    assert!(err && text.contains("`write` scope"), "{text}");
    // OAuth callers get ChatGPT's tool-level auth challenge
    let r = h
        .rpc(
            access,
            "tools/call",
            json!({"name": "shopping_add", "arguments": {"items": ["x"]}}),
        )
        .await;
    let ch = r["_meta"]["mcp/www_authenticate"][0].as_str().unwrap();
    assert!(
        ch.contains("error=\"insufficient_scope\"") && ch.contains("write"),
        "{ch}"
    );
}

#[tokio::test]
async fn pkce_failure() {
    let h = start().await;
    let client_id = public_client(&h).await;
    let code = get_code(&h, &client_id, "read", false).await;
    let (s, v) = token(
        &h,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &client_id),
            (
                "code_verifier",
                "wrong-verifier-wrong-verifier-wrong-verifier-123",
            ),
        ],
    )
    .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("invalid_grant")));
    assert!(v["error_description"].as_str().unwrap().contains("PKCE"));
    // the code is burnt: the right verifier no longer works
    let (s, _) = token(
        &h,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &client_id),
            ("code_verifier", VERIFIER),
        ],
    )
    .await;
    assert_eq!(s, 400);
    // no verifier at all
    let code = get_code(&h, &client_id, "read", false).await;
    let (s, v) = token(
        &h,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &client_id),
        ],
    )
    .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("invalid_request")));
    // authorize without PKCE, or with plain, is refused (redirected back with an error)
    for extra in [
        vec![],
        vec![
            ("code_challenge", "abc"),
            ("code_challenge_method", "plain"),
        ],
    ] {
        let mut p = vec![
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", REDIRECT),
        ];
        p.extend(extra);
        let (s, _, loc) = authorize(&h, &p).await;
        assert_eq!(s, 303);
        assert_eq!(
            query_param(loc.as_deref().unwrap(), "error").as_deref(),
            Some("invalid_request")
        );
    }
    // a foreign resource is refused
    let ch = challenge(VERIFIER);
    let (_, _, loc) = authorize(
        &h,
        &[
            ("response_type", "code"),
            ("client_id", &client_id),
            ("code_challenge", &ch),
            ("code_challenge_method", "S256"),
            ("resource", "https://evil.example/mcp"),
        ],
    )
    .await;
    assert_eq!(
        query_param(&loc.unwrap(), "error").as_deref(),
        Some("invalid_target")
    );
}

#[tokio::test]
async fn bad_redirect_uris() {
    let h = start().await;
    for bad in [
        "http://chatgpt.com/connector_platform_oauth_redirect",
        "https://evil.example/callback",
        "https://chatgpt.com.evil.example/connector/oauth/x",
        "https://chatgpt.com/connector_platform_oauth_redirect#x",
        "http://localhost:3000/callback",
    ] {
        let (s, v) = register(&h, json!({"client_name": "x", "redirect_uris": [bad], "token_endpoint_auth_method": "none"})).await;
        assert_eq!(
            (s, v["error"].as_str()),
            (400, Some("invalid_redirect_uri")),
            "{bad}"
        );
    }
    let (s, _) = register(&h, json!({"redirect_uris": []})).await;
    assert_eq!(s, 400);
    // the per-connector fallback callback is fine
    let (s, _) = register(
        &h,
        json!({"redirect_uris": ["https://chatgpt.com/connector/oauth/cb_123"], "token_endpoint_auth_method": "none"}),
    )
    .await;
    assert_eq!(s, 201);

    // authorize with a redirect_uri the client did not register: an error page, never a redirect
    let client_id = public_client(&h).await;
    let ch = challenge(VERIFIER);
    for r in [
        "https://evil.example/cb",
        "https://chatgpt.com/connector/oauth/other",
    ] {
        let (s, body, loc) = authorize(
            &h,
            &[
                ("response_type", "code"),
                ("client_id", &client_id),
                ("redirect_uri", r),
                ("code_challenge", &ch),
                ("code_challenge_method", "S256"),
            ],
        )
        .await;
        assert_eq!(s, 400, "{r}");
        assert!(loc.is_none());
        assert!(body.contains("redirect_uri does not match"), "{body}");
    }
    let (s, body, loc) = authorize(
        &h,
        &[("response_type", "code"), ("client_id", "mcpc_unknown")],
    )
    .await;
    assert_eq!(s, 400);
    assert!(loc.is_none() && body.contains("Unknown or refused client"));
}

#[tokio::test]
async fn deny_and_rate_limit() {
    let h = start().await;
    let client_id = public_client(&h).await;
    let ch = challenge(VERIFIER);
    let params = [
        ("response_type", "code"),
        ("client_id", client_id.as_str()),
        ("redirect_uri", REDIRECT),
        ("state", "zz"),
        ("code_challenge", ch.as_str()),
        ("code_challenge_method", "S256"),
    ];
    let (_, html, _) = authorize(&h, &params).await;
    let (s, _, loc) = submit(
        &h,
        &[("request_id", &request_id(&html)), ("decision", "deny")],
    )
    .await;
    assert_eq!(s, 303);
    let loc = loc.unwrap();
    assert_eq!(query_param(&loc, "error").as_deref(), Some("access_denied"));
    assert_eq!(query_param(&loc, "state").as_deref(), Some("zz"));

    let (_, html, _) = authorize(&h, &params).await;
    let rid = request_id(&html);
    for _ in 0..5 {
        let (_, body, _) = submit(
            &h,
            &[
                ("request_id", &rid),
                ("email", OWNER_EMAIL),
                ("password", "bad"),
                ("decision", "approve"),
                ("scope_read", "1"),
            ],
        )
        .await;
        assert!(body.contains("Wrong email or password"));
    }
    let (s, body, _) = submit(
        &h,
        &[
            ("request_id", &rid),
            ("email", OWNER_EMAIL),
            ("password", OWNER_PASSWORD),
            ("decision", "approve"),
            ("scope_read", "1"),
        ],
    )
    .await;
    assert_eq!(s, 400);
    assert!(body.contains("Too many sign-in attempts"), "{body}");
    // the limited attempt never reached secretary-server
    assert_eq!(h.mock.requests("/v1/session").len(), 5);
}

#[tokio::test]
async fn confidential_client_secret_basic() {
    let h = start().await;
    let (s, v) = register(
        &h,
        json!({"client_name": "ChatGPT", "redirect_uris": [REDIRECT]}),
    )
    .await;
    assert_eq!(s, 201);
    assert_eq!(v["token_endpoint_auth_method"], "client_secret_basic");
    let id = v["client_id"].as_str().unwrap().to_owned();
    let secret = v["client_secret"].as_str().unwrap().to_owned();
    let code = get_code(&h, &id, "read", false).await;
    // without the secret
    let (s, v) = token(
        &h,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &id),
            ("code_verifier", VERIFIER),
        ],
    )
    .await;
    assert_eq!((s, v["error"].as_str()), (401, Some("invalid_client")));
    let basic = STANDARD.encode(format!("{id}:{secret}"));
    let r = h
        .http
        .post(format!("{}/oauth/token", h.base))
        .header("authorization", format!("Basic {basic}"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("code_verifier", VERIFIER),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let wrong = STANDARD.encode(format!("{id}:nope"));
    let r = h
        .http
        .post(format!("{}/oauth/token", h.base))
        .header("authorization", format!("Basic {wrong}"))
        .form(&[("grant_type", "refresh_token"), ("refresh_token", "mcpr_x")])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn client_id_metadata_document() {
    let h = start_with(|c, m| c.cimd_prefixes = vec![format!("{}/oauth/", m.url)]).await;
    let client_id = format!("{}/oauth/client.json", h.mock.url);
    let code = get_code(&h, &client_id, "read chat write", false).await;
    let (s, tok) = token(
        &h,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &client_id),
            ("code_verifier", VERIFIER),
        ],
    )
    .await;
    assert_eq!(s, 200, "{tok}");
    let access = tok["access_token"].as_str().unwrap();
    let (err, text) = h.call(access, "ask_lucina", json!({"message": "hi"})).await;
    assert!(!err);
    // a localhost CIMD host is not chatgpt.com, so the caller is named after the host
    assert!(text.contains("from 127"), "{text}");

    // client_id URLs outside the allowed prefixes are refused
    let (s, body, loc) = authorize(
        &h,
        &[
            ("response_type", "code"),
            ("client_id", "https://evil.example/oauth/client.json"),
        ],
    )
    .await;
    assert_eq!(s, 400);
    assert!(loc.is_none() && body.contains("not allowed"), "{body}");
}

#[tokio::test]
async fn agents_scope_is_never_implied() {
    let h = start().await;
    let client_id = public_client(&h).await;
    let ch = challenge(VERIFIER);
    let page = |scope: Option<&'static str>| {
        let mut p = vec![
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", REDIRECT),
            ("code_challenge", ch.as_str()),
            ("code_challenge_method", "S256"),
            ("resource", RESOURCE),
        ];
        if let Some(s) = scope {
            p.push(("scope", s));
        }
        p
    };
    // Asked for: offered, but not ticked.
    let (s, html, _) = authorize(&h, &page(Some("read agents"))).await;
    assert_eq!(s, 200, "{html}");
    assert!(html.contains("name=\"scope_agents\" value=\"1\">"), "{html}");
    assert!(html.contains("name=\"scope_read\" value=\"1\" checked>"), "{html}");
    // Not asked for: not offered at all.
    let (_, html, _) = authorize(&h, &page(None)).await;
    assert!(!html.contains("scope_agents") && html.contains("scope_computer"), "{html}");
    // The owner ticks it: the grant has it and the tools appear.
    let (_, html, _) = authorize(&h, &page(Some("read agents"))).await;
    let rid = request_id(&html);
    let (s, body, loc) = submit(
        &h,
        &[
            ("request_id", rid.as_str()),
            ("email", OWNER_EMAIL),
            ("password", OWNER_PASSWORD),
            ("decision", "approve"),
            ("scope_read", "1"),
            ("scope_agents", "1"),
        ],
    )
    .await;
    assert_eq!(s, 303, "{body}");
    let code = query_param(&loc.unwrap(), "code").unwrap();
    let (_, tok) = token(
        &h,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", &client_id),
            ("code_verifier", VERIFIER),
        ],
    )
    .await;
    assert_eq!(tok["scope"], "read agents");
    let names = h.tool_names(tok["access_token"].as_str().unwrap()).await;
    assert!(names.contains(&"agent_run".into()) && !names.contains(&"note_save".into()));
}
