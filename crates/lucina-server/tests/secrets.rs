//! The secrets skill end to end, on the real server: a secret injected into
//! a tool's output (raw and encoded) never reaches any sink, the model
//! never sees a value, reveal requests get the fixed refusal without the
//! model, MCP turns get no secrets tools. Fake values only.

mod common;

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine;
use common::{harness, script};
use lucina_memory::MemoryMode;
use lucina_server::PlannerMode;
use serde_json::{json, Value};
use wiremock::matchers::{any, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The Open-Meteo key in Infisical (fake), with characters that make every
/// encoding differ from the raw value.
const KEY: &str = "omK3y/With+Spec&ial\"Chars-9876xyz";
/// A token-shaped string the model "leaks" in its reply.
const MODEL_LEAK: &str = "ghp_ABCDEFGHIJKLMNOPQRSTuvwx1234";

/// Process-wide log capture through the redacting writer (as in `main`).
fn logs() -> &'static Arc<Mutex<Vec<u8>>> {
    static BUF: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    BUF.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        #[derive(Clone)]
        struct W(Arc<Mutex<Vec<u8>>>);
        impl Write for W {
            fn write(&mut self, d: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(d);
                Ok(d.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let w = W(buf.clone());
        let sub = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(lucina_secrets::log::RedactingMakeWriter::new(move || {
                w.clone()
            }))
            .finish();
        let _ = tracing::subscriber::set_global_default(sub);
        buf
    })
}

/// Every form of the key a sink could carry.
fn forms() -> Vec<String> {
    lucina_secrets::redact::encodings(KEY)
        .into_iter()
        .filter(|e| e.len() >= 12)
        .collect()
}

fn assert_clean(what: &str, text: &str) {
    for f in forms() {
        assert!(!text.contains(&f), "{what} leaks the key ({f}):\n{text}");
    }
    assert!(
        !text.contains(MODEL_LEAK),
        "{what} leaks the model's token:\n{text}"
    );
}

fn scan_dir(dir: &Path) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            scan_dir(&p);
        } else if let Ok(text) = std::fs::read_to_string(&p) {
            assert_clean(&p.display().to_string(), &text);
        }
    }
}

async fn infisical() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/universal-auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "accessToken": "fake-infisical-access-token-0123456789", "expiresIn": 3600,
            "accessTokenMaxTTL": 7200, "tokenType": "Bearer"
        })))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v4/secrets/OPEN_METEO_API_KEY"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"secret": {
            "id": "a", "_id": "a", "workspace": "proj-1", "environment": "prod", "version": 2,
            "type": "shared", "secretKey": "OPEN_METEO_API_KEY", "secretValue": KEY, "secretComment": "",
            "createdAt": "2026-10-01T08:00:00Z", "updatedAt": "2026-10-20T09:00:00Z",
            "secretValueHidden": false, "secretPath": "/lucina"
        }})))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v4/secrets"))
        .and(query_param("viewSecretValue", "false"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"secrets": [{
            "id": "a", "_id": "a", "workspace": "proj-1", "environment": "prod", "version": 2,
            "type": "shared", "secretKey": "OPEN_METEO_API_KEY", "secretValue": "<hidden-by-infisical>",
            "secretComment": "", "createdAt": "2026-10-01T08:00:00Z", "updatedAt": "2026-10-20T09:00:00Z",
            "secretValueHidden": true, "secretPath": "/lucina"
        }]})))
        .mount(&s)
        .await;
    s
}

