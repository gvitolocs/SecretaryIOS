//! The browsing engine's pure parts: the action-space builder, the step state
//! machine and the stop-before-pay guard.

use std::collections::BTreeMap;

use lucina_jev::{Answer, Decision};
use serde_json::{json, Value};

use super::guard::{self, Kind};
use super::snapshot::{finish, parse_desktop_elements};
use super::step::{self, Memory, Next, Plan};
use super::*;

// ---------------------------------------------------------------- fixtures

fn el(i: u64, role: &str, label: &str, actions: &[&str], x: f64, y: f64) -> Value {
    json!({"element_index": i, "element_token": format!("s9:{i}"), "role": role, "label": label,
           "actions": actions, "enabled": true, "in_web_content": true,
           "frame": {"x": x, "y": y, "w": 120.0, "h": 30.0}})
}

/// A page: the viewport is (0,114)-(1280,800); `rows` are in-page elements.
fn page(title: &str, rows: Vec<Value>) -> Value {
    let mut els = vec![
        json!({"element_index": 3, "element_token": "s9:3", "role": "push button", "label": "Close",
               "actions": ["press"], "enabled": true, "frame": {"x": 1247.0, "y": 27.0, "w": 33.0, "h": 34.0}}),
        json!({"element_index": 9, "element_token": "s9:9", "role": "push button", "label": "Back",
               "actions": ["press"], "enabled": true, "frame": {"x": 0.0, "y": 73.0, "w": 40.0, "h": 34.0}}),
        json!({"element_index": 13, "element_token": "s9:13", "role": "entry", "label": "Address and search bar",
               "value": "https://www.google.com/travel/flights", "actions": ["activate"], "enabled": true,
               "frame": {"x": 158.0, "y": 78.0, "w": 925.0, "h": 24.0}}),
        json!({"element_index": 77, "element_token": "s9:77", "role": "page tab", "label": "Mail - Outlook",
               "actions": ["doDefault", "click"], "enabled": true, "frame": {"x": 40.0, "y": 30.0, "w": 200.0, "h": 30.0}}),
        json!({"element_index": 24, "element_token": "s9:24", "role": "document web", "label": title,
               "actions": ["doDefault"], "enabled": true, "frame": {"x": 0.0, "y": 114.0, "w": 1280.0, "h": 686.0}}),
    ];
    els.extend(rows);
    json!({"elements": els})
}

fn flights() -> Value {
    page(
        "Google Flights",
        vec![
            el(40, "heading", "Flights", &["clickAncestor"], 500.0, 200.0),
            el(41, "combo box", "Where from?", &["activate"], 100.0, 300.0),
            el(42, "combo box", "Where to?", &["activate"], 300.0, 300.0),
            el(43, "combo box", "Departure", &["activate"], 500.0, 300.0),
            el(44, "push button", "Search", &["press"], 600.0, 400.0),
            el(45, "section", "￼", &["clickAncestor"], 600.0, 400.0),
            // Off-screen: the driver says so, and its centre is above the viewport.
            json!({"element_index": 46, "element_token": "s9:46", "role": "link", "label": "Skip to content",
                   "actions": ["jump"], "enabled": true, "in_web_content": true,
                   "description": "off-screen: scroll it into view before a pixel action; element actions still reach it",
                   "frame": {"x": 331.0, "y": -886.0, "w": 100.0, "h": 30.0}}),
            json!({"element_index": 47, "element_token": "s9:47", "role": "check box", "label": "Nonstop only",
                   "actions": ["click"], "checked": true, "enabled": true, "in_web_content": true,
                   "frame": {"x": 700.0, "y": 500.0, "w": 100.0, "h": 30.0}}),
            // Below the fold: not Jev's view, but the answer reads it.
            el(49, "static", "Norwegian 284 Danish kroner", &[], 300.0, 1500.0),
            json!({"element_index": 48, "element_token": "s9:48", "role": "push button", "label": "Hidden",
                   "actions": ["press"], "enabled": false, "in_web_content": true,
                   "frame": {"x": 700.0, "y": 550.0, "w": 100.0, "h": 30.0}}),
        ],
    )
}

