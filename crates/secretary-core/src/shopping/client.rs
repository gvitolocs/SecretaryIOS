//! HTTP client for the lucina-proxy. The core talks to ONE base URL (the
//! proxy's `lucina_*` routes) — it has no knowledge of upstream hosts.

use crate::shopping::models::{LucinaItem, LucinaList, Token};
use crate::shopping::Result;

#[derive(Clone)]
pub struct LucinaClient {
    http: reqwest::Client,
    pub base_url: String,
    pub token: Option<Token>,
}

#[derive(serde::Serialize)]
struct AuthBody<'a> {
    email: &'a str,
    password: &'a str,
}

impl LucinaClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent(concat!("lucina-core/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: None,
        }
    }

    pub fn with_token(mut self, token: Token) -> Self {
        self.token = Some(token);
        self
    }

    fn bearer(&self) -> Result<String> {
        let t = self
            .token
            .as_ref()
            .ok_or(crate::shopping::LucinaError::NoToken)?;
        Ok(format!("Bearer {}", t.access_token))
    }

    fn user_uuid(&self) -> Result<String> {
        Ok(self
            .token
            .as_ref()
            .ok_or(crate::shopping::LucinaError::NoToken)?
            .user_uuid
            .clone())
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(crate::shopping::LucinaError::Api {
                status: status.as_u16(),
                body,
            });
        }
        Ok(resp)
    }

    // -- auth ---------------------------------------------------------------

    pub async fn login(&mut self, email: &str, password: &str) -> Result<Token> {
        let resp = self
            .http
            .post(self.url("/lucina_auth"))
            .json(&AuthBody { email, password })
            .send()
            .await?;
        let token: Token = Self::check(resp).await?.json().await?;
        self.token = Some(token.clone());
        Ok(token)
    }

    pub async fn signup_anonym(&mut self) -> Result<Token> {
        let resp = self
            .http
            .post(self.url("/lucina_signup_anon"))
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await?;
        let token: Token = Self::check(resp).await?.json().await?;
        self.token = Some(token.clone());
        Ok(token)
    }

    // -- lists ----------------------------------------------------------------

    pub async fn lists(&self) -> Result<Vec<LucinaList>> {
        let user = self.user_uuid()?;
        let resp = self
            .http
            .get(self.url(&format!("/lucina_lists/{user}")))
            .header("Authorization", self.bearer()?)
            .send()
            .await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    /// Full snapshot of one list, normalized into our flat item model.
    /// Upstream splits items into checked/unchecked buckets; that shape
    /// never leaves this function.
    pub async fn items(&self, list_uuid: &str) -> Result<Vec<LucinaItem>> {
        let resp = self
            .http
            .get(self.url(&format!("/lucina_list/{list_uuid}")))
            .header("Authorization", self.bearer()?)
            .send()
            .await?;
        let v: serde_json::Value = Self::check(resp).await?.json().await?;
        let mut out = Vec::new();
        for (key, checked) in [("items", false), ("checked", true)] {
            if let Some(arr) = v.get(key).and_then(|x| x.as_array()) {
                for it in arr {
                    out.push(LucinaItem {
                        uuid: it
                            .get("uuid")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .into(),
                        name: it
                            .get("itemId")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .into(),
                        spec: it
                            .get("spec")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .into(),
                        checked,
                        section: it
                            .get("usersection")
                            .and_then(|x| x.as_str())
                            .map(String::from),
                    });
                }
            }
        }
        Ok(out)
    }

    /// Batch write of a list's unchecked items (full-snapshot PUT contract).
    pub async fn put_items(&self, list_uuid: &str, items: &[LucinaItem]) -> Result<()> {
        let payload = serde_json::json!({
            "listUuid": list_uuid,
            "items": items
                .iter()
                .filter(|i| !i.checked)
                .map(|i| serde_json::json!({
                    "uuid": i.uuid,
                    "itemId": i.name,
                    "spec": i.spec,
                    "usersection": i.section,
                }))
                .collect::<Vec<_>>(),
        });
        let resp = self
            .http
            .put(self.url(&format!("/lucina_items_put/{list_uuid}")))
            .header("Authorization", self.bearer()?)
            .json(&payload)
            .send()
            .await?;
        Self::check(resp).await?;
        Ok(())
    }

    // -- settings -------------------------------------------------------------

    pub async fn settings(&self) -> Result<serde_json::Value> {
        let user = self.user_uuid()?;
        let resp = self
            .http
            .get(self.url(&format!("/lucina_settings/{user}")))
            .header("Authorization", self.bearer()?)
            .send()
            .await?;
        Ok(Self::check(resp).await?.json().await?)
    }

    pub async fn set_setting(&self, key: &str, value: serde_json::Value) -> Result<()> {
        let user = self.user_uuid()?;
        let resp = self
            .http
            .post(self.url(&format!("/lucina_settings_set/{user}/{key}")))
            .header("Authorization", self.bearer()?)
            .json(&value)
            .send()
            .await?;
        Self::check(resp).await?;
        Ok(())
    }
}
