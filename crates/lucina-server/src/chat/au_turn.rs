//! AU Find in chat (Aarhus University people, rooms, buildings and campus
//! maps), on the JSON planner path, MCP turns included (read-only):
//!
//! - **Routing:** strong cues ("AU Find", "Aarhus University", `AU` with a
//!   person / room / building / map word, "bygning 1532", a room code
//!   `1532-316`, "campus map") route directly. Weaker ones ("who is X",
//!   "where is X's office", "lokale") ask Jev one yes/no question and route
//!   from `LUCINA_JEV_FIRST_MIN` (0.75) up; with Jev off they do not route.
//! - **What to look up:** regex first, then the planner model once for strict
//!   JSON; nothing found goes on to the normal turn.
//! - **Answer:** `lucina_tools::tools::au` runs the owner's procedure (health,
//!   retries, disambiguation); the reply is written from that result only, under
//!   the answer contract, with [`render_people`] / [`render_buildings`] as the
//!   deterministic fallback. Found places get a map card and the map PNG;
//!   not found / unavailable get a "Search au.dk" button that hands the search
//!   to her browser (`lucina_intent::computer_route`).

use std::sync::OnceLock;

use lucina_jev::Question;
use lucina_tools::tools::au::{
    self, AuBuildings, AuClient, AuPeople, Status, BUILDING_ZOOM, CAMPUS_ZOOM, PARK,
};
use regex::Regex;
use serde_json::{json, Value};

use super::{Finish, Turn};

pub const ACTION: &str = "au_find";

/// The AU sites and their rules, for the reply model.
const CONTRACT: &str = "AU answer contract: answer only from the AU Find block. Say the source: \"AU Find (aufind)\" for search fields, \"www.au.dk profile <url>\" for profile fields. Quote rooms as <building>-<room> (e.g. 1872-237; the room's first digit is the floor). Never invent emails, phones, rooms, coordinates or URLs; leave out what the block does not have. Add nothing of your own about the place either (no districts, neighbourhoods, distances or directions). No door-by-door routes.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Person,
    Building,
    Map,
}

/// What the message asks to look up. `query` is a name, a building id, or
/// (for a map) a building id or empty for the campus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub kind: Kind,
    pub query: String,
}

