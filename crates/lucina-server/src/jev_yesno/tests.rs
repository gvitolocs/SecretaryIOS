use super::*;
use chrono::TimeZone;

fn tz() -> Tz {
    "Europe/Copenhagen".parse().unwrap()
}

/// Thursday 29 October 2026, 12:47 in Copenhagen.
fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 29, 11, 47, 0).unwrap()
}

fn verdict(p_yes: f64, grounded: Option<f64>) -> Verdict {
    Verdict {
        p_yes,
        grounded,
        evidence: None,
        ms: 200,
        input_tokens: 900,
    }
}

#[test]
fn the_screen_lets_yes_no_shapes_through_in_three_languages() {
    for q in [
        "Ho palestra oggi?",
        "Is milk on my shopping list?",
        "Har jeg en forelæsning i morgen?",
        "c'è lezione domani?",
        "C’è il latte nella lista?",
        "ciao, devo comprare il pane?",
        // Voice and Alexa: no question mark.
        "har jeg en forelæsning i morgen",
        "do I have class tomorrow",
        "Dovrei andare a correre stasera?",
    ] {
        assert!(looks_like_question(q), "{q}");
    }
}

#[test]
fn the_screen_skips_open_questions_statements_links_and_commands() {
    for q in [
        "Cosa ho domani?",
        "What's on my shopping list?",
        "Cos'è il latte di avena?",
        "Dov'è la palestra?",
        "Hvad har jeg i morgen?",
        "Quando parte il volo?",
        "Who is Ira Assent?",
        "Ho comprato il latte",
        // Italian without a question mark reads as a statement.
        "ho palestra oggi",
        "sono stanco",
        "aggiungi latte alla lista",
        "https://example.com/x?y=1",
        "/status?",
        "ok",
        "?",
    ] {
        assert!(!looks_like_question(q), "{q}");
    }
    assert!(!looks_like_question(&"è vero? ".repeat(60)), "too long");
}

#[test]
fn first_word_handles_apostrophes_and_leading_punctuation() {
    assert_eq!(first_word("C’è lezione?"), "c'è");
    assert_eq!(first_word("¿Har jeg tid?"), "har");
    assert_eq!(first_word("  \"Is it?\""), "is");
}

#[test]
fn thresholds_answer_only_when_sure_and_settled() {
    let d = |k, p, g| decide_with(k, &verdict(p, g), 0.75, 0.6);
    assert_eq!(d(Kind::Personal, 0.93, Some(0.9)), Decision::Answer { yes: true, certainty: 0.93 });
    let Decision::Answer { yes: false, certainty } = d(Kind::Personal, 0.1, Some(0.8)) else {
        panic!("a sure no");
    };
    assert!((certainty - 0.9).abs() < 1e-9);
    // The bars themselves count.
    assert!(matches!(d(Kind::Personal, 0.75, Some(0.6)), Decision::Answer { yes: true, .. }));
    assert!(matches!(d(Kind::Personal, 0.25, Some(0.6)), Decision::Answer { yes: false, .. }));
    // Between the bars: unsure.
    assert_eq!(d(Kind::Personal, 0.74, Some(0.9)), Decision::Unsure);
    assert_eq!(d(Kind::Opinion, 0.5, None), Decision::Unsure);
    assert_eq!(d(Kind::General, 0.26, None), Decision::Unsure);
    // His own data or a live fact: the facts must settle it, however sure P(yes) is.
    assert_eq!(d(Kind::Personal, 0.99, Some(0.4)), Decision::NotSettled);
    assert_eq!(d(Kind::Personal, 0.99, None), Decision::NotSettled);
    assert_eq!(d(Kind::Live, 0.02, Some(0.3)), Decision::NotSettled);
    assert_eq!(d(Kind::Lucina, 0.99, None), Decision::NotSettled);
    // Settled but between the bars: unsure, not "not settled".
    assert_eq!(d(Kind::Personal, 0.6, Some(0.9)), Decision::Unsure);
    // General knowledge and advice need no facts.
    assert!(matches!(d(Kind::General, 0.97, None), Decision::Answer { yes: true, .. }));
    assert!(matches!(d(Kind::Opinion, 0.1, None), Decision::Answer { yes: false, .. }));
}

#[test]
fn every_kind_option_round_trips() {
    for (name, _) in KIND_OPTIONS {
        assert_eq!(Kind::from_name(name).map(Kind::name), Some(name));
    }
}

#[test]
fn threshold_defaults_and_env_overrides() {
    assert_eq!(GATE_MIN, 0.75);
    assert_eq!(ANSWER_MIN, 0.75);
    assert_eq!(SENSITIVE_MIN, 0.75);
    assert_eq!(env_f64("LUCINA_JEV_YESNO_TEST_UNSET", 0.75), 0.75);
}

