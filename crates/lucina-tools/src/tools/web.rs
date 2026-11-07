//! `web_search` (SearXNG) and `web_extract` (public pages), ported from the
//! Hermes Agent web tools. The SSRF guard and the HTML-to-text reader are the
//! proven ones from the share extractor, moved here so both use them.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use schemars::JsonSchema;
use scraper::{Html, Selector};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

/// Conservatively refuse special-use networks, including IPv4-in-IPv6.
pub fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0)
        || (a == 192 && b == 2)
        || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
        || (a == 203 && b == 0 && c == 113)
        || a >= 224)
}

/// True for globally routable addresses only; loopback and friends are refused.
pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_v4(v4);
            }
            let oct = ip.octets();
            // Global unicast only. Exclude documentation, 6to4 and Teredo
            // transition prefixes which can encode private IPv4 endpoints.
            (oct[0] & 0xe0) == 0x20
                && !(oct[0] == 0x20 && oct[1] == 0x02)
                && !(oct[..4] == [0x20, 0x01, 0, 0])
                && !(oct[..4] == [0x20, 0x01, 0x0d, 0xb8])
        }
    }
}

/// `(title, text)` of an HTML page: meta description/og first, then the body
/// text without script/style/nav/footer/head/noscript/svg.
pub fn html_text(input: &str) -> (String, String) {
    let html = Html::parse_document(input);
    let title = html
        .select(&Selector::parse("title").expect("static selector"))
        .next()
        .map(|e| e.text().collect::<String>())
        .unwrap_or_default()
        .trim()
        .to_owned();
    let mut out = String::new();
    for meta in html.select(&Selector::parse("meta").expect("static selector")) {
        if meta.value().attr("name") == Some("description")
            || meta
                .value()
                .attr("property")
                .is_some_and(|p| p.starts_with("og:"))
        {
            if let Some(content) = meta.value().attr("content") {
                out.push_str(content);
                out.push('\n');
            }
        }
    }
    for node in html.tree.nodes() {
        if let scraper::node::Node::Text(text) = node.value() {
            let blocked = node.ancestors().any(|a| {
                a.value().as_element().is_some_and(|e| {
                    [
                        "script", "style", "nav", "footer", "head", "noscript", "svg",
                    ]
                    .contains(&e.name())
                })
            });
            if !blocked {
                out.push_str(text);
                out.push(' ');
            }
        }
        if out.len() > 100_000 {
            break;
        }
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    (title, out)
}

#[derive(Deserialize, JsonSchema)]
struct SearchArgs {
    /// What to look for.
    #[schemars(length(min = 1, max = 400))]
    query: String,
    /// How many results (default 5, at most 10).
    limit: Option<u32>,
}

pub struct WebSearchTool {
    /// SearXNG base URL.
    pub base: String,
}

#[async_trait::async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &'static str {
        "web_search"
    }
    fn description(&self) -> &'static str {
        "Searches the web (SearXNG) and returns titles, links and snippets. Use it to find pages; read one with web_extract."
    }
    fn parameters(&self) -> Value {
        schema_for::<SearchArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    async fn call(&self, args: Value, _ctx: &Ctx) -> ToolResult {
        let a: SearchArgs = super::args(args)?;
        let limit = a.limit.unwrap_or(5).clamp(1, 10) as usize;
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| ToolError::new(format!("web search failed: {e}")))?;
        let resp = http
            .get(format!("{}/search", self.base.trim_end_matches('/')))
            .query(&[("q", a.query.as_str()), ("format", "json")])
            .send()
            .await
            .map_err(|e| ToolError::new(format!("web search failed: {}", e.without_url())))?;
        if !resp.status().is_success() {
            return Err(ToolError::new(format!(
                "web search failed: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| ToolError::new(format!("web search failed: {e}")))?;
        let results: Vec<Value> = v["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r["url"].as_str().is_some())
            .take(limit)
            .enumerate()
            .map(|(i, r)| {
                json!({
                    "position": i + 1,
                    "title": r["title"],
                    "url": r["url"],
                    "description": r["content"],
                })
            })
            .collect();
        Ok(json!({ "query": a.query, "results": results }))
    }
}

#[derive(Deserialize, JsonSchema)]
struct ExtractArgs {
    /// Pages to read, 1-5.
    #[schemars(length(min = 1, max = 5))]
    urls: Vec<String>,
    /// Characters of text kept per page (default 15000, 1000-50000).
    char_limit: Option<usize>,
}

pub struct WebExtractTool {
    /// Tests only: allow loopback addresses.
    pub allow_loopback: bool,
}

/// Fetch one public URL: http/https only, no userinfo, every resolved address
/// public (or loopback when `allow_loopback`), DNS pinned, no proxy, redirects
/// followed by hand and re-checked, 15 s overall, 3 MB body cap.
async fn fetch_public(url: &str, allow_loopback: bool) -> Result<(Vec<u8>, String), ToolError> {
    let refused =
        || ToolError::new("refused: the URL resolves to a private or special-use address");
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut url =
            reqwest::Url::parse(url).map_err(|e| ToolError::new(format!("invalid URL: {e}")))?;
        for redirects in 0..=5 {
            if !["http", "https"].contains(&url.scheme())
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(ToolError::new(
                    "unsupported URL: only public http/https pages",
                ));
            }
            let host = url
                .host_str()
                .ok_or_else(|| ToolError::new("URL has no host"))?
                .trim_matches(['[', ']'])
                .to_owned();
            let port = url
                .port_or_known_default()
                .ok_or_else(|| ToolError::new("URL has no port"))?;
            let addresses: Vec<SocketAddr> = match host.parse::<IpAddr>() {
                Ok(ip) => vec![SocketAddr::new(ip, port)],
                Err(_) => tokio::net::lookup_host((host.as_str(), port))
                    .await
                    .map_err(|e| ToolError::new(format!("host lookup failed: {e}")))?
                    .collect(),
            };
            if addresses.is_empty() {
                return Err(ToolError::new("host has no addresses"));
            }
            for address in &addresses {
                let allowed =
                    public_ip(address.ip()) || (allow_loopback && address.ip().is_loopback());
                if !allowed {
                    return Err(refused());
                }
            }
            // Pin the checked DNS answer and disable environment proxies, so
            // resolution cannot change between validation and connection.
            let http = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .resolve_to_addrs(&host, &addresses)
                .timeout(Duration::from_secs(15))
                .user_agent("Mozilla/5.0 (X11; Linux x86_64) LucinaWeb/1")
                .build()
                .map_err(|e| ToolError::new(format!("fetch failed: {e}")))?;
            let mut response = http
                .get(url.clone())
                .send()
                .await
                .map_err(|e| ToolError::new(format!("fetch failed: {}", e.without_url())))?;
            if response.status().is_redirection() {
                if redirects == 5 {
                    return Err(ToolError::new("too many redirects"));
                }
                let target = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .ok_or_else(|| ToolError::new("redirect missing location"))?
                    .to_str()
                    .map_err(|e| ToolError::new(format!("bad redirect location: {e}")))?;
                url = url
                    .join(target)
                    .map_err(|e| ToolError::new(format!("bad redirect location: {e}")))?;
                continue;
            }
            if !response.status().is_success() {
                return Err(ToolError::new(format!(
                    "HTTP {}",
                    response.status().as_u16()
                )));
            }
            let mime = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("text/html")
                .split(';')
                .next()
                .unwrap_or("text/html")
                .trim()
                .to_ascii_lowercase();
            let mut data = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|e| ToolError::new(format!("fetch failed: {}", e.without_url())))?
            {
                if data.len() + chunk.len() > 3 * 1024 * 1024 {
                    return Err(ToolError::new("page exceeds 3 MB"));
                }
                data.extend_from_slice(&chunk);
            }
            return Ok((data, mime));
        }
        Err(ToolError::new("too many redirects"))
    })
    .await
    .map_err(|_| ToolError::new("fetch timed out"))
    .and_then(|r| r)
}

