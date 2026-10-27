//! The one error shape the app sees: `{"error":{"code","message"}}` with a
//! matching HTTP status.

use axum::extract::rejection::{BytesRejection, JsonRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::de::DeserializeOwned;
use serde_json::json;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
    /// Seconds for a `Retry-After` header (429).
    pub retry_after: Option<u64>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
            retry_after: None,
        }
    }

    pub fn unauthorized(message: &str) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }

    pub fn internal(message: &str) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }

    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "no such route")
    }

    pub fn rate_limited(wait: std::time::Duration) -> Self {
        let mut e = Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many requests; slow down",
        );
        e.retry_after = Some(wait.as_secs().max(1));
        e
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(json!({"error": {"code": self.code, "message": self.message}}));
        let mut resp = (self.status, body).into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            resp.headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        if let Some(secs) = self.retry_after {
            resp.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(secs));
        }
        resp
    }
}

/// Lucina 503 codes that describe a feature state the app can show, passed
/// through as 503 instead of being folded into `lucina_error`.
pub const LUCINA_503_PASSTHROUGH: &[&str] = &[
    "reminders_disabled",
    "owner_chat_unconfigured",
    "grocery_unavailable",
    "computer_use_disabled",
    "sandbox_unavailable",
    "viewer_unavailable",
    "recovery_required",
];

/// Lucina failures → app-facing errors.
///
/// - validation → 400 `bad_request` (Lucina's code for the same thing)
/// - Lucina 4xx → same status and Lucina's code (`due_in_past`,
///   `reminder_not_found`, `already_sent`, ...), **except**:
///   - 401/403: Lucina rejected the *server's* token, not the device's
///     (a 401 would make the app drop a valid device token);
///   - 404 `not_found` / 405 `method_not_allowed`: the secretary API is
///     disabled on Lucina or the route is missing;
///
///   these become 502 `lucina_error`
/// - Lucina 503 `reminders_disabled` / `owner_chat_unconfigured` /
///   `grocery_unavailable` → 503 with that code
/// - other Lucina 5xx / `{ok:false}` on 2xx / undecodable body → 502
///   `lucina_error`
/// - connection failure → 502 `lucina_unreachable`
/// - deadline → 504 `lucina_timeout`
impl From<secretary_core::Error> for ApiError {
    fn from(e: secretary_core::Error) -> Self {
        use secretary_core::Error as E;
        match e {
            E::Invalid { field, message } => Self::new(
                StatusCode::BAD_REQUEST,
                "bad_request",
                format!("{field}: {message}"),
            ),
            E::Api {
                status: 410,
                code,
                message,
            } if code == "cursor_expired" => Self::new(
                StatusCode::GONE,
                code,
                message.unwrap_or_else(|| "timeline cursor expired".into()),
            ),
            E::Api { status, code, .. }
                if (status == 404 && code == "not_found")
                    || (status == 405 && code == "method_not_allowed") =>
            {
                Self::new(
                    StatusCode::BAD_GATEWAY,
                    "lucina_error",
                    "Lucina's secretary API is disabled or lacks this route",
                )
            }
            E::Api {
                status: 503,
                code,
                message,
            } if LUCINA_503_PASSTHROUGH.contains(&code.as_str()) => {
                let message = message.unwrap_or_else(|| code.replace('_', " "));
                Self::new(StatusCode::SERVICE_UNAVAILABLE, code, message)
            }
            E::Api {
                status,
                code,
                message,
            } if (400..500).contains(&status) && status != 401 && status != 403 => {
                let st = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST);
                let message = message.unwrap_or_else(|| {
                    st.canonical_reason()
                        .unwrap_or("request rejected")
                        .to_owned()
                });
                Self::new(st, code, message)
            }
            E::Api { status, code, .. } if status == 401 || status == 403 => Self::new(
                StatusCode::BAD_GATEWAY,
                "lucina_error",
                format!("Lucina rejected the server's credentials ({code})"),
            ),
            E::Api { status, code, .. } => Self::new(
                StatusCode::BAD_GATEWAY,
                "lucina_error",
                format!("Lucina failed with {status} ({code})"),
            ),
            E::Decode(_) => Self::new(
                StatusCode::BAD_GATEWAY,
                "lucina_error",
                "Lucina sent a response that does not match the contract",
            ),
            E::Unreachable(_) => Self::new(
                StatusCode::BAD_GATEWAY,
                "lucina_unreachable",
                "Lucina is not reachable",
            ),
            E::Timeout => Self::new(
                StatusCode::GATEWAY_TIMEOUT,
                "lucina_timeout",
                "Lucina did not answer in time",
            ),
            E::Config(_) => Self::internal("server misconfigured"),
        }
    }
}

fn from_status(status: StatusCode, detail: String) -> ApiError {
    match status {
        // axum: JSON syntax error (400) vs. wrong shape (422)
        StatusCode::BAD_REQUEST => ApiError::new(status, "invalid_json", detail),
        StatusCode::PAYLOAD_TOO_LARGE => {
            ApiError::new(status, "payload_too_large", "request body is too large")
        }
        StatusCode::UNSUPPORTED_MEDIA_TYPE => ApiError::new(
            status,
            "unsupported_media_type",
            "expected Content-Type: application/json",
        ),
        _ => ApiError::new(StatusCode::BAD_REQUEST, "bad_request", detail),
    }
}

impl From<JsonRejection> for ApiError {
    fn from(r: JsonRejection) -> Self {
        from_status(r.status(), r.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(r: QueryRejection) -> Self {
        ApiError::new(StatusCode::BAD_REQUEST, "bad_request", r.body_text())
    }
}

impl From<BytesRejection> for ApiError {
    fn from(r: BytesRejection) -> Self {
        from_status(r.status(), r.body_text())
    }
}

/// `Json<T>` whose rejections use [`ApiError`].
pub struct JsonBody<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for JsonBody<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(v) = Json::<T>::from_request(req, state).await?;
        Ok(Self(v))
    }
}

/// `Query<T>` whose rejections use [`ApiError`].
pub struct QueryArgs<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequestParts<S> for QueryArgs<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let axum::extract::Query(v) =
            axum::extract::Query::<T>::from_request_parts(parts, state).await?;
        Ok(Self(v))
    }
}
