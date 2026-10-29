//! AU Find (Aarhus University): /api/secretary/au/* and the chat shortcut,
//! against a fake af-server.

mod common;

use common::{harness, script};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn profile(name: &str, au_room: &str, email: &str) -> serde_json::Value {
    json!({"uuid": "u", "name": name, "position": "IT Staff Member", "units": ["AU IT"],
        "phone": "+45 93 50 85 70", "email": email, "building": &au_room[..4], "room": &au_room[5..],
        "au_room": au_room, "address": ["Universitetsbyen 81", "8000 Aarhus C"],
        "profile_url": format!("https://www.au.dk/en/show/person/{email}"), "source": "www.au.dk"})
}

async fn af_server() -> MockServer {
    let af = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status": "ok", "service": "af-server", "version": "0.1.0"})),
        )
        .mount(&af)
        .await;
    // One Vittrup.
    Mock::given(method("GET")).and(path("/api/people/search")).and(query_param("q", "Dennis Vittrup"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"source": "aufind", "total": 1,
            "upstream": "https://aufind.au.dk/searchPeople.php?q=Dennis%20Vittrup",
            "hits": [{"uuid": "u1", "name": "Dennis Henrik Peder Vittrup", "first_name": "Dennis Henrik Peder", "last_name": "Vittrup",
                "profile": profile("Dennis Henrik Peder Vittrup", "1872-237", "dhk@au.dk")}]})))
        .mount(&af).await;
    // Many Jensens.
    let jensens: Vec<_> = (0..5).map(|i| json!({"uuid": format!("j{i}"), "name": format!("Anna Jensen {i}"),
        "profile": profile(&format!("Anna Jensen {i}"), &format!("153{i}-31{i}"), &format!("aj{i}@au.dk"))})).collect();
    Mock::given(method("GET"))
        .and(path("/api/people/search"))
        .and(query_param("q", "Jensen"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"source": "aufind", "total": 387, "hits": jensens})),
        )
        .mount(&af)
        .await;
    // Nobody else.
    Mock::given(method("GET"))
        .and(path("/api/people/search"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"source": "aufind", "total": 0, "hits": []})),
        )
        .mount(&af)
        .await;
    let b1872 = json!({"id": "1872", "names": ["Bygning 1872"], "address": "Universitetsbyen 81, 8000 Aarhus C",
        "latitude": 56.1745, "longitude": 10.1972, "polygon": [[56.1744, 10.1971], [56.1746, 10.1973], [56.1744, 10.1974]]});
    Mock::given(method("GET"))
        .and(path("/api/buildings/1872"))
        .respond_with(ResponseTemplate::new(200).set_body_json(b1872.clone()))
        .mount(&af)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/buildings"))
        .and(query_param("q", "1872"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([b1872])))
        .mount(&af)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/buildings/1872/map"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(vec![0x89, b'P', b'N', b'G', 9])
                .insert_header("content-type", "image/png"),
        )
        .mount(&af)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/buildings/9999"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "not found"})))
        .mount(&af)
        .await;
    af
}

