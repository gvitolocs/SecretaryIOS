use super::*;

fn entry(id: &str, word: &str, sounds: &[&str], meaning: Option<&str>, upd: &str) -> Entry {
    Entry {
        id: id.into(),
        word: word.into(),
        sounds_like: sounds.iter().map(|s| s.to_string()).collect(),
        meaning: meaning.map(str::to_owned),
        lang: None,
        created_at: upd.into(),
        updated_at: upd.into(),
        source: "app".into(),
    }
}

fn owner() -> Vocab {
    Vocab {
        entries: vec![
            entry(
                "1",
                "Skibigårdsvej",
                &[],
                Some("the owner's home street"),
                "2026-11-01T10:00:00Z",
            ),
            entry(
                "2",
                "Horus",
                &["orus"],
                Some("the owner's project"),
                "2026-11-01T11:00:00Z",
            ),
        ],
    }
}

fn fixed(v: &Vocab, text: &str) -> String {
    v.correct(text).text
}

#[test]
fn street_misheard_in_all_the_ways_whisper_does() {
    let v = owner();
    for heard in [
        "skibi gaardsvej",
        "Skibigårdsvai",
        "skibigordsvej",
        "Skibigaardsvej",
        "skibi gårds vej",
        "Skibi Gaard's Vej",
        "Skibigårds-vej",
        "skibygordsvay",
        "Skipigårdsvej",
        "Skibigårdsway",
    ] {
        let r = v.correct(&format!("Sto tornando a {heard}."));
        assert_eq!(r.text, "Sto tornando a Skibigårdsvej.", "{heard}");
        assert_eq!(
            r.corrections,
            vec![(heard.to_string(), "Skibigårdsvej".to_string())]
        );
        assert!(r.ambiguous.is_empty(), "{heard}");
    }
}

#[test]
fn horus_and_its_mishearings() {
    let v = owner();
    assert_eq!(
        fixed(&v, "Can you check horus status"),
        "Can you check Horus status"
    );
    assert_eq!(
        fixed(&v, "Ricordami di Orus domani"),
        "Ricordami di Horus domani"
    );
    // The explicit alias is exact, the key matches the rest.
    assert_eq!(
        fixed(&v, "Orus è il mio progetto"),
        "Horus è il mio progetto"
    );
    // One vowel off in a four-letter key is only borderline: ask, don't replace.
    let r = v.correct("Horas è il mio progetto");
    assert_eq!(r.text, "Horas è il mio progetto");
    assert_eq!(r.ambiguous.len(), 1);
    assert_eq!(r.ambiguous[0].candidates, ["Horus"]);
    // Already right: untouched, nothing reported.
    let r = v.correct("Horus è il mio progetto");
    assert_eq!(r.text, "Horus è il mio progetto");
    assert!(r.corrections.is_empty() && r.ambiguous.is_empty());
}

#[test]
fn apostrophe_elision_keeps_the_article() {
    let v = owner();
    assert_eq!(fixed(&v, "Com'è l'Horus oggi?"), "Com'è l'Horus oggi?");
    assert_eq!(fixed(&v, "Com'è l'horus oggi?"), "Com'è l'Horus oggi?");
    assert_eq!(fixed(&v, "a Skibigårdsvej"), "a Skibigårdsvej");
    assert_eq!(fixed(&v, "a skibigordsvej"), "a Skibigårdsvej");
}

#[test]
fn ordinary_sentences_are_left_alone() {
    let v = owner();
    for s in [
        "Sto tornando a casa, ricordami di comprare il latte quando arrivo.",
        "Che ore sono? Quali promemoria ho oggi?",
        "Cosa c'è nella lista della spesa di domani sera?",
        "Aggiungi il pane e le uova alla lista della spesa.",
        "Chiama Giuseppe e dimmi che orario ha il treno per Aarhus.",
        "Can you check the status of my project and tell me how many hours are left?",
        "Remind me to buy milk when I arrive at the house, and turn the horse around.",
        "What time is it in Copenhagen right now, and is the museum open today?",
        "Please add oranges, bread and a horror movie to my list.",
        "Jeg er på vej hjem fra arbejde, og jeg skal købe mælk på vejen.",
        "Kan du huske at ringe til min mor i morgen aften?",
        "Hvad er vejret i Aarhus i dag, og hvornår går toget?",
        "Skibsværftet ligger ved havnen, og gårdsvejen er lukket.",
        "I hours of Horace and the Orson Welles film were boring.",
        "Il progetto Orsini è in ritardo ma il budget è ok.",
        "Dov'è la via più breve per la stazione centrale?",
    ] {
        let r = v.correct(s);
        assert_eq!(r.text, s, "{s}");
        assert!(r.corrections.is_empty(), "{s}: {:?}", r.corrections);
    }
}

