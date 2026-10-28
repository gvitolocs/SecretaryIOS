//! Chromium's page as Jev sees it: the visible, actionable elements of the
//! page (from `get_window_state` with `tree_format: "elements"`), each with its
//! state, plus the visible text. Off-screen elements are not offered: scrolling
//! is an explicit action, as in browser-use's jev-ultrafast.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use serde_json::Value;

use super::guard::{self, Kind};
use super::{Element, Screen, CHROME_ALLOWED, MAX_ELEMENTS};

/// The arguments for a read the engine can act on.
pub fn read_args() -> Value {
    serde_json::json!({
        "include_screenshot": false,
        "tree_format": "elements",
        "max_elements": 1200,
        "timeout_ms": 3000,
    })
}

/// Actions that do something (not `clickAncestor`, `showContextMenu`, `doDefault`).
const REAL_ACTIONS: &[&str] = &[
    "click", "press", "jump", "activate", "open", "toggle", "select", "expand or contract",
    "expand", "collapse", "check", "uncheck",
];
/// Roles that hold other things; never an action target of their own.
const STRUCTURE: &[&str] = &[
    "frame", "panel", "filler", "document web", "document frame", "scroll pane", "tool bar",
    "landmark", "static", "label", "paragraph", "redundant object", "unknown", "form",
    "page tab list", "menu bar", "status bar", "grouping", "article", "footer", "header",
];
/// Typing goes into these.
const EDITABLE: &[&str] = &["entry", "combo box", "spin button", "text", "search box", "editbar"];
/// Text worth reading for "done?" and the answer.
const TEXT_ROLES: &[&str] = &[
    "static", "heading", "label", "paragraph", "caption", "list item", "table cell",
    "description term", "description value", "section",
];

fn clean(s: &str) -> String {
    s.replace('\u{fffc}', "").split_whitespace().collect::<Vec<_>>().join(" ")
}

fn s<'a>(e: &'a Value, k: &str) -> &'a str {
    e.get(k).and_then(Value::as_str).unwrap_or("")
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(s.as_str(), "true" | "yes" | "mixed"),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        _ => false,
    }
}

/// `value="…" checked expanded`, from whichever keys the driver sent.
fn state_of(e: &Value) -> (String, String) {
    let value = match e.get("value") {
        Some(Value::String(v)) => clean(v),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    let mut flags: Vec<&str> = Vec::new();
    let states: Vec<String> = e
        .get("states")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_lowercase).collect())
        .unwrap_or_default();
    for f in ["checked", "selected", "expanded", "pressed"] {
        if truthy(e.get(f)) || states.iter().any(|s| s == f) {
            flags.push(f);
        }
    }
    (value, flags.join(" "))
}

#[derive(Clone, Copy)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

fn rect(e: &Value) -> Option<Rect> {
    let f = e.get("frame")?;
    let n = |k: &str| f.get(k).and_then(Value::as_f64);
    Some(Rect {
        x: n("x")?,
        y: n("y")?,
        w: n("w")?,
        h: n("h")?,
    })
}

impl Rect {
    fn centre_in(&self, v: &Rect) -> bool {
        let (cx, cy) = (self.x + self.w / 2.0, self.y + self.h / 2.0);
        self.w > 0.0
            && self.h > 0.0
            && cx >= v.x
            && cx <= v.x + v.w
            && cy >= v.y
            && cy <= v.y + v.h
    }
}

