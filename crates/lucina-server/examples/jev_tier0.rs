//! Where Jev would override a broad tier-0 rule (needs JEV_API_KEY).
use lucina_server::jev_route::{checks_tier0, classify_intent, overrides};
use lucina_server::planner::TurnInput;
const MSGS: &[&str] = &[
    "quanto devono bollire i fusilli più o meno?",
    "che ore sono a Tokyo adesso?",
    "e per la seconda parte?",
    "fix: il promemoria di domani è sbagliato, spostalo alle 10",
    "puoi controllare il sito di Netto e dirmi se ha senso l'abbonamento?",
    "cerca su internet le recensioni del Pixel 10",
    "mi spieghi la differenza tra TCP e UDP?",
    "deploy the new version of pokoin web please",
    "come va?",
    "ci sei?",
    "quanto manca al mio esame?",
    "dimmi cosa pensi di https://example.com",
];
#[tokio::main]
async fn main() {
    let s = lucina_intent::heuristics::Settings::default();
    let jev = lucina_jev::shared();
    for m in MSGS {
        let t0 = lucina_intent::tier0::classify_tier0(m, "", &s);
        let Some(c) = t0 else { println!("tier0=none                | {m}"); continue };
        if !checks_tier0(&c) { println!("tier0={:<24} (strict) | {m}", c.intent); continue; }
        let t = TurnInput { user_text: m, memory: "", user_label: "Giuseppe", platform: "telegram", device: "" };
        let (v, conf) = classify_intent(jev, &t).await.unwrap();
        let ji = v["intent"].as_str().unwrap();
        let mark = if overrides(&c.intent, ji, conf) { "OVERRIDE" } else if ji != c.intent { "differ " } else { "agree  " };
        println!("tier0={:<24} {:.2} jev={:<24} {conf:.2} {mark} | {m}", c.intent, c.confidence, ji);
    }
}