fn screen_of(v: &Value, title: &str, kind: Kind, prev: Option<&Screen>) -> Screen {
    let mut s = parse_desktop_elements(v, title, kind);
    finish(&mut s, prev);
    s
}

fn labels(s: &Screen) -> Vec<&str> {
    s.elements.iter().map(|e| e.label.as_str()).collect()
}

fn choice(name: &str, pick: &str, conf: f64, probs: &[(&str, f64)]) -> (String, Answer) {
    (
        name.to_owned(),
        Answer::Choice {
            choice: pick.to_owned(),
            confidence: conf,
            probabilities: probs.iter().map(|(k, p)| ((*k).to_owned(), *p)).collect::<BTreeMap<_, _>>(),
        },
    )
}

fn decision(answers: Vec<(String, Answer)>, done: f64, stuck: f64) -> Decision {
    let mut a: BTreeMap<String, Answer> = answers.into_iter().collect();
    a.insert("done".into(), Answer::Noul(done));
    a.insert("stuck".into(), Answer::Noul(stuck));
    Decision {
        model: "jev-test".into(),
        answers: a,
        input_tokens: 5000,
        elapsed_ms: 170,
    }
}

fn key_of(s: &Screen, label: &str) -> String {
    s.elements.iter().find(|e| e.label == label).unwrap().key.clone()
}

// --------------------------------------------------------- action space

#[test]
fn only_visible_in_page_elements_with_a_real_action_are_offered() {
    let s = screen_of(&flights(), "Google Flights - Chromium", Kind::Browse, None);
    assert_eq!(
        labels(&s),
        ["Back", "Where from?", "Where to?", "Departure", "Search", "Nonstop only"]
    );
    assert_eq!(s.url, "https://www.google.com/travel/flights");
    let from = &s.elements[1];
    assert_eq!((from.key.as_str(), from.handle.as_str(), from.editable), ("e41", "s9:41", true));
    let nonstop = s.elements.iter().find(|e| e.label == "Nonstop only").unwrap();
    assert_eq!(nonstop.state, "checked");
    assert!(s.text.contains("# Page: Google Flights"));
    assert!(s.text.contains("# Flights"));
    assert!(!s.text.contains("Skip to content"), "off-screen text is not Jev's view");
    assert!(!s.text.contains("Norwegian 284"));
    assert!(s.page_text.contains("Norwegian 284") && s.page_text.contains("# Flights"));
    assert!(s.all_text().contains("Norwegian 284"));
}

#[test]
fn new_elements_are_marked_and_offered_first() {
    let before = screen_of(&flights(), "Google Flights - Chromium", Kind::Browse, None);
    let mut v = flights();
    v["elements"].as_array_mut().unwrap().push(json!({
        "element_index": 90, "element_token": "s9:90", "role": "list item", "label": "Billund BLL",
        "actions": ["click"], "enabled": true, "in_web_content": true,
        "frame": {"x": 100.0, "y": 340.0, "w": 200.0, "h": 30.0}}));
    v["elements"][6]["value"] = json!("Billund");
    let after = screen_of(&v, "Google Flights - Chromium", Kind::Browse, Some(&before));
    assert_eq!(after.elements[0].label, "Billund BLL");
    assert!(after.elements[0].fresh);
    let from = after.elements.iter().find(|e| e.label == "Where from?").unwrap();
    assert_eq!(from.state, "value=\"Billund\"");
    assert!(!from.fresh, "a changed value is not a new element");
    assert!(!after.elements.iter().any(|e| e.label == "Search" && e.fresh));
    assert_ne!(before.fingerprint, after.fingerprint);
    // A whole new page has nothing "new" to point at.
    let other = screen_of(&page("Other", vec![el(5, "link", "Home", &["jump"], 10.0, 200.0)]), "Other", Kind::Browse, Some(&before));
    assert!(!other.elements.iter().any(|e| e.fresh));
}