#[test]
fn calendar_facts_carry_the_day_and_today_or_tomorrow() {
    let evs = vec![
        json!({"title": "Research Methodology", "start": "2026-10-29T07:00:00Z", "end": "2026-10-29T11:00:00Z",
               "location": "5124-038 Seminarrum", "allDay": false}),
        json!({"title": "Ethical Hacking", "start": "2026-10-30T07:00:00Z", "end": "2026-10-30T11:00:00Z", "allDay": false}),
        json!({"title": "Halloween", "start": "2026-10-31", "allDay": true}),
        json!({"title": "no start"}),
    ];
    let f = calendar_facts(&evs, now(), tz());
    assert_eq!(f.len(), 3);
    assert_eq!(f[0].source, "calendar");
    assert!(f[0].text.starts_with("Thu 2026-10-29 (today) 08:00–12:00 Research Methodology"), "{}", f[0].text);
    assert!(f[0].text.contains("@ 5124-038"));
    assert!(f[1].text.starts_with("Fri 2026-10-30 (tomorrow) 08:00–12:00 Ethical Hacking"), "{}", f[1].text);
    assert!(f[2].text.starts_with("Sat 2026-10-31 all day Halloween"), "{}", f[2].text);
}

#[test]
fn the_calendar_says_which_days_it_covers() {
    let r = |f: &str, t: &str| (f.to_owned(), t.to_owned());
    assert_eq!(
        calendar_coverage(&[r("2026-10-29", "2026-11-05"), r("2027-08-28", "2027-08-28")]),
        "Calendar shown for 2026-10-29..2026-11-05 and 2027-08-28 only; other days are not shown here (unknown, not empty)."
    );
    // The day he names is read when the parser knows it.
    let q = lucina_intent::calendar::parse_question("per il 28 agosto ho da fare qualcosa?", now(), tz());
    if let Some(q) = q {
        assert!(q.from.ends_with("-08-28"), "{}", q.from);
    }
}

#[test]
fn shopping_facts_list_each_item_and_say_when_empty() {
    let items = vec![
        json!({"product": "latte", "status": "needed", "quantity": "2 l"}),
        json!({"product": "pane", "status": "needed"}),
        json!({"product": "uova", "status": "purchased"}),
    ];
    let f = shopping_facts(&items);
    let texts: Vec<&str> = f.iter().map(|f| f.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "Shopping list, still to buy: latte (2 l)",
            "Shopping list, still to buy: pane",
            "Shopping list, recently bought (not to buy): uova",
        ]
    );
    let empty = shopping_facts(&[json!({"product": "uova", "status": "purchased"})]);
    assert_eq!(empty[0].text, "Shopping list: nothing left to buy.");
}

#[test]
fn gym_and_body_facts_read_the_secretary_views() {
    let gym = json!({"view": {"summary": {"weekly_workouts": 1, "weekly_goal": 3, "active_completed_sets": 4},
                              "state": {"active": {"id": "w1"}}}});
    let f = gym_facts(&gym);
    assert!(f[0].text.starts_with("Gym this week: 1 of 3 workouts done"));
    assert!(f[1].text.contains("in progress"));
    assert!(gym_facts(&json!({"ok": false})).is_empty());
    let body = json!({"view": {"stats": {"count": 12, "goal_reached": false},
        "stat_labels": {"current": "81.2 kg", "avg7": "81.5 kg", "weekly_change": "-0.4 kg", "trend30": "-1.1 kg",
                        "goal": "78 kg", "goal_distance": "3.2 kg"}}});
    let b = body_facts(&body);
    assert_eq!(b[0].text, "Body weight: current 81.2 kg, 7-day average 81.5 kg, weekly change -0.4 kg, 30-day trend -1.1 kg; goal 78 kg (3.2 kg to go).");
    assert!(body_facts(&json!({"view": {"stats": {"count": 0}}})).is_empty());
}

#[test]
fn reminder_facts_keep_pending_ones_in_local_time() {
    let r: lucina_store::reminders::Reminder = serde_json::from_value(json!({
        "id": "r1", "text": "chiama il dentista", "dueAt": "2026-10-30T08:00:00.000Z", "status": "pending"
    }))
    .or_else(|_| serde_json::from_value(json!({
        "id": "r1", "text": "chiama il dentista", "due_at": "2026-10-30T08:00:00.000Z", "status": "pending"
    })))
    .unwrap();
    let mut done = r.clone();
    done.status = "sent".into();
    let f = reminder_facts(&[r, done], tz());
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].text, "Reminder Fri 2026-10-30 09:00: chiama il dentista");
    assert_eq!(reminder_facts(&[], tz())[0].text, "Reminders: none pending.");
}

