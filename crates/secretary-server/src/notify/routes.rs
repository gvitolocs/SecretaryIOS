//! HTTP routes of the notifier (`docs/server-api.md`, "Notifications").
//!
//! Device routes (Bearer or cookie + CSRF for writes):
//! - `GET /v1/notifications?limit=&before=&unread=1`
//! - `POST /v1/notifications/read {ids?, all?}`
//! - `GET|PUT /v1/notifications/settings`
//! - `POST /v1/notifications/presence {active}`
//! - `GET /v1/push/key`
//! - `POST|DELETE /v1/push/subscribe`, `DELETE /v1/push/subscriptions/{id}`
//! - `POST /v1/push/test {id?}`
//! - `POST /v1/push/apns {token}` (503 `apns_disabled` until configured)
//!
//! Service route (the `svc_` token, Lucina): `POST /v1/notify`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{middleware, Extension, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use super::model::{clip, now_ms, Category, NewNotification, Outcome};
use super::push::{self, Kind, Subscription};
use super::settings::Settings;
use super::store::DEFAULT_PAGE;
use crate::auth::Identity;
use crate::error::{ApiError, JsonBody, QueryArgs};
use crate::AppState;

type R = Result<Json<Value>, ApiError>;

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", msg)
}

fn io_error(e: std::io::Error) -> ApiError {
    tracing::error!(error = %e, "notification store failed");
    ApiError::internal("notification store unavailable")
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError::internal("notification store failed"))?
        .map_err(io_error)
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<String>,
    before: Option<String>,
    unread: Option<String>,
}

async fn list(State(st): State<AppState>, QueryArgs(q): QueryArgs<ListQuery>) -> R {
    let limit = match q.limit.as_deref() {
        None | Some("") => DEFAULT_PAGE,
        Some(l) => l
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=super::store::MAX_PAGE).contains(n))
            .ok_or_else(|| bad("limit: an integer from 1 to 200"))?,
    };
    if let Some(b) = q.before.as_deref() {
        secretary_core::validate::id("before", b)?;
    }
    let unread = match q.unread.as_deref() {
        None | Some("") | Some("0") | Some("false") => false,
        Some("1") | Some("true") => true,
        Some(_) => return Err(bad("unread: 0 or 1")),
    };
    let page = st.notify.inbox.page(q.before.as_deref(), limit, unread);
    Ok(Json(serde_json::to_value(page).unwrap_or(Value::Null)))
}

#[derive(Deserialize)]
struct ReadBody {
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    all: bool,
}

async fn read(State(st): State<AppState>, JsonBody(body): JsonBody<ReadBody>) -> R {
    if body.ids.is_empty() && !body.all {
        return Err(bad("send {ids: [...]} or {all: true}"));
    }
    if body.ids.len() > 500 {
        return Err(bad("ids: at most 500"));
    }
    for id in &body.ids {
        secretary_core::validate::id("ids", id)?;
    }
    let n = st.notify.clone();
    let r = blocking(move || n.inbox.mark_read(&body.ids, body.all, now_ms())).await?;
    Ok(Json(json!(r)))
}

fn settings_view(st: &AppState, ident: &Identity) -> Value {
    let n = &st.notify;
    let subs: Vec<_> = n.pusher.list().iter().map(Subscription::view).collect();
    json!({
        "settings": n.settings.get(),
        "categories": Category::ALL.iter().map(|c| json!({"id": c.as_str(), "defaultOn": c.default_on(), "locked": c.always_on()})).collect::<Vec<_>>(),
        "subscriptions": subs,
        "currentDevice": {"id": ident.device_id, "name": ident.device_name},
        "webPush": {"enabled": true, "publicKey": n.pusher.vapid.public_key},
        "apns": {
            "enabled": n.pusher.apns.is_some(),
            "environment": n.pusher.apns.as_ref().map(|a| if a.cfg.sandbox { "sandbox" } else { "production" }),
        },
        "present": n.present(),
    })
}

async fn get_settings(State(st): State<AppState>, Extension(ident): Extension<Identity>) -> R {
    Ok(Json(settings_view(&st, &ident)))
}

async fn put_settings(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    JsonBody(next): JsonBody<Settings>,
) -> R {
    next.validate().map_err(bad)?;
    let n = st.notify.clone();
    blocking(move || n.settings.update(|s| *s = next)).await?;
    Ok(Json(settings_view(&st, &ident)))
}

