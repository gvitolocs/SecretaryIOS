//! End to end against a mock backend, a mock Instagram CDN and a mock lucina-media (wiremock),
//! through the real router: gateway, signed URLs, cache, rate limits, save.

mod common;

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use common::*;
use secretary_instagram::media::Signer;
use serde_json::{json, Value};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, ResponseTemplate};

// ---- gateway --------------------------------------------------------------------------------

#[tokio::test]
async fn every_route_but_signed_media_needs_the_gateway_header() {
    let e = env().await;
    let routes = [
        ("GET", "/v1/instagram/status"),
        ("GET", "/v1/instagram/profile/natgeo"),
        ("GET", "/v1/instagram/posts/natgeo"),
        ("GET", "/v1/instagram/reels/natgeo"),
        ("GET", "/v1/instagram/stories/natgeo"),
        ("GET", "/v1/instagram/highlights/natgeo"),
        ("GET", "/v1/instagram/highlights/items?id=highlight:1"),
        (
            "GET",
            "/v1/instagram/resolve?url=https://www.instagram.com/p/DeXzbFEl5DG/",
        ),
        ("GET", "/v1/instagram/recent"),
        ("POST", "/v1/instagram/save"),
    ];
    for (m, uri) in routes {
        for gw in [
            None,
            Some("wrong"),
            Some(""),
            Some("gateway-secret-for-test"),
        ] {
            let r = send(&e.app, m, uri, gw, Some(json!({"items": []})), &[]).await;
            assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{m} {uri} with {gw:?}");
            assert_eq!(r.json["error"], "unauthorized");
        }
    }
    // Nothing reached the backend.
    assert_eq!(e.server.received_requests().await.unwrap().len(), 0);
    let r = get(&e.app, "/v1/instagram/status").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["ok"], true);
}

// ---- profile and items ----------------------------------------------------------------------

#[tokio::test]
async fn the_profile_has_only_our_signed_urls_and_drops_foreign_hosts() {
    let e = env().await;
    let mut b = bundle();
    // A media item whose URL points somewhere else is dropped, never signed.
    b["posts"]["items"].as_array_mut().unwrap().push(json!({
        "id": "DeEVIL", "media_type": "image", "thumbnail_url": "https://evil.test/t.jpg",
        "media_url": worker("https://evil.test/x.jpg"), "caption": null, "taken_at": null,
        "like_count": 0, "comment_count": 0, "is_video": false, "carousel_urls": []
    }));
    mount_json(&e.server, "/api/v1/users/natgeo/bundle", b).await;

    let r = get(&e.app, "/v1/instagram/profile/@NatGeo").await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.bytes)
    );
    let text = String::from_utf8_lossy(&r.bytes).to_string();
    assert!(
        !text.contains("workers.dev") && !text.contains("yodams") && !text.contains("evil.test"),
        "{text}"
    );
    let d = &r.json["data"];
    assert_eq!(d["profile"]["username"], "natgeo");
    assert_eq!(d["profile"]["fullName"], "National Geographic");
    assert_eq!(d["profile"]["isVerified"], true);
    assert_eq!(d["profile"]["followerCount"], 1000);
    assert_eq!(d["highlights"][0]["title"], "BOTW");
    assert_eq!(d["highlights"][0]["mediaCount"], 3);

    let items = d["posts"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "the foreign item is dropped");
    assert_eq!(d["posts"]["nextCursor"], "CUR1");
    assert_eq!(items[0]["kind"], "image");
    assert_eq!(items[0]["caption"], "caption of DeAAAAAAAA1");
    assert_eq!(items[0]["takenAt"], 1_700_000_000);
    assert_eq!(
        items[0]["permalink"],
        "https://www.instagram.com/p/DeAAAAAAAA1/"
    );
    assert_eq!(items[1]["kind"], "video");
    assert_eq!(items[2]["kind"], "carousel");
    let kids = items[2]["children"].as_array().unwrap();
    assert_eq!(kids.len(), 3);
    assert_eq!(kids[0]["kind"], "image");
    assert_eq!(kids[1]["kind"], "video");
    assert_eq!(kids[1]["thumbUrl"], Value::Null);

    let mut urls = Vec::new();
    all_urls(d, &mut urls);
    assert!(urls.len() >= 10);
    for u in urls {
        assert!(u.starts_with("/v1/instagram/s/"), "{u}");
    }
}

