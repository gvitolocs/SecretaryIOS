//! One account shared by HTTP and the blocking App API. Generations fence late async results.
use crate::{
    account::{Account, SessionState},
    broker,
    oauth::{self, Secret, TokenError, TokenSet},
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub broker: broker::Config,
    pub oauth: oauth::Config,
}
struct Login {
    state: String,
    verifier: Secret,
    at: Instant,
}
struct Inner {
    token: Option<TokenSet>,
    pending_refresh: Option<Secret>,
    state: &'static str,
    store: Option<&'static str>,
    removal: Option<&'static str>,
    error: Option<&'static str>,
    connected_at: Option<String>,
    logins: VecDeque<Login>,
    generation: u64,
}
pub struct Connection {
    cfg: Config,
    inner: Mutex<Inner>,
    refresh_lock: Mutex<()>,
    mutations: tokio::sync::Mutex<()>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectError(pub u16, pub &'static str);
impl Connection {
    pub fn new(cfg: Config) -> Arc<Self> {
        let configured = cfg.broker.token_file.is_some();
        Arc::new(Self {
            cfg,
            inner: Mutex::new(Inner {
                token: None,
                pending_refresh: None,
                state: "unavailable",
                store: None,
                removal: None,
                error: Some(if configured {
                    "broker_unavailable"
                } else {
                    "broker_unconfigured"
                }),
                connected_at: None,
                logins: VecDeque::new(),
                generation: 0,
            }),
            refresh_lock: Mutex::new(()),
            mutations: tokio::sync::Mutex::new(()),
        })
    }
    pub fn startup(self: &Arc<Self>) {
        let c = self.clone();
        let generation = c.inner.lock().unwrap().generation;
        tokio::spawn(async move {
            if c.cfg.broker.token_file.is_none() {
                return;
            }
            loop {
                if c.inner.lock().unwrap().generation != generation {
                    return;
                }
                match c.cfg.broker.read().await {
                    Ok(token) => {
                        {
                            let mut i = c.inner.lock().unwrap();
                            if i.generation != generation {
                                return;
                            }
                            i.error = None;
                            i.state = if token.is_some() {
                                "connected"
                            } else {
                                "disconnected"
                            };
                            i.store = token.as_ref().map(|_| "saved");
                            i.pending_refresh = token;
                        }
                        let cc = c.clone();
                        let _ = tokio::task::spawn_blocking(move || cc.access(false)).await;
                        return;
                    }
                    Err(_) => {
                        let mut i = c.inner.lock().unwrap();
                        if i.generation != generation {
                            return;
                        }
                        i.state = "unavailable";
                        i.error = Some("broker_unavailable");
                    }
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }
    pub fn connected(&self) -> bool {
        self.inner.lock().unwrap().state == "connected"
    }
    pub fn has_session(&self) -> bool {
        matches!(self.inner.lock().unwrap().state, "connected" | "expired")
    }
    pub fn account(&self) -> Account {
        let i = self.inner.lock().unwrap();
        match i.state {
            "connected" => i
                .token
                .as_ref()
                .map(|t| Account {
                    session: SessionState::Ok,
                    premium: t.user.premium,
                    r18: Some(t.user.x_restrict >= 1),
                })
                .unwrap_or_else(Account::unknown),
            "expired" => Account {
                session: SessionState::Expired,
                premium: false,
                r18: Some(false),
            },
            _ => Account::anonymous(),
        }
    }
    pub fn json(&self) -> Value {
        Self::snapshot(&self.inner.lock().unwrap())
    }
    fn snapshot(i: &Inner) -> Value {
        let t = i.token.as_ref().filter(|_| i.state == "connected");
        json!({"state":i.state,"user":t.map(|t| json!({"id":t.user.id,"name":t.user.name,"account":t.user.account})),
            "premium":t.map(|t| t.user.premium),"r18":t.map(|t| t.user.x_restrict>=1),"r18g":t.map(|t| t.user.x_restrict>=2),
            "store":i.store,"removal":i.removal,"error":i.error,"connectedAt":i.connected_at})
    }
    fn busy(i: &Inner) -> bool {
        i.store == Some("waiting_approval") || i.removal == Some("waiting_approval")
    }
    pub fn start(&self) -> Result<Value, ConnectError> {
        if self.cfg.broker.token_file.is_none() {
            return Err(ConnectError(503, "broker_unconfigured"));
        }
        let mut i = self.inner.lock().unwrap();
        if Self::busy(&i) {
            return Err(ConnectError(409, "busy"));
        }
        let verifier = oauth::verifier().map_err(|_| ConnectError(502, "pixiv_login_failed"))?;
        let state = oauth::random_hex().map_err(|_| ConnectError(502, "pixiv_login_failed"))?;
        let result =
            json!({"loginUrl":oauth::login_url(&verifier),"state":state,"expiresInSecs":600});
        i.logins
            .retain(|l| l.at.elapsed() < Duration::from_secs(600));
        if i.logins.len() >= 5 {
            i.logins.pop_front();
        }
        i.logins.push_back(Login {
            state,
            verifier,
            at: Instant::now(),
        });
        Ok(result)
    }
    pub async fn finish(self: &Arc<Self>, state: &str, code: &str) -> Result<Value, ConnectError> {
        let (login, generation) = {
            let mut i = self.inner.lock().unwrap();
            if Self::busy(&i) {
                return Err(ConnectError(409, "busy"));
            }
            i.logins
                .retain(|l| l.at.elapsed() < Duration::from_secs(600));
            let pos = i
                .logins
                .iter()
                .position(|l| l.state == state && l.at.elapsed() < Duration::from_secs(600))
                .ok_or(ConnectError(400, "connect_expired"))?;
            i.generation += 1;
            (i.logins.remove(pos).unwrap(), i.generation)
        };
        let token = self
            .cfg
            .oauth
            .exchange(code, &login.verifier)
            .await
            .map_err(|_| ConnectError(502, "pixiv_login_failed"))?;
        let refresh = token.refresh.clone();
        let result = {
            let mut i = self.inner.lock().unwrap();
            if i.generation != generation {
                return Err(ConnectError(400, "connect_expired"));
            }
            i.token = Some(token);
            i.pending_refresh = None;
            i.state = "connected";
            i.store = Some("waiting_approval");
            i.error = None;
            i.connected_at =
                Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
            Self::snapshot(&i)
        };
        let c = self.clone();
        tokio::spawn(async move {
            let _mutation = c.mutations.lock().await;
            if c.inner.lock().unwrap().generation != generation {
                return;
            }
            let result = match c.cfg.broker.exists().await {
                Ok(exists) => {
                    c.cfg
                        .broker
                        .mutate(if exists { "update" } else { "create" }, Some(&refresh))
                        .await
                }
                Err(e) => Err(e),
            };
            let mut i = c.inner.lock().unwrap();
            if i.generation != generation {
                return;
            }
            match result {
                Ok(()) => i.store = Some("saved"),
                Err(e) => {
                    i.token = None;
                    i.state = "disconnected";
                    i.store = None;
                    i.connected_at = None;
                    i.error = Some(match e {
                        broker::Error::Denied => "approval_denied",
                        broker::Error::Expired => "approval_expired",
                        broker::Error::ApprovalUnavailable => "approval_unavailable",
                        _ => "save_failed",
                    });
                }
            }
        });
        Ok(result)
    }
    pub fn disconnect(self: &Arc<Self>) -> Value {
        let (generation, saved, result) = {
            let mut i = self.inner.lock().unwrap();
            i.generation += 1;
            let saved = i.store == Some("saved");
            i.store = if saved { Some("saved") } else { None };
            i.token = None;
            i.pending_refresh = None;
            i.state = "disconnected";
            i.connected_at = None;
            i.error = None;
            i.logins.clear();
            // Reserve the removal slot before checking the broker to fence a concurrent connect.
            i.removal = self
                .cfg
                .broker
                .token_file
                .as_ref()
                .map(|_| "waiting_approval");
            (i.generation, saved, Self::snapshot(&i))
        };
        if self.cfg.broker.token_file.is_none() {
            return result;
        }
        let c = self.clone();
        tokio::spawn(async move {
            let _mutation = c.mutations.lock().await;
            if c.inner.lock().unwrap().generation != generation {
                return;
            }
            let (res, exists) = if saved {
                (c.cfg.broker.mutate("delete", None).await, true)
            } else {
                match c.cfg.broker.exists().await {
                    Ok(true) => (c.cfg.broker.mutate("delete", None).await, true),
                    Ok(false) => (Ok(()), false),
                    Err(e) => (Err(e), false),
                }
            };
            let mut i = c.inner.lock().unwrap();
            if i.generation != generation {
                return;
            }
            i.removal = None;
            match res {
                Ok(()) => {
                    i.store = None;
                }
                Err(e) => {
                    i.error = Some(if e == broker::Error::Denied {
                        "remove_denied"
                    } else {
                        "remove_failed"
                    });
                    if exists {
                        i.store = Some("saved");
                    }
                }
            }
        });
        result
    }
    /// Serialized refresh without holding the state mutex during network I/O.
    pub fn access(&self, force: bool) -> Result<Secret, String> {
        let _guard = self.refresh_lock.lock().unwrap();
        let (refresh, generation) = {
            let i = self.inner.lock().unwrap();
            if i.state != "connected" {
                return Err("token unavailable".into());
            }
            if let Some(t) = &i.token {
                if !force
                    && t.expires_at.saturating_duration_since(Instant::now())
                        >= Duration::from_secs(60)
                {
                    return Ok(t.access.clone());
                }
            }
            (
                i.token
                    .as_ref()
                    .map(|t| t.refresh.clone())
                    .or_else(|| i.pending_refresh.clone())
                    .ok_or("token unavailable")?,
                i.generation,
            )
        };
        let result = self.cfg.oauth.refresh(&refresh);
        let mut i = self.inner.lock().unwrap();
        if i.generation != generation || i.state != "connected" {
            return Err("connection changed".into());
        }
        match result {
            Ok(t) => {
                let access = t.access.clone();
                i.token = Some(t);
                i.pending_refresh = None;
                i.error = None;
                if i.connected_at.is_none() {
                    i.connected_at =
                        Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
                }
                Ok(access)
            }
            Err(TokenError::Rejected) => {
                i.token = None;
                i.pending_refresh = None;
                i.state = "expired";
                i.error = Some("token_rejected");
                Err("token rejected".into())
            }
            Err(_) => Err("network: token endpoint unavailable".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn expired_pkce_state_is_not_exchanged() {
        let mut cfg = Config::default();
        // No broker or OAuth I/O should be reached on an expired login.
        cfg.broker.token_file = Some("/no-such-test-token".into());
        let c = Connection::new(cfg);
        let start = c.start().unwrap();
        c.inner.lock().unwrap().logins[0].at = Instant::now() - Duration::from_secs(601);
        assert_eq!(
            c.finish(start["state"].as_str().unwrap(), "code")
                .await
                .unwrap_err(),
            ConnectError(400, "connect_expired")
        );
    }
}
