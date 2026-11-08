use lucina_intent::heuristics::{
    looks_like_actionable, looks_like_delegation_request, looks_like_reminder_request, Settings,
};
use lucina_intent::tier0::classify_tier0;

const MSG_INCIDENT: &str = "Urgent production incident, need GO/NO now. pokoin-cdn-origin does not resolve on pi-home since 16:05 and the CDN returns 502. Fix: add 127.0.0.1 pokoin-cdn-origin to /etc/hosts on pi-home, restart cloudflared-cdn, verify with curl -I https://cdn.pokoin.com/health. Recommendation: GO now.";
const MSG_UPDATE: &str = "Update: the CDN incident is FIXED. I applied it myself at 17:16 CET (added the hosts entry and restarted cloudflared-cdn, curl returns 200). Please stop or cancel the Harness task you started, it is no longer needed.";

fn settings() -> [Settings; 2] {
    [
        Settings {
            fast_chat: true,
            ..Settings::default()
        },
        Settings::default(),
    ]
}

#[test]
fn incident_and_update_do_not_trigger_side_effects() {
    for text in [MSG_INCIDENT, MSG_UPDATE] {
        assert!(!looks_like_reminder_request(text), "{text}");
        assert!(!looks_like_delegation_request(text), "{text}");
        for settings in settings() {
            if let Some(c) = classify_tier0(text, "", &settings) {
                assert!(
                    ![
                        "cursor_task",
                        "schedule_reminder",
                        "social_post",
                        "computer_task",
                        "spreadsheet_update",
                        "revive_qwen",
                        "poko_news_scout_retry",
                        "oracle_monitor",
                        "generate_image",
                    ]
                    .contains(&c.intent.as_str()),
                    "{text}: {c:?} with {settings:?}"
                );
            }
        }
    }
    assert!(!looks_like_actionable(MSG_UPDATE));
}

#[test]
fn explicit_requests_and_short_reminders_keep_their_routes() {
    let cases = [
        (
            "ricordami alle 17 di chiamare il dentista",
            "schedule_reminder",
        ),
        ("ricordami domani alle 9 di pagare", "schedule_reminder"),
        ("alle 18:30 dentista", "schedule_reminder"),
        ("at 9:00 call mum", "schedule_reminder"),
        ("Timer 20 minutes", "schedule_reminder"),
        ("remind me at 21:30 to take the pills", "schedule_reminder"),
        (
            "fai fare a cursor il fix del bug di login su pokoin",
            "cursor_task",
        ),
        (
            "usa harness per controllare i log di hermes e dimmi cosa noti",
            "lucina_log_inspect",
        ),
        (
            "ask codex to fix the failing build in lucina",
            "cursor_task",
        ),
        ("controlla lo stato del server", "cursor_task"),
        ("puoi controlla lo stato del server", "cursor_task"),
        ("potresti controlla lo stato del server", "cursor_task"),
        ("controlla i log di lucina", "lucina_log_inspect"),
        ("use qwen to fix the flight booking code", "cursor_task"),
        ("fai fare a Mario il fix", "cursor_task"),
        ("coding agent task", "cursor_task"),
    ];
    for settings in settings() {
        for (text, expected) in cases {
            let actual = classify_tier0(text, "", &settings).map(|c| c.intent);
            assert_eq!(actual.as_deref(), Some(expected), "{text}: {settings:?}");
        }
        for text in [
            "the deploy failed with error 502, fix the nginx config",
            "The incident report says controlla lo stato del server",
            "fix the flight booking bug in lucina",
            "check the deploy error and fix the nginx config",
        ] {
            let actual = classify_tier0(text, "", &settings).map(|c| c.intent);
            assert_ne!(actual.as_deref(), Some("cursor_task"), "{text}");
        }
    }
}

#[test]
fn delegation_requires_a_request_and_target() {
    for text in [
        "fai fare a cursor il fix del bug di login su pokoin",
        "usa harness per controllare i log di hermes e dimmi cosa noti",
        "ask codex to fix the failing build in lucina",
        "fai fare a Mario il fix",
        "delega a Mario il lavoro",
        "delegate to Alice the build repair",
        "cursor_task",
        "project work",
        "coding agent task",
        "fai fare il fix con cursor",
        "chiedi a codex di correggere il bug",
        "use deepseek harness to inspect the build",
        "have dsh fix the build",
        "get claude code to fix the build",
        "let a coding agent fix the build",
        "run the tests on qwen",
        "lancia zcode per il fix",
        "avvia opencode per il fix",
        "start cursor to fix the build",
        "launch codex to fix the build",
        "delega il fix a harness",
        "delegate the fix to codex",
        "manda a cursor il fix",
        "give the fix to codex",
        "correggi il bug con cursor",
        "correggi il bug tramite harness",
        "fix the build via codex",
        "fix the build with claude code",
    ] {
        assert!(looks_like_delegation_request(text), "{text}");
    }
    for text in [
        MSG_INCIDENT,
        MSG_UPDATE,
        "the deploy failed with error 502, fix the nginx config",
        "fix deploy restart commit bug error logs /etc/hosts curl",
        "Codex fixed the build and Harness restarted the server",
        "The build was fixed by the coding agent",
        "controlla lo stato del server",
        "controlla i log di lucina",
        "use curl to check the server",
        "the cursor moved with the mouse",
        "",
    ] {
        assert!(!looks_like_delegation_request(text), "{text}");
    }
}

#[test]
fn bare_times_are_notes_to_self_not_reports() {
    for word in [
        "applied",
        "fixed",
        "did",
        "done",
        "was",
        "were",
        "happened",
        "finished",
        "started",
        "restarted",
        "deployed",
        "added",
        "returned",
        "since",
        "ho",
        "abbiamo",
        "hai",
        "è stato",
        "sono stato",
        "sono stati",
        "fatto",
        "risolto",
        "sistemato",
        "applicato",
        "finito",
    ] {
        let text = format!("{word} at 17:16");
        assert!(!looks_like_reminder_request(&text), "{text}");
        assert!(!looks_like_actionable(&text), "{text}");
    }
    let eighty = format!("{} call mum at 9:00", "x".repeat(63));
    assert_eq!(eighty.encode_utf16().count(), 80);
    assert!(looks_like_reminder_request(&eighty));
    assert!(!looks_like_reminder_request(&format!("x{eighty}")));
    // Supplementary characters count as two UTF-16 code units.
    assert!(!looks_like_reminder_request(&format!("😀{}", &eighty[1..])));
    assert!(!looks_like_reminder_request("the value format 17:16"));
    assert!(looks_like_reminder_request(&format!(
        "  at 9:00 {}",
        "call mum ".repeat(20)
    )));
    for text in [
        "I fixed the build; remind me at 21:30 to take the pills",
        "I was busy; set a reminder at 21:30 to take the pills",
        "ho finito; ricordami alle 17 di chiamare",
        "ho finito; avvisami alle 17",
        "ho finito; mi ricordi di chiamare alle 17",
        "ho finito; promemoria per domani alle 17",
        "2026-08-28 — call mum",
        "28 August 2026 — call mum",
    ] {
        assert!(looks_like_reminder_request(text), "{text}");
    }
}