#[test]
fn the_offer_drops_banned_actions_and_caps_scrolls_and_waits() {
    let s = screen_of(&flights(), "Google Flights - Chromium", Kind::Browse, None);
    let plan = Plan::default();
    let mut mem = Memory::default();
    mem.opened_tab = true;
    let o = step::offer(Where::Desktop, &s, &mem, &plan);
    let ops: Vec<&str> = o.ops.iter().map(|(k, _)| *k).collect();
    assert_eq!(ops, ["click", "type", "scroll_down", "escape", "wait", "done", "blocked"]);
    // Clicking "Search" changed nothing: not offered again on this page.
    mem.push("click", "Search", "clicked push button \"Search\"".into(), s.fingerprint);
    mem.settled(s.fingerprint);
    let o = step::offer(Where::Desktop, &s, &mem, &plan);
    assert!(!o.click.iter().any(|e| e.label == "Search"));
    assert!(o.ops.iter().any(|(k, _)| *k == "back"));
    // Four scrolls down in a row: no fifth; scroll_up appears after scrolling down.
    let mut m = Memory::default();
    for i in 0..4 {
        m.push("scroll_down", "", "scroll down".into(), i);
        m.settled(i + 100);
    }
    let ops: Vec<&str> = step::offer(Where::Desktop, &s, &m, &plan).ops.iter().map(|(k, _)| *k).collect();
    assert!(!ops.contains(&"scroll_down"));
    assert!(ops.contains(&"scroll_up"));
    // Two waits in a row: no third.
    let mut m = Memory::default();
    m.push("wait", "", "wait".into(), 1);
    m.push("wait", "", "wait".into(), 1);
    assert!(!step::offer(Where::Desktop, &s, &m, &plan).ops.iter().any(|(k, _)| *k == "wait"));
    // open_url only before the drive has its tab, and only without a planned start.
    let fresh = Memory::default();
    assert!(step::offer(Where::Desktop, &s, &fresh, &plan).ops.iter().any(|(k, _)| *k == "open_url"));
    let planned = Plan { start: Some("https://wolt.com".into()), ..Plan::default() };
    assert!(!step::offer(Where::Desktop, &s, &fresh, &planned).ops.iter().any(|(k, _)| *k == "open_url"));
}

#[test]
fn questions_carry_states_new_marks_and_the_task_values() {
    let s = screen_of(&flights(), "Google Flights - Chromium", Kind::Browse, None);
    let plan = Plan {
        slots: vec![("from".into(), "Billund".into()), ("to".into(), "London".into())],
        ..Plan::default()
    };
    let mem = Memory::default();
    let o = step::offer(Where::Desktop, &s, &mem, &plan);
    let qs = step::questions(Where::Desktop, &o, &plan, &[]);
    let names: Vec<&str> = qs.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, ["operation", "click_target", "type_target", "type_value", "done", "stuck"]);
    let lucina_jev::Question::Choice { options, .. } = &qs[3].1 else { panic!() };
    let keys: Vec<&str> = options.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["slot_from", "slot_to", "other"]);
    let lucina_jev::Question::Choice { options, .. } = &qs[1].1 else { panic!() };
    assert!(options.iter().any(|(_, d)| d == "check box \"Nonstop only\" checked"));
    for (n, q) in &qs {
        q.validate(n).unwrap();
    }
}

// ------------------------------------------------------- gate and fallback