#[derive(Deserialize)]
struct PresenceBody {
    active: bool,
}

async fn presence(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    JsonBody(body): JsonBody<PresenceBody>,
) -> R {
    st.notify.set_presence(&ident.device_id, body.active);
    Ok(Json(json!({"present": st.notify.present()})))
}

async fn key(State(st): State<AppState>) -> R {
    Ok(Json(
        json!({"publicKey": st.notify.pusher.vapid.public_key}),
    ))
}

async fn subscribe(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    JsonBody(body): JsonBody<push::SubscribeBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let n = st.notify.clone();
    n.pusher.cfg.check_endpoint(&body.endpoint).map_err(bad)?;
    let (p256dh, auth) = push::check_keys(&body.keys).map_err(bad)?;
    if let Some(l) = body.lang.as_deref() {
        if !push::valid_lang(l) {
            return Err(bad("lang: one of en, it, da"));
        }
    }
    let sub = Subscription {
        id: format!(
            "ps_{}",
            crate::devices::random_b64::<12>().map_err(io_error)?
        ),
        kind: Kind::Webpush,
        device_id: ident.device_id.clone(),
        device_name: ident.device_name.clone(),
        label: push::clean_label(body.label.as_deref()),
        endpoint: body.endpoint,
        p256dh,
        auth,
        lang: body.lang,
        created_at: now_ms(),
        last_success_at: None,
        failures: 0,
        last_error: None,
    };
    let saved = blocking(move || n.pusher.upsert(sub)).await?;
    Ok((StatusCode::CREATED, Json(json!(saved.view()))))
}

#[derive(Deserialize)]
struct UnsubscribeBody {
    endpoint: String,
}

async fn unsubscribe(State(st): State<AppState>, JsonBody(body): JsonBody<UnsubscribeBody>) -> R {
    let n = st.notify.clone();
    let removed = blocking(move || n.pusher.remove_where(|s| s.endpoint == body.endpoint)).await?;
    Ok(Json(json!({"removed": removed})))
}

async fn remove_subscription(State(st): State<AppState>, Path(id): Path<String>) -> R {
    secretary_core::validate::id("id", &id)?;
    let n = st.notify.clone();
    let removed = blocking(move || n.pusher.remove_where(|s| s.id == id)).await?;
    if removed == 0 {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "subscription_not_found",
            "no such push subscription",
        ));
    }
    Ok(Json(json!({"removed": removed})))
}

#[derive(Deserialize, Default)]
struct TestBody {
    id: Option<String>,
}

/// A test push to one subscription, or to every subscription of the caller's device.
async fn test(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    body: axum::body::Bytes,
) -> R {
    let id = if body.iter().all(u8::is_ascii_whitespace) {
        None
    } else {
        serde_json::from_slice::<TestBody>(&body)
            .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "invalid_json", e.to_string()))?
            .id
    };
    let subs: Vec<Subscription> = st
        .notify
        .pusher
        .list()
        .into_iter()
        .filter(|s| match &id {
            Some(id) => &s.id == id,
            None => s.device_id == ident.device_id,
        })
        .collect();
    if subs.is_empty() {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "subscription_not_found",
            "no push subscription to test",
        ));
    }
    let results = st.notify.test_push(subs).await;
    let out: Vec<Value> = results
        .into_iter()
        .map(|(id, d)| json!({"id": id, "ok": d.ok(), "delivery": d}))
        .collect();
    Ok(Json(json!({"results": out})))
}

#[derive(Deserialize)]
struct ApnsBody {
    token: String,
    #[serde(default)]
    lang: Option<String>,
}

async fn apns_register(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    JsonBody(body): JsonBody<ApnsBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if st.notify.pusher.apns.is_none() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "apns_disabled",
            "APNs is not configured on this server (needs a paid Apple developer team)",
        ));
    }
    let token = body.token.trim().to_ascii_lowercase();
    if !super::apns::valid_device_token(&token) {
        return Err(bad("token: an APNs device token (hex)"));
    }
    let sub = Subscription {
        id: format!(
            "ps_{}",
            crate::devices::random_b64::<12>().map_err(io_error)?
        ),
        kind: Kind::Apns,
        device_id: ident.device_id.clone(),
        device_name: ident.device_name.clone(),
        label: format!("{} (app)", ident.device_name),
        endpoint: token,
        p256dh: String::new(),
        auth: String::new(),
        lang: body.lang.filter(|l| push::valid_lang(l)),
        created_at: now_ms(),
        last_success_at: None,
        failures: 0,
        last_error: None,
    };
    let n = st.notify.clone();
    let saved = blocking(move || n.pusher.upsert(sub)).await?;
    Ok((StatusCode::CREATED, Json(json!(saved.view()))))
}

