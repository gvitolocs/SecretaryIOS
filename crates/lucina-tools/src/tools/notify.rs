//! `notify`: Lucina sends the owner a notification on his Secretary apps
//! (web push, home-screen app, iPhone app inbox).
//!
//! `POST /v1/notify` on secretary-server with the service token (see
//! SecretaryIOS `docs/notifications.md`). The server stores it in the inbox
//! and pushes it to the devices the owner allows, honouring quiet hours,
//! Do Not Disturb and per-category switches. Only the owner's own devices
//! get it: there is no recipient argument.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

/// Categories Lucina may use (the server also has `device_confirm` and
/// `presence`, which come from the timeline and the server itself).
pub const CATEGORIES: [&str; 5] = [
    "lucina_message",
    "reminder",
    "calendar",
    "shopping",
    "system",
];

#[derive(Deserialize, JsonSchema)]
struct Args {
    /// The notification text (1–500 characters), in the owner's language.
    body: String,
    /// Optional short title (default: by category, e.g. "Lucina").
    title: Option<String>,
    /// lucina_message (default), reminder, calendar, shopping or system.
    category: Option<String>,
    /// App screen to open on tap: "/" (chat, default), "/shopping", "/gym", "/weight".
    route: Option<String>,
    /// Optional key: the same key twice notifies once (e.g. "checkin:2026-10-22").
    dedup_key: Option<String>,
}

pub struct NotifyTool;

fn server(ctx: &Ctx) -> Result<(String, String), ToolError> {
    let env = &ctx.env;
    let url = env
        .secretary_url
        .as_deref()
        .map(|u| u.trim_end_matches('/').to_owned())
        .filter(|u| !u.is_empty());
    let token = env
        .secretary_service_token_file
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| t.trim().to_owned())
        .filter(|t| t.starts_with("svc_"));
    match (url, token) {
        (Some(u), Some(t)) => Ok((u, t)),
        _ => Err(ToolError::new(
            "app notifications are not configured (LUCINA_SECRETARY_SERVER_URL and LUCINA_SECRETARY_SERVICE_TOKEN_FILE)",
        )),
    }
}

const ROUTES: [&str; 4] = ["/", "/shopping", "/gym", "/weight"];

