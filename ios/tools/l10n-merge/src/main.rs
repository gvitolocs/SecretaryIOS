//! Merges translations into an Xcode String Catalog (.xcstrings).
//!
//!   cargo run --manifest-path ios/tools/l10n-merge/Cargo.toml -- \
//!       ios/Secretary/Resources/Localizable.xcstrings ios/l10n/translations.json
//!
//! translations.json: `{ "<English key>": ["<Italian>", "<Danish>"] }`. Keys that are
//! in the catalog but not translated are listed (and fail the run with --strict), so new
//! UI strings don't ship English-only by accident. Format specifiers must match.

use serde_json::{json, Map, Value};
use std::{env, fs, process::ExitCode};

const LANGS: [&str; 2] = ["it", "da"];

fn specifiers(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 1 < bytes.len() {
            let rest = &text[i + 1..];
            for spec in ["lld", "@", "d", "f", "%"] {
                if rest.starts_with(spec) {
                    out.push(spec.to_string());
                    i += spec.len();
                    break;
                }
            }
        }
        i += 1;
    }
    out
}

/// Keys that need no translation (symbols, brand names, placeholders).
fn untranslatable(key: &str) -> bool {
    key.chars().all(|c| !c.is_alphabetic())
        || ["Secretary", "Lucina", "Lucina Gym", "Dansk", "English", "Italiano", "cm", "sec_…", "https://… or http://localhost:8791", "🧺"].contains(&key)
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let strict = args.iter().any(|a| a == "--strict");
    let paths: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if paths.len() != 2 {
        eprintln!("usage: l10n-merge <Localizable.xcstrings> <translations.json> [--strict]");
        return ExitCode::from(2);
    }
    let mut catalog: Value = serde_json::from_str(&fs::read_to_string(paths[0]).expect("read catalog")).expect("catalog JSON");
    let translations: Map<String, Value> = serde_json::from_str(&fs::read_to_string(paths[1]).expect("read translations")).expect("translations JSON");

    let strings = catalog["strings"].as_object_mut().expect("strings");
    let mut missing = Vec::new();
    let mut bad = Vec::new();
    let mut merged = 0;
    for (key, entry) in strings.iter_mut() {
        if key.is_empty() || untranslatable(key) {
            // Mark as not needing translation so Xcode doesn't flag it.
            entry["shouldTranslate"] = json!(false);
            continue;
        }
        let Some(values) = translations.get(key).and_then(Value::as_array) else {
            missing.push(key.clone());
            continue;
        };
        let localizations = entry
            .as_object_mut()
            .expect("entry")
            .entry("localizations")
            .or_insert_with(|| json!({}));
        for (index, lang) in LANGS.iter().enumerate() {
            let Some(text) = values.get(index).and_then(Value::as_str) else { continue };
            if specifiers(text) != specifiers(key) {
                bad.push(format!("{lang}: {key:?} -> {text:?}"));
                continue;
            }
            localizations[*lang] = json!({ "stringUnit": { "state": "translated", "value": text } });
            merged += 1;
        }
    }
    let output = serde_json::to_string_pretty(&catalog).expect("serialize") + "\n";
    fs::write(paths[0], output.replace("\": ", "\" : ")).expect("write catalog");
    println!("merged {merged} translations; {} keys untranslated", missing.len());
    for key in &missing {
        println!("  missing: {key:?}");
    }
    for line in &bad {
        eprintln!("  format mismatch: {line}");
    }
    if !bad.is_empty() || (strict && !missing.is_empty()) {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