/// Recorded from the backend on 2026-10-29 (the bundle's posts cut to two).
fn recorded(name: &str) -> Value {
    let text = match name {
        "bundle" => include_str!("fixtures/natgeo.bundle.json"),
        "highlights" => include_str!("fixtures/natgeo.hl.json"),
        "items" => include_str!("fixtures/natgeo.items.json"),
        _ => unreachable!(),
    };
    serde_json::from_str(text).unwrap()
}

#[tokio::test]
async fn highlights_left_out_of_the_bundle_come_from_the_highlights_list() {
    let e = env().await;
    assert_eq!(recorded("bundle")["highlights"], json!([]));
    mount_json(&e.server, "/api/v1/users/natgeo/bundle", recorded("bundle")).await;
    mount_json(
        &e.server,
        "/api/v1/users/natgeo/highlights",
        recorded("highlights"),
    )
    .await;
    mount_json(&e.server, "/api/v1/highlights/items", recorded("items")).await;

    let r = get(&e.app, "/v1/instagram/profile/natgeo").await;
    assert_eq!(r.status, StatusCode::OK);
    let d = &r.json["data"];
    assert_eq!(d["profile"]["username"], "natgeo");
    assert_eq!(d["posts"]["items"].as_array().unwrap().len(), 2);
    let hs = d["highlights"].as_array().unwrap();
    assert_eq!(hs.len(), 1);
    assert_eq!(hs[0]["id"], "highlight:18023864549864045");
    assert_eq!(hs[0]["title"], "BOTW 2027");
    assert!(
        hs[0]["coverUrl"]
            .as_str()
            .unwrap()
            .starts_with("/v1/instagram/s/"),
        "{}",
        hs[0]["coverUrl"]
    );
    assert_eq!(
        hs[0]["mediaCount"],
        Value::Null,
        "the backend's 0 is not a count"
    );

    // The highlights route shares the profile's cache entry.
    let r = get(&e.app, "/v1/instagram/highlights/natgeo").await;
    assert_eq!(r.json["data"]["items"][0]["title"], "BOTW 2027");
    assert_eq!(count(&e.server, "/api/v1/users/natgeo/highlights").await, 1);

    let r = get(
        &e.app,
        "/v1/instagram/highlights/items?id=highlight%3A18023864549864045",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["data"]["items"].as_array().unwrap().len(), 4);
    assert_eq!(r.json["data"]["nextCursor"], Value::Null);
}

#[tokio::test]
async fn a_private_profile_never_asks_for_the_highlights_list() {
    let e = env().await;
    let mut b = recorded("bundle");
    b["profile"]["is_private"] = json!(true);
    mount_json(&e.server, "/api/v1/users/natgeo/bundle", b).await;
    mount_json(
        &e.server,
        "/api/v1/users/natgeo/highlights",
        recorded("highlights"),
    )
    .await;
    let r = get(&e.app, "/v1/instagram/profile/natgeo").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["data"]["highlights"], json!([]));
    assert_eq!(count(&e.server, "/api/v1/users/natgeo/highlights").await, 0);
}

#[tokio::test]
async fn a_bundle_with_highlights_needs_no_second_call() {
    let e = env().await;
    mount_json(&e.server, "/api/v1/users/natgeo/bundle", bundle()).await;
    let r = get(&e.app, "/v1/instagram/profile/natgeo").await;
    assert_eq!(r.json["data"]["highlights"][0]["title"], "BOTW");
    assert_eq!(count(&e.server, "/api/v1/users/natgeo/highlights").await, 0);
}

#[tokio::test]
async fn a_failing_highlights_list_still_gives_the_profile() {
    let e = env().await;
    mount_json(&e.server, "/api/v1/users/natgeo/bundle", recorded("bundle")).await;
    Mock::given(method("GET"))
        .and(path("/api/v1/users/natgeo/highlights"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&e.server)
        .await;
    let r = get(&e.app, "/v1/instagram/profile/natgeo").await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.bytes)
    );
    assert_eq!(r.json["data"]["profile"]["username"], "natgeo");
    assert_eq!(r.json["data"]["highlights"], json!([]));
    assert!(count(&e.server, "/api/v1/users/natgeo/highlights").await >= 1);
}

