use std::fs::OpenOptions;
use std::io::Write;

use lucina_store::timeline::{
    conversation_key, normalize, ConversationFilter, Meta, NewItem, TimelineStore,
};
use lucina_store::StoreError;

fn input(channel: &str, device: Option<&str>, text: &str) -> NewItem {
    NewItem {
        channel: channel.into(),
        role: "user".into(),
        text: text.into(),
        meta: Some(Meta {
            device: device.map(str::to_owned),
            ..Meta::default()
        }),
        ..NewItem::default()
    }
}

#[test]
fn conversation_rules() {
    for (channel, expected) in [
        ("web", "legacy:web"),
        ("ios", "legacy:ios"),
        ("telegram", "telegram"),
        ("discord", "discord"),
        ("alexa", "alexa"),
        ("voice", "voice"),
        ("other", "lucina"),
    ] {
        assert_eq!(conversation_key(channel, None), expected);
    }
    let mut meta = Meta {
        device: Some("Phone_1-A".into()),
        ..Meta::default()
    };
    for channel in ["web", "ios"] {
        assert_eq!(conversation_key(channel, Some(&meta)), "device:Phone_1-A");
    }
    meta.via = Some("mcp".into());
    meta.caller = Some("ChAt GPT!_-/é".into());
    assert_eq!(conversation_key("ios", Some(&meta)), "agent:chatgpt_-");
    meta.caller = Some("!?".into());
    assert_eq!(conversation_key("telegram", Some(&meta)), "agent:mcp");
}

#[test]
fn device_meta_normalization_roundtrip_and_raw_legacy() {
    let mut i = input("ios", Some("Phone-1"), "hello");
    i.meta.as_mut().unwrap().device_name = Some(format!(" \u{0000}{}\n ", "😀".repeat(40)));
    let normalized = normalize(i, 0).unwrap();
    let json = serde_json::to_string(&normalized).unwrap();
    let parsed: lucina_store::TimelineItem = serde_json::from_str(&json).unwrap();
    let meta = parsed.meta.unwrap();
    assert_eq!(meta.device.as_deref(), Some("Phone-1"));
    assert_eq!(meta.device_name.unwrap(), "😀".repeat(32));
    assert!(json.contains("\"device\":\"Phone-1\",\"deviceName\":"));
    for invalid in ["", "bad id", "a/b", &"x".repeat(65)] {
        let item = normalize(input("web", Some(invalid), "x"), 0).unwrap();
        assert!(item.meta.is_none());
    }
    let merged = Meta::default().merge(Meta {
        device: Some("x".into()),
        device_name: Some("Phone".into()),
        ..Meta::default()
    });
    assert_eq!(merged.device_name.as_deref(), Some("Phone"));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("timeline.jsonl");
    let raw = r#"{ "id":"01K83ZK4Q6X1V9W2D3E4F5G6H7", "ts":"2026-10-21T16:30:02.118Z", "channel":"web", "role":"user", "text":"old", "unknown":true }"#;
    std::fs::write(&path, format!("{raw}\n")).unwrap();
    let s = TimelineStore::new(path, 10000, true);
    assert_eq!(
        &*s.page(None, None, 50, &ConversationFilter::All)
            .unwrap()
            .items[0]
            .raw,
        raw
    );
}