#[async_trait::async_trait]
impl Tool for NotifyTool {
    fn name(&self) -> &'static str {
        "notify"
    }
    fn description(&self) -> &'static str {
        "Send Giuseppe a notification on his Secretary apps (iPhone and browsers), for something he should see \
         even when he is not chatting: a heads-up, a follow-up, a result he asked to be told about. Only his own \
         devices get it; quiet hours and Do Not Disturb are respected by the server. Keep it short and in his \
         language. Do not use it to repeat the reply of the current chat turn."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Device
    }
    fn summarize(&self, args: &Value) -> String {
        format!(
            "notify {}",
            args["category"].as_str().unwrap_or("lucina_message")
        )
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let body = a.body.trim();
        if body.is_empty() || body.chars().count() > 500 {
            return Err(ToolError::new("body: 1 to 500 characters"));
        }
        let category = a.category.as_deref().unwrap_or("lucina_message");
        if !CATEGORIES.contains(&category) {
            return Err(ToolError::new(format!(
                "category: one of {}",
                CATEGORIES.join(", ")
            )));
        }
        let route = a.route.as_deref().unwrap_or("/");
        if !ROUTES.contains(&route) {
            return Err(ToolError::new(format!(
                "route: one of {}",
                ROUTES.join(", ")
            )));
        }
        let mut payload =
            json!({"category": category, "body": body, "route": route, "channel": ctx.channel});
        if let Some(t) = a.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            payload["title"] = json!(t.chars().take(120).collect::<String>());
        }
        if let Some(k) = a
            .dedup_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
        {
            payload["dedupKey"] = json!(format!(
                "lucina:{}",
                k.chars().take(180).collect::<String>()
            ));
        }
        if !payload["channel"].as_str().is_some_and(|c| {
            !c.is_empty() && c.len() <= 32 && c.bytes().all(|b| b.is_ascii_lowercase())
        }) {
            payload.as_object_mut().map(|o| o.remove("channel"));
        }
        // Notifications are a sink: no secret leaves on a push.
        lucina_secrets::redactor().redact_json(&mut payload);
        let (base, token) = server(ctx)?;
        let resp = ctx
            .env
            .http
            .post(format!("{base}/v1/notify"))
            .bearer_auth(&token)
            .json(&payload)
            .send()
            .await
            .map_err(|e| ToolError::new(format!("secretary-server unreachable: {e}")))?;
        let status = resp.status().as_u16();
        let answer: Value = resp.json().await.unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            let code = answer["error"]["code"].as_str().unwrap_or_default();
            return Err(ToolError::new(format!(
                "secretary-server answered {status} {code}"
            )));
        }
        // created | duplicate | disabled (the owner turned the category off) | present (he is in the app)
        Ok(json!({
            "ok": true,
            "status": answer["status"],
            "pushed": answer["pushed"],
            "deferred": answer["deferred"],
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::NotifyTool;
    use crate::{Ctx, SideEffect, Tool, ToolEnv};

    const SVC: &str = "svc_test_token_0123456789abcdefghijklmnop";

    fn ctx(dir: &std::path::Path, url: Option<String>) -> Ctx {
        let token = dir.join("service.token");
        std::fs::write(&token, format!("{SVC}\n")).unwrap();
        let mut env = ToolEnv::new(dir, chrono_tz::Europe::Rome);
        env.secretary_url = url;
        env.secretary_service_token_file = Some(token);
        Ctx::new(Arc::new(env), "telegram", "s1")
    }

    #[tokio::test]
    async fn posts_to_v1_notify_with_the_service_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/notify"))
            .and(header("authorization", format!("Bearer {SVC}").as_str()))
            .and(body_json(json!({
                "category": "shopping", "body": "Ho aggiunto il latte.", "route": "/shopping",
                "channel": "telegram", "title": "Spesa", "dedupKey": "lucina:milk-1"
            })))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(json!({"status": "created", "pushed": 2, "deferred": false})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let c = ctx(dir.path(), Some(format!("{}/", server.uri())));
        let out = NotifyTool
            .call(
                json!({"body": " Ho aggiunto il latte. ", "title": "Spesa", "category": "shopping",
                       "route": "/shopping", "dedup_key": "milk-1"}),
                &c,
            )
            .await
            .unwrap();
        assert_eq!(
            out,
            json!({"ok": true, "status": "created", "pushed": 2, "deferred": false})
        );
        assert_eq!(NotifyTool.side_effect(), SideEffect::Device);
        assert!(
            !NotifyTool.needs_confirmation(&json!({})),
            "only the owner's own devices"
        );
    }

    #[tokio::test]
    async fn validates_and_reports_server_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/notify"))
            .respond_with(ResponseTemplate::new(401).set_body_json(
                json!({"error": {"code": "unauthorized", "message": "unknown service token"}}),
            ))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let c = ctx(dir.path(), Some(server.uri()));
        for bad in [
            json!({"body": ""}),
            json!({"body": "x".repeat(501)}),
            json!({"body": "x", "category": "device_confirm"}),
            json!({"body": "x", "route": "https://evil.example"}),
        ] {
            assert!(NotifyTool.call(bad.clone(), &c).await.is_err(), "{bad}");
        }
        let err = NotifyTool
            .call(json!({"body": "ciao"}), &c)
            .await
            .unwrap_err();
        assert!(err.0.contains("401 unauthorized"), "{err}");
        let unconfigured = ctx(dir.path(), None);
        let err = NotifyTool
            .call(json!({"body": "ciao"}), &unconfigured)
            .await
            .unwrap_err();
        assert!(err.0.contains("not configured"));
    }
}
