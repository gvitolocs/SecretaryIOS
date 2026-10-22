//! Golden transcripts (tests/golden/secretary.json) replayed against the
//! fake DeepSeek: action, reply, prompt-kind sequence and executed tools.

mod common;

use common::{harness, script};
use lucina_server::fake::kind_of;
use lucina_server::PlannerMode;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn golden_secretary_transcripts() {
    let golden: Value = serde_json::from_str(include_str!("golden/secretary.json")).unwrap();
    let meteo = MockServer::start().await;
    Mock::given(method("GET")).and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": [{"name": "Copenhagen", "latitude": 55.676, "longitude": 12.566, "country": "Denmark"}]})))
        .mount(&meteo).await;
    Mock::given(method("GET")).and(path("/v1/forecast"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"current": {"weather_code": 61}, "daily": {"time": ["2026-10-22"], "weather_code": [61]}})))
        .mount(&meteo).await;
    let mut failures = Vec::new();
    for case in golden["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let tools = case["planner"] == "tools";
        let uri = meteo.uri();
        let h = harness(script(json!({"rules": case["rules"]})), move |c| {
            if tools {
                c.planner = PlannerMode::Tools;
            }
            c.endpoints.open_meteo_forecast = uri.clone();
            c.endpoints.open_meteo_geocoding = uri;
        })
        .await;
        let d = h.chat(case["message"].as_str().unwrap()).await;
        let e = &case["expect"];
        let mut check = |ok: bool, what: String| {
            if !ok {
                failures.push(format!("{name}: {what}"));
            }
        };
        check(
            d["action"] == e["action"],
            format!("action {} != {}", d["action"], e["action"]),
        );
        if let Some(r) = e["reply"].as_str() {
            check(d["reply"] == r, format!("reply {:?}", d["reply"]));
        }
        if let Some(p) = e["replyStartsWith"].as_str() {
            check(
                d["reply"].as_str().unwrap_or("").starts_with(p),
                format!("reply {:?}", d["reply"]),
            );
        }
        if let Some(t) = e["reminderText"].as_str() {
            check(
                d["reminder"]["text"] == t,
                format!("reminder {}", d["reminder"]),
            );
        }
        let calls: Vec<&str> = h.fake.requests().iter().map(kind_of).collect();
        let want: Vec<&str> = e["calls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        check(calls == want, format!("calls {calls:?} != {want:?}"));
        let lines = h.timeline_lines();
        let executed: Vec<Value> = lines
            .iter()
            .filter(|l| l["meta"]["kind"] == "tool")
            .cloned()
            .collect();
        let names: Vec<&str> = executed
            .iter()
            .map(|l| l["meta"]["tool"].as_str().unwrap_or(""))
            .collect();
        let want_tools: Vec<&str> = e["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        check(
            names == want_tools,
            format!("tools {names:?} != {want_tools:?}"),
        );
        if let Some(oks) = e["toolOk"].as_array() {
            let got: Vec<Value> = executed.iter().map(|l| l["meta"]["ok"].clone()).collect();
            check(&got == oks, format!("tool ok {got:?}"));
        }
        if e["job"] == true {
            check(d["job"]["status"] == "queued", format!("job {}", d["job"]));
            // The job's own item follows the ack.
            check(
                lines.get(1).is_some_and(|l| l["id"] == d["lucinaItemId"]),
                "timeline shape (job)".into(),
            );
        } else {
            check(
                lines.first().is_some_and(|l| l["role"] == "user")
                    && lines.last().is_some_and(|l| l["id"] == d["lucinaItemId"]),
                "timeline shape".into(),
            );
        }
    }
    assert!(
        failures.is_empty(),
        "golden mismatches:\n{}",
        failures.join("\n")
    );
}
