//! Lucina Prompter's judge (docs/prompter.md): `POST /api/secretary/prompter/judge`.
//!
//! lucina-memo turns the owner's speech into text while he talks and asks here,
//! with the Secretary API bearer, which talking points the latest words covered.
//! Jev answers on Lucina's own key; the local matcher decides whenever Jev is
//! off, cooling down or failing ([`lucina_prompter::judge`]). Only transcript
//! text reaches Jev, and only on the keyed TypeSafe route: an override of
//! `LUCINA_JEV_URL` to any other host (an anonymous free route) turns Jev off here.
use async_trait::async_trait;
use axum::body::Body;
use axum::http::Method;
use axum::response::Response;
use lucina_jev::{Decision, JevError, Question};
use lucina_prompter::{Decider, JudgeRequest};
use serde_json::Value;

use crate::api::{ok, read_json, ApiError};

const BODY_BYTES: usize = 64 * 1024;
const KEYED_HOST: &str = "api.typesafe.ai";

/// Lucina's process-wide Jev client, keyed route only.
struct Keyed;

#[async_trait]
impl Decider for Keyed {
    fn available(&self) -> bool {
        keyed_route(std::env::var("LUCINA_JEV_URL").ok().as_deref())
            && lucina_jev::shared().available()
    }
    async fn decide(
        &self,
        state: Value,
        questions: &[(&str, Question)],
    ) -> Result<Decision, JevError> {
        lucina_jev::shared()
            .decide("prompter", state, questions)
            .await
    }
}

/// The default URL, or an override that still points at the keyed TypeSafe API.
fn keyed_route(url: Option<&str>) -> bool {
    match url.map(str::trim).filter(|u| !u.is_empty()) {
        None => true,
        Some(u) => u
            .strip_prefix("https://")
            .and_then(|rest| rest.split(['/', ':', '?']).next())
            .is_some_and(|host| host.eq_ignore_ascii_case(KEYED_HOST)),
    }
}

/// `/api/secretary/prompter/<parts>`.
pub async fn route(method: &Method, parts: &[String], body: Body) -> Result<Response, ApiError> {
    match parts
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["judge"] => {
            if method != Method::POST {
                return Err(ApiError::method_not_allowed(&["POST"]));
            }
            let req: JudgeRequest = serde_json::from_value(read_json(body, BODY_BYTES).await?)
                .map_err(|e| ApiError::bad_request(format!("judge: {e}")))?;
            let r = lucina_prompter::judge(&req, Some(&Keyed)).await;
            Ok(ok(serde_json::to_value(r).unwrap_or_default()))
        }
        _ => Err(ApiError::not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_keyed_typesafe_host_may_see_speech() {
        assert!(keyed_route(None));
        assert!(keyed_route(Some("")));
        assert!(keyed_route(Some("https://api.typesafe.ai/v1/systemone")));
        assert!(keyed_route(Some(
            "https://API.typesafe.ai:443/v1/systemone"
        )));
        assert!(!keyed_route(Some("https://opencode.ai/zen/v1/systemone")));
        assert!(!keyed_route(Some("http://api.typesafe.ai/v1/systemone")));
        assert!(!keyed_route(Some(
            "https://api.typesafe.ai.evil.example/v1"
        )));
        assert!(!keyed_route(Some("https://evil.example/api.typesafe.ai")));
    }

    #[tokio::test]
    async fn judge_answers_with_the_matcher_when_jev_is_unkeyed() {
        // Tests run without JEV_API_KEY, so Jev is unavailable and the matcher decides.
        let body = Body::from(
            r#"{"window":"whisper runs on my own server at home","candidates":[{"id":"a","text":"Whisper runs on my own server"}]}"#,
        );
        let r = route(&Method::POST, &["judge".into()], body).await.unwrap();
        let v: Value =
            serde_json::from_slice(&axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap())
                .unwrap();
        assert_eq!(v["ok"], true);
        assert!(v["data"]["verdicts"][0]["p"].as_f64().unwrap() >= 0.75);
        assert_eq!(v["data"]["verdicts"][0]["source"], "local");

        let r = route(&Method::GET, &["judge".into()], Body::empty())
            .await
            .unwrap_err();
        assert_eq!(r.status.as_u16(), 405);
        let r = route(
            &Method::POST,
            &["judge".into()],
            Body::from(r#"{"window":1}"#),
        )
        .await
        .unwrap_err();
        assert_eq!(r.status.as_u16(), 400);
    }
}