#[tokio::test]
async fn routes_run_the_procedure_against_af_server() {
    let af = af_server().await;
    let uri = af.uri();
    let h = harness(script(json!({"rules": []})), |c| {
        c.au_enabled = true;
        c.au_url = uri;
    })
    .await;

    let (s, b) = h.get("/au/health").await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["reachable"], true, "{b}");

    let (s, b) = h.get("/au/people?q=Dennis%20Vittrup").await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["status"], "found", "{b}");
    assert_eq!(b["data"]["source"], "aufind");
    assert_eq!(b["data"]["people"][0]["au_room"], "1872-237");
    assert_eq!(b["data"]["people"][0]["email"], "dhk@au.dk");

    let (s, b) = h.get("/au/people?q=Jensen").await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["status"], "ambiguous", "{b}");
    assert_eq!(b["data"]["people"].as_array().unwrap().len(), 3, "{b}");

    // Surname, then prefix, then "not found in AU Find".
    let (s, b) = h.get("/au/people?q=Surayya%20Zzzqx").await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["status"], "not_found", "{b}");
    assert_eq!(b["data"]["tried"].as_array().unwrap().len(), 3, "{b}");

    let (s, b) = h.get("/au/buildings?q=1872").await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["buildings"][0]["id"], "1872", "{b}");

    let r = h
        .http
        .get(h.url("/au/map.png?building=1872&z=17"))
        .bearer_auth(common::TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "image/png");
    assert_eq!(&r.bytes().await.unwrap()[..4], &[0x89, b'P', b'N', b'G']);
    let (s, _) = h.get("/au/map.png?building=9999").await;
    assert_eq!(s, 404);
    let (s, _) = h.get("/au/map.png?building=1872&z=30").await;
    assert_eq!(s, 400);
    let (s, _) = h.get("/au/people?q=").await;
    assert_eq!(s, 400);
    let (s, _, _) = h.call(reqwest::Method::POST, "/au/people?q=x", None).await;
    assert_eq!(s, 405);
    // without the token nothing is relayed
    let r = h
        .http
        .get(h.url("/au/people?q=Jensen"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn routes_say_when_af_server_is_down_or_au_is_off() {
    let h = harness(script(json!({"rules": []})), |c| {
        c.au_enabled = true;
        c.au_url = "http://127.0.0.1:9".into();
    })
    .await;
    let (s, b) = h.get("/au/health").await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["reachable"], false, "{b}");
    let (s, b) = h.get("/au/people?q=Vittrup").await;
    assert_eq!(s, 503, "{b}");
    assert_eq!(b["error"], "au_unavailable", "{b}");

    let off = harness(script(json!({"rules": []})), |_| {}).await;
    let (s, b) = off.get("/au/people?q=Vittrup").await;
    assert_eq!(s, 404, "{b}");
    assert_eq!(b["error"], "au_disabled", "{b}");
}

#[tokio::test]
async fn chat_answers_from_au_find_with_its_source_and_a_map() {
    let af = af_server().await;
    let uri = af.uri();
    let h = harness(
        script(json!({"rules": [
            {"kind": "reply", "content": "Dennis Henrik Peder Vittrup is an IT Staff Member at AU IT, room 1872-237 (dhk@au.dk). Source: AU Find (aufind)."}
        ]})),
        |c| {
            c.au_enabled = true;
            c.au_url = uri;
        },
    )
    .await;
    let d = h.chat("Who is Dennis Vittrup at AU?").await;
    assert_eq!(d["action"], "au_find", "{d}");
    assert!(d["reply"].as_str().unwrap().contains("1872-237"), "{d}");
    // The knowledge block and the answer contract reached the reply prompt.
    let reqs = h.fake.state.requests.lock().unwrap().clone();
    let all = reqs.iter().map(|r| r.to_string()).collect::<String>();
    assert!(all.contains("dhk@au.dk"), "the AU Find block is missing");
    assert!(
        all.contains("Never invent emails"),
        "the answer contract is missing"
    );
    let blocks = d["blocks"].as_array().cloned().unwrap_or_default();
    assert!(
        blocks
            .iter()
            .any(|b| b["type"] == "image" && b["src"] == "/v1/au/map.png?building=1872&z=17"),
        "no map image block: {d}"
    );
    assert!(
        blocks.iter().any(|b| b["type"] == "place"),
        "no place card: {d}"
    );

    // Not found: the reply offers the au.dk search.
    let d = h.chat("Who is Surayya Zzzqx at AU?").await;
    assert_eq!(d["action"], "au_find", "{d}");
    let actions = d["actions"].as_array().cloned().unwrap_or_default();
    assert!(
        actions.iter().any(|a| a["id"] == "au-search"),
        "no au.dk search button: {d}"
    );
}