/// Head+tail truncation: first 80% and last 20% of `limit` chars, with the
/// omitted count between them.
fn truncate(text: &str, limit: usize) -> (String, bool) {
    let chars = text.chars().count();
    if chars <= limit {
        return (text.to_owned(), false);
    }
    let head_n = limit * 80 / 100;
    let tail_n = limit - head_n;
    let head: String = text.chars().take(head_n).collect();
    let tail: String = text.chars().skip(chars - tail_n).collect();
    (
        format!(
            "{head}\n\n[… {} characters omitted …]\n\n{tail}",
            chars - limit
        ),
        true,
    )
}

#[async_trait::async_trait]
impl Tool for WebExtractTool {
    fn name(&self) -> &'static str {
        "web_extract"
    }
    fn description(&self) -> &'static str {
        "Reads public web pages (http/https) and returns their title and text. Up to 5 URLs per call."
    }
    fn parameters(&self) -> Value {
        schema_for::<ExtractArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    /// Each URL may take its own 15 s fetch.
    fn time_limit(&self, args: &Value) -> Duration {
        let n = args["urls"].as_array().map_or(1, Vec::len).clamp(1, 5) as u64;
        Duration::from_secs(15 * n + 5)
    }
    async fn call(&self, args: Value, _ctx: &Ctx) -> ToolResult {
        let a: ExtractArgs = super::args(args)?;
        let char_limit = a.char_limit.unwrap_or(15_000).clamp(1_000, 50_000);
        let mut results = Vec::new();
        for url in &a.urls {
            let entry = match fetch_public(url, self.allow_loopback).await {
                Ok((data, mime)) => {
                    let body = String::from_utf8_lossy(&data);
                    let (title, text) = if mime == "text/html" || mime == "application/xhtml+xml" {
                        html_text(&body)
                    } else if mime.starts_with("text/") || mime == "application/json" {
                        (String::new(), body.into_owned())
                    } else {
                        results.push(json!({ "url": url, "error": format!("unsupported content type {mime}") }));
                        continue;
                    };
                    let chars = text.chars().count();
                    let (content, truncated) = truncate(&text, char_limit);
                    json!({
                        "url": url,
                        "title": title,
                        "content": content,
                        "chars": chars,
                        "truncated": truncated,
                    })
                }
                Err(e) => json!({ "url": url, "error": e.0 }),
            };
            results.push(entry);
        }
        Ok(json!({ "results": results }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolEnv;
    use std::sync::Arc;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ctx() -> Ctx {
        let dir = tempfile::tempdir().unwrap();
        Ctx::new(
            Arc::new(ToolEnv::new(dir.path(), chrono_tz::Europe::Copenhagen)),
            "web",
            "test",
        )
    }

    #[test]
    fn ssrf_addresses() {
        for ip in [
            "0.0.0.0",
            "10.0.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.1.1",
            "172.31.255.255",
            "192.168.1.1",
            "100.64.0.1",
            "100.107.164.33",
            "100.127.255.255",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:192.168.1.2",
            "::ffff:100.107.164.33",
            "::127.0.0.1",
            "2002:7f00:1::",
            "64:ff9b::7f00:1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "100.63.255.255",
            "100.128.0.1",
            "172.32.0.1",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(public_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn readable_html() {
        let (title, text) = html_text("<html><head><title>Test &amp; page</title><meta name='description' content='A description'><meta property='og:title' content='Social title'><style>hide me</style></head><body><nav>nav garbage</nav><main><h1>Hello</h1><p>World &amp; friends</p></main><script>bad code</script><footer>footer garbage</footer></body></html>");
        assert_eq!(title, "Test & page");
        assert!(
            text.contains("A description")
                && text.contains("Hello")
                && text.contains("World & friends")
                && text.contains("Social title")
        );
        for s in ["hide me", "nav garbage", "bad code", "footer garbage"] {
            assert!(!text.contains(s));
        }
    }

    #[tokio::test]
    async fn search_returns_positions_and_skips_url_less() {
        let srv = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("q", "rust news"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    {"title": "One", "url": "https://a.example/1", "content": "first"},
                    {"title": "No url", "content": "skipped"},
                    {"title": "Two", "url": "https://b.example/2", "content": "second"},
                    {"title": "Three", "url": "https://c.example/3", "content": "third"}
                ]
            })))
            .mount(&srv)
            .await;
        let tool = WebSearchTool { base: srv.uri() };
        let out = tool
            .call(json!({"query": "rust news", "limit": 2}), &ctx())
            .await
            .unwrap();
        assert_eq!(out["query"], "rust news");
        let results = out["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["position"], 1);
        assert_eq!(results[0]["url"], "https://a.example/1");
        assert_eq!(results[0]["description"], "first");
        assert_eq!(results[1]["position"], 2);
        assert_eq!(results[1]["url"], "https://b.example/2");
    }

    #[tokio::test]
    async fn search_http_error() {
        let srv = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&srv)
            .await;
        let tool = WebSearchTool { base: srv.uri() };
        let err = tool.call(json!({"query": "x"}), &ctx()).await.unwrap_err();
        assert!(err.0.starts_with("web search failed"), "{}", err.0);
        assert!(err.0.contains("500"), "{}", err.0);
    }

    #[tokio::test]
    async fn extract_html_text_and_plain() {
        let srv = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "<html><head><title>Hi</title></head><body><script>bad</script><p>Body text</p></body></html>",
                "text/html; charset=utf-8",
            ))
            .mount(&srv)
            .await;
        Mock::given(method("GET"))
            .and(path("/plain"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("just text", "text/plain"))
            .mount(&srv)
            .await;
        let tool = WebExtractTool {
            allow_loopback: true,
        };
        let out = tool
            .call(
                json!({"urls": [format!("{}/page", srv.uri()), format!("{}/plain", srv.uri())]}),
                &ctx(),
            )
            .await
            .unwrap();
        let results = out["results"].as_array().unwrap();
        assert_eq!(results[0]["title"], "Hi");
        assert!(results[0]["content"]
            .as_str()
            .unwrap()
            .contains("Body text"));
        assert!(!results[0]["content"].as_str().unwrap().contains("bad"));
        assert_eq!(results[0]["truncated"], false);
        assert_eq!(results[1]["content"], "just text");
    }

    #[tokio::test]
    async fn extract_unsupported_type_refused_and_ftp() {
        let srv = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/img"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("png", "image/png"))
            .mount(&srv)
            .await;
        let tool = WebExtractTool {
            allow_loopback: true,
        };
        let out = tool
            .call(
                json!({"urls": [format!("{}/img", srv.uri()), "ftp://example.com/x"]}),
                &ctx(),
            )
            .await
            .unwrap();
        let results = out["results"].as_array().unwrap();
        assert_eq!(results[0]["error"], "unsupported content type image/png");
        assert!(results[1]["error"].as_str().unwrap().len() > 0);

        let strict = WebExtractTool {
            allow_loopback: false,
        };
        let out = strict
            .call(json!({"urls": [format!("{}/img", srv.uri())]}), &ctx())
            .await
            .unwrap();
        assert_eq!(
            out["results"][0]["error"],
            "refused: the URL resolves to a private or special-use address"
        );
    }

    #[tokio::test]
    async fn extract_follows_redirect_and_truncates() {
        let srv = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/hop"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/target", srv.uri())),
            )
            .mount(&srv)
            .await;
        let long = "abcdefghij".repeat(500); // 5000 chars
        Mock::given(method("GET"))
            .and(path("/target"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(long.clone(), "text/plain"))
            .mount(&srv)
            .await;
        let tool = WebExtractTool {
            allow_loopback: true,
        };
        let out = tool
            .call(
                json!({"urls": [format!("{}/hop", srv.uri())], "char_limit": 1000}),
                &ctx(),
            )
            .await
            .unwrap();
        let r = &out["results"][0];
        assert_eq!(r["chars"], 5000);
        assert_eq!(r["truncated"], true);
        let content = r["content"].as_str().unwrap();
        assert!(
            content.contains("[… 4000 characters omitted …]"),
            "{content}"
        );
        assert_eq!(
            content.chars().count(),
            1000 + "\n\n[… 4000 characters omitted …]\n\n".chars().count()
        );
        assert!(content.starts_with("abcdefghij"));
    }

    #[test]
    fn extract_schema_caps_urls_at_five() {
        let tool = WebExtractTool {
            allow_loopback: false,
        };
        let validator = jsonschema::validator_for(&tool.parameters()).unwrap();
        let six = json!({"urls": ["https://a.example/1", "https://a.example/2", "https://a.example/3", "https://a.example/4", "https://a.example/5", "https://a.example/6"]});
        assert!(!validator.is_valid(&six));
        let five = json!({"urls": ["https://a.example/1", "https://a.example/2", "https://a.example/3", "https://a.example/4", "https://a.example/5"]});
        assert!(validator.is_valid(&five));
    }
}
