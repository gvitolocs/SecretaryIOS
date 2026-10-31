use super::*;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(url: &str) -> Jev {
    Jev::new(Config {
        url: format!("{url}/v1/systemone"),
        model: DEFAULT_MODEL.into(),
        key: Some("test-key".into()),
        timeout: Duration::from_secs(2),
        enabled: true,
    })
}

fn questions() -> Vec<(&'static str, Question)> {
    vec![
        (
            "intent",
            Question::choice(
                "What does the user want?",
                [("reminder", "set a reminder"), ("chat", "just chatting")],
            ),
        ),
        ("greeting", Question::noul("Does the message greet?")),
        (
            "urgency",
            Question::score("How urgent?", ["not", "somewhat", "very"]),
        ),
    ]
}

// The shape of a real jev-1.14.0 answer (captured 2026-10-28, intent + greeting).
fn answer() -> Value {
    json!({"model":"jev-1.14.0","answers":{
        "intent":{"type":"choice","choice":"reminder","confidence":0.97,"probabilities":{"reminder":0.97,"chat":0.03}},
        "greeting":{"type":"noul","noul":0.98},
        "urgency":{"type":"score","score":1,"legend":{"0":"not","1":"somewhat","2":"very"},"probabilities":{"0":0.1,"1":0.8,"2":0.1},"confidence":0.8}},
        "usage":{"input_tokens":353,"output_tokens":59}})
}

#[test]
fn parses_every_answer_type() {
    let d = parse_decision(&answer(), &questions()).unwrap();
    assert_eq!(d.model, "jev-1.14.0");
    assert_eq!(d.choice("intent"), Some(("reminder", 0.97)));
    assert_eq!(d.probability("intent", "chat"), Some(0.03));
    assert_eq!(d.yes("greeting"), Some(0.98));
    assert_eq!(d.score("urgency"), Some((1, 0.8)));
    assert_eq!(d.input_tokens, 353);
}

#[test]
fn refuses_unknown_options_missing_answers_and_bad_probabilities() {
    let mut v = answer();
    v["answers"]["intent"]["choice"] = json!("search");
    assert!(matches!(parse_decision(&v, &questions()), Err(JevError::Malformed(_))));

    let mut v = answer();
    v["answers"].as_object_mut().unwrap().remove("greeting");
    assert!(matches!(parse_decision(&v, &questions()), Err(JevError::Malformed(_))));

    let mut v = answer();
    v["answers"]["greeting"]["noul"] = json!(1.4);
    assert!(matches!(parse_decision(&v, &questions()), Err(JevError::Malformed(_))));

    let mut v = answer();
    v["answers"]["intent"]["probabilities"] = json!({"reminder": 0.5, "chat": 0.2});
    assert!(matches!(parse_decision(&v, &questions()), Err(JevError::Malformed(_))));

    let mut v = answer();
    v["answers"]["urgency"]["score"] = json!(3);
    assert!(matches!(parse_decision(&v, &questions()), Err(JevError::Malformed(_))));
}

#[test]
fn validates_questions_before_sending() {
    let one = Question::choice("x", [("a", "only one")]);
    assert!(matches!(one.validate("q"), Err(JevError::Invalid(_))));
    let dup = Question::choice("x", [("a", "1"), ("a", "2")]);
    assert!(matches!(dup.validate("q"), Err(JevError::Invalid(_))));
    let many: Vec<(String, String)> = (0..256).map(|i| (format!("o{i}"), "x".into())).collect();
    assert!(Question::choice("x", many).validate("q").is_err());
    assert!(Question::score("x", ["only"]).validate("q").is_err());
    assert!(Question::noul_with("x", "yes", "no").validate("q").is_ok());
}

#[test]
fn wire_format_uses_lowercase_types_and_criteria() {
    let q = Question::noul_with("Done?", "the page shows it", "it does not");
    assert_eq!(
        q.to_wire(),
        json!({"type":"noul","instructions":"Done?","criteria":{"true":"the page shows it","false":"it does not"}})
    );
    let s = Question::score("How?", ["low", "high"]).to_wire();
    assert_eq!(s["type"], "score");
    assert_eq!(s["criteria"], json!(["low", "high"]));
}