#[test]
fn the_state_numbers_facts_live_sources_first_and_keeps_background_last() {
    let facts = Facts {
        now: now_line(now(), tz()),
        facts: vec![
            Fact { source: "calendar", text: "Thu 2026-10-29 (today) 17:00–18:30 Palestra".into() },
            Fact { source: "shopping", text: "Shopping list, still to buy: latte".into() },
        ],
        background: background("Giuseppe goes to the gym on Mondays.", "Giuseppe: ciao\nLucina: ciao!"),
    };
    let s = state_text("Ho palestra oggi?", Kind::Personal, &facts);
    assert!(s.starts_with("Now: Thursday 29 October 2026, 12:47 (Europe/Copenhagen). Today is Thu 2026-10-29, tomorrow is Fri 2026-10-30."), "{s}");
    assert!(s.contains("asks Lucina (his assistant): \"Ho palestra oggi?\""));
    let f1 = s.find("f1 [calendar] Thu 2026-10-29 (today) 17:00–18:30 Palestra").unwrap();
    let f2 = s.find("f2 [shopping] Shopping list, still to buy: latte").unwrap();
    let bg = s.find("Background").unwrap();
    assert!(f1 < f2 && f2 < bg);
    // General knowledge: no personal data in the state at all.
    let g = state_text("Is Copenhagen the capital of Denmark?", Kind::General, &facts);
    assert!(!g.contains("Palestra") && !g.contains("latte") && !g.contains("Mondays"), "{g}");
}

#[test]
fn background_is_clipped_and_keeps_the_newest_turns() {
    let recent = format!("{}NEWEST", "x".repeat(5000));
    let b = background("", &recent);
    assert!(b.ends_with("NEWEST"));
    assert!(b.chars().count() <= RECENT_CHARS + 1);
}

#[test]
fn the_answer_questions_offer_every_fact_and_none() {
    let facts = Facts {
        facts: (0..3).map(|i| Fact { source: "calendar", text: format!("event {i}") }).collect(),
        ..Facts::default()
    };
    let qs = answer_questions(Kind::Personal, &facts);
    let names: Vec<&str> = qs.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, ["answer", "grounded", "evidence"]);
    let Question::Choice { options, .. } = &qs[2].1 else { panic!() };
    let keys: Vec<&str> = options.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["f1", "f2", "f3", "none"]);
    for (n, q) in &qs {
        q.validate(n).unwrap();
    }
    assert_eq!(answer_questions(Kind::General, &facts).len(), 1);
    for (n, q) in gate_questions() {
        q.validate(n).unwrap();
    }
}

#[test]
fn replies_are_short_in_his_language() {
    assert_eq!(reply_text("it", true, 0.93, "Oggi alle 17:00 hai Palestra."), "Sì (93%, quasi sicuro). Oggi alle 17:00 hai Palestra.");
    assert_eq!(reply_text("da", false, 0.97, "Der står ingen forelæsning i kalenderen i morgen."), "Nej (97%, sikkert). Der står ingen forelæsning i kalenderen i morgen.");
    assert_eq!(reply_text("en", true, 0.8, ""), "Yes (80%, likely).");
    assert_eq!(answer_word("it", false), "No");
    assert_eq!(lang_code("Reply language: Danish (the owner's message)."), "da");
    assert_eq!(lang_code("Reply language: Italian …"), "it");
    assert_eq!(lang_code(""), "en");
    assert!(unsure_line("it").starts_with("Non ne sono sicura"));
    assert!(hedge_line("da", "medical").contains("medicinsk"));
    assert!(hedge_line("en", "financial").contains("not professional advice"));
}

#[test]
fn reasons_come_from_the_evidence_and_its_source() {
    let facts = Facts {
        facts: vec![
            Fact { source: "calendar", text: "A".into() },
            Fact { source: "shopping", text: "B".into() },
            Fact { source: "calendar", text: "C".into() },
        ],
        ..Facts::default()
    };
    assert_eq!(reason_facts(&facts, Some(2)), ["C", "A"]);
    assert_eq!(reason_facts(&facts, None), ["A", "B", "C"]);
    assert_eq!(reason_facts(&facts, Some(9)), ["A", "B", "C"]);
}

#[test]
fn model_reasons_are_one_clean_sentence_or_nothing() {
    assert_eq!(clean_reason(" \"Oggi alle 17:00 hai Palestra.\" ").as_deref(), Some("Oggi alle 17:00 hai Palestra."));
    assert_eq!(clean_reason("NONE"), None);
    assert_eq!(clean_reason("DISAGREE"), None);
    assert_eq!(clean_reason("Sì, hai palestra."), None, "no yes/no word first");
    assert_eq!(clean_reason("Line one.\nLine two."), None);
    assert_eq!(clean_reason(&"a".repeat(300)), None);
    assert_eq!(clean_reason(""), None);
}

#[test]
fn the_badge_names_the_answer_and_never_a_pass() {
    let mut r = Run {
        outcome: Outcome::Answer { reply: "Sì (93%, quasi sicuro).".into() },
        gate: None,
        verdict: Some(verdict(0.07, Some(0.9))),
        kind: Some(Kind::Personal),
        jev_ms: 412,
        total_ms: 1300,
        facts: 5,
    };
    let b = r.badge().unwrap();
    assert_eq!(b["feature"], "yesno");
    assert_eq!(b["answer"], "no");
    assert_eq!(b["p"], 0.93);
    assert_eq!(b["ms"], 412);
    r.outcome = Outcome::Unsure { line: "x".into() };
    assert_eq!(r.badge().unwrap()["answer"], "unsure");
    r.outcome = Outcome::Pass { why: "not_yes_no" };
    assert!(r.badge().is_none());
}