fn rx(cell: &'static OnceLock<Vec<Regex>>, patterns: &[&str]) -> &'static [Regex] {
    cell.get_or_init(|| {
        patterns
            .iter()
            .map(|p| Regex::new(p).expect("au turn regex"))
            .collect()
    })
}

/// Person, room, building, office, teacher and map words (en / it / da).
const PLACE_OR_PERSON: &str = r"(?i)\b(?:person|people|staff|employee|room|office|building|teacher|lecturer|professor|prof|supervisor|map|campus|where|who|email|phone|stanza|aula|ufficio|edificio|professore|professoressa|docente|insegnante|mappa|dove|chi|lokale|kontor|bygning|underviser|lærer|vejleder|kort|medarbejder|hvor|hvem)\b";

/// A message that is about AU Find without asking anyone.
pub fn strong_cue(text: &str) -> bool {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    let rs = rx(
        &R,
        &[
            r"(?i)\bau[\s-]?find\b",
            r"(?i)\baarhus\s+universit(?:y|et|à|a)\b",
            r"(?i)\b(?:bygning|building|edificio)\s+\d{4}[a-z]?\b",
            r"\b\d{4}-\d{3}\b",
            r"(?i)\b(?:campus\s?map|mappa\s+del\s+campus|campuskort)\b",
        ],
    );
    if rs.iter().any(|r| r.is_match(text)) {
        return true;
    }
    static AU: OnceLock<Regex> = OnceLock::new();
    static WORDS: OnceLock<Regex> = OnceLock::new();
    AU.get_or_init(|| Regex::new(r"\bAU\b").expect("au regex"))
        .is_match(text)
        && WORDS
            .get_or_init(|| Regex::new(PLACE_OR_PERSON).expect("au words"))
            .is_match(text)
}

/// A message that may be about AU (Jev decides).
pub fn weak_cue(text: &str) -> bool {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    let rs = rx(
        &R,
        &[
            r"(?i)\bwhere(?:'s|\s+is)\b.*\b(?:office|room)\b",
            r"(?i)\bwho(?:'s|\s+is)\s+\S",
            r"(?i)\bchi\s+(?:è|e'|e)\s+(?:il|la|lo)?\s*(?:prof|professore|professoressa|docente)\b",
            r"(?i)\bufficio\s+(?:di|del|della)\b",
            r"(?i)\bhvem\s+er\b",
            r"(?i)\b(?:lokale|kontor)\b",
            r"(?i)\b(?:teacher|lecturer|professor)\s+of\s+my\s+course\b",
            r"(?i)\b\w+'s\s+(?:office|room)\b",
        ],
    );
    rs.iter().any(|r| r.is_match(text))
}

const TITLES: &[&str] = &[
    "the",
    "professor",
    "prof",
    "prof.",
    "dr",
    "dr.",
    "mr",
    "mr.",
    "ms",
    "ms.",
    "il",
    "la",
    "lo",
    "professore",
    "professoressa",
    "docente",
    "teacher",
    "lecturer",
    "lektor",
    "underviser",
];

/// First words that make a capture a description, not a name.
const NOT_NAMES: &[&str] = &[
    "my", "your", "his", "her", "our", "their", "this", "that", "it", "mio", "mia", "tuo", "tua",
    "suo", "sua", "questo", "quello", "min", "mit", "din", "dit", "hans", "hendes", "den", "det",
    "you", "i", "me", "lucina", "a", "an", "un", "una", "en", "et", "what", "where", "who", "che",
    "cosa", "hvad",
];

/// A name from a capture: up to the first stop word, without titles and the
/// "at AU" tail; `None` when it is not a name.
fn clean_name(raw: &str) -> Option<String> {
    let mut words: Vec<&str> = Vec::new();
    for w in raw.split_whitespace() {
        let lw = w.to_lowercase();
        if matches!(
            lw.as_str(),
            "at" | "in"
                | "on"
                | "from"
                | "a"
                | "ad"
                | "al"
                | "all'"
                | "alla"
                | "all'au"
                | "på"
                | "hos"
                | "ved"
                | "i"
                | "of"
                | "di"
                | "del"
                | "della"
                | "and"
                | "e"
                | "og"
                | "please"
                | "per"
                | "favore"
                | "tak"
        ) && !words.is_empty()
        {
            break;
        }
        // "Dennis Vittrup's office": the name ends at the possessive.
        if let Some(stem) = w.strip_suffix("'s").or_else(|| w.strip_suffix("’s")) {
            words.push(stem);
            break;
        }
        words.push(w);
    }
    while words
        .first()
        .is_some_and(|w| TITLES.contains(&w.to_lowercase().as_str()))
    {
        words.remove(0);
    }
    let name = words
        .join(" ")
        .trim_end_matches("'s")
        .trim_end_matches("’s")
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_owned();
    let first = name.split_whitespace().next()?.to_lowercase();
    let n = name.split_whitespace().count();
    (n <= 4
        && name.chars().count() >= 2
        && name.chars().any(char::is_alphabetic)
        && !NOT_NAMES.contains(&first.as_str())
        && !["au", "aufind", "aarhus"].contains(&first.as_str()))
    .then_some(name)
}

/// A building id named in the message ("bygning 1532", "building 1532A", a room code's building).
pub fn building_in(text: &str) -> Option<String> {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    let rs = rx(
        &R,
        &[
            r"(?i)\b(?:bygning|building|edificio|bldg\.?)\s*(?P<id>\d{4}[a-z]?)\b",
            r"\b(?P<id>\d{4})-\d{3}\b",
        ],
    );
    rs.iter()
        .find_map(|r| r.captures(text))
        .map(|c| c["id"].to_uppercase())
}

/// What to look up, by regex alone.
pub fn extract(text: &str) -> Option<Ask> {
    let t = text.trim();
    static MAP: OnceLock<Regex> = OnceLock::new();
    let map = MAP
        .get_or_init(|| {
            Regex::new(r"(?i)\b(?:map|maps|mappa|cartina|kort|kortet|campus\s?map|campuskort)\b")
                .expect("map regex")
        })
        .is_match(t);
    let building = building_in(t);
    if map {
        return Some(Ask {
            kind: Kind::Map,
            query: building.unwrap_or_default(),
        });
    }
    if let Some(id) = building {
        return Some(Ask {
            kind: Kind::Building,
            query: id,
        });
    }
    static P: OnceLock<Vec<Regex>> = OnceLock::new();
    let rs = rx(
        &P,
        &[
            r"(?i)\b(?P<n>[\p{L}][\p{L}'.-]*(?:\s+[\p{L}][\p{L}'.-]*){0,3})(?:'s|’s)\s+(?:office|room|email|e-mail|phone|number|building)\b",
            r"(?i)\b(?:office|room|email|e-mail|phone(?:\s+number)?|ufficio|stanza|telefono|kontor|lokale)\s+(?:of|di|del|della|dello|til|for)\s+(?P<n>[^?!,;:\n]+)",
            r"(?i)\b(?:who(?:'s|\s+is)|chi\s+(?:è|e'|e)|hvem\s+er)\s+(?P<n>[^?!,;:\n]+)",
            r"(?i)\b(?:find|look\s+up|lookup|search\s+for|search|cerca(?:mi)?|trova(?:mi)?|søg\s+efter|find\s+frem\s+til)\s+(?P<n>[^?!,;:\n]+)",
            r"(?i)\bwhere(?:'s|\s+is)\s+(?P<n>[^?!,;:\n]+)",
            r"(?i)\bdove\s+(?:si\s+trova|sta|è)\s+(?P<n>[^?!,;:\n]+)",
        ],
    );
    let query = rs
        .iter()
        .filter_map(|r| r.captures(t))
        .find_map(|c| clean_name(&c["n"]))?;
    Some(Ask {
        kind: Kind::Person,
        query,
    })
}

/// The planner model's extraction (strict JSON), when the regex found nothing.
pub fn parse_extraction(content: &str) -> Option<Ask> {
    let v = crate::planner::extract_json_object(content)?;
    let query: String = v["query"]
        .as_str()
        .unwrap_or("")
        .trim()
        .chars()
        .take(80)
        .collect();
    let kind = match v["kind"].as_str()? {
        "person" => Kind::Person,
        "building" => Kind::Building,
        "map" => Kind::Map,
        _ => return None,
    };
    let query = match kind {
        Kind::Person => clean_name(&query)?,
        Kind::Building => {
            let q = au::building_id_of(&query).unwrap_or(query);
            (!q.is_empty()).then_some(q)?
        }
        Kind::Map => au::building_id_of(&query).unwrap_or_default(),
    };
    Some(Ask { kind, query })
}

async fn model_extract(t: &Turn<'_>, memory: &str) -> Option<Ask> {
    let extra = "Task: extract what the owner wants to look up at Aarhus University. Reply with ONLY one JSON object, no prose: {\"kind\":\"person|building|map\",\"query\":\"…\"}. person: the person's name as written; building: the building number or name; map: the building number, or \"\" for the whole campus. If the message names no person, building or map, reply {\"kind\":\"none\",\"query\":\"\"}.";
    let out =
        t.st.planner
            .generate_reply(&t.input(memory, t.platform_label()), extra, "", "reply")
            .await
            .ok()?;
    parse_extraction(&out)
}

/// Jev's yes/no on a weak cue; `false` when Jev is off or failed.
async fn jev_says_au(text: &str) -> bool {
    let jev = lucina_jev::shared();
    if !jev.available() {
        return false;
    }
    let q = [(
        "au",
        Question::noul("Is this message asking to look up a person, office/room, building or campus map at Aarhus University (AU)?"),
    )];
    let Ok(d) = jev.decide("au_route", json!({"message": text}), &q).await else {
        return false;
    };
    let p = d.yes("au").unwrap_or(0.0);
    let min = crate::jev_route::jev_first_min(ACTION);
    tracing::info!(target: "lucina_jev::route", p, min, ms = d.elapsed_ms, "jev au route");
    p >= min
}

// ---------------------------------------------------------------- texts

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// `1872-237` (af-server's `au_room`, else building-room when both are there).
pub fn room_of(p: &Value) -> Option<String> {
    let r = s(p, "au_room");
    if !r.is_empty() {
        return Some(r.to_owned());
    }
    let (b, room) = (s(p, "building"), s(p, "room"));
    (!b.is_empty() && !room.is_empty()).then(|| format!("{b}-{room}"))
}

fn units(p: &Value) -> String {
    p["units"]
        .as_array()
        .map(|u| {
            u.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" · ")
        })
        .unwrap_or_default()
}