#[test]
fn short_words_are_never_touched_by_the_fuzzy_match() {
    let v = Vocab {
        entries: vec![
            entry("1", "Ada", &[], None, "1"),
            entry("2", "Ugo", &[], None, "1"),
        ],
    };
    for s in ["ora ada ugo", "ido ado ugo ada", "Ugo e Ada ed io"] {
        assert!(v.correct(s).corrections.len() <= 2);
    }
    // 3-char keys: only an exact (case-insensitive) match is touched.
    let r = v.correct("ado ida ugu");
    assert_eq!(r.text, "ado ida ugu");
    assert_eq!(v.correct("ada e ugo").text, "Ada e Ugo");
}

#[test]
fn another_entrys_exact_word_is_not_taken_from_it() {
    let v = Vocab {
        entries: vec![
            entry("1", "Aarhus", &[], None, "1"),
            entry("2", "Horus", &["orus"], None, "2"),
        ],
    };
    // "Aarhus" and "Horus" are one vowel apart in key; each exact spelling stays.
    for s in ["Vado ad Aarhus", "Horus ad Aarhus", "Vado ad Horus"] {
        let r = v.correct(s);
        assert_eq!(r.text, s);
        assert!(r.ambiguous.is_empty(), "{s}");
    }
}

#[test]
fn two_close_entries_are_reported_not_replaced() {
    let v = Vocab {
        entries: vec![
            entry("1", "Nicola", &[], None, "1"),
            entry("2", "Nicolò", &[], None, "2"),
        ],
    };
    let r = v.correct("Ho parlato con nicola e nicolò.");
    assert_eq!(r.text, "Ho parlato con Nicola e Nicolò.");
    let r = v.correct("Ho parlato con Nicolu.");
    assert_eq!(r.text, "Ho parlato con Nicolu.");
    assert_eq!(r.ambiguous.len(), 1);
    assert_eq!(r.ambiguous[0].heard, "Nicolu");
    let mut c = r.ambiguous[0].candidates.clone();
    c.sort();
    assert_eq!(c, ["Nicola", "Nicolò"]);
    assert!(r.corrections.is_empty());
}

#[test]
fn borderline_is_reported_with_its_one_candidate() {
    let v = Vocab {
        entries: vec![entry("1", "Skibigårdsvej", &[], None, "1")],
    };
    // Close, but too many edits to replace on our own: ask instead.
    let r = v.correct("Sto tornando a skebardvaj.");
    assert_eq!(r.text, "Sto tornando a skebardvaj.");
    assert!(r.corrections.is_empty());
    assert_eq!(
        r.ambiguous,
        vec![Ambiguity {
            heard: "skebardvaj".into(),
            candidates: vec!["Skibigårdsvej".into()]
        }]
    );
    // A few edits away is a replacement.
    assert_eq!(v.correct("a skebardsvaj").text, "a Skibigårdsvej");
    let far = v.correct("Sto tornando a Skagen e Skibsværftet");
    assert!(far.ambiguous.is_empty() && far.corrections.is_empty());
}

#[test]
fn explicit_sounds_like_wins_over_the_stoplist() {
    let v = Vocab {
        entries: vec![entry("1", "Horus", &["hours", "ora's"], None, "1")],
    };
    assert_eq!(fixed(&v, "check hours status"), "check Horus status");
    assert_eq!(fixed(&v, "check the hours"), "check the Horus");
}

#[test]
fn a_multi_word_alias_is_matched_exactly() {
    let v = Vocab {
        entries: vec![entry("1", "Skibigårdsvej", &["ski bi gor svej"], None, "1")],
    };
    assert_eq!(
        fixed(&v, "vivo in Ski Bi Gor Svej"),
        "vivo in Skibigårdsvej"
    );
}