/// `POST /v1/notify` (service token): Lucina pushes a notification.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NotifyBody {
    category: String,
    #[serde(default)]
    title: Option<String>,
    body: String,
    #[serde(default)]
    route: Option<String>,
    #[serde(default)]
    dedup_key: Option<String>,
    #[serde(default)]
    timeline_id: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    data: Option<Value>,
}

fn valid_route(r: &str) -> bool {
    r.starts_with('/')
        && !r.starts_with("//")
        && r.len() <= 200
        && !r.contains('\\')
        && r.bytes().all(|b| b.is_ascii_graphic())
}

async fn notify(
    State(st): State<AppState>,
    JsonBody(b): JsonBody<NotifyBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let category = Category::parse(&b.category).ok_or_else(|| {
        bad("category: one of lucina_message, reminder, calendar, device_confirm, shopping, presence, system")
    })?;
    let body = b.body.trim();
    if body.is_empty() || body.chars().count() > 2000 {
        return Err(bad("body: 1 to 2000 characters"));
    }
    if let Some(t) = &b.title {
        if t.trim().is_empty() || t.chars().count() > 120 {
            return Err(bad("title: 1 to 120 characters"));
        }
    }
    if let Some(r) = &b.route {
        if !valid_route(r) {
            return Err(bad("route: an app path like /shopping"));
        }
    }
    if let Some(k) = &b.dedup_key {
        if k.is_empty() || k.len() > 200 || k.chars().any(char::is_control) {
            return Err(bad("dedupKey: 1 to 200 characters"));
        }
    }
    if let Some(t) = &b.timeline_id {
        secretary_core::validate::id("timelineId", t)?;
    }
    if let Some(c) = &b.channel {
        if c.is_empty() || c.len() > 32 || !c.bytes().all(|x| x.is_ascii_lowercase()) {
            return Err(bad("channel: a timeline channel name"));
        }
    }
    if let Some(d) = &b.data {
        if serde_json::to_vec(d).map_or(usize::MAX, |v| v.len()) > 4096 {
            return Err(bad("data: at most 4 KiB"));
        }
    }
    let n = NewNotification {
        category: Some(category),
        title: b.title.map(|t| clip(&t, 120)),
        body: clip(body, 2000),
        route: b.route,
        channel: b.channel,
        timeline_id: b.timeline_id,
        dedup_key: b.dedup_key,
        data: b.data,
    };
    let outcome = st.notify.publish(n).await.map_err(io_error)?;
    Ok(match outcome {
        Outcome::Created {
            notification,
            pushed,
            deferred,
        } => (
            StatusCode::CREATED,
            Json(
                json!({"status": "created", "notification": notification, "pushed": pushed, "deferred": deferred}),
            ),
        ),
        other => (StatusCode::OK, Json(json!({"status": other.status()}))),
    })
}

/// Behind `require_device`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/notifications", get(list))
        .route("/v1/notifications/read", post(read))
        .route(
            "/v1/notifications/settings",
            get(get_settings).put(put_settings),
        )
        .route("/v1/notifications/presence", post(presence))
        .route("/v1/push/key", get(key))
        .route("/v1/push/subscribe", post(subscribe).delete(unsubscribe))
        .route("/v1/push/subscriptions/{id}", delete(remove_subscription))
        .route("/v1/push/test", post(test))
        .route("/v1/push/apns", post(apns_register))
}

/// Behind `require_service`.
pub fn service_routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/v1/notify", post(notify))
        .route_layer(middleware::from_fn_with_state(
            state,
            crate::service::require_service,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_are_app_paths() {
        assert!(valid_route("/shopping"));
        assert!(valid_route("/gym/history/abc"));
        assert!(!valid_route("//evil.example"));
        assert!(!valid_route("https://evil.example"));
        assert!(!valid_route("/a b"));
        assert!(!valid_route("/\\evil"));
    }
}
