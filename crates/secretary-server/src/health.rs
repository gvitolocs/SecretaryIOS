//! `GET /v1/health`: server liveness plus a cached probe of Lucina.

use std::time::{Duration, Instant};

use secretary_core::{Call, LucinaClient};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LucinaStatus {
    pub reachable: bool,
    /// Round-trip time of the probe; `null` when no answer arrived.
    pub latency_ms: Option<u64>,
    /// Why the probe failed (`lucina_timeout`, `lucina_unreachable`,
    /// `not_configured`, or Lucina's error code); absent when reachable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Lucina contract version from its `/health` (`apiVersion`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_version: Option<u64>,
}

pub struct HealthCache {
    ttl: Duration,
    timeout: Duration,
    /// Held across the probe so concurrent callers share one probe.
    slot: Mutex<Option<(Instant, LucinaStatus)>>,
}

impl HealthCache {
    pub fn new(ttl: Duration, timeout: Duration) -> Self {
        Self {
            ttl,
            timeout,
            slot: Mutex::new(None),
        }
    }

    pub async fn status(&self, lucina: Option<&LucinaClient>) -> LucinaStatus {
        let mut slot = self.slot.lock().await;
        if let Some((at, st)) = slot.as_ref() {
            if at.elapsed() < self.ttl {
                return st.clone();
            }
        }
        let st = self.probe(lucina).await;
        *slot = Some((Instant::now(), st.clone()));
        st
    }

    async fn probe(&self, lucina: Option<&LucinaClient>) -> LucinaStatus {
        let Some(client) = lucina else {
            return LucinaStatus {
                reachable: false,
                latency_ms: None,
                error: Some("not_configured".into()),
                api_version: None,
            };
        };
        let start = Instant::now();
        let res = client
            .call_with_timeout::<Value>(&Call::health(), self.timeout)
            .await;
        let ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        use secretary_core::Error as E;
        match res {
            Ok(data) => LucinaStatus {
                reachable: true,
                latency_ms: Some(ms),
                error: None,
                api_version: data.get("apiVersion").and_then(Value::as_u64),
            },
            Err(E::Api { code, .. }) => LucinaStatus {
                reachable: false,
                latency_ms: Some(ms),
                error: Some(code),
                api_version: None,
            },
            Err(E::Decode(_)) => LucinaStatus {
                reachable: false,
                latency_ms: Some(ms),
                error: Some("lucina_error".into()),
                api_version: None,
            },
            Err(E::Timeout) => LucinaStatus {
                reachable: false,
                latency_ms: None,
                error: Some("lucina_timeout".into()),
                api_version: None,
            },
            Err(_) => LucinaStatus {
                reachable: false,
                latency_ms: None,
                error: Some("lucina_unreachable".into()),
                api_version: None,
            },
        }
    }
}