#[test]
fn filtered_pages_and_arrival_cursors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("timeline.jsonl");
    let s = TimelineStore::new(&path, 100000, true);
    let a = s.append(input("web", Some("A"), "a")).unwrap();
    let legacy = s.append(input("web", None, "legacy")).unwrap();
    let b = s.append(input("ios", Some("B"), "b")).unwrap();
    let own = s.append(input("other", None, "own")).unwrap();
    // A smaller unique id, delivered after all the above items.
    let late = normalize(
        NewItem {
            id: Some("00000000000000000000000001".into()),
            ..input("web", Some("A"), "late")
        },
        0,
    )
    .unwrap();
    writeln!(
        OpenOptions::new().append(true).open(&path).unwrap(),
        "{}",
        serde_json::to_string(&late).unwrap()
    )
    .unwrap();
    let mine = ConversationFilter::Mine {
        device: "A".into(),
        channel: "web".into(),
    };
    let p = s.page(None, None, 2, &mine).unwrap();
    assert_eq!(
        p.items.iter().map(|i| &i.id).collect::<Vec<_>>(),
        vec![&own.id, &legacy.id]
    );
    assert_eq!(p.cursor.as_deref(), Some(late.id.as_str()));
    let p = s.page(p.next_before.as_deref(), None, 2, &mine).unwrap();
    assert_eq!(
        p.items.iter().map(|i| &i.id).collect::<Vec<_>>(),
        vec![&a.id, &late.id]
    );
    assert!(p.next_before.is_none());
    let summary = s
        .conversations()
        .unwrap()
        .into_iter()
        .find(|c| c.key == "device:A")
        .unwrap();
    assert_eq!(summary.count, 2);
    assert_eq!(summary.last_id, a.id);
    assert_eq!(summary.last_text, "a");
    let key = ConversationFilter::Key("device:A".into());
    let p = s.page(None, None, 1, &key).unwrap();
    assert_eq!(p.items[0].id, a.id);
    let p = s.page(p.next_before.as_deref(), None, 1, &key).unwrap();
    assert_eq!(p.items[0].id, late.id);
    assert!(p.next_before.is_none());
    let p = s.page(None, Some(&a.id), 1, &mine).unwrap();
    assert_eq!(p.items[0].id, legacy.id);
    assert!(p.has_more);
    assert_eq!(p.cursor.as_deref(), Some(legacy.id.as_str()));
    let p = s.page(None, p.cursor.as_deref(), 1, &mine).unwrap();
    assert_eq!(p.items[0].id, own.id);
    assert!(p.has_more);
    let p = s.page(None, p.cursor.as_deref(), 1, &mine).unwrap();
    assert_eq!(p.items[0].id, late.id);
    assert!(!p.has_more);
    let p = s.page(None, p.cursor.as_deref(), 1, &mine).unwrap();
    assert!(p.items.is_empty());
    assert!(!p.has_more);
    let p = s
        .page(
            None,
            Some(&b.id),
            10,
            &ConversationFilter::Key("missing".into()),
        )
        .unwrap();
    assert!(p.items.is_empty());
    assert_eq!(p.cursor.as_deref(), Some(late.id.as_str()));
    assert!(!p.has_more);
    let p = s
        .page(None, Some(&legacy.id), 10, &ConversationFilter::All)
        .unwrap();
    assert_eq!(
        p.items.iter().map(|i| &i.id).collect::<Vec<_>>(),
        vec![&own.id, &b.id, &late.id]
    );
    assert!(matches!(
        s.page(None, Some("unknown"), 1, &mine),
        Err(StoreError::CursorExpired)
    ));
}

#[test]
fn summaries_follow_foreign_writers_and_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("timeline.jsonl");
    let s = TimelineStore::new(&path, 700, true);
    let mut i = input("ios", Some("A"), " first\n  message ");
    i.meta.as_mut().unwrap().device_name = Some("My phone".into());
    let first = s.append(i).unwrap();
    let mut i = input("web", Some("A"), "second\n   message");
    i.role = "lucina".into();
    i.meta.as_mut().unwrap().device_name = Some("New name".into());
    let foreign = normalize(i, lucina_store::timeutil::now_ms() + 1000).unwrap();
    writeln!(
        OpenOptions::new().append(true).open(&path).unwrap(),
        "{}",
        serde_json::to_string(&foreign).unwrap()
    )
    .unwrap();
    let summaries = s.conversations().unwrap();
    let c = &summaries[0];
    assert_eq!(c.count, 2);
    assert_eq!(c.title, "New name");
    assert_eq!(c.last_id, foreign.id);
    assert_eq!(c.last_ts, foreign.ts);
    assert_eq!(c.last_role, "lucina");
    assert_eq!(c.last_text, "second message");
    for n in 0..20 {
        s.append(input(
            "telegram",
            None,
            &format!("filler {n} {}", "😀".repeat(90)),
        ))
        .unwrap();
    }
    let all = s.read_all().unwrap();
    assert!(!all.iter().any(|i| i.id == first.id));
    let c = s.conversations().unwrap();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].count, all.len());
    assert_eq!(c[0].last_id, all.last().unwrap().id);
    assert!(c[0].last_text.encode_utf16().count() <= 160);
    let fresh = TimelineStore::new(&path, 700, true);
    assert_eq!(
        serde_json::to_value(c).unwrap(),
        serde_json::to_value(fresh.conversations().unwrap()).unwrap()
    );
    for n in 0..8 {
        fresh
            .append(input(
                "discord",
                None,
                &format!("external rotation {n} {}", "x".repeat(200)),
            ))
            .unwrap();
    }
    let observer = s.conversations().unwrap();
    assert_eq!(
        serde_json::to_value(&observer).unwrap(),
        serde_json::to_value(fresh.conversations().unwrap()).unwrap()
    );
    assert_eq!(
        observer.iter().map(|c| c.count).sum::<usize>(),
        s.len().unwrap()
    );
    assert!(matches!(
        s.page(None, Some(&first.id), 1, &ConversationFilter::All),
        Err(StoreError::CursorExpired)
    ));
}

