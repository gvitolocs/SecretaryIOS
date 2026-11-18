//! The tailnet llama-server (`raffy`) as a settings set: what `RAFFY_*` reads, what its defaults are,
//! and that the other providers' variables stay out of it. Offline: lookup closures only, no network.

use std::time::Duration;

use lucina_llm::{DeepSeekConfig, Dialect, KeySource, RAFFY_BASE_URL, RAFFY_MODEL};

#[test]
fn settings_come_from_raffy_variables_only() {
    let cfg = DeepSeekConfig::raffy_from_lookup(|k| match k {
        "RAFFY_BASE_URL" => Some("http://10.0.0.5:8080/v1/".into()),
        "RAFFY_MODEL" => Some("qwen3-4b-16k".into()),
        "RAFFY_API_KEY" => Some(" r-key ".into()),
        "DEEPSEEK_API_KEY" => Some("not-this-one".into()),
        "MISTRAL_API_KEY" => Some("not-this-one".into()),
        _ => None,
    });
    assert_eq!(cfg.base_url, "http://10.0.0.5:8080/v1");
    assert_eq!(cfg.model, "qwen3-4b-16k");
    assert_eq!(
        cfg.dialect,
        Dialect::DeepSeek,
        "a --jinja llama-server speaks the plain OpenAI shape"
    );
    assert!(matches!(&cfg.key, KeySource::Inline(k) if k == "r-key"));

    // The other two ignore RAFFY_*.
    let ds = DeepSeekConfig::from_lookup(|k| (k == "RAFFY_API_KEY").then(|| "no".to_owned()));
    assert!(matches!(ds.key, KeySource::None));
    assert_eq!(ds.base_url, "https://api.deepseek.com/v1");
    let mi = DeepSeekConfig::mistral_from_lookup(|k| (k == "RAFFY_MODEL").then(|| "x".to_owned()));
    assert_eq!(mi.model, "mistral-small-latest");
}

#[test]
fn with_no_variable_set_it_is_the_tailnet_gateway_running_qwen3_4b() {
    let cfg = DeepSeekConfig::raffy_from_lookup(|_| None);
    assert_eq!(RAFFY_BASE_URL, "http://100.74.158.126:8080/v1");
    assert_eq!(RAFFY_MODEL, "qwen3-4b");
    assert_eq!(cfg.base_url, RAFFY_BASE_URL);
    assert_eq!(cfg.model, RAFFY_MODEL);
    assert_eq!(cfg.dialect, Dialect::DeepSeek);
    assert!(matches!(cfg.key, KeySource::None), "no key ships with it");
    assert_eq!(
        cfg.timeout,
        Duration::from_millis(90_000),
        "the existing default"
    );
    assert_eq!(cfg.max_retries, 3, "the existing default");
}

#[test]
fn a_key_file_is_read_as_a_file_source() {
    let cfg = DeepSeekConfig::raffy_from_lookup(|k| match k {
        "RAFFY_KEY_FILE" => Some(" /home/nez/.config/raffy/key ".into()),
        _ => None,
    });
    assert!(
        matches!(&cfg.key, KeySource::File(p) if p.to_string_lossy() == "/home/nez/.config/raffy/key")
    );
}

#[test]
fn an_inline_key_is_never_printed() {
    let cfg = DeepSeekConfig::raffy_from_lookup(|k| {
        (k == "RAFFY_API_KEY").then(|| "raffy-secret-value".to_owned())
    });
    let shown = format!("{cfg:?}");
    assert!(!shown.contains("raffy-secret-value"), "{shown}");
    assert!(shown.contains("Inline(<redacted>)"), "{shown}");
}