/// The search result as the reply model's knowledge block (raw fields, attempts, sources).
pub fn knowledge_people(r: &AuPeople) -> String {
    let mut out = format!(
        "AU Find (aufind) people search for \"{}\": status {}, total {}.\n",
        r.query,
        r.status.as_str(),
        r.total
    );
    let tried: Vec<String> = r
        .tried
        .iter()
        .map(|a| format!("\"{}\" ({} hits)", a.q, a.total))
        .collect();
    if !tried.is_empty() {
        out.push_str(&format!("Searches tried: {}.\n", tried.join(", ")));
    }
    if let Some(u) = &r.upstream {
        out.push_str(&format!("Search source: AU Find (aufind) {u}\n"));
    }
    if let Some(e) = &r.error {
        out.push_str(&format!("Error: {e}\n"));
    }
    for (i, p) in r.people.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", i + 1, compact(p)));
    }
    if let Some(m) = r.more {
        out.push_str(&format!("({m} more matches not shown)\n"));
    }
    out
}

fn compact(p: &Value) -> String {
    let mut v = serde_json::Map::new();
    for k in [
        "name",
        "position",
        "units",
        "email",
        "phone",
        "building",
        "room",
        "au_room",
        "address",
        "profile_url",
        "map_url",
        "profile_source",
    ] {
        if let Some(x) = p.get(k).filter(|x| !x.is_null()) {
            v.insert(k.into(), x.clone());
        }
    }
    Value::Object(v).to_string()
}

