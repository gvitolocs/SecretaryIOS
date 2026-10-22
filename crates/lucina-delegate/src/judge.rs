//! Lucina's self-review before each delegated call: one JSON-mode model
//! call that returns `{goal, risk, reversible, why, decision}`. She acts on
//! her own decision; the judgement goes on the timeline card. The hard
//! limits ([`crate::policy`]) run before and after it and it cannot lift them.

use std::time::Duration;

use lucina_llm::{ChatModel, ChatRequest, Message};
use serde_json::Value;

use crate::types::{AgentId, Decision, Judgement, Kind, Request, Risk};

pub fn system_prompt(lang: &str) -> String {
    let language = match lang {
        "it" => "Italian",
        "da" => "Danish",
        _ => "English",
    };
    [
        "You are the self-review step of Lucina, Giuseppe's personal assistant. Lucina is about to hand a task to another AI agent on her own, without asking Giuseppe. Judge it honestly, as Lucina, before she acts.",
        "Return ONLY one json object: {\"goal\":\"…\",\"risk\":\"low|med|high\",\"reversible\":true,\"why\":\"…\",\"decision\":\"act|skip|revise\",\"revised_task\":\"…\"}",
        "goal: what the delegation achieves for Giuseppe, one short line.",
        "risk: low = the agent only reads and answers (questions, research, analysis); med = it writes code or files in a sandbox or a repo branch; high = it would publish, send messages, change real systems or data, or act in someone's name.",
        "reversible: true when the effect can be undone (answers, sandbox work, a branch) and false for sends, posts and other things that cannot be taken back.",
        "decision: act when the task serves Giuseppe and the agent fits it. revise when the task is fine but should be sent differently: it carries private details the agent does not need (names, addresses, health, money, contacts), it is ambiguous, or it asks for more than needed; then put the minimal, self-contained task in revised_task. skip when it is pointless, harmful, against Giuseppe's interest, or would need a purchase or a money movement (never allowed).",
        "High risk with an irreversible effect is allowed when it is clearly what Giuseppe wants; say so plainly in why.",
        &format!("why: one sentence in {language}, first person as Lucina (\"I ask Grok because …\"). Never include secrets or passwords."),
    ]
    .join("\n")
}

pub fn user_prompt(req: &Request, agent: AgentId, sandbox_note: &str) -> String {
    format!(
        "Agent: {} (good at {})\nKind: {}\nRequested by: {}\nWhere it runs: {}\nTask:\n{}",
        agent.label(),
        agent.strengths(),
        req.kind.as_str(),
        req.origin.as_str(),
        sandbox_note,
        req.task.chars().take(6000).collect::<String>()
    )
}

/// The judgement when the judge model is unavailable: read-only work and
/// sandboxed code go ahead (she says the review was unavailable).
pub fn fallback(kind: Kind, lang: &str) -> Judgement {
    let why = match lang {
        "it" => "Autovalutazione non disponibile; procedo perché il lavoro è in sola lettura o isolato in una sandbox.",
        "da" => "Selvvurdering ikke tilgængelig; jeg fortsætter, fordi opgaven kun læser eller kører i en sandkasse.",
        _ => "Self-review unavailable; I go ahead because the work is read-only or isolated in a sandbox.",
    };
    Judgement {
        goal: String::new(),
        risk: if kind.read_only() {
            Risk::Low
        } else {
            Risk::Med
        },
        reversible: true,
        why: why.into(),
        decision: Decision::Act,
        revised_task: None,
        source: "fallback".into(),
    }
}

