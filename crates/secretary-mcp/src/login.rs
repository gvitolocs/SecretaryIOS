//! The OAuth sign-in + consent page (plain HTML, no script).

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};

use crate::scope::{Scope, Scopes};

/// No script, no remote anything, never framed.
const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'";

pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

const STYLE: &str = r#"
:root { color-scheme: light dark; --bg:#f6f4f1; --card:#fff; --fg:#1d1b19; --muted:#6b655e; --accent:#8a4fff; --err:#b3261e; --line:#e4dfd8; }
@media (prefers-color-scheme: dark) { :root { --bg:#151312; --card:#1f1c1a; --fg:#f1ece6; --muted:#a59d94; --accent:#b48cff; --err:#ff8a80; --line:#3a3532; } }
* { box-sizing: border-box; }
body { margin:0; background:var(--bg); color:var(--fg); font:16px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; }
main { max-width:420px; margin:6vh auto; padding:0 16px; }
.card { background:var(--card); border:1px solid var(--line); border-radius:16px; padding:24px; }
h1 { font-size:1.3rem; margin:0 0 4px; }
p.sub { color:var(--muted); margin:0 0 20px; font-size:.95rem; }
label { display:block; font-size:.9rem; margin:14px 0 4px; }
input[type=email], input[type=password] { width:100%; padding:10px 12px; border-radius:10px; border:1px solid var(--line); background:transparent; color:var(--fg); font-size:1rem; }
fieldset { border:1px solid var(--line); border-radius:12px; margin:18px 0 0; padding:10px 14px; }
legend { font-size:.85rem; color:var(--muted); padding:0 4px; }
.scope { display:flex; gap:10px; align-items:flex-start; margin:8px 0; font-size:.92rem; }
.scope b { display:block; }
.row { display:flex; gap:10px; margin-top:22px; }
button { flex:1; padding:11px; border-radius:10px; border:1px solid var(--line); background:transparent; color:var(--fg); font-size:1rem; cursor:pointer; }
button.primary { background:var(--accent); border-color:var(--accent); color:#fff; font-weight:600; }
.err { color:var(--err); margin:12px 0 0; font-size:.95rem; }
.note { color:var(--muted); font-size:.8rem; margin-top:16px; }
code { font-size:.85em; word-break:break-all; }
"#;

fn page(title: &str, body: &str, status: StatusCode) -> Response {
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><meta name=\"referrer\" content=\"no-referrer\"><title>{}</title><style>{STYLE}</style></head><body><main>{body}</main></body></html>",
        esc(title)
    );
    let mut r = (status, Html(html)).into_response();
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

pub fn error_page(msg: &str) -> Response {
    page(
        "Lucina sign-in",
        &format!(
            "<div class=\"card\"><h1>Can't continue</h1><p class=\"err\">{}</p><p class=\"note\">Close this window and start the connection again from your app.</p></div>",
            esc(msg)
        ),
        StatusCode::BAD_REQUEST,
    )
}

/// Sign-in + consent form. `request_id` points at the validated request
/// kept server-side, so nothing security-relevant travels in hidden fields.
pub fn consent_page(
    request_id: &str,
    client_name: &str,
    redirect_uri: &str,
    scopes: &Scopes,
    error: Option<&str>,
) -> Response {
    let host = reqwest::Url::parse(redirect_uri)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    let mut boxes = String::new();
    for s in Scope::ALL {
        if !scopes.contains(s) {
            continue;
        }
        boxes.push_str(&format!(
            "<label class=\"scope\"><input type=\"checkbox\" name=\"scope_{name}\" value=\"1\"{checked}><span><b>{name}</b>{desc}</span></label>",
            name = s.as_str(),
            checked = if s.explicit_only() { "" } else { " checked" },
            desc = esc(s.describe())
        ));
    }
    let err = error
        .map(|e| format!("<p class=\"err\" role=\"alert\">{}</p>", esc(e)))
        .unwrap_or_default();
    let body = format!(
        r#"<div class="card">
<h1>Connect {client} to Lucina</h1>
<p class="sub">Sign in with your Secretary account to let <b>{client}</b> ({host}) use Lucina. Only continue if you just started this connection yourself, in your own account.</p>
<form method="post" action="/oauth/authorize" autocomplete="on">
<input type="hidden" name="request_id" value="{rid}">
<label for="email">Email</label>
<input id="email" name="email" type="email" autocomplete="username" required>
<label for="password">Password</label>
<input id="password" name="password" type="password" autocomplete="current-password" required>
<fieldset><legend>Allow</legend>{boxes}</fieldset>
{err}
<div class="row"><button type="submit" name="decision" value="deny" formnovalidate>Deny</button><button class="primary" type="submit" name="decision" value="approve">Allow</button></div>
</form>
<p class="note">This connection has no tools that buy things, message other people or delete data; requests like that go to Lucina and her own confirmation flow. Revoke it any time with <code>secretary-mcp oauth revoke</code>.</p>
</div>"#,
        client = esc(client_name),
        host = esc(&host),
        rid = esc(request_id),
    );
    page(
        "Connect to Lucina",
        &body,
        if error.is_some() {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::OK
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes() {
        assert_eq!(
            esc("<a href=\"x\">'&"),
            "&lt;a href=&quot;x&quot;&gt;&#39;&amp;"
        );
    }
}