struct Words {
    found_n: &'static str,
    top3: &'static str,
    more: &'static str,
    which: &'static str,
    not_found: &'static str,
    tried: &'static str,
    offer: &'static str,
    down: &'static str,
    room: &'static str,
    source: &'static str,
    profile: &'static str,
    building: &'static str,
    buildings: &'static str,
}

fn words(lang: &str) -> Words {
    match lang {
        "da" => Words {
            found_n: "personer i AU Find matcher",
            top3: "De første 3",
            more: "flere",
            which: "Hvem mener du?",
            not_found: "Ikke fundet i AU Find",
            tried: "Jeg prøvede",
            offer: "Jeg kan søge på au.dk i min browser.",
            down: "AU Find kan ikke nås lige nu, så jeg kan ikke slå det op (og jeg gætter ikke).",
            room: "Lokale",
            source: "Kilde",
            profile: "profil",
            building: "Bygning",
            buildings: "bygninger i AU Find matcher",
        },
        _ => Words {
            found_n: "people in AU Find match",
            top3: "The top 3",
            more: "more",
            which: "Which one do you mean?",
            not_found: "Not found in AU Find",
            tried: "I tried",
            offer: "I can search au.dk in my browser.",
            down: "AU Find is unreachable right now, so I can't look that up (and I won't guess).",
            room: "Room",
            source: "Source",
            profile: "profile",
            building: "Building",
            buildings: "buildings in AU Find match",
        },
    }
}

fn person_line(p: &Value) -> String {
    let mut parts = vec![format!("**{}**", s(p, "name"))];
    let pos = s(p, "position");
    if !pos.is_empty() {
        parts.push(pos.to_owned());
    }
    let u = units(p);
    if !u.is_empty() {
        parts.push(u);
    }
    if let Some(r) = room_of(p) {
        parts.push(r);
    }
    parts.join(" — ")
}

