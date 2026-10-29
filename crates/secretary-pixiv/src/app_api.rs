//! Blocking Pixiv App API adapter. Only this client carries the Bearer token.
use crate::{backend::Backend, connection::Connection};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

pub struct AppBackend {
    connection: Arc<Connection>,
    base: String,
}
impl AppBackend {
    pub fn new(connection: Arc<Connection>, base: String) -> Self {
        Self { connection, base }
    }
    fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value, String> {
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| "network: client unavailable")?;
        for attempt in 0..2 {
            let token = self.connection.access(attempt == 1)?;
            let bearer = zeroize::Zeroizing::new(format!("Bearer {}", token.expose()));
            let mut authorization =
                reqwest::header::HeaderValue::from_str(&bearer).map_err(|_| "token invalid")?;
            authorization.set_sensitive(true);
            let response = client
                .get(format!("{}{}", self.base.trim_end_matches('/'), path))
                .header("authorization", authorization)
                .header("app-os", "ios")
                .header("app-os-version", "14.6")
                .header("user-agent", "PixivIOSApp/7.13.3 (iOS 14.6; iPhone13,2)")
                .header("accept-language", "en-us")
                .query(&[("filter", "for_ios")])
                .query(query)
                .send()
                .map_err(|_| "network: App API unavailable")?;
            let status = response.status();
            let bytes = response.bytes().map_err(|_| "read: App API failed")?;
            let parsed: Result<Value, _> = serde_json::from_slice(&bytes);
            if !status.is_success() {
                let invalid = parsed.as_ref().ok().is_some_and(|v| {
                    // Inspect only known error fields. Never return their contents.
                    let error = &v["error"];
                    error == "invalid_grant"
                        || [
                            Some(error),
                            error.get("message"),
                            error.get("reason"),
                            error.get("user_message"),
                            v.get("message"),
                            v.get("error_description"),
                        ]
                        .iter()
                        .flatten()
                        .filter_map(|s| s.as_str())
                        .any(|s| {
                            let s = s.to_lowercase();
                            s.contains("oauth process")
                                || s.contains("invalid_grant")
                                || (s.contains("access token")
                                    && (s.contains("invalid") || s.contains("expired")))
                        })
                });
                if matches!(status.as_u16(), 400 | 401 | 403) && invalid && attempt == 0 {
                    continue;
                }
                let missing = parsed.as_ref().ok().is_some_and(|v| {
                    [
                        &v["error"],
                        &v["error"]["message"],
                        &v["error"]["user_message"],
                    ]
                    .iter()
                    .filter_map(|s| s.as_str())
                    .any(|s| {
                        let s = s.to_lowercase();
                        s.contains("not found") || s.contains("not exist") || s.contains("deleted")
                    })
                });
                if status.as_u16() == 400 && missing && !invalid && path == "/v1/illust/detail" {
                    return Err("pixiv: not found".into());
                }
                return Err(format!("HTTP {} App API refused request", status.as_u16()));
            }
            return parsed.map_err(|_| "bad JSON: App API response".into());
        }
        Err("token rejected".into())
    }
}
fn id(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}
fn image<'a>(images: &'a Value, key: &str, fallback: &str) -> &'a str {
    images[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| images[fallback].as_str())
        .unwrap_or_default()
}
fn summary(v: &Value) -> Value {
    json!({"id":id(&v["id"]),"title":v["title"],"author":v["user"]["name"],"author_id":id(&v["user"]["id"]),
        "tags":v["tags"].as_array().map(|a|a.iter().filter_map(|t|t["name"].as_str()).collect::<Vec<_>>()).unwrap_or_default(),
        "thumb":image(&v["image_urls"],"square_medium","medium"),"pages":v["page_count"],"r18":v["x_restrict"].as_i64().unwrap_or(0)>=1,
        "bookmarks":v["total_bookmarks"],"createDate":v["create_date"]})
}
fn listing(v: &Value, page: u32, kind: &str) -> Value {
    let items = v["illusts"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|i| kind == "all" || i["type"] == kind)
                .map(summary)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({"items":items,"page":page,"total":null,"num_pages":null,"hasMore":!v["next_url"].is_null()})
}
fn caption(s: &str) -> String {
    let mut plain = String::new();
    let mut inside = false;
    let mut tag = String::new();
    for c in s.chars() {
        match c {
            '<' => {
                inside = true;
                tag.clear();
            }
            '>' => {
                inside = false;
                if tag == "br" || tag == "br /" || tag == "br/" || tag == "/p" {
                    plain.push('\n');
                }
            }
            _ if inside => tag.push(c),
            _ => plain.push(c),
        }
    }
    html_escape::decode_html_entities(&plain).into_owned()
}
impl Backend for AppBackend {
    fn ranking(&self, mode: &str, page: u32) -> Result<Value, String> {
        let api_mode = match mode {
            "original" => "week_original",
            "rookie" => "week_rookie",
            m => m,
        };
        let v = self.get(
            "/v1/illust/ranking",
            &[
                ("mode", api_mode.into()),
                ("offset", (page.saturating_sub(1) * 30).to_string()),
            ],
        )?;
        let mut l = listing(&v, page, "all");
        l["mode"] = json!(mode);
        l["next"] = json!(!v["next_url"].is_null());
        Ok(l)
    }
    fn search(&self, args: &[String]) -> Result<Value, String> {
        let split = args
            .iter()
            .position(|s| s.starts_with("--"))
            .unwrap_or(args.len());
        let mut word = args[..split].join(" ");
        let arg = |flag: &str, default: &str| {
            args.windows(2)
                .find(|a| a[0] == flag)
                .map(|a| a[1].clone())
                .unwrap_or_else(|| default.into())
        };
        let page = arg("--page", "1").parse::<u32>().unwrap_or(1).max(1);
        let kind = arg("--type", "all");
        let order = arg("--order", "date_d");
        if arg("--mode", "safe") == "r18" {
            word.push_str(" R-18");
        }
        if page.saturating_sub(1).saturating_mul(30) > 5000 {
            return Ok(json!({"items":[],"total":null,"num_pages":null,"hasMore":false}));
        }
        let mut query = vec![
            ("word", word),
            ("search_target", "partial_match_for_tags".into()),
            ("merge_plain_keyword_results", "true".into()),
            ("include_translated_tag_results", "true".into()),
        ];
        let path = if order == "popular_preview" {
            "/v1/search/popular-preview/illust"
        } else {
            query.push((
                "sort",
                match order.as_str() {
                    "popular_d" => "popular_desc",
                    "date" => "date_asc",
                    _ => "date_desc",
                }
                .into(),
            ));
            query.push(("offset", (page.saturating_sub(1) * 30).to_string()));
            "/v1/search/illust"
        };
        Ok(listing(&self.get(path, &query)?, page, &kind))
    }
    fn illust(&self, id_: &str) -> Result<Value, String> {
        let response = self.get("/v1/illust/detail", &[("illust_id", id_.into())])?;
        let v = &response["illust"];
        if v.is_null() {
            return Err("pixiv: not found".into());
        }
        let page = |images: &Value, original: &str| json!({"thumb":image(images,"square_medium","medium"),"regular":image(images,"large","medium"),"orig":original});
        let pages = match v["meta_pages"].as_array().filter(|a| !a.is_empty()) {
            Some(a) => a
                .iter()
                .map(|p| {
                    page(
                        &p["image_urls"],
                        p["image_urls"]["original"].as_str().unwrap_or_default(),
                    )
                })
                .collect::<Vec<_>>(),
            None => vec![page(
                &v["image_urls"],
                v["meta_single_page"]["original_image_url"]
                    .as_str()
                    .unwrap_or_default(),
            )],
        };
        Ok(
            json!({"id":id(&v["id"]),"title":v["title"],"author":v["user"]["name"],"author_id":id(&v["user"]["id"]),"created":v["create_date"],
            "type":match v["type"].as_str(){Some("manga")=>1,Some("ugoira")=>2,_=>0},"r18":v["x_restrict"].as_i64().unwrap_or(0)>=1,
            "description":caption(v["caption"].as_str().unwrap_or_default()),"page_count":v["page_count"],"cover":image(&v["image_urls"],"large","medium"),
            "stats":{"bookmarks":v["total_bookmarks"],"views":v["total_view"],"likes":v["total_likes"].as_u64().or_else(|| v["likes"].as_u64()).unwrap_or(0),"comments":v["total_comments"].as_u64().unwrap_or(0)},
            "tags":v["tags"].as_array().map(|a|a.iter().map(|t|json!({"tag":t["name"],"en":t["translated_name"].as_str().unwrap_or_default()})).collect::<Vec<_>>()).unwrap_or_default(),"pages":pages}),
        )
    }
    fn self_status(&self) -> Result<Value, String> {
        let a = self.connection.account();
        Ok(
            json!({"is_logged_in":a.usable(),"is_premium":a.premium,"x_restrict":a.r18.map(i64::from)}),
        )
    }
    fn has_session(&self) -> bool {
        self.connection.has_session()
    }
}