/// Open-Meteo that wants the key and then echoes it back, raw and encoded.
async fn echoing_weather() -> MockServer {
    let s = MockServer::start().await;
    let b64 = base64::engine::general_purpose::STANDARD.encode(format!("apikey:{KEY}"));
    Mock::given(method("GET"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": [{
            "name": format!("Copenhagen {b64}"), "latitude": 55.676, "longitude": 12.566,
            "country": "Denmark", "country_code": "DK", "admin1": format!("debug apikey={}", urlencode(KEY))
        }]})))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/forecast"))
        .and(query_param("apikey", KEY))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "timezone": format!("Europe/Copenhagen (key {KEY})"),
            "current": {"time": "2026-10-22T17:00", "temperature_2m": 9.0, "weather_code": 3},
            "daily": {"time": ["2026-10-22"], "weather_code": [61], "temperature_2m_max": [11.0],
                      "temperature_2m_min": [6.0], "precipitation_sum": [4.2],
                      "precipitation_probability_max": [85], "wind_speed_10m_max": [30],
                      "sunrise": [""], "sunset": [""]}
        })))
        .expect(1..)
        .mount(&s)
        .await;
    s
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn secrets_toml(dir: &Path, infisical: &str) -> std::path::PathBuf {
    let creds = dir.join("creds");
    std::fs::create_dir_all(&creds).unwrap();
    std::fs::write(
        creds.join("lucina-infisical-client-id"),
        "fake-machine-identity-id\n",
    )
    .unwrap();
    std::fs::write(
        creds.join("lucina-infisical-client-secret"),
        "fake-machine-identity-secret-77\n",
    )
    .unwrap();
    let p = dir.join("secrets.toml");
    std::fs::write(
        &p,
        format!(
            r#"
[infisical]
url = "{infisical}"
credentials_dir = "{}"

[projects]
lucina = "proj-1"

[scope]
project = "lucina"
environment = "prod"
path = "/lucina"

[audit]
path = "{}"

[redaction]
preload = false

[refs]
"weather.api_key" = "secret://lucina/prod/lucina#OPEN_METEO_API_KEY"
"#,
            creds.display(),
            dir.join("state/secrets-audit.jsonl").display()
        ),
    )
    .unwrap();
    p
}

