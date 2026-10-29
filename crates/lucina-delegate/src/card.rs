//! The timeline card of a delegation (Lucina message format v1): "Ho
//! chiesto a Grok: … ✅ risposta pronta", the agent, the duration, her
//! judgement ("I did X because Y"), a link to the full note and the undo
//! command when there is one.

use lucina_store::rich::{self, RichMessage};
use serde_json::{json, Value};

use crate::types::{clip, clip_sentences, JobRecord, JobStatus, Risk};

/// Formats a duration the owner reads.
pub fn duration(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min {} s", s / 60, s % 60)
    } else {
        format!("{} h {} min", s / 3600, (s % 3600) / 60)
    }
}

fn t<'a>(lang: &str, it: &'a str, en: &'a str, da: &'a str) -> &'a str {
    match lang {
        "it" => it,
        "da" => da,
        _ => en,
    }
}

fn risk_word(lang: &str, r: Risk) -> &'static str {
    match (lang, r) {
        ("it", Risk::Low) => "rischio basso",
        ("it", Risk::Med) => "rischio medio",
        ("it", Risk::High) => "rischio alto",
        ("da", Risk::Low) => "lav risiko",
        ("da", Risk::Med) => "middel risiko",
        ("da", Risk::High) => "høj risiko",
        (_, Risk::Low) => "low risk",
        (_, Risk::Med) => "medium risk",
        (_, Risk::High) => "high risk",
    }
}

/// The card's title line.
pub fn title(job: &JobRecord) -> String {
    let l = job.lang.as_str();
    let task = clip(&job.task, 90);
    let agent = job.agent.label();
    if job.status == JobStatus::Skipped {
        match l {
            "it" => format!("Ho deciso di non chiedere a {agent}: «{task}»"),
            "da" => format!("Jeg valgte ikke at spørge {agent}: «{task}»"),
            _ => format!("I decided not to ask {agent}: “{task}”"),
        }
    } else {
        match l {
            "it" => format!("Ho chiesto a {agent}: «{task}»"),
            "da" => format!("Jeg spurgte {agent}: «{task}»"),
            _ => format!("I asked {agent}: “{task}”"),
        }
    }
}

/// The status line: ⏳ / ✅ / ⚠️ / ⏭️ with the duration and the path used.
pub fn status_line(job: &JobRecord) -> String {
    let l = job.lang.as_str();
    let d = duration(job.duration_ms);
    match job.status {
        JobStatus::Running => t(
            l,
            "⏳ ci sta lavorando",
            "⏳ working on it",
            "⏳ arbejder på det",
        )
        .to_owned(),
        JobStatus::Done => format!(
            "{} · {d} · {}",
            t(l, "✅ risposta pronta", "✅ answer ready", "✅ svar klar"),
            job.backend
        ),
        JobStatus::Error => format!(
            "{} · {d}: {}",
            t(l, "⚠️ non riuscito", "⚠️ failed", "⚠️ mislykkedes"),
            clip(job.error.as_deref().unwrap_or(""), 160)
        ),
        JobStatus::Skipped => t(l, "⏭️ non l'ho fatto", "⏭️ not done", "⏭️ ikke gjort").to_owned(),
    }
}

/// "Perché: … (rischio basso, reversibile)", plus the plain warning for a
/// high-risk irreversible act.
pub fn why_line(job: &JobRecord) -> Option<String> {
    let j = job.judgement.as_ref()?;
    let l = job.lang.as_str();
    let rev = if j.reversible {
        t(l, "reversibile", "reversible", "kan fortrydes")
    } else {
        t(l, "non reversibile", "not reversible", "kan ikke fortrydes")
    };
    let mut s = format!(
        "{} {} ({}, {rev})",
        t(l, "Perché:", "Why:", "Hvorfor:"),
        j.why.trim(),
        risk_word(l, j.risk)
    );
    if j.bold() && job.status != JobStatus::Skipped {
        s.push(' ');
        s.push_str(t(
            l,
            "⚠️ Rischio alto e non reversibile: ho deciso di procedere comunque.",
            "⚠️ High risk and not reversible: I decided to go ahead anyway.",
            "⚠️ Høj risiko og kan ikke fortrydes: jeg valgte at fortsætte alligevel.",
        ));
    }
    Some(s)
}

