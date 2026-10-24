//! The memo's PDF: summary first, then the timed transcript. Written as
//! HTML and printed with headless Chrome (installed on nezopt), so every
//! script and accent prints with the system fonts.
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use chrono_tz::Tz;
use tokio::process::Command;

use crate::model::{clock, Item, Memo, Summary, Transcript};

/// Section names in the memo's language.
pub struct Labels {
    pub summary: &'static str,
    pub key_points: &'static str,
    pub decisions: &'static str,
    pub action_items: &'static str,
    pub transcript: &'static str,
    pub recorded: &'static str,
    pub duration: &'static str,
    pub language: &'static str,
    pub machine_note: &'static str,
    pub owner_me: &'static str,
    pub due: &'static str,
    pub listen: &'static str,
}

pub fn labels(lang: &str) -> Labels {
    match lang {
        "it" => Labels {
            summary: "Resoconto",
            key_points: "Punti chiave",
            decisions: "Decisioni",
            action_items: "Cose da fare",
            transcript: "Trascrizione",
            recorded: "Registrato",
            duration: "Durata",
            language: "Lingua",
            machine_note: "Trascrizione automatica (Whisper, in locale): nomi e termini tecnici possono essere sbagliati.",
            owner_me: "tu",
            due: "entro",
            listen: "Ascolta nell'app",
        },
        "da" => Labels {
            summary: "Resumé",
            key_points: "Hovedpunkter",
            decisions: "Beslutninger",
            action_items: "Opgaver",
            transcript: "Transskription",
            recorded: "Optaget",
            duration: "Varighed",
            language: "Sprog",
            machine_note: "Automatisk transskription (Whisper, lokalt): navne og fagudtryk kan være forkerte.",
            owner_me: "dig",
            due: "senest",
            listen: "Lyt i appen",
        },
        _ => Labels {
            summary: "Summary",
            key_points: "Key points",
            decisions: "Decisions",
            action_items: "Action items",
            transcript: "Transcript",
            recorded: "Recorded",
            duration: "Duration",
            language: "Language",
            machine_note: "Automatic transcript (Whisper, on-premises): names and technical terms may be wrong.",
            owner_me: "you",
            due: "due",
            listen: "Listen in the app",
        },
    }
}

pub fn language_name(code: &str) -> &str {
    match code {
        "it" => "Italiano",
        "en" => "English",
        "da" => "Dansk",
        "de" => "Deutsch",
        "es" => "Español",
        "fr" => "Français",
        "sv" => "Svenska",
        "no" | "nb" => "Norsk",
        other => other,
    }
}

pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

/// Date line in the recording's local time, e.g. `24/10/2026 14:31`.
pub fn local_when(m: &Memo, tz: Tz) -> String {
    let tz = m.timezone.as_deref().and_then(|z| z.parse::<Tz>().ok()).unwrap_or(tz);
    m.recorded_at.with_timezone(&tz).format("%d/%m/%Y %H:%M").to_string()
}

fn item_li(i: &Item, l: &Labels) -> String {
    let mut s = format!("<li>{}", esc(&i.text));
    let mut meta = Vec::new();
    if let Some(o) = &i.owner {
        meta.push(if o.eq_ignore_ascii_case("me") { l.owner_me.to_owned() } else { esc(o) });
    }
    if let Some(d) = &i.due {
        meta.push(format!("{} {}", l.due, esc(d)));
    }
    if let Some(at) = i.at {
        meta.push(clock(at));
    }
    if !meta.is_empty() {
        s.push_str(&format!(" <span class=\"meta\">· {}</span>", meta.join(" · ")));
    }
    s.push_str("</li>");
    s
}

fn section(title: &str, items: &[Item], l: &Labels, checkbox: bool) -> String {
    if items.is_empty() {
        return String::new();
    }
    let lis: String = items.iter().map(|i| item_li(i, l)).collect();
    format!(
        "<h2>{}</h2><ul class=\"{}\">{lis}</ul>",
        esc(title),
        if checkbox { "todo" } else { "points" }
    )
}