#[tokio::test]
async fn bad_input_is_a_400_and_never_reaches_the_backend() {
    let e = env().await;
    for uri in [
        "/v1/instagram/profile/a%20b",
        "/v1/instagram/profile/..%2Fx",
        "/v1/instagram/posts/x?cursor=a%20b",
        "/v1/instagram/highlights/items?id=notahighlight",
        "/v1/instagram/highlights/items",
        "/v1/instagram/resolve?url=https://evil.test/p/abc/",
        "/v1/instagram/resolve?url=https://www.instagram.com/natgeo/",
        "/v1/instagram/resolve",
    ] {
        let r = get(&e.app, uri).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(r.json["error"], "bad_request");
    }
    assert_eq!(e.server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn stories_highlights_reels_resolve_and_not_found() {
    let e = env().await;
    mount_json(
        &e.server,
        "/api/v1/users/natgeo/stories",
        page(
            vec![
                item("4015963173684253222", "video", "s1.mp4", &[]),
                item("4015963173684253223", "image", "s2.jpg", &[]),
            ],
            None,
        ),
    )
    .await;
    mount_json(&e.server, "/api/v1/users/natgeo/highlights", json!([{"id": "highlight:1", "title": "A", "cover_url": worker(&cdn("h.jpg")), "media_count": 2}])).await;
    mount_json(
        &e.server,
        "/api/v1/highlights/items",
        page(vec![item("111", "image", "h1.jpg", &[])], Some("NEXT")),
    )
    .await;
    mount_json(
        &e.server,
        "/api/v1/users/natgeo/reels",
        page(
            vec![item("DeRRRRRRRR1", "video", "r1.mp4", &[])],
            Some("RC2"),
        ),
    )
    .await;
    mount_json(
        &e.server,
        "/api/v1/resolve",
        page(
            vec![item(
                "DeXzbFEl5DG",
                "carousel",
                "x0.jpg",
                &["x0.jpg", "x1.jpg"],
            )],
            None,
        ),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/users/ghost/stories"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"detail": "User not found"})))
        .mount(&e.server)
        .await;

    let r = get(&e.app, "/v1/instagram/stories/natgeo").await;
    assert_eq!(r.status, StatusCode::OK);
    let items = r.json["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["kind"], "video");
    assert_eq!(
        items[0]["permalink"],
        Value::Null,
        "story ids are numeric: no permalink"
    );
    assert_eq!(r.json["data"]["nextCursor"], Value::Null);

    let r = get(&e.app, "/v1/instagram/highlights/natgeo").await;
    assert_eq!(r.json["data"]["items"][0]["id"], "highlight:1");
    assert!(r.json["data"]["items"][0]["coverUrl"]
        .as_str()
        .unwrap()
        .starts_with("/v1/instagram/s/"));

    let r = get(
        &e.app,
        "/v1/instagram/highlights/items?id=highlight%3A1&cursor=abc",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["data"]["nextCursor"], "NEXT");
    let seen = e.server.received_requests().await.unwrap();
    let hi = seen
        .iter()
        .find(|r| r.url.path() == "/api/v1/highlights/items")
        .unwrap();
    assert_eq!(hi.url.query(), Some("id=highlight%3A1&cursor=abc"));

    let r = get(&e.app, "/v1/instagram/reels/natgeo?cursor=RC1").await;
    assert_eq!(r.json["data"]["items"][0]["kind"], "video");
    assert_eq!(
        r.json["data"]["items"][0]["permalink"],
        "https://www.instagram.com/reel/DeRRRRRRRR1/"
    );
    assert_eq!(r.json["data"]["nextCursor"], "RC2");
    let seen = e.server.received_requests().await.unwrap();
    assert!(seen.iter().any(
        |r| r.url.path() == "/api/v1/users/natgeo/reels" && r.url.query() == Some("cursor=RC1")
    ));

    let r = get(
        &e.app,
        "/v1/instagram/resolve?url=https%3A%2F%2Fwww.instagram.com%2Fp%2FDeXzbFEl5DG%2F",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["data"]["items"][0]["kind"], "carousel");
    assert_eq!(
        r.json["data"]["items"][0]["children"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let r = get(&e.app, "/v1/instagram/stories/ghost").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json["error"], "not_found");
}

#[tokio::test]
async fn upstream_json_is_cached_for_a_short_time() {
    let e = env().await;
    mount_json(&e.server, "/api/v1/users/natgeo/bundle", bundle()).await;
    for _ in 0..3 {
        assert_eq!(
            get(&e.app, "/v1/instagram/profile/natgeo").await.status,
            StatusCode::OK
        );
    }
    assert_eq!(count(&e.server, "/api/v1/users/natgeo/bundle").await, 1);
}

#[tokio::test]
async fn a_posts_page_that_is_the_first_page_again_is_the_end() {
    // The backend ignores the cursor on /posts (live, 2026-10-28): the same posts come back.
    let e = env().await;
    mount_json(&e.server, "/api/v1/users/natgeo/bundle", bundle()).await;
    mount_json(
        &e.server,
        "/api/v1/users/natgeo/posts",
        page(posts_items(), Some("CUR2")),
    )
    .await;
    assert_eq!(
        get(&e.app, "/v1/instagram/profile/natgeo").await.status,
        StatusCode::OK
    );

    let r = get(&e.app, "/v1/instagram/posts/natgeo?cursor=CUR1").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["data"]["items"].as_array().unwrap().len(), 0);
    assert_eq!(r.json["data"]["nextCursor"], Value::Null);
    assert_eq!(r.json["data"]["exhausted"], true);

    // Without a cursor it is just the page.
    let r = get(&e.app, "/v1/instagram/posts/natgeo").await;
    assert_eq!(r.json["data"]["items"].as_array().unwrap().len(), 3);
    assert_eq!(r.json["data"]["nextCursor"], "CUR2");
}

#[tokio::test]
async fn a_posts_page_with_new_posts_passes_through() {
    let e = env().await;
    mount_json(&e.server, "/api/v1/users/natgeo/bundle", bundle()).await;
    mount_json(
        &e.server,
        "/api/v1/users/natgeo/posts",
        page(
            vec![item("DeNEWNEWNE1", "image", "n1.jpg", &[])],
            Some("CUR3"),
        ),
    )
    .await;
    get(&e.app, "/v1/instagram/profile/natgeo").await;
    let r = get(&e.app, "/v1/instagram/posts/natgeo?cursor=CUR1").await;
    assert_eq!(r.json["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(r.json["data"]["nextCursor"], "CUR3");
    assert!(r.json["data"].get("exhausted").is_none());
}

#[tokio::test]
async fn opened_profiles_are_remembered_newest_first_in_a_private_file() {
    let e = env().await;
    mount_json(&e.server, "/api/v1/users/natgeo/bundle", bundle()).await;
    let mut other = bundle();
    other["profile"]["username"] = json!("nasa");
    other["profile"]["full_name"] = json!("NASA");
    mount_json(&e.server, "/api/v1/users/nasa/bundle", other).await;
    get(&e.app, "/v1/instagram/profile/natgeo").await;
    get(&e.app, "/v1/instagram/profile/nasa").await;
    get(&e.app, "/v1/instagram/profile/natgeo").await;
    let r = get(&e.app, "/v1/instagram/recent").await;
    let items = r.json["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["username"], "natgeo");
    assert_eq!(items[1]["username"], "nasa");
    assert_eq!(items[1]["fullName"], "NASA");
    assert!(items[0]["picUrl"]
        .as_str()
        .unwrap()
        .starts_with("/v1/instagram/s/"));
    let p = e.dir.path().join("recent.json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    // A failed lookup is not remembered.
    let r = get(&e.app, "/v1/instagram/profile/nobody").await;
    assert_ne!(r.status, StatusCode::OK);
    assert_eq!(
        get(&e.app, "/v1/instagram/recent").await.json["data"]["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

// ---- media: signed URLs, allowlist, cache, ranges -------------------------------------------

#[tokio::test]
async fn signed_media_is_fetched_once_cached_and_range_capable() {
    let e = env().await;
    mount_media(&e.server, "p1.jpg", JPEG, "image/jpeg").await;
    let signed = e.st.service.signer.url_for(&cdn("p1.jpg"));

    // No gateway header needed.
    let r = send(&e.app, "GET", &signed, None, None, &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.bytes, JPEG);
    assert_eq!(r.headers["content-type"], "image/jpeg");
    assert_eq!(r.headers["accept-ranges"], "bytes");
    assert_eq!(r.headers["x-content-type-options"], "nosniff");
    assert!(r.headers["cache-control"]
        .to_str()
        .unwrap()
        .contains("private"));

    // A second URL for the same picture (new oh/oe) hits the cache.
    let signed2 =
        e.st.service.signer.url_for(
            "https://scontent-other.cdninstagram.com/v/t51/p1.jpg?stp=dst-jpg_e35&oh=9&oe=9",
        );
    let r = send(&e.app, "GET", &signed2, None, None, &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.bytes, JPEG);

    let r = send(
        &e.app,
        "GET",
        &signed,
        None,
        None,
        &[("range", "bytes=2-5")],
    )
    .await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.bytes, &JPEG[2..6]);
    assert_eq!(
        r.headers["content-range"],
        format!("bytes 2-5/{}", JPEG.len()).as_str()
    );
    let r = send(&e.app, "GET", &signed, None, None, &[("range", "bytes=-4")]).await;
    assert_eq!(r.bytes, &JPEG[JPEG.len() - 4..]);
    let r = send(
        &e.app,
        "GET",
        &signed,
        None,
        None,
        &[("range", "bytes=9999-")],
    )
    .await;
    assert_eq!(r.status, StatusCode::RANGE_NOT_SATISFIABLE);
    let r = send(&e.app, "HEAD", &signed, None, None, &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["content-length"], JPEG.len().to_string().as_str());
    assert!(r.bytes.is_empty());

    assert_eq!(
        count(&e.server, "/cdn/").await,
        1,
        "one upstream fetch for all of that"
    );
}

#[tokio::test]
async fn concurrent_requests_for_one_media_item_fetch_it_once() {
    let e = env().await;
    Mock::given(method("GET"))
        .and(path_regex("^/cdn/.*/slow.mp4$"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "video/mp4")
                .set_delay(Duration::from_millis(200))
                .set_body_bytes(vec![9u8; 4096]),
        )
        .mount(&e.server)
        .await;
    let signed = e.st.service.signer.url_for(&cdn("slow.mp4"));
    let mut tasks = Vec::new();
    for _ in 0..5 {
        let app = e.app.clone();
        let u = signed.clone();
        tasks.push(tokio::spawn(async move {
            send(&app, "GET", &u, None, None, &[("range", "bytes=0-9")]).await
        }));
    }
    for t in tasks {
        let r = t.await.unwrap();
        assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(r.headers["content-type"], "video/mp4");
    }
    assert_eq!(count(&e.server, "/cdn/").await, 1);
}

#[tokio::test]
async fn bad_expired_and_foreign_signed_urls_are_refused_without_a_fetch() {
    let e = env().await;
    mount_media(&e.server, "p1.jpg", JPEG, "image/jpeg").await;
    let signer = &e.st.service.signer;
    let good = signer.url_for(&cdn("p1.jpg"));

    // Bad signature: one hex digit flipped.
    let mut parts: Vec<String> = good.split('/').map(String::from).collect();
    let sig = parts[5].clone();
    let flipped = if sig.starts_with('0') { "1" } else { "0" };
    parts[5] = format!("{flipped}{}", &sig[1..]);
    let r = send(&e.app, "GET", &parts.join("/"), None, None, &[]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // Signed with another key.
    let other = Signer::new(vec![9; 32]).url_for(&cdn("p1.jpg"));
    assert_eq!(
        send(&e.app, "GET", &other, None, None, &[]).await.status,
        StatusCode::FORBIDDEN
    );

    // Tampered token (a different picture under the old signature).
    let mut parts: Vec<String> = good.split('/').map(String::from).collect();
    parts[6] = format!("{}A", parts[6]);
    assert_eq!(
        send(&e.app, "GET", &parts.join("/"), None, None, &[])
            .await
            .status,
        StatusCode::FORBIDDEN
    );

    // Expired.
    let old = signer.url_with_exp(&cdn("p1.jpg"), chrono::Utc::now().timestamp() - 10);
    let r = send(&e.app, "GET", &old, None, None, &[]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json["error"], "forbidden");

    // Garbage in the expiry slot.
    assert_eq!(
        send(
            &e.app,
            "GET",
            "/v1/instagram/s/abc/def/ghi",
            None,
            None,
            &[]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );

    // A correctly signed URL for a host that is not Instagram's CDN is still refused.
    for evil in [
        "https://evil.test/x.jpg",
        "https://scontent.cdninstagram.com.evil.test/x.jpg",
        "http://scontent.cdninstagram.com/x.jpg",
        "https://127.0.0.1/x.jpg",
        "https://fbcdn.net/x.jpg",
    ] {
        let u = signer.url_for(evil);
        let r = send(&e.app, "GET", &u, None, None, &[]).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{evil}");
    }
    assert_eq!(
        count(&e.server, "/cdn/").await,
        0,
        "nothing was fetched for any of them"
    );
}

#[tokio::test]
async fn media_that_is_not_media_or_has_expired_upstream_is_refused() {
    let e = env().await;
    mount_media(&e.server, "page.jpg", b"<html>login</html>", "text/html").await;
    Mock::given(method("GET"))
        .and(path_regex("^/cdn/.*/gone.jpg$"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&e.server)
        .await;
    let signer = &e.st.service.signer;
    let r = send(
        &e.app,
        "GET",
        &signer.url_for(&cdn("page.jpg")),
        None,
        None,
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    let r = send(
        &e.app,
        "GET",
        &signer.url_for(&cdn("gone.jpg")),
        None,
        None,
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::GONE);
    assert_eq!(r.json["error"], "media_expired");
    // Nothing half-written is left in the cache.
    let parts: Vec<_> = std::fs::read_dir(e.dir.path().join("cache/media"))
        .unwrap()
        .flatten()
        .collect();
    assert!(parts.is_empty(), "{parts:?}");
}

#[tokio::test]
async fn the_cache_cap_evicts_the_oldest_media() {
    let e = env_with(Opts {
        cache_max: 2_500,
        ..Default::default()
    })
    .await;
    let signer = &e.st.service.signer;
    for n in 1..=4 {
        mount_media(
            &e.server,
            &format!("m{n}.jpg"),
            &vec![n as u8; 1000],
            "image/jpeg",
        )
        .await;
    }
    for n in 1..=4 {
        let r = send(
            &e.app,
            "GET",
            &signer.url_for(&cdn(&format!("m{n}.jpg"))),
            None,
            None,
            &[],
        )
        .await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(r.bytes.len(), 1000);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let cache = &e.st.service.cache;
    assert!(cache.total_bytes() <= 2_500, "{}", cache.total_bytes());
    assert_eq!(count(&e.server, "/cdn/").await, 4);
    // The newest is still cached, the oldest was evicted and is fetched again.
    send(
        &e.app,
        "GET",
        &signer.url_for(&cdn("m4.jpg")),
        None,
        None,
        &[],
    )
    .await;
    assert_eq!(count(&e.server, "/cdn/").await, 4);
    send(
        &e.app,
        "GET",
        &signer.url_for(&cdn("m1.jpg")),
        None,
        None,
        &[],
    )
    .await;
    assert_eq!(count(&e.server, "/cdn/").await, 5);
}

// ---- rate limits ----------------------------------------------------------------------------

#[tokio::test]
async fn upstream_429_backs_off_then_answers_a_clean_429_and_then_leaves_the_backend_alone() {
    let e = env().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/users/natgeo/bundle"))
        .respond_with(
            ResponseTemplate::new(429).set_body_json(json!({"detail": "Too many requests"})),
        )
        .mount(&e.server)
        .await;
    let t = Instant::now();
    let r = get(&e.app, "/v1/instagram/profile/natgeo").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.json["ok"], false);
    assert_eq!(r.json["error"], "rate_limited");
    let retry = r.json["retryAfter"].as_u64().unwrap();
    assert!((1..=2).contains(&retry), "{retry}");
    assert_eq!(r.headers["retry-after"], retry.to_string().as_str());
    // First attempt plus two retries, with growing waits (10 ms, 20 ms plus jitter).
    assert_eq!(count(&e.server, "/api/v1/users/natgeo/bundle").await, 3);
    assert!(
        t.elapsed() >= Duration::from_millis(30),
        "{:?}",
        t.elapsed()
    );

    // Blocked: the next call (any route) fails fast without contacting the backend.
    let r = get(&e.app, "/v1/instagram/stories/nasa").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.json["error"], "rate_limited");
    assert_eq!(count(&e.server, "/api/v1/").await, 3);
}

#[tokio::test]
async fn retry_after_is_honoured() {
    let e = env().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/users/natgeo/bundle"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
        .up_to_n_times(1)
        .mount(&e.server)
        .await;
    mount_json(&e.server, "/api/v1/users/natgeo/bundle", bundle()).await;
    let t = Instant::now();
    let r = get(&e.app, "/v1/instagram/profile/natgeo").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        t.elapsed() >= Duration::from_millis(1000),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(count(&e.server, "/api/v1/users/natgeo/bundle").await, 2);
}

#[tokio::test]
async fn a_long_retry_after_is_not_waited_out_but_blocks_the_backend() {
    let e = env().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/users/natgeo/bundle"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "120"))
        .mount(&e.server)
        .await;
    let t = Instant::now();
    let r = get(&e.app, "/v1/instagram/profile/natgeo").await;
    assert!(t.elapsed() < Duration::from_secs(2));
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.json["retryAfter"], 120);
    assert_eq!(count(&e.server, "/api/v1/").await, 1);
}

#[tokio::test]
async fn upstream_5xx_is_retried_then_a_502() {
    let e = env().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/users/natgeo/stories"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&e.server)
        .await;
    let r = get(&e.app, "/v1/instagram/stories/natgeo").await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert_eq!(r.json["error"], "upstream_unavailable");
    assert_eq!(count(&e.server, "/api/v1/users/natgeo/stories").await, 3);
    // A 5xx does not block the backend.
    mount_json(&e.server, "/api/v1/users/nasa/stories", page(vec![], None)).await;
    assert_eq!(
        get(&e.app, "/v1/instagram/stories/nasa").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn backend_requests_are_one_at_a_time_with_a_minimum_spacing() {
    let e = env_with(Opts {
        api_interval: Duration::from_millis(200),
        ..Default::default()
    })
    .await;
    for u in ["a", "b", "c"] {
        mount_json(
            &e.server,
            &format!("/api/v1/users/{u}/stories"),
            page(vec![], None),
        )
        .await;
    }
    let t = Instant::now();
    let mut tasks = Vec::new();
    for u in ["a", "b", "c"] {
        let app = e.app.clone();
        tasks.push(tokio::spawn(async move {
            get(&app, &format!("/v1/instagram/stories/{u}")).await
        }));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap().status, StatusCode::OK);
    }
    assert!(
        t.elapsed() >= Duration::from_millis(400),
        "{:?}",
        t.elapsed()
    );
}

// ---- save to Lucina -------------------------------------------------------------------------

#[tokio::test]
async fn save_downloads_then_calls_lucina_media_ingest() {
    let e = env().await;
    mount_media(&e.server, "p1.jpg", JPEG, "image/jpeg").await;
    mount_media(&e.server, "v1.mp4", &[3u8; 600], "video/mp4").await;
    Mock::given(method("POST"))
        .and(path("/internal/v1/ingest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"item_id": "itm_1", "duplicate": false, "version": 1, "kind": "photo"}),
        ))
        .up_to_n_times(1)
        .mount(&e.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/internal/v1/ingest"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"item_id": "itm_2", "duplicate": true})),
        )
        .mount(&e.server)
        .await;
    let signer = &e.st.service.signer;
    let img = signer.url_for(&cdn("p1.jpg"));
    let vid = signer.url_for(&cdn("v1.mp4"));
    let evil = signer.url_for("https://evil.test/x.jpg");

    let r = send(
        &e.app,
        "POST",
        "/v1/instagram/save",
        Some(SECRET),
        Some(json!({"items": [
            {"url": img, "kind": "image", "username": "@NatGeo", "takenAt": 1_700_000_000, "caption": "hello world"},
            {"url": vid, "kind": "video", "username": "natgeo"},
            {"url": "https://getsecretary.tech/nothing", "kind": "image", "username": "natgeo"},
            {"url": evil, "kind": "image", "username": "natgeo"},
            {"url": img, "kind": "audio", "username": "natgeo"},
        ]})),
        &[],
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.bytes)
    );
    let d = &r.json["data"];
    assert_eq!(d["saved"].as_array().unwrap().len(), 2);
    assert_eq!(d["saved"][0]["itemId"], "itm_1");
    assert_eq!(d["saved"][0]["duplicate"], false);
    assert_eq!(d["saved"][1]["itemId"], "itm_2");
    assert_eq!(d["saved"][1]["duplicate"], true);
    let failed = d["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 3);
    assert_eq!(failed[0]["error"], "bad_url");
    assert_eq!(failed[1]["error"], "forbidden");
    assert_eq!(failed[2]["error"], "bad_request");

    let reqs: Vec<_> = e
        .server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == "/internal/v1/ingest")
        .collect();
    assert_eq!(reqs.len(), 2);
    let a: Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(a["source"], "instagram");
    assert_eq!(a["album"], "Instagram");
    assert_eq!(a["caption"], "@natgeo hello world");
    assert_eq!(a["move"], true);
    assert_eq!(a["mime"], "image/jpeg");
    assert!(a["name"].as_str().unwrap().starts_with("instagram-natgeo-"));
    assert!(a["name"].as_str().unwrap().ends_with(".jpg"));
    assert_eq!(a["taken_at"], "2023-11-14T22:13:20+00:00");
    let spooled = std::path::PathBuf::from(a["path"].as_str().unwrap());
    assert!(spooled.starts_with(e.dir.path().join("spool")));
    assert_eq!(
        std::fs::read(&spooled).unwrap(),
        JPEG,
        "the file was there when ingest was called"
    );
    let b: Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert_eq!(b["caption"], "@natgeo");
    assert_eq!(b["mime"], "video/mp4");
    assert_eq!(b["taken_at"], Value::Null);
    // The cache copy is untouched (the spool file is what lucina-media moves).
    assert!(e.st.service.cache.total_bytes() >= (JPEG.len() + 600) as u64);
}

#[tokio::test]
async fn a_refused_ingest_is_reported_and_leaves_no_spool_file() {
    let e = env().await;
    mount_media(&e.server, "p1.jpg", JPEG, "image/jpeg").await;
    Mock::given(method("POST"))
        .and(path("/internal/v1/ingest"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&e.server)
        .await;
    let img = e.st.service.signer.url_for(&cdn("p1.jpg"));
    let r = send(
        &e.app,
        "POST",
        "/v1/instagram/save",
        Some(SECRET),
        Some(json!({"items": [{"url": img, "kind": "image", "username": "natgeo"}]})),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["data"]["saved"].as_array().unwrap().len(), 0);
    assert_eq!(r.json["data"]["failed"][0]["error"], "ingest_failed");
    let left: Vec<_> = std::fs::read_dir(e.dir.path().join("spool"))
        .unwrap()
        .flatten()
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[tokio::test]
async fn save_validates_the_request() {
    let e = env().await;
    for body in [
        json!({"items": []}),
        json!({"nothing": 1}),
        json!({"items": (0..51).map(|_| json!({"url": "x", "username": "a"})).collect::<Vec<_>>()}),
    ] {
        let r = send(
            &e.app,
            "POST",
            "/v1/instagram/save",
            Some(SECRET),
            Some(body),
            &[],
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
    }
    assert_eq!(count(&e.server, "/internal/").await, 0);
}