#[test]
fn conversation_titles() {
    let dir = tempfile::tempdir().unwrap();
    let s = TimelineStore::new(dir.path().join("timeline.jsonl"), 100000, true);
    for (caller, title) in [
        ("chatgpt", "ChatGPT"),
        ("claude", "Claude"),
        ("claude-code", "Claude Code"),
        ("grok", "Grok Bot"),
        ("grok-bot", "Grok Bot"),
        ("cursor", "Cursor"),
        ("codex", "Codex"),
        ("Custom", "Custom"),
    ] {
        let mut i = input("web", None, "hello");
        i.meta = Some(Meta {
            via: Some("mcp".into()),
            caller: Some(caller.into()),
            ..Meta::default()
        });
        let id = s.append(i).unwrap().id;
        let c = s
            .conversations()
            .unwrap()
            .into_iter()
            .find(|c| c.last_id == id)
            .unwrap();
        assert_eq!(c.title, title);
        assert_eq!(c.kind, "agent");
    }
}

#[test]
fn the_voice_thread_is_one_conversation_for_every_speaker() {
    let dir = tempfile::tempdir().unwrap();
    let s = TimelineStore::new(dir.path().join("timeline.jsonl"), 100000, true);
    // A speaker's device never splits the thread, and the app's own devices keep theirs.
    for (device, name) in [("spk-kitchen", "Kitchen"), ("spk-desk", "Desk")] {
        let mut i = input("voice", Some(device), "che ore sono?");
        i.meta.as_mut().unwrap().device_name = Some(name.into());
        s.append(i).unwrap();
    }
    s.append(input("ios", Some("Phone-1"), "ciao")).unwrap();
    let voice = s
        .conversations()
        .unwrap()
        .into_iter()
        .find(|c| c.key == "voice")
        .unwrap();
    assert_eq!(voice.title, "Lucina Voice");
    assert_eq!(voice.kind, "voice");
    assert_eq!(voice.channel, "voice");
    assert_eq!(voice.count, 2);
    assert!(voice.device.is_none() && voice.device_name.is_none());
    let key = ConversationFilter::Key("voice".into());
    assert_eq!(s.page(None, None, 10, &key).unwrap().items.len(), 2);
    // `scope=mine` of an app device is its own chat and the main one, not the speakers' thread.
    let mine = ConversationFilter::Mine {
        device: "Phone-1".into(),
        channel: "ios".into(),
    };
    let texts: Vec<String> = s
        .page(None, None, 10, &mine)
        .unwrap()
        .items
        .iter()
        .map(|i| i.raw.to_string())
        .collect();
    assert_eq!(texts.len(), 1, "{texts:?}");
}

#[test]
fn voice_meta_keeps_numbers_and_short_words_only() {
    let mut i = input("voice", Some("spk-1"), "che ore sono?");
    i.meta.as_mut().unwrap().voice = Some(serde_json::json!({
        "turnId": "t-1",
        "ms": {"stt": 80, "total": 1234.4, "bad key!": 5, "neg": -1, "text": "che ore sono"},
        "transcript": "never stored",
        "route": {"intent": "temporal_query", "fast": true, "source": "jev", "confidence": 0.9731,
            "jevMs": 212, "note": "x",
            "layers": [
                {"layer": "intent", "verdict": "temporal_query", "confidence": 0.97, "ms": 212, "used": true},
                {"layer": "slots<", "verdict": "x"},
                {"layer": "confirm", "verdict": "ask: sure?", "used": true}
            ]}
    }));
    let item = normalize(i, 0).unwrap();
    let v = item.meta.unwrap().voice.unwrap();
    assert_eq!(v["turnId"], "t-1");
    assert_eq!(v["ms"]["stt"], 80);
    assert_eq!(v["ms"]["total"], 1234.4);
    assert!(v["ms"].get("bad key!").is_none() && v["ms"].get("neg").is_none());
    assert!(v["ms"].get("text").is_none());
    assert!(v.get("transcript").is_none());
    assert_eq!(v["route"]["intent"], "temporal_query");
    assert_eq!(v["route"]["fast"], true);
    assert_eq!(v["route"]["confidence"], 0.973);
    assert!(v["route"].get("note").is_none());
    let layers = v["route"]["layers"].as_array().unwrap();
    // A layer with a bad name is dropped; a verdict that is not a short word is dropped from its layer.
    assert_eq!(layers.len(), 2, "{layers:?}");
    assert_eq!(layers[0]["layer"], "intent");
    assert_eq!(layers[0]["used"], true);
    assert_eq!(layers[1]["layer"], "confirm");
    assert!(layers[1].get("verdict").is_none());
    // Nothing usable: no meta at all.
    let mut i = input("voice", None, "x");
    i.meta.as_mut().unwrap().voice = Some(serde_json::json!({"transcript": "no"}));
    assert!(normalize(i, 0).unwrap().meta.is_none());
}
