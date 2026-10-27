//! Meetings recorded by the owner's Mac (Secretary Code's ScreenCaptureKit recorder):
//! `mic.m4a` (the owner), `system.m4a` (everyone else), optional `video.mp4`. Upload contract
//! `MEETINGS_API.md` in Secretary Code, API in [`api`], docs `docs/meetings.md`.
pub mod analyze;
pub mod api;
pub mod brief;
pub mod kernel;
pub mod model;
pub mod publish;
pub mod store;
pub mod timeline;
pub mod worker;

/// Mail providers that say nothing about who someone works for.
const PERSONAL_DOMAINS: &[&str] = &[
    "gmail", "googlemail", "outlook", "hotmail", "live", "icloud", "me", "mac", "yahoo", "proton", "protonmail",
    "pm", "gmx", "libero", "aol", "mail",
];

/// The company most attendees write from ("anna@acme.com" → "Acme"); None for personal
/// addresses or names without an address.
pub fn company(attendees: &[String]) -> Option<String> {
    let own: Vec<String> = std::env::var("LUCINA_MEETINGS_OWN_DOMAINS")
        .unwrap_or_default()
        .split(',')
        .map(|d| d.trim().to_ascii_lowercase())
        .filter(|d| !d.is_empty())
        .collect();
    let mut counts: Vec<(String, usize)> = Vec::new();
    for a in attendees {
        let Some((_, domain)) = a.trim().trim_end_matches('>').rsplit_once('@') else {
            continue;
        };
        let domain = domain.to_ascii_lowercase();
        if own.iter().any(|o| domain == *o || domain.ends_with(&format!(".{o}"))) {
            continue;
        }
        let labels: Vec<&str> = domain.split('.').filter(|l| !l.is_empty()).collect();
        if labels.len() < 2 {
            continue;
        }
        // The registrable name: "mail.acme.co.uk" → "acme".
        let name = if labels.len() >= 3 && labels[labels.len() - 2].len() <= 3 {
            labels[labels.len() - 3]
        } else {
            labels[labels.len() - 2]
        };
        if PERSONAL_DOMAINS.contains(&name) {
            continue;
        }
        match counts.iter_mut().find(|c| c.0 == name) {
            Some(c) => c.1 += 1,
            None => counts.push((name.to_owned(), 1)),
        }
    }
    let best = counts.iter().max_by_key(|c| c.1)?.0.clone();
    let mut c = best.chars();
    Some(c.next()?.to_uppercase().chain(c).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn companies() {
        let a = |v: &[&str]| super::company(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(a(&["Anna <anna@acme.com>", "bo@acme.com", "x@gmail.com"]), Some("Acme".into()));
        assert_eq!(a(&["p@mail.bigco.co.uk"]), Some("Bigco".into()));
        assert_eq!(a(&["me@gmail.com", "Prof. Hansen"]), None);
        assert_eq!(a(&[]), None);
    }
}
