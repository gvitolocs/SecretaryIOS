//! "Track this 3UW1S8IAAKA23", "dov'è il mio pacco?": the owner gives a tracking number, or asks
//! which parcels are being followed. Lucina detects the carrier herself ([`lucina_parcels`]), no
//! model call. A turn Jev routed to `parcel_track` gets here. The owner's own turns only; a
//! self-test session looks parcels up but writes nothing.

use chrono::Utc;
use lucina_parcels::{text, Added, ParcelState, Service, Stage, TrackError};
use lucina_store::timeline::Meta;

use super::{Finish, Turn};

pub const ACTION: &str = "parcel_track";
/// A Jev route below this goes on to the normal turn.
const ROUTE_MIN: f64 = 0.6;
/// A listing refreshes at most this many parcels, and only ones last checked this long ago.
const LIST_REFRESH_MAX: usize = 5;
const LIST_REFRESH_MINUTES: i64 = 30;
const SELFTEST_NOTE: &str = " (self-test: not saved)";

pub(super) async fn try_turn(t: &Turn<'_>, routed_intent: &str, confidence: f64) -> Option<Finish> {
    let svc = t.st.tool_env.parcels.clone()?;
    if t.guarded
        || t.mcp.is_some()
        || !t.surface.is_owner()
        || routed_intent != ACTION
        || confidence < ROUTE_MIN
    {
        return None;
    }
    let selftest = crate::selftest::is_session(&t.session);
    // The scraper down: the normal turn answers.
    let detection = svc.scraper.detect(&t.message).await.ok()?;
    let (reply, marker) = match detection.found() {
        Some(number) => {
            let added = svc
                .add(
                    number,
                    detection.sure_carrier(),
                    None,
                    Utc::now(),
                    !selftest,
                )
                .await;
            let mut reply = add_reply(&svc, &added);
            if selftest {
                reply.push_str(SELFTEST_NOTE);
            }
            (reply, format!("[parcel {}]", added.parcel.id))
        }
        None => (
            list_reply(&svc, !selftest).await,
            "[parcels listed]".to_owned(),
        ),
    };
    tracing::info!(selftest, "parcel turn");
    Some(Finish::reply(reply, ACTION).with(marker, Some(Meta::kind("parcel_turn"))))
}

fn add_reply(svc: &Service, added: &Added) -> String {
    let name = text::name(&added.parcel);
    match &added.lookup {
        Ok(s) if s.stage.is_final() => {
            let what = if s.stage == Stage::Returned { "is being returned to the sender" } else { "has been delivered" };
            format!("{name} {what}: {}. Nothing more to follow.", text::status_line(s, svc.cfg.tz))
        }
        Ok(s) if !added.created => format!("I'm already tracking {name}: {}.", text::status_line(s, svc.cfg.tz)),
        Ok(s) => format!(
            "Tracking {name}: {}. I'll check it every day and send one morning update, and every hour once it's close to you.",
            text::status_line(s, svc.cfg.tz)
        ),
        Err(TrackError::InputRequired(field)) => {
            format!("Saved {name}, but the carrier needs the {field} before it will show this parcel. I can't follow it until then.")
        }
        Err(TrackError::NotFound) => format!("Saved {name}; the carrier has no record of it yet, I'll keep checking."),
        Err(TrackError::Invalid) => format!("The carrier doesn't accept {} as a tracking number, so I didn't save it.", added.parcel.number),
        Err(TrackError::Unavailable(_)) => format!("Saved {name}; I couldn't reach the carrier just now, I'll keep checking."),
    }
}

async fn list_reply(svc: &Service, refresh: bool) -> String {
    let now = Utc::now();
    let active = |svc: &Service| -> Vec<_> {
        svc.store
            .list()
            .into_iter()
            .filter(|p| p.state == ParcelState::Active)
            .collect()
    };
    if refresh {
        let stale = chrono::Duration::minutes(LIST_REFRESH_MINUTES);
        for p in active(svc)
            .iter()
            .filter(|p| p.last_checked.is_none_or(|c| now - c > stale))
            .take(LIST_REFRESH_MAX)
        {
            let _ = svc.refresh(p, now).await;
        }
    }
    let parcels = active(svc);
    if parcels.is_empty() {
        return "I'm not tracking any parcel. Send me a tracking number and I'll follow it."
            .to_owned();
    }
    let lines: Vec<String> = parcels
        .iter()
        .map(|p| match (&p.status, &p.last_error) {
            (Some(s), _) => format!("• {}: {}", text::name(p), text::status_line(s, svc.cfg.tz)),
            (None, Some(e)) => format!("• {}: {e}", text::name(p)),
            (None, None) => format!("• {}: no status yet", text::name(p)),
        })
        .collect();
    format!(
        "I'm tracking {} parcel{}:\n{}",
        parcels.len(),
        if parcels.len() == 1 { "" } else { "s" },
        lines.join("\n")
    )
}