/// Parse the `elements` read of Chromium's window for a task of `kind`.
pub fn parse_desktop_elements(v: &Value, title: &str, kind: Kind) -> Screen {
    let empty = Vec::new();
    let elements = v.get("elements").and_then(Value::as_array).unwrap_or(&empty);
    let mut screen = Screen {
        title: title.to_owned(),
        ..Screen::default()
    };
    let viewport = elements
        .iter()
        .find(|e| s(e, "role") == "document web")
        .and_then(rect);
    let mut text: Vec<String> = Vec::new();
    if let Some(doc) = elements.iter().find(|e| s(e, "role") == "document web") {
        let name = clean(s(doc, "label"));
        if !name.is_empty() {
            text.push(format!("# Page: {name}"));
        }
    }
    let mut seen_text: HashSet<String> = HashSet::new();
    let mut seen_el: HashSet<(String, String, i64, i64)> = HashSet::new();
    for e in elements {
        let role = s(e, "role");
        let label = clean(s(e, "label"));
        let description = s(e, "description");
        let offscreen = description.starts_with("off-screen");
        let in_page = truthy(e.get("in_web_content"));
        let r = rect(e);
        // The page viewport bounds page elements; browser chrome lies outside it.
        let visible = !offscreen
            && match (r, viewport) {
                (Some(r), Some(v)) if in_page => r.centre_in(&v),
                _ => true,
            };
        if role == "entry" && label == "Address and search bar" {
            screen.url = state_of(e).0;
            continue;
        }
        if !in_page && !CHROME_ALLOWED.contains(&label.as_str()) {
            continue;
        }
        if role == "password text" && visible {
            screen.password_field = true;
        }
        let actions: Vec<&str> = e
            .get("actions")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let acts = actions.iter().any(|a| REAL_ACTIONS.contains(a));
        let enabled = e.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        // Visible text (headings marked, for "done?" and the answer).
        if in_page && visible && !label.is_empty() && (TEXT_ROLES.contains(&role) || role.starts_with("heading")) {
            let line = if role.starts_with("heading") { format!("# {label}") } else { label.clone() };
            // Sections repeat their children's text: keep short ones only.
            if (role != "section" || line.chars().count() <= 200) && seen_text.insert(line.clone()) {
                text.push(line);
            }
        }
        if !acts || !enabled || !visible || STRUCTURE.contains(&role) || role == "password text" {
            continue;
        }
        let (value, flags) = state_of(e);
        let label = if label.is_empty() {
            clean(description)
        } else {
            label
        };
        if label.is_empty() && value.is_empty() {
            continue;
        }
        let label = if label.is_empty() { value.clone() } else { label };
        if guard::declines_cookies(&label) && screen.cookie_decline.is_none() {
            screen.cookie_decline = Some(element(e, role, &label, String::new()));
        }
        let full = if value.is_empty() || value == label {
            label.clone()
        } else {
            format!("{label} = {value}")
        };
        if guard::risky(&full, kind) {
            if guard::hard_risky(&full) {
                screen.hard_labels.push(lucina_jev::clip(&full, 120));
            }
            continue;
        }
        // Nested duplicates (a link and its inner span) offer one element.
        let centre = r.map(|r| ((r.x + r.w / 2.0) as i64 / 4, (r.y + r.h / 2.0) as i64 / 4)).unwrap_or((-1, -1));
        if !seen_el.insert((role.to_owned(), full.clone(), centre.0, centre.1)) {
            continue;
        }
        if screen.elements.len() >= MAX_ELEMENTS {
            continue;
        }
        let mut state = String::new();
        if !value.is_empty() && value != label {
            state.push_str(&format!("value=\"{}\"", lucina_jev::clip(&value, 60)));
        }
        if !flags.is_empty() {
            if !state.is_empty() {
                state.push(' ');
            }
            state.push_str(&flags);
        }
        screen.elements.push(element(e, role, &label, state));
    }
    screen.text = lucina_jev::clip(&text.join("\n"), 6000);
    screen
}

fn element(e: &Value, role: &str, label: &str, state: String) -> Element {
    let index = e.get("element_index").and_then(Value::as_u64).unwrap_or(0);
    Element {
        key: format!("e{index}"),
        role: role.to_owned(),
        label: lucina_jev::clip(label, 120),
        editable: EDITABLE.contains(&role),
        handle: s(e, "element_token").to_owned(),
        state,
        fresh: false,
    }
}

/// Marks what is new since `previous`, puts it first (Jev leans towards the
/// first options, and after typing the new suggestions are what matters), and
/// computes the page fingerprint.
pub fn finish(screen: &mut Screen, previous: Option<&Screen>) {
    if let Some(p) = previous {
        let before: HashSet<(&str, &str)> = p
            .elements
            .iter()
            .map(|e| (e.role.as_str(), e.label.as_str()))
            .collect();
        for e in screen.elements.iter_mut() {
            e.fresh = !before.contains(&(e.role.as_str(), e.label.as_str()));
        }
        // A different page (new title, or nothing of the page kept) has nothing
        // "new" to point at.
        let kept = screen
            .elements
            .iter()
            .any(|e| !e.fresh && !CHROME_ALLOWED.contains(&e.label.as_str()));
        if screen.title != p.title || !kept {
            for e in screen.elements.iter_mut() {
                e.fresh = false;
            }
        }
        screen.elements.sort_by_key(|e| !e.fresh);
    }
    screen.fingerprint = fingerprint(screen);
}

/// Title, offered elements with their state, and the start of the text.
pub fn fingerprint(screen: &Screen) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    screen.title.hash(&mut h);
    let mut els: Vec<(&str, &str, &str)> = screen
        .elements
        .iter()
        .map(|e| (e.role.as_str(), e.label.as_str(), e.state.as_str()))
        .collect();
    els.sort_unstable();
    els.hash(&mut h);
    let t: String = screen.text.chars().take(2000).collect();
    t.hash(&mut h);
    h.finish()
}