fn parse(content: &str) -> Option<Judgement> {
    let text = content.trim();
    let (s, e) = (text.find('{')?, text.rfind('}')?);
    let v: Value = serde_json::from_str(text.get(s..=e)?).ok()?;
    let risk = match v["risk"].as_str()?.trim().to_lowercase().as_str() {
        "low" => Risk::Low,
        "med" | "medium" => Risk::Med,
        "high" => Risk::High,
        _ => return None,
    };
    let decision = match v["decision"].as_str()?.trim().to_lowercase().as_str() {
        "act" => Decision::Act,
        "skip" => Decision::Skip,
        "revise" => Decision::Revise,
        _ => return None,
    };
    let reversible = match &v["reversible"] {
        Value::Bool(b) => *b,
        Value::String(s) => s.eq_ignore_ascii_case("true"),
        _ => return None,
    };
    let revised_task = v["revised_task"]
        .as_str()
        .or_else(|| v["revisedTask"].as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    // A revise without a task is an act on the original.
    let decision = if decision == Decision::Revise && revised_task.is_none() {
        Decision::Act
    } else {
        decision
    };
    let clip = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
    Some(Judgement {
        goal: clip(v["goal"].as_str().unwrap_or(""), 200),
        risk,
        reversible,
        why: clip(v["why"].as_str().unwrap_or(""), 400),
        decision,
        revised_task: if decision == Decision::Revise {
            revised_task
        } else {
            None
        },
        source: "model".into(),
    })
}

/// The self-review (one retry on invalid JSON; the fallback otherwise).
pub async fn judge(
    model: &dyn ChatModel,
    req: &Request,
    agent: AgentId,
    lang: &str,
    sandbox_note: &str,
    timeout: Duration,
) -> Judgement {
    let mut messages = vec![
        Message::system(system_prompt(lang)),
        Message::user(user_prompt(req, agent, sandbox_note)),
    ];
    for attempt in 0..2 {
        let request = ChatRequest {
            messages: messages.clone(),
            temperature: Some(0.0),
            purpose: "delegate-judge",
            json_object: true,
            max_tokens: Some(400),
            ..ChatRequest::default()
        };
        let content = match tokio::time::timeout(timeout, model.complete(request)).await {
            Ok(Ok(r)) => r.message.text().to_owned(),
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "delegate judge failed");
                break;
            }
            Err(_) => {
                tracing::warn!("delegate judge timed out");
                break;
            }
        };
        if let Some(j) = parse(&content) {
            return j;
        }
        tracing::warn!(attempt, "delegate judge: invalid json");
        messages.push(Message::assistant(
            content.chars().take(2000).collect::<String>(),
        ));
        messages.push(Message::user(
            "That was not the json object asked for. Return only the json object.",
        ));
    }
    fallback(req.kind, lang)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_llm::ScriptedModel;

    #[tokio::test]
    async fn parses_retries_and_falls_back() {
        let m = ScriptedModel::new();
        m.push_text("sure!");
        m.push_text(r#"{"goal":"news","risk":"low","reversible":true,"why":"Chiedo a Grok perché ha X.","decision":"act"}"#);
        let req = Request::owner(Kind::Research, "ultime notizie su Rust");
        let j = judge(
            &m,
            &req,
            AgentId::Grok,
            "it",
            "read-only",
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(j.decision, Decision::Act);
        assert_eq!(j.risk, Risk::Low);
        assert_eq!(j.source, "model");
        {
            let reqs = m.requests.lock().unwrap();
            assert!(reqs.iter().all(|r| r.json_object));
            assert!(reqs[0].messages[0].text().contains("json object"));
        }

        let m = ScriptedModel::new();
        m.push_text(r#"{"goal":"g","risk":"medium","reversible":"false","why":"w","decision":"revise","revised_task":"  only the question  "}"#);
        let j = judge(&m, &req, AgentId::Grok, "en", "", Duration::from_secs(5)).await;
        assert_eq!(
            (j.decision, j.risk, j.reversible),
            (Decision::Revise, Risk::Med, false)
        );
        assert_eq!(j.revised_task.as_deref(), Some("only the question"));

        let m = ScriptedModel::new();
        m.push(Err(lucina_llm::LlmError::Timeout));
        let j = judge(
            &m,
            &Request::owner(Kind::Code, "x"),
            AgentId::Claude,
            "en",
            "",
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(
            (j.decision, j.risk, j.source.as_str()),
            (Decision::Act, Risk::Med, "fallback")
        );
        assert!(j.why.contains("unavailable"));
    }
}
