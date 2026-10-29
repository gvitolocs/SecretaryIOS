//! Owner-facing text for secrets status and audit (fixed templates, no
//! model): names, set or missing, rotation and last use. Never values.

use crate::audit::AuditEntry;
use crate::reveal::Lang;
use crate::store::SecretStatus;

fn day(ts: &Option<String>) -> Option<&str> {
    ts.as_deref().map(|t| t.get(..10).unwrap_or(t))
}

fn when(ts: &str) -> String {
    // 2026-10-22T14:00:00Z -> 2026-10-22 14:00 (UTC)
    ts.get(..16).unwrap_or(ts).replacen('T', " ", 1)
}

pub fn status_text(lang: Lang, folder: &str, items: &[SecretStatus]) -> String {
    let set = items.iter().filter(|s| s.set).count();
    let missing = items.len() - set;
    let mut lines = Vec::new();
    lines.push(match lang {
        Lang::It => format!("Segreti di Lucina in Infisical ({folder}): {set} impostati, {missing} mancanti. I valori non li vedo e non li mostro."),
        Lang::En => format!("Lucina's secrets in Infisical ({folder}): {set} set, {missing} missing. I never see or show the values."),
        Lang::Da => format!("Lucinas hemmeligheder i Infisical ({folder}): {set} sat, {missing} mangler. Værdierne ser og viser jeg aldrig."),
    });
    for s in items {
        let mut parts = vec![match (lang, s.set) {
            (Lang::It, true) => "impostato".to_owned(),
            (Lang::It, false) => "mancante".to_owned(),
            (Lang::En, true) => "set".to_owned(),
            (Lang::En, false) => "missing".to_owned(),
            (Lang::Da, true) => "sat".to_owned(),
            (Lang::Da, false) => "mangler".to_owned(),
        }];
        if let Some(d) = day(&s.last_rotated) {
            parts.push(match lang {
                Lang::It => format!("ultima modifica {d}"),
                Lang::En => format!("last changed {d}"),
                Lang::Da => format!("sidst ændret {d}"),
            });
        }
        match (&s.last_used, &s.last_used_by) {
            (Some(t), Some(by)) => parts.push(match lang {
                Lang::It => format!("ultimo uso {} ({by})", when(t)),
                Lang::En => format!("last used {} ({by})", when(t)),
                Lang::Da => format!("sidst brugt {} ({by})", when(t)),
            }),
            _ => parts.push(match lang {
                Lang::It => "mai usato".into(),
                Lang::En => "never used".into(),
                Lang::Da => "aldrig brugt".into(),
            }),
        }
        if !s.used_by.is_empty() {
            parts.push(format!("→ {}", s.used_by.join(", ")));
        }
        if s.refused.is_some() {
            parts.push(match lang {
                Lang::It => "bloccato dalla policy".into(),
                Lang::En => "blocked by policy".into(),
                Lang::Da => "blokeret af politikken".into(),
            });
        }
        lines.push(format!("- {}: {}", s.name, parts.join(", ")));
    }
    lines.join("\n")
}

pub fn audit_text(lang: Lang, entries: &[AuditEntry]) -> String {
    if entries.is_empty() {
        return match lang {
            Lang::It => "Nessun uso di segreti registrato.".into(),
            Lang::En => "No secret use recorded.".into(),
            Lang::Da => "Ingen brug af hemmeligheder registreret.".into(),
        };
    }
    let mut lines = vec![match lang {
        Lang::It => format!("Ultimi {} usi dei segreti (nessun valore):", entries.len()),
        Lang::En => format!("Last {} secret uses (no values):", entries.len()),
        Lang::Da => format!(
            "Seneste {} brug af hemmeligheder (ingen værdier):",
            entries.len()
        ),
    }];
    for e in entries {
        let outcome = match (e.ok, e.cached) {
            (true, true) => "ok (cache)".to_owned(),
            (true, false) => "ok".to_owned(),
            (false, _) => e.error.clone().unwrap_or_else(|| "error".into()),
        };
        lines.push(format!(
            "- {} {} → {} {}",
            when(&e.ts),
            e.name,
            e.tool,
            outcome
        ));
    }
    lines.join("\n")
}

/// Secrets are not set up on this host.
pub fn not_configured(lang: Lang) -> &'static str {
    match lang {
        Lang::It => "I segreti non sono configurati su questo host (manca LUCINA_SECRETS_CONFIG).",
        Lang::En => "Secrets are not configured on this host (LUCINA_SECRETS_CONFIG is not set).",
        Lang::Da => "Hemmeligheder er ikke sat op på denne vært (LUCINA_SECRETS_CONFIG mangler).",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_have_names_not_values() {
        let items = vec![
            SecretStatus {
                name: "OPEN_METEO_API_KEY".into(),
                reference: "secret://lucina/prod/lucina#OPEN_METEO_API_KEY".into(),
                set: true,
                used_by: vec!["weather.api_key".into()],
                version: Some(3),
                last_rotated: Some("2026-10-20T09:00:00Z".into()),
                last_used: Some("2026-10-22T14:00:05Z".into()),
                last_used_by: Some("weather".into()),
                refused: None,
            },
            SecretStatus {
                name: "TODOIST_TOKEN".into(),
                reference: "secret://lucina/prod/lucina#TODOIST_TOKEN".into(),
                set: false,
                used_by: vec![],
                version: None,
                last_rotated: None,
                last_used: None,
                last_used_by: None,
                refused: None,
            },
        ];
        let it = status_text(Lang::It, "lucina/prod/lucina", &items);
        assert!(it.contains("1 impostati, 1 mancanti"), "{it}");
        assert!(it.contains("OPEN_METEO_API_KEY: impostato, ultima modifica 2026-10-20, ultimo uso 2026-10-22 14:00 (weather)"), "{it}");
        let en = status_text(Lang::En, "x", &items);
        assert!(en.contains("TODOIST_TOKEN: missing, never used"), "{en}");
        assert!(status_text(Lang::Da, "x", &items).contains("mangler"));
        assert_eq!(audit_text(Lang::En, &[]), "No secret use recorded.");
    }
}