#[test]
fn jev_is_acted_on_at_075_and_below_the_fallback_gets_the_top_five() {
    let s = screen_of(&flights(), "Google Flights - Chromium", Kind::Browse, None);
    let plan = Plan::default();
    let mut mem = Memory::default();
    mem.opened_tab = true;
    let o = step::offer(Where::Desktop, &s, &mem, &plan);
    let search = key_of(&s, "Search");
    let from = key_of(&s, "Where from?");
    let sure = decision(
        vec![
            choice("operation", "click", 0.8, &[("click", 0.85), ("type", 0.1), ("done", 0.05)]),
            choice("click_target", &search, 0.9, &[(&search, 0.92), (&from, 0.08)]),
        ],
        0.1,
        0.1,
    );
    match step::decide(&o, &sure, 0.75, &plan) {
        Next::Act(a) => {
            assert_eq!((a.op.as_str(), a.by), ("click", "jev"));
            assert_eq!(a.target.unwrap().label, "Search");
        }
        other => panic!("{other:?}"),
    }
    // The operation is sure but the target is not: the fallback decides.
    let unsure = decision(
        vec![
            choice("operation", "click", 0.8, &[("click", 0.6), ("type", 0.3), ("scroll_down", 0.1)]),
            choice("click_target", &search, 0.5, &[(&search, 0.5), (&from, 0.3), ("e42", 0.2)]),
            choice("type_target", &from, 0.9, &[(&from, 0.9), ("e42", 0.1)]),
        ],
        0.1,
        0.1,
    );
    let Next::Fallback(c) = step::decide(&o, &unsure, 0.75, &plan) else { panic!() };
    assert_eq!(c.len(), step::TOP_K);
    let got: Vec<(String, String, String)> = c
        .iter()
        .map(|c| (c.id.clone(), c.op.clone(), c.target.as_ref().map(|e| e.label.clone()).unwrap_or_default()))
        .collect();
    // 0.6×0.5, 0.6×0.3, 0.3×0.9, 0.6×0.2 and 0.1 (scroll), best first.
    assert_eq!(got[0], ("c1".into(), "click".into(), "Search".into()));
    assert_eq!(got[1], ("c2".into(), "type".into(), "Where from?".into()));
    assert_eq!(got[2].2, "Where from?");
    assert!(c.windows(2).all(|w| w[0].p >= w[1].p));
    assert_eq!(step::parse_pick(r#"{"pick":"c2"}"#, &c).unwrap().op, "type");
    // Jev leaning towards done puts done in the list.
    let leaning = decision(
        vec![
            choice("operation", "click", 0.5, &[("click", 0.9), ("done", 0.0)]),
            choice("click_target", &search, 0.4, &[(&search, 0.4), (&from, 0.3), ("e42", 0.2), ("e43", 0.1)]),
        ],
        0.55,
        0.0,
    );
    let c = step::candidates(&o, &leaning);
    assert_eq!(c.len(), step::TOP_K);
    assert_eq!(c.last().unwrap().op, "done", "added in place of the fifth click");
    assert!(step::parse_pick(r#"{"pick":"none"}"#, &c).is_none());
    assert!(step::parse_pick("click Search", &c).is_none());
}

#[test]
fn done_stuck_and_blocked_thresholds() {
    let s = screen_of(&flights(), "t", Kind::Browse, None);
    let plan = Plan::default();
    let o = step::offer(Where::Desktop, &s, &Memory::default(), &plan);
    let d = |op: &str, c: f64, done: f64, stuck: f64| decision(vec![choice("operation", op, c, &[(op, c)])], done, stuck);
    assert_eq!(step::decide(&o, &d("click", 0.1, 0.85, 0.0), 0.75, &plan), Next::Finish);
    assert_eq!(step::decide(&o, &d("done", 0.9, 0.3, 0.0), 0.75, &plan), Next::Finish);
    assert!(matches!(step::decide(&o, &d("done", 0.6, 0.3, 0.0), 0.75, &plan), Next::Fallback(_)));
    // Agreeing signals: "done" on top and the done question at 0.65.
    assert_eq!(step::decide(&o, &d("done", 0.4, 0.65, 0.0), 0.75, &plan), Next::Finish);
    assert!(matches!(step::decide(&o, &d("wait", 0.9, 0.1, 0.9), 0.75, &plan), Next::Escalate(_)));
    assert!(matches!(step::decide(&o, &d("blocked", 0.8, 0.1, 0.1), 0.75, &plan), Next::Escalate(_)));
    assert!(matches!(step::decide(&o, &d("wait", 0.9, 0.1, 0.1), 0.75, &plan), Next::Act(_)));
}

#[test]
fn a_sure_task_value_is_typed_without_the_text_model() {
    let s = screen_of(&flights(), "t", Kind::Browse, None);
    let plan = Plan {
        slots: vec![("from".into(), "Billund".into()), ("to".into(), "London".into())],
        ..Plan::default()
    };
    let o = step::offer(Where::Desktop, &s, &Memory::default(), &plan);
    let from = key_of(&s, "Where from?");
    let mk = |slot: &str, c: f64| {
        decision(
            vec![
                choice("operation", "type", 0.9, &[("type", 0.95)]),
                choice("type_target", &from, 0.9, &[(&from, 0.95)]),
                choice("type_value", slot, c, &[(slot, c)]),
            ],
            0.0,
            0.0,
        )
    };
    let Next::Act(a) = step::decide(&o, &mk("slot_from", 0.9), 0.75, &plan) else { panic!() };
    assert_eq!(a.value.as_deref(), Some("Billund"));
    let Next::Act(a) = step::decide(&o, &mk("other", 0.9), 0.75, &plan) else { panic!() };
    assert_eq!(a.value, None, "other: the text model writes it");
    let Next::Act(a) = step::decide(&o, &mk("slot_to", 0.5), 0.75, &plan) else { panic!() };
    assert_eq!(a.value, None, "an unsure slot is not typed");
}

// ------------------------------------------------------------ state machine

#[test]
fn three_actions_that_change_nothing_end_the_drive() {
    let s = screen_of(&flights(), "t", Kind::Browse, None);
    let plan = Plan::default();
    let mut mem = Memory::default();
    mem.opened_tab = true;
    for l in ["A", "B", "C"] {
        assert_eq!(step::before_jev(Where::Desktop, &s, &mem, &plan), None);
        mem.push("click", l, format!("clicked {l}"), s.fingerprint);
        mem.settled(s.fingerprint);
    }
    assert!(matches!(step::before_jev(Where::Desktop, &s, &mem, &plan), Some(Next::Escalate(_))));
    assert!(mem.history()[0].ends_with("(nothing changed)"));
    // A change resets the run.
    mem.push("click", "D", "clicked D".into(), s.fingerprint);
    mem.settled(s.fingerprint + 1);
    assert_eq!(mem.unchanged_run, 0);
    // A refused action is banned on its page and does not count as a loop.
    let mut m = Memory::default();
    m.push("type", "Search in Wolt...", "typed \"Holy Pizza\"".into(), 5);
    m.failed(5);
    assert!(m.banned(5, "type", "Search in Wolt..."));
    assert_eq!(m.unchanged_run, 0);
    assert!(m.history()[0].contains("(failed"));
    // The same action on the same page: the drive counts the repeats.
    let mut m = Memory::default();
    assert_eq!(m.push("click", "X", "x".into(), 7), 0);
    assert_eq!(m.push("click", "X", "x".into(), 7), 1);
    assert_eq!(m.push("click", "X", "x".into(), 8), 0);
}

#[test]
fn the_planned_start_opens_first_then_cookies_are_declined() {
    let consent = page(
        "Before you continue to Google",
        vec![
            el(50, "static", "We use cookies and data to deliver Google services", &[], 400.0, 300.0),
            el(51, "push button", "Accept all", &["press"], 400.0, 500.0),
            el(52, "push button", "Reject all", &["press"], 600.0, 500.0),
            el(53, "push button", "More options", &["press"], 800.0, 500.0),
        ],
    );
    let s = screen_of(&consent, "Before you continue to Google - Chromium", Kind::Browse, None);
    assert!(!labels(&s).contains(&"Accept all"));
    let plan = Plan {
        start: Some("https://www.google.com/travel/flights?hl=en".into()),
        ..Plan::default()
    };
    let mut mem = Memory::default();
    let Some(Next::Act(a)) = step::before_jev(Where::Desktop, &s, &mem, &plan) else { panic!() };
    assert_eq!((a.op.as_str(), a.by, a.value.as_deref()), ("open_url", "rule", plan.start.as_deref()));
    mem.push("open_url", "x", "opened".into(), 1);
    let Some(Next::Act(a)) = step::before_jev(Where::Desktop, &s, &mem, &plan) else { panic!() };
    assert_eq!((a.op.as_str(), a.by), ("click", "cookie"));
    assert_eq!(a.target.unwrap().label, "Reject all");
    // Declined once and nothing changed: not again.
    mem.push("click", "Reject all", "declined".into(), s.fingerprint);
    mem.settled(s.fingerprint);
    assert_eq!(step::before_jev(Where::Desktop, &s, &mem, &plan), None);
}

#[test]
fn cookie_buttons_decline_but_never_accept_all() {
    for l in ["Reject all", "Afvis alle", "Accepter kun nødvendige", "Rifiuta tutto", "Only necessary", "Use necessary cookies only", "Tillad kun nødvendige"] {
        assert!(guard::declines_cookies(l), "{l}");
    }
    for l in ["Accept all", "Accepter alle", "Tillad alle", "Indstillinger", "More options", "Allow all cookies", ""] {
        assert!(!guard::declines_cookies(l), "{l}");
    }
    assert!(guard::consent_wall("Vi bruger cookies til at forbedre", &[]));
    assert!(!guard::consent_wall("Flights from Billund", &["Search"]));
}

// ---------------------------------------------------------- stop before pay

#[test]
fn basket_steps_are_offered_only_when_shopping_and_payment_never() {
    let wolt = page(
        "Holy Pizza | Wolt",
        vec![
            el(60, "push button", "Tilføj til ordre", &["press"], 400.0, 300.0),
            el(61, "push button", "Gå til kassen", &["press"], 400.0, 350.0),
            el(62, "push button", "Betal 189,00 kr.", &["press"], 400.0, 400.0),
            el(63, "push button", "Bestil", &["press"], 400.0, 450.0),
            el(64, "push button", "Apple Pay", &["press"], 400.0, 500.0),
            el(65, "push button", "Order and pay", &["press"], 400.0, 550.0),
            el(66, "push button", "Add card", &["press"], 400.0, 600.0),
            el(67, "push button", "Click to order", &["press"], 400.0, 650.0),
            el(68, "push button", "Margherita", &["press"], 400.0, 700.0),
        ],
    );
    let shop = screen_of(&wolt, "Holy Pizza | Wolt - Chromium", Kind::Shopping, None);
    assert_eq!(labels(&shop), ["Back", "Tilføj til ordre", "Gå til kassen", "Margherita"]);
    let browse = screen_of(&wolt, "Holy Pizza | Wolt - Chromium", Kind::Browse, None);
    assert_eq!(labels(&browse), ["Back", "Margherita"]);
    for l in ["Add to order 89,00 kr", "View order", "Go to checkout", "Aggiungi al carrello"] {
        assert!(!guard::risky(l, Kind::Shopping), "{l}");
        assert!(guard::risky(l, Kind::Browse), "{l}");
    }
    for l in ["Place order", "Bestil nu", "Betal med MobilePay", "Conferma ordine", "Pay 189 kr", "Fortsæt til betaling", "Payment method"] {
        assert!(guard::risky(l, Kind::Shopping), "{l}");
    }
    assert!(!guard::risky("Bestilling af bord", Kind::Browse), "bestil only as a whole word");
}

#[test]
fn the_checkout_is_recognised_and_its_total_read() {
    // Wolt-like: the checkout title and a total, the pay button removed by the filter.
    assert!(guard::at_checkout(
        "Checkout | Wolt - Chromium",
        "https://wolt.com/en/dnk/aarhus/restaurant/holy-pizza/checkout",
        "# Checkout\nHoly Pizza\n1 × Margherita 89,00 kr\nDelivery 39,00 kr\nTotal 128,00 kr",
        &["Click to order".into()],
    ));
    // Just-Eat-like: a payment heading, "I alt", and "Bestil og betal".
    assert!(guard::at_checkout(
        "Just Eat - Chromium",
        "",
        "# Betaling\nDin ordre\nI alt 245,00 kr.",
        &["Bestil og betal".into()],
    ));
    // A menu page with a basket summary is not the checkout.
    assert!(!guard::at_checkout(
        "Holy Pizza | Wolt - Chromium",
        "https://wolt.com/en/dnk/aarhus/restaurant/holy-pizza",
        "# Pizza\nMargherita 89,00 kr\nYour order\nTotal 89,00 kr",
        &[],
    ));
    assert_eq!(guard::total("Subtotal 89,00 kr\nTotal 128,00 kr").as_deref(), Some("128,00 kr"));
    assert_eq!(guard::total("I alt\n245,00 kr.").as_deref(), Some("245,00 kr."));
    assert_eq!(guard::total("Totale: € 12,50").as_deref(), Some("€ 12,50"));
    assert_eq!(guard::total("Total DKK 1 299,50").as_deref(), Some("DKK 1 299,50"));
    assert_eq!(guard::total("Totale 12.50 €").as_deref(), Some("12.50 €"));
    assert_eq!(guard::total("Total kr 189,50").as_deref(), Some("kr 189,50"));
    assert_eq!(guard::total("Total items: 3"), None);
}

#[test]
fn a_shopping_drive_stops_at_the_checkout_whatever_jev_would_do() {
    let checkout = page(
        "Checkout | Wolt",
        vec![
            el(70, "heading", "Checkout", &["clickAncestor"], 400.0, 200.0),
            el(71, "static", "Total 128,00 kr", &[], 400.0, 250.0),
            el(72, "push button", "Click to order", &["press"], 400.0, 600.0),
            el(73, "push button", "Gå til kassen", &["press"], 400.0, 650.0),
        ],
    );
    let s = screen_of(&checkout, "Checkout | Wolt - Chromium", Kind::Shopping, None);
    assert_eq!(s.hard_labels, ["Click to order"]);
    let plan = Plan { kind: Kind::Shopping, start: Some("https://wolt.com".into()), slots: vec![] };
    let mut mem = Memory::default();
    mem.opened_tab = true;
    let stop = step::before_jev(Where::Desktop, &s, &mem, &plan);
    assert_eq!(stop, Some(Next::NeedsOwner("the checkout page is open".into())));
    // Even with nothing opened yet, the stop comes before any action.
    assert!(matches!(step::before_jev(Where::Desktop, &s, &Memory::default(), &plan), Some(Next::NeedsOwner(_))));
    // A sign-in wall in a shopping flow is the owner's too.
    let mut login = screen_of(&flights(), "Wolt", Kind::Shopping, None);
    login.text = "# Page: Wolt\n# Log in or sign up".into();
    assert!(matches!(step::before_jev(Where::Desktop, &login, &mem, &plan), Some(Next::NeedsOwner(_))));
    // Google's one-tap "Continue with Google" on a shop page is not a wall.
    let mut shop = screen_of(&flights(), "Aarhus | Wolt Delivery", Kind::Shopping, None);
    shop.text = "# Page: Aarhus | Wolt\nContinue with Google\n# Discovery".into();
    assert_eq!(step::before_jev(Where::Desktop, &shop, &mem, &plan), None);
    assert!(guard::login_wall("Log ind – Google Konti", "", false));
    assert!(guard::login_wall("Wolt", "", true));
    // Browsing never stops there (and never offers the pay button anyway).
    let b = screen_of(&checkout, "Checkout | Wolt - Chromium", Kind::Browse, None);
    let browse = Plan { kind: Kind::Browse, ..plan.clone() };
    assert_eq!(step::before_jev(Where::Desktop, &b, &mem, &browse), None);
    assert!(!labels(&b).contains(&"Click to order"));
}

// --------------------------------------------------------------- planning

#[test]
fn the_plan_is_validated() {
    let p = step::parse_plan(
        r#"{"kind":"browse","start":"https://www.google.com/travel/flights?hl=en","slots":{"From":"Billund","to":"London","depart":"Fri, Nov 6"}}"#,
        "find flights from Billund to London next Friday",
    )
    .unwrap();
    assert_eq!(p.kind, Kind::Browse);
    assert_eq!(p.slots[0], ("from".into(), "Billund".into()));
    assert_eq!(p.slots.len(), 3);
    let p = step::parse_plan(r#"{"kind":"browse","start":"javascript:alert(1)","slots":{}}"#, "x").unwrap();
    assert_eq!(p.start, None);
    let many: serde_json::Map<String, Value> = (0..12).map(|i| (format!("s{}", (b'a' + i) as char), json!("v"))).collect();
    let p = step::parse_plan(&json!({"kind":"browse","start":"","slots": many}).to_string(), "x").unwrap();
    assert_eq!(p.slots.len(), 8);
    // Food is shopping whatever the planner said.
    let p = step::parse_plan(r#"{"kind":"browse","start":"https://wolt.com","slots":[{"name":"item","value":"Margherita"}]}"#, "order my usual from Holy Pizza").unwrap();
    assert_eq!(p.kind, Kind::Shopping);
    assert_eq!(p.slots, [("item".to_string(), "Margherita".to_string())]);
    assert!(step::parse_plan("not json", "x").is_none());
    assert!(!step::shopping_task("find me flights to London next Friday"));
    assert!(step::shopping_task("bestil min sædvanlige pizza"));
}

#[test]
fn enter_submits_plain_fields_only() {
    let f = |role: &str, label: &str, state: &str| Element {
        role: role.into(),
        label: label.into(),
        state: state.into(),
        editable: true,
        ..Element::default()
    };
    assert!(!submits_on_enter(&f("combo box", "Where from?", "")));
    assert!(!submits_on_enter(&f("entry", "Where to?", "")));
    assert!(!submits_on_enter(&f("entry", "Departure", "")));
    assert!(!submits_on_enter(&f("entry", "Delivery address", "")));
    assert!(!submits_on_enter(&f("entry", "Search", "expanded")));
    assert!(submits_on_enter(&f("entry", "Search in Wolt", "")));
    assert!(submits_on_enter(&f("entry", "Search Google or type a URL", "")));
}

#[test]
fn stats_price_jev_tokens() {
    let s = DriveStats {
        jev_input_tokens: 90_558,
        ..DriveStats::default()
    };
    assert!((s.cost_usd() - 0.0038).abs() < 0.0001);
    assert_eq!(s.to_json()["costUsd"], json!(0.003803));
}

#[test]
fn a_dialog_button_survives_the_cap_next_to_a_calendar() {
    let mut rows: Vec<Value> = (0..250)
        .map(|i| el(100 + i, "table cell", &format!("Day {i}, 2026"), &["click"], 400.0 + (i % 7) as f64, 300.0 + (i / 7) as f64))
        .collect();
    rows.push(el(1811, "push button", "Done.", &["press"], 1029.0, 751.0));
    let s = screen_of(&page("Google Flights", rows), "Google Flights - Chromium", Kind::Browse, None);
    assert_eq!(s.elements.len(), 200);
    assert!(s.elements.iter().any(|e| e.label == "Done."));
    assert_eq!(s.elements[0].label, "Back", "buttons before the bulk of day cells");
}

#[test]
fn the_opened_host_is_checked() {
    assert_eq!(host_of("https://wolt.com/en/dnk/aarhus/search?q=Holy%20Pizza"), Some("wolt.com"));
    assert_eq!(host_of("https://www.google.com/travel/flights?hl=en"), Some("google.com"));
    assert_eq!(host_of("flights Billund London"), None);
    let at = |title: &str| Screen { title: title.into(), ..Screen::default() };
    assert!(missed("wolt.com", &at("DuckDuckGo Private Search Engine - Chromium")));
    assert!(missed("wolt.com", &at("New Tab - Chromium")));
    assert!(!missed("wolt.com", &at("Search | Wolt - Chromium")));
}

#[test]
fn allow_all_is_never_offered_and_an_unticked_selection_declines() {
    for l in ["Allow all", "Accept all cookies", "Tillad alle", "Accetta tutti", "Godkend alle"] {
        assert!(guard::risky(l, Kind::Browse) && guard::risky(l, Kind::Shopping), "{l}");
    }
    let rows = |ticked: bool| {
        let mut perf = el(45, "check box", "Performance cookies", &["click"], 400.0, 300.0);
        if ticked {
            perf["checked"] = json!(true);
        }
        vec![
            el(39, "static", "These cookies are essential for the website to function properly.", &[], 400.0, 250.0),
            perf,
            el(55, "push button", "Allow selection", &["press"], 400.0, 500.0),
            el(56, "push button", "Allow all", &["press"], 600.0, 500.0),
        ]
    };
    let s = screen_of(&page("Rejseplan", rows(false)), "Rejseplan - Chromium", Kind::Browse, None);
    assert!(!labels(&s).contains(&"Allow all"));
    assert_eq!(s.cookie_decline.as_ref().map(|e| e.label.as_str()), Some("Allow selection"));
    let s = screen_of(&page("Rejseplan", rows(true)), "Rejseplan - Chromium", Kind::Browse, None);
    assert_eq!(s.cookie_decline, None, "a ticked optional box: saving the selection would consent");
}