/// The card as plain text (timeline fallback, MCP readers) and as v1.
pub fn render(job: &JobRecord) -> (String, Option<RichMessage>) {
    let l = job.lang.as_str();
    let head = title(job);
    let status = status_line(job);
    let why = why_line(job);
    let body = match job.status {
        JobStatus::Done => job
            .summary
            .clone()
            .or_else(|| job.result.clone())
            .map(|b| clip_sentences(&b, 600)),
        _ => None,
    };
    let undo = job.undo.as_ref().map(|u| {
        format!(
            "{} `{u}` {} «{} {}»",
            t(l, "Annulla:", "Undo:", "Fortryd:"),
            t(l, "oppure scrivimi", "or tell me", "eller skriv"),
            t(l, "annulla delega", "undo delegation", "fortryd delegering"),
            job.short_id()
        )
    });
    let note = job.note_slug.as_ref().map(|s| {
        format!(
            "{} /notes/{s}",
            t(
                l,
                "📝 Risposta completa nelle Note:",
                "📝 Full answer in Notes:",
                "📝 Hele svaret i Noter:"
            )
        )
    });
    let plain = [
        Some(format!("{head} — {status}")),
        body.clone(),
        why.clone(),
        note.clone(),
        undo.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n");

    let (emoji, tone) = match job.status {
        JobStatus::Running => ("⏳", "info"),
        JobStatus::Done => ("✅", "success"),
        JobStatus::Error => ("⚠️", "warning"),
        JobStatus::Skipped => ("⏭️", "info"),
    };
    let mut blocks = vec![json!({
        "type": "card", "emoji": emoji, "title": head, "subtitle": status,
        "body": body.clone().unwrap_or_default(), "tone": tone,
    })];
    if let Some(w) = &why {
        let bold = job.judgement.as_ref().is_some_and(|j| j.bold());
        blocks.push(json!({
            "type": "card", "emoji": "🧭", "title": t(l, "La mia valutazione", "My judgement", "Min vurdering"),
            "body": w, "tone": if bold { "warning" } else { "info" },
        }));
    }
    if let Some(u) = &undo {
        blocks.push(json!({"type": "card", "emoji": "↩️", "title": t(l, "Come annullare", "How to undo", "Sådan fortryder du"), "body": u, "tone": "info"}));
    }
    let mut actions: Vec<Value> = Vec::new();
    if let Some(s) = &job.note_slug {
        actions.push(json!({"id": "delegation-note", "label": t(l, "Apri la nota", "Open the note", "Åbn noten"), "emoji": "📝", "kind": "route", "payload": {"path": format!("/notes/{s}")}}));
    }
    if job.undo.is_some() {
        actions.push(json!({"id": "delegation-undo", "label": t(l, "Annulla", "Undo", "Fortryd"), "emoji": "↩️", "kind": "reply",
            "payload": {"text": format!("{} {}", t(l, "annulla delega", "undo delegation", "fortryd delegering"), job.short_id()), "prefill": true}}));
    }
    let bubble = match job.status {
        JobStatus::Done => format!(
            "{head} {}",
            t(l, "✅ risposta pronta", "✅ answer ready", "✅ svar klar")
        ),
        _ => format!("{head} — {status}"),
    };
    let msg = json!({
        "format": rich::FORMAT,
        "lang": l,
        "bubbles": [clip(&bubble, 270)],
        "blocks": blocks,
        "actions": actions,
    });
    let rich = rich::normalize(&msg).ok().map(|(m, _)| m);
    (plain, rich)
}

/// The one-time message when a cap pauses delegation.
pub fn pause_text(lang: &str, why: &str) -> String {
    match lang {
        "it" => format!("⏸️ Ho messo in pausa le deleghe per oggi: {why}. Riprendo domani da sola; se ti serve prima, alza il limite in delegate.toml."),
        "da" => format!("⏸️ Jeg har sat delegering på pause i dag: {why}. Jeg fortsætter i morgen."),
        _ => format!("⏸️ I paused delegation for today: {why}. I resume on my own tomorrow; raise the cap in delegate.toml if you need it sooner."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentId, Decision, Judgement, Kind, Origin};

    fn job() -> JobRecord {
        JobRecord {
            id: "1a2b3c4d-0000".into(),
            agent: AgentId::Grok,
            backend: "xai-api".into(),
            kind: Kind::Research,
            origin: Origin::Owner,
            status: JobStatus::Done,
            task: "ultime notizie su Rust".into(),
            lang: "it".into(),
            session_id: "s".into(),
            channel: "web".into(),
            judgement: Some(Judgement {
                goal: "news".into(),
                risk: Risk::Low,
                reversible: true,
                why: "Chiedo a Grok perché legge il web e X in tempo reale.".into(),
                decision: Decision::Act,
                revised_task: None,
                source: "model".into(),
            }),
            created_at: "x".into(),
            finished_at: None,
            duration_ms: 42_000,
            result: Some("Rust 1.95 è uscito.".into()),
            summary: None,
            note_slug: Some("grok-ultime-notizie".into()),
            error: None,
            session: None,
            turns: 1,
            sandbox: None,
            branch: None,
            repo: None,
            undo: None,
        }
    }

    #[test]
    fn done_card() {
        let (plain, rich) = render(&job());
        assert!(
            plain.starts_with(
                "Ho chiesto a Grok: «ultime notizie su Rust» — ✅ risposta pronta · 42 s · xai-api"
            ),
            "{plain}"
        );
        assert!(plain.contains("Perché: Chiedo a Grok"));
        assert!(plain.contains("(rischio basso, reversibile)"));
        assert!(plain.contains("/notes/grok-ultime-notizie"));
        let m = rich.unwrap();
        assert_eq!(
            m.bubbles,
            vec!["Ho chiesto a Grok: «ultime notizie su Rust» ✅ risposta pronta"]
        );
        assert_eq!(m.blocks.len(), 2);
        assert_eq!(m.blocks[0]["tone"], "success");
        assert_eq!(
            m.actions[0]["payload"]["path"],
            "/notes/grok-ultime-notizie"
        );
    }

    #[test]
    fn bold_and_undo() {
        let mut j = job();
        j.lang = "en".into();
        j.agent = AgentId::Claude;
        j.status = JobStatus::Done;
        j.undo = Some("git -C /r branch -D lucina/1a2b3c4d".into());
        if let Some(g) = j.judgement.as_mut() {
            g.risk = Risk::High;
            g.reversible = false;
        }
        let (plain, rich) = render(&j);
        assert!(plain.contains("High risk and not reversible: I decided to go ahead anyway."));
        assert!(plain.contains("undo delegation 1a2b3c4d"));
        let m = rich.unwrap();
        assert_eq!(m.blocks[1]["tone"], "warning");
        assert!(m
            .actions
            .iter()
            .any(|a| a["payload"]["text"] == "undo delegation 1a2b3c4d"));
        j.status = JobStatus::Skipped;
        assert!(title(&j).starts_with("I decided not to ask Claude"));
        assert_eq!(duration(185_000), "3 min 5 s");
    }
}