#[test]
fn empty_vocabulary_and_empty_text_are_no_ops() {
    assert_eq!(
        Vocab::default().correct("ciao Horus"),
        Corrected {
            text: "ciao Horus".into(),
            ..Corrected::default()
        }
    );
    assert_eq!(owner().correct("").text, "");
    assert!(!owner().prompt(600).is_empty());
    assert_eq!(Vocab::default().prompt(600), "");
}

#[test]
fn unicode_text_keeps_byte_offsets_right() {
    let v = owner();
    let r = v.correct("Però è già così: «skibigordsvej», ✓ e orus!");
    assert_eq!(r.text, "Però è già così: «Skibigårdsvej», ✓ e Horus!");
    assert_eq!(r.corrections.len(), 2);
}

#[test]
fn prompt_is_newest_first_and_bounded() {
    let v = owner();
    assert_eq!(v.prompt(600), "Horus, Skibigårdsvej");
    assert_eq!(v.prompt(5), "Horus");
    assert_eq!(v.prompt(4), "");
    assert_eq!(v.prompt(19), "Horus");
    // A long entry that does not fit is skipped; a shorter, older one still fits.
    let mut v = owner();
    v.entries
        .push(entry("3", "Ab", &[], None, "2026-11-01T09:00:00Z"));
    assert_eq!(v.prompt(10), "Horus, Ab");
    assert!(v.prompt(600).chars().count() <= 600);
}

#[test]
fn meanings_come_from_words_in_the_text() {
    let v = owner();
    let m = v.meanings_for("Sto tornando a skibigardsvej, poi guardo Horus.");
    assert_eq!(
        m,
        vec![
            (
                "Skibigårdsvej".to_string(),
                "the owner's home street".to_string()
            ),
            ("Horus".to_string(), "the owner's project".to_string()),
        ]
    );
    assert!(v
        .meanings_for("Nessuna parola qui, solo Horuses e horusx.")
        .is_empty());
    let mut v2 = owner();
    v2.entries[1].meaning = None;
    assert_eq!(v2.meanings_for("Horus").len(), 0);
}

#[test]
fn validation_rules() {
    let ok = entry("1", "Horus", &["orus"], Some("m"), "t");
    assert!(validate_entry(&ok).is_ok());
    let bad = |f: &dyn Fn(&mut Entry)| {
        let mut e = ok.clone();
        f(&mut e);
        validate_entry(&e).is_err()
    };
    assert!(bad(&|e| e.word = " Horus".into()));
    assert!(bad(&|e| e.word = String::new()));
    assert!(bad(&|e| e.word = "x".repeat(101)));
    assert!(!bad(&|e| e.word = "x".repeat(100)));
    assert!(bad(&|e| e.sounds_like = vec![String::new()]));
    assert!(bad(&|e| e.sounds_like = vec!["a".repeat(101)]));
    assert!(bad(&|e| e.sounds_like = vec!["a".into(); 21]));
    assert!(bad(&|e| e.meaning = Some("m".repeat(101))));
    assert!(bad(&|e| e.lang = Some("fr".into())));
    assert!(!bad(&|e| e.lang = Some("da".into())));
    assert!(bad(&|e| e.source = "web".into()));
    assert!(bad(&|e| e.word = "a\nb".into()));
}

#[test]
fn vocabulary_wide_validation_and_edits() {
    let mut v = owner();
    assert!(v.validate().is_ok());
    assert!(
        v.add(entry("3", "HORUS", &[], None, "t")).is_err(),
        "case-insensitive duplicate"
    );
    assert!(
        v.add(entry("2", "Altro", &[], None, "t")).is_err(),
        "duplicate id"
    );
    assert!(v.add(entry("3", "Altro", &[], None, "t")).is_ok());
    assert!(v.replace("3", entry("3", "Horus", &[], None, "t")).is_err());
    assert!(v
        .replace("3", entry("3", "Altro", &["a"], None, "t"))
        .is_ok());
    assert!(v.replace("9", entry("9", "Zzz", &[], None, "t")).is_err());
    assert!(v.remove("3") && !v.remove("3"));
    assert_eq!(v.get("2").unwrap().word, "Horus");
    let mut full = Vocab::default();
    for i in 0..MAX_ENTRIES {
        full.add(entry(&format!("i{i}"), &format!("w{i}"), &[], None, "t"))
            .unwrap();
    }
    assert!(full.add(entry("x", "extra", &[], None, "t")).is_err());
    full.entries.push(entry("x", "extra", &[], None, "t"));
    assert!(full.validate().is_err());
}