#[test]
fn config_reads_the_environment_and_redacts_the_key() {
    let env = |k: &str| match k {
        "JEV_API_KEY" => Some(" sk-secret ".to_owned()),
        "LUCINA_JEV_TIMEOUT_MS" => Some("1500".to_owned()),
        _ => None,
    };
    let c = Config::from_lookup(&env);
    assert_eq!(c.key.as_deref(), Some("sk-secret"));
    assert_eq!(c.timeout, Duration::from_millis(1500));
    assert_eq!(c.model, DEFAULT_MODEL);
    assert!(c.enabled);
    assert!(!format!("{c:?}").contains("sk-secret"));
    let off = Config::from_lookup(&|k: &str| match k {
        "JEV_API_KEY" => Some("k".to_owned()),
        "LUCINA_JEV" => Some("off".to_owned()),
        _ => None,
    });
    assert!(!Jev::new(off).configured());
}

#[test]
fn clips_long_state() {
    let long = "é".repeat(MAX_STATE_CHARS + 10);
    let Value::String(s) = clip_state(Value::String(long)) else {
        panic!()
    };
    assert!(s.chars().count() <= MAX_STATE_CHARS);
    assert!(s.ends_with("[…clipped]"));
}

#[tokio::test]
async fn decides_over_http_with_the_bearer_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer()))
        .expect(1)
        .mount(&server)
        .await;
    let j = client(&server.uri());
    let d = j
        .decide("test", json!("ricordami il latte"), &questions())
        .await
        .unwrap();
    assert_eq!(d.choice("intent").unwrap().0, "reminder");
    let s = j.stats();
    assert_eq!(s["ok"], 1);
    assert_eq!(s["inputTokens"], 353);
    assert_eq!(s["byPurpose"]["test"]["ok"], 1);
}

#[tokio::test]
async fn quota_opens_the_breaker_and_later_calls_fail_fast() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "60"))
        .expect(1)
        .mount(&server)
        .await;
    let j = client(&server.uri());
    assert_eq!(
        j.decide("t", json!("x"), &questions()).await,
        Err(JevError::Quota(429))
    );
    assert!(!j.available());
    assert!(matches!(
        j.decide("t", json!("x"), &questions()).await,
        Err(JevError::CoolingDown(_))
    ));
    assert_eq!(j.stats()["coolingDown"], "rate limit");
}

#[tokio::test]
async fn credits_and_bad_requests_are_told_apart() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(422).set_body_string("questions.x.type invalid"))
        .mount(&server)
        .await;
    let j = client(&server.uri());
    let e = j.decide("t", json!("x"), &questions()).await.unwrap_err();
    assert_eq!(e.code(), "bad_request");
    assert!(j.available(), "a bad request does not stop later calls");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(402))
        .mount(&server)
        .await;
    let j = client(&server.uri());
    assert_eq!(
        j.decide("t", json!("x"), &questions()).await,
        Err(JevError::Credits(402))
    );
    assert!(!j.available());
}

#[tokio::test]
async fn unconfigured_client_never_calls_out() {
    let j = Jev::new(Config {
        key: None,
        ..client("http://127.0.0.1:9").cfg.clone()
    });
    assert_eq!(
        j.decide("t", json!("x"), &questions()).await,
        Err(JevError::NotConfigured)
    );
    assert_eq!(j.stats()["skipped"], 1);
}

#[test]
fn the_request_keeps_question_and_option_order() {
    let qs = vec![
        ("zeta", Question::choice("Pick", [("open_app", "first"), ("back", "second \"quoted\"")])),
        ("alpha", Question::noul("Done?")),
    ];
    let body = request_body("jev-1.14.0", json!({"task": "x"}), &qs);
    let v: Value = serde_json::from_str(&body).expect("valid JSON");
    assert_eq!(v["questions"]["zeta"]["criteria"]["back"], "second \"quoted\"");
    assert_eq!(v["model"], "jev-1.14.0");
    assert!(body.find("\"zeta\"").unwrap() < body.find("\"alpha\"").unwrap());
    assert!(body.find("\"open_app\"").unwrap() < body.find("\"back\"").unwrap());
}