pub fn html(m: &Memo, s: &Summary, t: &Transcript, tz: Tz, app_url: &str) -> String {
    let l = labels(&s.language);
    let rows: String = t
        .segments
        .iter()
        .map(|seg| {
            let who = seg
                .speaker
                .as_ref()
                .map(|sp| format!("<b>{}</b> ", esc(sp)))
                .unwrap_or_default();
            format!(
                "<tr><td class=\"ts\">{}</td><td>{who}{}</td></tr>",
                clock(seg.start),
                esc(&seg.text)
            )
        })
        .collect();
    let tags: String = s.tags.iter().map(|t| format!("<span class=\"tag\">#{}</span>", esc(t))).collect();
    format!(
        r#"<!doctype html>
<html lang="{lang}"><head><meta charset="utf-8"><title>{title}</title>
<style>
@page {{ size: A4; margin: 18mm 17mm 20mm 17mm; }}
:root {{ --ink:#1d2330; --muted:#667085; --line:#e4e7ec; --accent:#6b4fbb; --soft:#f4f1fb; }}
* {{ box-sizing: border-box; }}
body {{ font-family: "Inter","Helvetica Neue","DejaVu Sans",Arial,sans-serif; color: var(--ink); font-size: 10.5pt; line-height: 1.5; margin: 0; }}
header {{ border-bottom: 2px solid var(--accent); padding-bottom: 8pt; margin-bottom: 14pt; }}
.kicker {{ color: var(--accent); font-size: 8.5pt; letter-spacing: .08em; text-transform: uppercase; font-weight: 700; }}
h1 {{ font-size: 20pt; line-height: 1.2; margin: 4pt 0 6pt; }}
.facts {{ color: var(--muted); font-size: 9pt; }}
.facts span + span::before {{ content: "  ·  "; }}
.overview {{ background: var(--soft); border-left: 3px solid var(--accent); padding: 9pt 12pt; border-radius: 4pt; margin: 0 0 6pt; }}
h2 {{ font-size: 12.5pt; margin: 16pt 0 5pt; color: var(--accent); break-after: avoid; }}
ul {{ margin: 0; padding-left: 16pt; }}
li {{ margin: 2pt 0; }}
ul.todo {{ list-style: none; padding-left: 2pt; }}
ul.todo li::before {{ content: "☐  "; color: var(--accent); }}
.meta {{ color: var(--muted); font-size: 9pt; }}
.tags {{ margin-top: 10pt; }}
.tag {{ display: inline-block; font-size: 8.5pt; color: var(--accent); background: var(--soft); border-radius: 8pt; padding: 1pt 7pt; margin-right: 4pt; }}
.transcript {{ break-before: page; }}
.note {{ color: var(--muted); font-size: 8.5pt; margin: 0 0 8pt; }}
table {{ border-collapse: collapse; width: 100%; font-size: 9.5pt; }}
td {{ vertical-align: top; padding: 2.5pt 0; border-bottom: 0.5pt solid var(--line); }}
td.ts {{ width: 46pt; color: var(--muted); font-variant-numeric: tabular-nums; padding-right: 8pt; white-space: nowrap; }}
tr {{ break-inside: avoid; }}
a {{ color: var(--accent); text-decoration: none; }}
footer {{ margin-top: 14pt; font-size: 8.5pt; color: var(--muted); }}
</style></head><body>
<header>
  <div class="kicker">Lucina · {summary_label}</div>
  <h1>{title}</h1>
  <div class="facts"><span>{recorded} {when}</span><span>{duration} {dur}</span><span>{language} {langname}</span></div>
</header>
<p class="overview">{overview}</p>
{points}{decisions}{actions}
<div class="tags">{tags}</div>
<footer><a href="{url}">{listen}: {url}</a></footer>
<section class="transcript">
  <h2>{transcript_label}</h2>
  <p class="note">{note} {engine}.</p>
  <table>{rows}</table>
</section>
</body></html>"#,
        lang = esc(&s.language),
        title = esc(&s.title),
        summary_label = l.summary,
        recorded = l.recorded,
        when = local_when(m, tz),
        duration = l.duration,
        dur = clock(t.duration_sec),
        language = l.language,
        langname = esc(language_name(&t.language)),
        overview = esc(&s.overview),
        points = section(l.key_points, &s.key_points, &l, false),
        decisions = section(l.decisions, &s.decisions, &l, false),
        actions = section(l.action_items, &s.action_items, &l, true),
        tags = tags,
        url = esc(app_url),
        listen = l.listen,
        transcript_label = l.transcript,
        note = l.machine_note,
        engine = esc(&t.engine),
        rows = rows,
    )
}

#[derive(Debug, thiserror::Error)]
#[error("PDF render failed: {0}")]
pub struct RenderError(pub String);

/// Prints `html_file` to `pdf` with headless Chrome.
pub async fn print_pdf(chrome: &Path, html_file: &Path, pdf: &Path, work: &Path) -> Result<(), RenderError> {
    let profile = work.join("chrome-profile");
    let tmp = pdf.with_extension("pdf.tmp");
    let _ = tokio::fs::remove_file(&tmp).await;
    let url = format!("file://{}", html_file.display());
    let out = Command::new(chrome)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--disable-background-networking",
            "--disable-sync",
            "--hide-scrollbars",
            "--no-pdf-header-footer",
            "--run-all-compositor-stages-before-draw",
            "--virtual-time-budget=3000",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--print-to-pdf={}", tmp.display()))
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| RenderError(format!("spawn {}: {e}", chrome.display())))?;
    let out = tokio::time::timeout(Duration::from_secs(120), out.wait_with_output())
        .await
        .map_err(|_| RenderError("chrome timed out".into()))?
        .map_err(|e| RenderError(e.to_string()))?;
    let _ = tokio::fs::remove_dir_all(&profile).await;
    let ok = tokio::fs::metadata(&tmp).await.map(|m| m.len() > 800).unwrap_or(false);
    if !out.status.success() || !ok {
        let err = String::from_utf8_lossy(&out.stderr);
        let err: String = err.lines().rev().take(5).collect::<Vec<_>>().join(" | ");
        return Err(RenderError(format!("exit {:?}: {err}", out.status.code())));
    }
    tokio::fs::rename(&tmp, pdf).await.map_err(|e| RenderError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn escapes_everything_from_the_model() {
        assert_eq!(esc("<b>&\"'"), "&lt;b&gt;&amp;&quot;&#39;");
        let l = labels("it");
        let li = item_li(
            &Item { text: "<script>".into(), at: Some(61.0), owner: Some("me".into()), due: Some("domani".into()) },
            &l,
        );
        assert!(li.contains("&lt;script&gt;"));
        assert!(li.contains("tu · entro domani · 1:01"));
        let _ = Utc::now();
    }
}
