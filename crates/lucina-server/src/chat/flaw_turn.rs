//! "Gli emoji sono quadratini", "flaw: line breaks are lost on Juno": the owner reports a small
//! flaw. His words go in the tracker as they are ([`crate::flaws`]), no model call, and he hears
//! one line back. A turn Jev routed to `flaw_report`, or an explicit "flaw: …"
//! ([`lucina_intent::flaw`]) gets here. The owner's own turns only: a guest, a guarded turn or a
//! Lucina MCP caller must not start agents through a report.

use lucina_flaws::{NewFlaw, Platform};
use lucina_store::timeline::Meta;

use super::{Finish, Turn};

pub const ACTION: &str = "flaw_report";
/// A Jev route below this goes on to the normal turn.
const ROUTE_MIN: f64 = 0.6;

pub(super) fn try_turn(t: &Turn<'_>, routed_intent: &str, confidence: f64) -> Option<Finish> {
    let flaws = t.st.flaws.clone()?;
    if t.guarded || t.mcp.is_some() || !t.surface.is_owner() || crate::selftest::is_session(&t.session) {
        return None;
    }
    let explicit = lucina_intent::flaw::is_report(&t.message);
    if !explicit && !(routed_intent == ACTION && confidence >= ROUTE_MIN) {
        return None;
    }
    let affected: Vec<Platform> = lucina_intent::flaw::platforms(&t.message)
        .into_iter()
        .filter_map(|p| Platform::parse(p).ok())
        .collect();
    let new = NewFlaw {
        words: t.message.clone(),
        affected,
        thread: Some(format!("{}:{}", t.channel, t.session)),
        ..NewFlaw::default()
    };
    let r = match flaws.report(new, "owner") {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "flaw turn: not recorded");
            return None;
        }
    };
    let id = &r.flaw.flaw.id;
    tracing::info!(flaw = %id, explicit, "flaw reported in chat");
    let lang = crate::rich::guess_language(&t.message, &t.message);
    let same = r.possible_duplicates.first().map(|f| f.id.as_str());
    let reply = match (lang, same) {
        ("it", None) => format!("Segnato ({id}). Ci penso io e ti dico com'è andata."),
        ("it", Some(s)) => format!("Segnato ({id}). Somiglia a {s}: controllo. Ti dico com'è andata."),
        ("da", None) => format!("Noteret ({id}). Jeg tager mig af det og fortæller, hvordan det gik."),
        ("da", Some(s)) => format!("Noteret ({id}). Det ligner {s}: jeg tjekker. Jeg fortæller, hvordan det gik."),
        (_, None) => format!("Noted ({id}). I'll take care of it and tell you how it went."),
        (_, Some(s)) => format!("Noted ({id}). It looks like {s}: I'll check. I'll tell you how it went."),
    };
    Some(Finish::reply(reply, ACTION).with(format!("[flaw {id}]"), Some(Meta::kind("flaw_turn"))))
}