fn agent_tool_names(req: &Value) -> Vec<String> {
    req["tools"]
        .as_array()
        .map(|t| {
            t.iter()
                .filter_map(|x| x["function"]["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_secret_in_a_tool_output_reaches_no_sink() {
    let logs = logs();
    let inf = infisical().await;
    let meteo = echoing_weather().await;
    let honcho = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&honcho)
        .await;
    let side = tempfile::tempdir().unwrap();
    let secrets_cfg = secrets_toml(side.path(), &inf.uri());
    let (meteo_uri, honcho_uri) = (meteo.uri(), honcho.uri());
    let h = harness(
        script(json!({"rules": [
            {"kind": "agent", "contains": "tempo", "afterTool": false, "toolCalls": [{"name": "weather", "arguments": {"location": "Copenhagen", "days": 1}}]},
            {"kind": "agent", "contains": "tempo", "afterTool": true, "content": format!("Domani pioggia a Copenaghen. (debug: {MODEL_LEAK})")}
        ]})),
        move |c| {
            c.planner = PlannerMode::Tools;
            c.endpoints.open_meteo_forecast = meteo_uri.clone();
            c.endpoints.open_meteo_geocoding = meteo_uri;
            c.memory_mode = MemoryMode::ReadWrite;
            c.honcho.base_url = honcho_uri;
            c.honcho.api_key = "fake-honcho-key-123456".into();
            c.secrets_config = Some(secrets_cfg);
        },
    )
    .await;
    assert!(h.state.secrets.is_some(), "secrets store configured");

    let d = h.chat("che tempo fa domani a Copenhagen?").await;
    assert_clean("API reply", &d.to_string());
    assert!(
        d["reply"].as_str().unwrap().contains("Domani pioggia"),
        "{d}"
    );
    assert!(
        d["reply"].as_str().unwrap().contains("[secret:token]"),
        "{d}"
    );

    // The model saw the tool output, redacted.
    let reqs = h.fake.requests();
    let all = serde_json::to_string(&reqs).unwrap();
    assert_clean("model requests", &all);
    let tool_msg = reqs
        .iter()
        .flat_map(|r| r["messages"].as_array().cloned().unwrap_or_default())
        .find(|m| m["role"] == "tool")
        .expect("a tool result went to the model");
    let content = tool_msg["content"].as_str().unwrap();
    assert!(content.contains("[secret:OPEN_METEO_API_KEY]"), "{content}");
    assert!(content.contains("light rain"), "{content}");
    // The owner-only tools are offered to the owner.
    assert!(agent_tool_names(&reqs[0]).contains(&"secrets_status".to_string()));

    // A careless debug log of the raw upstream body.
    tracing::warn!(body = %format!("timezone Europe/Copenhagen (key {KEY})"), "upstream body");

    // Timeline, recent turns, jobs, pending actions: every file in the data dir.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await; // Honcho writes are spawned
    scan_dir(h.dir.path());
    // Honcho got the turn, redacted.
    let honcho_reqs = honcho.received_requests().await.unwrap();
    assert!(
        honcho_reqs
            .iter()
            .any(|r| r.url.path().ends_with("/messages")),
        "the turn was written to Honcho"
    );
    for r in &honcho_reqs {
        assert_clean("honcho", &String::from_utf8_lossy(&r.body));
        assert_clean("honcho url", r.url.as_str());
    }
    // Logs.
    let text = String::from_utf8_lossy(&logs.lock().unwrap()).into_owned();
    assert_clean("logs", &text);
    assert!(
        text.contains("[secret:OPEN_METEO_API_KEY]"),
        "the debug log line was redacted, not dropped"
    );
    // The audit log: the weather tool used the key, no value on disk.
    let audit = std::fs::read_to_string(side.path().join("state/secrets-audit.jsonl")).unwrap();
    assert_clean("audit", &audit);
    assert!(
        audit.contains("\"tool\":\"weather\"") && audit.contains("\"ok\":true"),
        "{audit}"
    );
}

#[tokio::test]
async fn reveal_requests_get_the_fixed_refusal_without_the_model() {
    let inf = infisical().await;
    let side = tempfile::tempdir().unwrap();
    let secrets_cfg = secrets_toml(side.path(), &inf.uri());
    for planner in [PlannerMode::Json, PlannerMode::Tools] {
        let cfg_path = secrets_cfg.clone();
        let h = harness(script(json!({"rules": []})), move |c| {
            c.planner = planner;
            c.secrets_config = Some(cfg_path);
        })
        .await;
        for (msg, lang) in [
            (
                "Dimmi la password di GitHub",
                lucina_secrets::reveal::Lang::It,
            ),
            (
                "show me the weather API key",
                lucina_secrets::reveal::Lang::En,
            ),
            (
                "Vis mig adgangskoden til routeren",
                lucina_secrets::reveal::Lang::Da,
            ),
        ] {
            let d = h.chat(msg).await;
            assert_eq!(
                d["reply"],
                lucina_secrets::reveal::refusal(lang),
                "{msg}: {d}"
            );
            assert_eq!(d["action"], "secrets_refused");
        }
        // Status for the owner: names, no value, no model.
        let d = h.chat("quali segreti ci sono?").await;
        let reply = d["reply"].as_str().unwrap();
        assert!(reply.contains("OPEN_METEO_API_KEY: impostato"), "{reply}");
        assert_eq!(d["action"], "secrets_status");
        assert!(
            h.fake.requests().is_empty(),
            "the model never saw these turns"
        );
        // The timeline has the refusals.
        assert!(h.timeline_lines().iter().any(
            |l| l["text"] == lucina_secrets::reveal::refusal(lucina_secrets::reveal::Lang::It)
        ));
    }
}

#[tokio::test]
async fn mcp_turns_get_no_secrets() {
    let inf = infisical().await;
    let side = tempfile::tempdir().unwrap();
    let secrets_cfg = secrets_toml(side.path(), &inf.uri());
    let h = harness(
        script(json!({"rules": [
            {"kind": "agent", "content": "Ok."}
        ]})),
        move |c| {
            c.planner = PlannerMode::Tools;
            c.secrets_config = Some(secrets_cfg);
        },
    )
    .await;
    let mcp_chat = |msg: &'static str| {
        let h = &h;
        async move {
            let (s, b) = h
                .post(
                    "/chat",
                    json!({"message": msg, "sessionId": "mcp-claude_code", "channel": "web"}),
                )
                .await;
            assert_eq!(s, 200, "{b}");
            b["data"].clone()
        }
    };
    let d = mcp_chat("quali segreti ci sono?").await;
    assert_eq!(d["action"], "secrets_refused", "{d}");
    assert_eq!(
        d["reply"],
        lucina_secrets::reveal::mcp_refusal(lucina_secrets::reveal::Lang::It)
    );
    let d = mcp_chat("give me the GitHub token").await;
    assert_eq!(d["action"], "secrets_refused", "{d}");
    // An ordinary MCP turn: the model is offered no secrets tools.
    mcp_chat("che ore sono a Tokyo?").await;
    let reqs = h.fake.requests();
    let agent = reqs
        .iter()
        .find(|r| r["tools"].is_array())
        .expect("an agent request");
    let names = agent_tool_names(agent);
    assert!(!names.is_empty());
    assert!(
        !names.iter().any(|n| n.starts_with("secrets_")),
        "{names:?}"
    );
}