fn tried_text(r: &AuPeople) -> String {
    r.tried
        .iter()
        .map(|a| format!("\"{}\" ({})", a.q, a.total))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The people answer without a model (also the fallback when the reply model fails).
pub fn render_people(r: &AuPeople, lang: &str) -> String {
    let w = words(lang);
    match r.status {
        Status::Unavailable => w.down.to_owned(),
        Status::NotFound => format!(
            "{}: \"{}\". {}: {}. {}",
            w.not_found,
            r.query,
            w.tried,
            tried_text(r),
            w.offer
        ),
        Status::Ambiguous => {
            let mut out = format!("{} {} \"{}\". {}:\n", r.total, w.found_n, r.query, w.top3);
            for (i, p) in r.people.iter().enumerate() {
                out.push_str(&format!("{}. {}\n", i + 1, person_line(p)));
            }
            if let Some(m) = r.more {
                out.push_str(&format!("(+{m} {})\n", w.more));
            }
            out.push_str(&format!("{}\n{}: AU Find (aufind)", w.which, w.source));
            out
        }
        Status::Found => {
            let Some(p) = r.people.first() else {
                return w.not_found.to_owned();
            };
            let mut out = person_line(p);
            if let Some(room) = room_of(p) {
                out.push_str(&format!("\n{}: {room}", w.room));
            }
            for (label, k) in [("Email", "email"), ("Tel.", "phone")] {
                let v = s(p, k);
                if !v.is_empty() {
                    out.push_str(&format!("\n{label}: {v}"));
                }
            }
            out.push_str(&format!("\n{}: AU Find (aufind)", w.source));
            let url = s(p, "profile_url");
            if !url.is_empty() {
                out.push_str(&format!("; {} www.au.dk {url}", w.profile));
            }
            out
        }
    }
}

fn building_title(b: &Value) -> String {
    let id = s(b, "id");
    let name = b["names"]
        .as_array()
        .and_then(|n| n.first())
        .and_then(Value::as_str)
        .unwrap_or("");
    match (id.is_empty(), name.is_empty()) {
        (false, false) if !name.contains(id) => format!("{name} ({id})"),
        (_, false) => name.to_owned(),
        _ => format!("AU {id}"),
    }
}

/// The building answer without a model.
pub fn render_buildings(r: &AuBuildings, lang: &str) -> String {
    let w = words(lang);
    match r.status {
        Status::Unavailable => w.down.to_owned(),
        Status::NotFound => format!("{}: \"{}\". {}", w.not_found, r.query, w.offer),
        Status::Ambiguous => {
            let mut out = format!("{} \"{}\":\n", w.buildings, r.query);
            for (i, b) in r.buildings.iter().enumerate() {
                out.push_str(&format!(
                    "{}. {} — {}\n",
                    i + 1,
                    building_title(b),
                    s(b, "address")
                ));
            }
            out.push_str(&format!("{}\n{}: AU Find (aufind)", w.which, w.source));
            out
        }
        Status::Found => {
            let b = &r.buildings[0];
            let mut out = format!("**{}**", building_title(b));
            let a = s(b, "address");
            if !a.is_empty() {
                out.push_str(&format!(" — {a}"));
            }
            out.push_str(&format!("\n{}: AU Find (aufind)", w.source));
            out
        }
    }
}

/// The text of the "Search au.dk" button: a browser task for her computer.
pub fn browser_request(query: &str, _lang: &str) -> String {
    let q: String = query.replace('"', "").chars().take(80).collect();
    format!("Use your browser on www.au.dk to find \"{q}\" (staff pages only; quote the page text and its URL; stop at any login).")
}

fn search_button(query: &str, lang: &str) -> Value {
    let label = if lang == "da" { "Søg på au.dk" } else { "Search au.dk" };
    json!({"id": "au-search", "label": label, "emoji": "🔎", "kind": "reply",
        "payload": {"text": browser_request(query, lang)}})
}

fn image_block(src: String, what: &str) -> Value {
    json!({"type": "image", "src": src, "alt": format!("Map of {what}"),
        "caption": format!("{what} (AU Find, © OpenStreetMap contributors)"), "width": 800, "height": 600})
}

fn place_block(title: &str, b: &Value) -> Value {
    json!({"type": "place", "title": title, "address": s(b, "address"),
        "lat": b["latitude"], "lng": b["longitude"]})
}

fn map_blocks(title: &str, b: &Value) -> Vec<Value> {
    let id = s(b, "id");
    let mut out = vec![place_block(title, b)];
    if au::valid_building_id(id) {
        out.push(image_block(
            au::building_map_path(id, BUILDING_ZOOM),
            &format!("AU building {id}"),
        ));
    }
    out
}

// ---------------------------------------------------------------- the turn

/// The reply: the model under the contract, else the deterministic text.
async fn write_reply(
    t: &Turn<'_>,
    memory: &str,
    extra: &[&str],
    knowledge: &str,
    fallback: String,
) -> String {
    let extra = std::iter::once(t.extra_system.as_str())
        .chain(std::iter::once(CONTRACT))
        .chain(extra.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    match t
        .st
        .planner
        .generate_reply(
            &t.input(memory, t.platform_label()),
            &extra,
            knowledge,
            "reply",
        )
        .await
    {
        Ok(r) if !r.trim().is_empty() && r.trim() != "I could not generate a reply." => r,
        _ => fallback,
    }
}

fn status_line(status: Status) -> &'static str {
    match status {
        Status::Found => "One match: give the person's details with their sources.",
        Status::Ambiguous => "Several matches: list the top 3 numbered with position and building-room, say how many more there are, and ask which one. Never pick one.",
        Status::NotFound => "No match: say \"not found in AU Find\" in English (Danish when he writes Danish; never Italian), list the searches tried, and offer to search au.dk in your browser.",
        Status::Unavailable => "AU Find is unreachable right now: say so in English (Danish when he writes Danish; never Italian) and do not guess any detail.",
    }
}

pub(super) async fn try_turn(t: &Turn<'_>, memory: &str) -> Option<Finish> {
    let st = t.st;
    if !st.cfg.au_enabled {
        return None;
    }
    let text = t.message.as_str();
    if !strong_cue(text) && !(weak_cue(text) && jev_says_au(text).await) {
        return None;
    }
    let ask = match extract(text) {
        Some(a) => a,
        None => model_extract(t, memory).await?,
    };
    // English, or Danish when he writes Danish; never Italian (owner rule 2026-11-02).
    let lang = if crate::rich::guess_language(text, "") == "da" { "da" } else { "en" };
    let client = AuClient::new(&st.tool_env.http, &st.cfg.au_url);
    match ask.kind {
        Kind::Person => {
            let r = client
                .people_search(&ask.query, if lang == "da" { "da" } else { "en" })
                .await;
            let fallback = render_people(&r, lang);
            // Several matches: the numbered list as is. The reply model led with one of them
            // ("closest match …") in a live test, which the procedure forbids.
            let reply = if r.status == Status::Ambiguous {
                fallback
            } else {
                write_reply(
                    t,
                    memory,
                    &[status_line(r.status)],
                    &knowledge_people(&r),
                    fallback,
                )
                .await
            };
            let mut f = Finish::reply(reply, ACTION).with(
                format!(
                    "[au people: {} {}/{}]",
                    r.status.as_str(),
                    r.people.len(),
                    r.total
                ),
                None,
            );
            match r.status {
                Status::NotFound | Status::Unavailable => {
                    f.routes.push(search_button(&r.query, lang))
                }
                Status::Found => {
                    let p = &r.people[0];
                    if let Some(b) = au::building_id_of(s(p, "building")) {
                        if let Ok(Some(found)) = client.building_by_id(&b).await {
                            let title = match room_of(p) {
                                Some(room) => format!("{} — {room}", s(p, "name")),
                                None => s(p, "name").to_owned(),
                            };
                            f.blocks = map_blocks(&title, &found);
                        }
                    }
                }
                Status::Ambiguous => {}
            }
            Some(f)
        }
        Kind::Building => {
            let r = client.building(&ask.query).await;
            let knowledge = format!(
                "AU Find (aufind) building lookup for \"{}\": status {}.\n{}\n",
                r.query,
                r.status.as_str(),
                r.buildings
                    .iter()
                    .map(|b| b.to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            let reply = write_reply(
                t,
                memory,
                &[status_line(r.status)],
                &knowledge,
                render_buildings(&r, lang),
            )
            .await;
            let mut f = Finish::reply(reply, ACTION).with(
                format!("[au building: {} {}]", r.status.as_str(), r.buildings.len()),
                None,
            );
            match r.status {
                Status::NotFound | Status::Unavailable => {
                    f.routes.push(search_button(&r.query, lang))
                }
                Status::Found => {
                    f.blocks = map_blocks(&building_title(&r.buildings[0]), &r.buildings[0])
                }
                Status::Ambiguous => {}
            }
            Some(f)
        }
        Kind::Map => {
            let w = words(lang);
            if ask.query.is_empty() {
                let reply = match lang {
                    "da" => "Her er kortet over Aarhus Universitets campus. Kilde: AU Find (© OpenStreetMap contributors).",
                    _ => "Here is the map of the Aarhus University campus. Source: AU Find (© OpenStreetMap contributors).",
                };
                let mut f = Finish::reply(reply, ACTION)
                    .with(format!("[au map: campus z{CAMPUS_ZOOM}]"), None);
                f.blocks = vec![image_block(
                    au::point_map_path(PARK.0, PARK.1, CAMPUS_ZOOM),
                    "Aarhus University campus",
                )];
                return Some(f);
            }
            let found = client.building_by_id(&ask.query).await;
            let mut f = match found {
                Ok(Some(b)) => {
                    let title = building_title(&b);
                    let reply = format!(
                        "{}: **{title}**{}\n{}: AU Find (© OpenStreetMap contributors)",
                        w.building,
                        if s(&b, "address").is_empty() {
                            String::new()
                        } else {
                            format!(" — {}", s(&b, "address"))
                        },
                        w.source
                    );
                    let mut f = Finish::reply(reply, ACTION)
                        .with(format!("[au map: {} z{BUILDING_ZOOM}]", ask.query), None);
                    f.blocks = map_blocks(&title, &b);
                    return Some(f);
                }
                Ok(None) => Finish::reply(
                    format!("{}: \"{}\". {}", w.not_found, ask.query, w.offer),
                    ACTION,
                )
                .with(format!("[au map: not_found {}]", ask.query), None),
                Err(_) => Finish::reply(w.down, ACTION).with("[au map: unavailable]", None),
            };
            f.routes.push(search_button(&ask.query, lang));
            Some(f)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_tools::tools::au::Attempt;

    #[test]
    fn strong_and_weak_cues() {
        for t in [
            "Find Dennis Vittrup on AU Find",
            "who is the lecturer Vittrup at Aarhus University?",
            "where is the AU office of Dennis?",
            "dov'è l'ufficio AU di Vittrup?",
            "where is bygning 1532?",
            "how do I get to 1532-316",
            "show me the campus map",
            "mappa del campus per favore",
        ] {
            assert!(strong_cue(t), "{t}");
        }
        for t in [
            "what's the weather in Aarhus?",
            "au revoir",
            "AU is great",
            "remind me at 2026-10-29",
            "who is coming tonight?",
        ] {
            assert!(!strong_cue(t), "{t}");
        }
        assert!(weak_cue("where is Dennis's office?"));
        assert!(weak_cue("chi è il professore Vittrup?"));
        assert!(weak_cue("hvem er Dennis Vittrup"));
        assert!(weak_cue("which lokale is it in"));
        assert!(!weak_cue("buy milk"));
    }

    #[test]
    fn extraction() {
        let p = |t: &str| extract(t).map(|a| (a.kind, a.query));
        assert_eq!(
            p("Who is Dennis Vittrup at AU?"),
            Some((Kind::Person, "Dennis Vittrup".into()))
        );
        assert_eq!(
            p("where is Dennis Vittrup's office at AU"),
            Some((Kind::Person, "Dennis Vittrup".into()))
        );
        assert_eq!(
            p("chi è il professore Vittrup all'AU?"),
            Some((Kind::Person, "Vittrup".into()))
        );
        assert_eq!(
            p("ufficio di Marianne Graves Petersen ad Aarhus University"),
            Some((Kind::Person, "Marianne Graves Petersen".into()))
        );
        assert_eq!(
            p("hvem er Dennis Vittrup på AU"),
            Some((Kind::Person, "Dennis Vittrup".into()))
        );
        assert_eq!(p("AU Find: find Anna"), Some((Kind::Person, "Anna".into())));
        assert_eq!(
            p("where is building 1532?"),
            Some((Kind::Building, "1532".into()))
        );
        assert_eq!(
            p("how do I get to 1532-316"),
            Some((Kind::Building, "1532".into()))
        );
        assert_eq!(
            p("show me the AU map of bygning 1872"),
            Some((Kind::Map, "1872".into()))
        );
        assert_eq!(p("campus map please"), Some((Kind::Map, String::new())));
        assert_eq!(p("who is my teacher at AU?"), None);
        assert_eq!(
            parse_extraction("```json\n{\"kind\":\"person\",\"query\":\"Dennis Vittrup\"}\n```"),
            Some(Ask {
                kind: Kind::Person,
                query: "Dennis Vittrup".into()
            })
        );
        assert_eq!(parse_extraction("{\"kind\":\"none\",\"query\":\"\"}"), None);
        assert_eq!(parse_extraction("sorry"), None);
    }

    fn people(status: Status, n: usize, total: u64) -> AuPeople {
        AuPeople {
            source: "aufind",
            status,
            query: "Anna".into(),
            tried: vec![Attempt { q: "Anna".into(), total }],
            total,
            people: (0..n)
                .map(|i| json!({"name": format!("Anna {i}"), "position": "Lecturer", "units": ["AU IT"],
                    "au_room": format!("1532-31{i}"), "email": format!("anna{i}@au.dk"), "phone": null,
                    "profile_url": format!("https://www.au.dk/en/show/person/anna{i}"), "source": "aufind"}))
                .collect(),
            more: (status == Status::Ambiguous && total > 3).then(|| total - 3),
            fallback: None,
            upstream: Some("https://aufind.au.dk/searchPeople.php?q=Anna".into()),
            af_server_version: None,
            error: None,
        }
    }

    #[test]
    fn deterministic_texts() {
        let found = render_people(&people(Status::Found, 1, 1), "en");
        assert!(
            found.contains("**Anna 0** — Lecturer — AU IT — 1532-310"),
            "{found}"
        );
        assert!(found.contains("Email: anna0@au.dk"));
        assert!(!found.contains("Tel."), "no phone is invented: {found}");
        assert!(found.contains(
            "Source: AU Find (aufind); profile www.au.dk https://www.au.dk/en/show/person/anna0"
        ));
        let amb = render_people(&people(Status::Ambiguous, 3, 12), "en");
        assert!(
            amb.starts_with("12 people in AU Find match \"Anna\". The top 3:\n1. **Anna 0**"),
            "{amb}"
        );
        assert!(
            amb.contains("3. **Anna 2**")
                && amb.contains("(+9 more)")
                && amb.contains("Which one do you mean?")
        );
        let mut nf = people(Status::NotFound, 0, 0);
        nf.tried = vec![
            Attempt {
                q: "Anna Zzz Bjerg".into(),
                total: 0,
            },
            Attempt {
                q: "Bjerg".into(),
                total: 0,
            },
        ];
        let t = render_people(&nf, "da");
        assert!(t.starts_with("Ikke fundet i AU Find"), "{t}");
        assert!(t.contains("\"Anna Zzz Bjerg\" (0), \"Bjerg\" (0)"));
        assert!(render_people(&people(Status::Unavailable, 0, 0), "en")
            .starts_with("AU Find is unreachable right now"));
        let k = knowledge_people(&people(Status::Ambiguous, 3, 12));
        assert!(
            k.contains("status ambiguous, total 12") && k.contains("(9 more matches not shown)")
        );
        assert!(k.contains("https://aufind.au.dk/searchPeople.php?q=Anna"));
    }

    #[test]
    fn the_search_button_goes_to_her_computer() {
        for lang in ["en", "da"] {
            let b = search_button("Dennis \"Zzz\" Vittrup", lang);
            let text = b["payload"]["text"].as_str().unwrap();
            let plan = lucina_intent::computer_route::plan(text)
                .unwrap_or_else(|| panic!("not routed: {text}"));
            assert_eq!(plan["action"], "computer_task");
            assert!(plan["task"].as_str().unwrap().contains("www.au.dk"));
            assert!(lucina_store::rich::normalize_action(&b, 0).is_some(), "{b}");
        }
        assert_eq!(search_button("x", "da")["label"], "Søg på au.dk");
        assert_eq!(search_button("x", "it")["label"], "Search au.dk", "never Italian");
    }

    #[test]
    fn blocks_survive_normalization() {
        let b = json!({"id": "1532", "names": ["Bygning 1532"], "address": "Ny Munkegade 120", "latitude": 56.168, "longitude": 10.203});
        let blocks = map_blocks("Dennis Vittrup — 1532-316", &b);
        let kept = lucina_store::rich::merge_blocks(&blocks, vec![]);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0]["type"], "place");
        assert_eq!(kept[0]["lat"], 56.168);
        assert_eq!(kept[1]["src"], "/v1/au/map.png?building=1532&z=17");
        assert_eq!(building_title(&b), "Bygning 1532");
    }
}