#[test]
fn save_and_load_round_trip_atomically_with_mode_0600() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/voice-learn.json");
    assert_eq!(
        Vocab::load(&path).unwrap(),
        Vocab::default(),
        "missing file = empty"
    );
    let v = owner();
    v.save(&path).unwrap();
    assert_eq!(Vocab::load(&path).unwrap(), v);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    // camelCase on disk, optional fields omitted.
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        raw.contains("\"soundsLike\"")
            && raw.contains("\"createdAt\"")
            && raw.contains("\"updatedAt\"")
    );
    assert!(!raw.contains("\"lang\""));
    // No temp file is left behind, and a second save replaces the first.
    let mut v2 = v.clone();
    v2.entries.pop();
    v2.save(&path).unwrap();
    assert_eq!(Vocab::load(&path).unwrap(), v2);
    let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["voice-learn.json"]);
    // An invalid vocabulary is refused and the file is untouched.
    let mut bad = v2.clone();
    bad.entries
        .push(entry("9", "SKIBIGÅRDSVEJ", &[], None, "t"));
    assert_eq!(
        bad.save(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(Vocab::load(&path).unwrap(), v2);
    // Garbage is InvalidData; an empty file is an empty vocabulary.
    std::fs::write(&path, "not json").unwrap();
    assert_eq!(
        Vocab::load(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    std::fs::write(&path, "  \n").unwrap();
    assert_eq!(Vocab::load(&path).unwrap(), Vocab::default());
}

#[test]
fn entries_parse_with_only_a_word() {
    let v: Vocab = serde_json::from_str(r#"{"entries":[{"word":"Horus"}]}"#).unwrap();
    assert_eq!(v.entries[0].source, "app");
    assert!(v.entries[0].sounds_like.is_empty());
    let e: Entry = serde_json::from_str(
        r#"{"id":"a","word":"W","soundsLike":["x"],"meaning":"m","lang":"it","createdAt":"c","updatedAt":"u","source":"correction"}"#,
    )
    .unwrap();
    assert_eq!(
        (e.sounds_like.len(), e.lang.as_deref(), e.source.as_str()),
        (1, Some("it"), "correction")
    );
}

#[test]
fn watched_rereads_on_change_and_survives_a_missing_or_broken_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v.json");
    let w = Watched::new(path.clone());
    assert!(w.get().entries.is_empty(), "missing file");
    owner().save(&path).unwrap();
    assert_eq!(w.get().entries.len(), 2);
    let a = w.get();
    assert!(Arc::ptr_eq(&a, &w.get()), "unchanged file is not re-read");
    let mut v = owner();
    v.entries.pop();
    v.save(&path).unwrap();
    assert_eq!(w.get().entries.len(), 1);
    std::fs::write(&path, "{broken").unwrap();
    assert_eq!(w.get().entries.len(), 1, "keeps the last good vocabulary");
    std::fs::remove_file(&path).unwrap();
    assert!(w.get().entries.is_empty());
}

#[test]
fn default_path_follows_the_environment() {
    // Only reads the variables; set/restore is racy across tests, so check the shape.
    let p = default_path();
    assert!(p.to_string_lossy().ends_with("voice-learn.json"));
}

#[test]
fn spans_locate_each_replacement_in_the_input() {
    let v = owner();
    let text = "A «skibi gaardsvej», e orus!";
    let (r, spans) = v.correct_with_spans(text);
    assert_eq!(r.corrections.len(), 2);
    assert_eq!(spans.len(), 2);
    assert_eq!(&text[spans[0].start..spans[0].end], "skibi gaardsvej");
    assert_eq!(spans[0].to, "Skibigårdsvej");
    assert_eq!(&text[spans[1].start..spans[1].end], "orus");
    assert_eq!(spans[1].to, "Horus");
}

#[test]
fn path_follows_the_lookup() {
    let p = path_from_lookup(|k| (k == "LUCINA_VOICE_LEARN_FILE").then(|| "/x/v.json".to_string()));
    assert_eq!(p, PathBuf::from("/x/v.json"));
    let p = path_from_lookup(|k| (k == "HOME").then(|| "/home/u".to_string()));
    assert_eq!(
        p,
        PathBuf::from("/home/u/Projects/Lucina/data/voice-learn.json")
    );
    assert_eq!(
        path_from_lookup(|_| None),
        PathBuf::from("voice-learn.json")
    );
}
