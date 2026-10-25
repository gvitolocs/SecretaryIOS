//! Model-backed extraction over text a handler already read: invoice and
//! receipt fields (checked against the source text, so a hallucinated
//! amount is dropped) and summaries of long documents.

use lucina_llm::{ChatModel, ChatRequest, Message};
use serde_json::{json, Map, Value};

/// Text longer than this gets a summary.
pub const SUMMARY_FROM_CHARS: usize = 12_000;
const CHUNK_CHARS: usize = 10_000;
const MAX_CHUNKS: usize = 8;

/// Words that mark a bill or invoice (it, da, en).
const INVOICE_WORDS: &[&str] = &[
    "fattura", "totale da pagare", "importo da pagare", "scadenza", "iban", "bolletta",
    "faktura", "at betale", "beløb", "forfald", "betalingsfrist", "invoice", "amount due",
    "due date", "total due", "payment reference", "bill to",
];

/// Whether a document's text reads like an invoice or bill.
pub fn looks_like_invoice(text: &str) -> bool {
    let t = text.to_lowercase();
    INVOICE_WORDS.iter().filter(|w| t.contains(*w)).count() >= 2
}

fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// Keep only the fields whose value can be found in the source text
/// (numbers compared by their digits, so "84,30" matches "€ 84.30").
pub fn verified(fields: Value, source: &str) -> Value {
    let Value::Object(map) = fields else {
        return Value::Null;
    };
    let source_lower = source.to_lowercase();
    let source_digits = digits(source);
    let mut out = Map::new();
    for (k, v) in map {
        let keep = match &v {
            Value::String(s) if s.trim().is_empty() => false,
            Value::String(s) => {
                let d = digits(s);
                if d.len() >= 2 {
                    source_digits.contains(&d)
                        || source_lower.contains(&s.to_lowercase())
                        // Dates may be reformatted (15/11/2026 → 2026-11-15): all parts must exist.
                        || s.split(|c: char| !c.is_ascii_digit())
                            .filter(|p| !p.is_empty())
                            .all(|p| source_digits.contains(p))
                } else {
                    // Words (merchant, issuer, currency): the first word must appear.
                    s.split_whitespace()
                        .next()
                        .is_some_and(|w| source_lower.contains(&w.to_lowercase()))
                        || k == "currency"
                }
            }
            Value::Number(n) => source_digits.contains(&digits(&n.to_string())),
            Value::Array(items) => !items.is_empty(),
            Value::Null => false,
            _ => true,
        };
        if keep {
            out.insert(k, v);
        }
    }
    if out.is_empty() {
        Value::Null
    } else {
        Value::Object(out)
    }
}

async fn ask_json(model: &dyn ChatModel, system: &str, user: String) -> anyhow::Result<Value> {
    let resp = model
        .complete(ChatRequest {
            messages: vec![Message::system(system), Message::user(user)],
            temperature: Some(0.0),
            max_tokens: Some(800),
            purpose: "share_extract",
            json_object: true,
            ..ChatRequest::default()
        })
        .await?;
    let text = resp.message.text();
    let start = text.find('{').unwrap_or(0);
    let end = text.rfind('}').map(|i| i + 1).unwrap_or(text.len());
    Ok(serde_json::from_str(&text[start..end])?)
}

/// Invoice fields from a document's text, verified against it.
pub async fn invoice(model: &dyn ChatModel, text: &str) -> anyhow::Result<Value> {
    let sample: String = text.chars().take(12_000).collect();
    let v = ask_json(
        model,
        "Extract invoice or bill fields from the document text. Answer only JSON: {\"issuer\": string, \"number\": string, \"date\": string, \"due_date\": string, \"total\": string, \"currency\": string, \"iban\": string, \"payment_ref\": string}. Copy values exactly as written in the text (amounts with their decimal separator, dates as written). Use \"\" for anything not in the text.",
        sample.clone(),
    )
    .await?;
    Ok(verified(v, &sample))
}

/// Receipt fields from OCR + vision text, verified against it.
pub async fn receipt(model: &dyn ChatModel, text: &str) -> anyhow::Result<Value> {
    let sample: String = text.chars().take(8_000).collect();
    let v = ask_json(
        model,
        "Extract receipt fields from the OCR text of a shop receipt. Answer only JSON: {\"merchant\": string, \"date\": string, \"total\": string, \"currency\": string, \"items\": [{\"name\": string, \"price\": string}]}. Copy values exactly as written; \"\" or [] when missing.",
        sample.clone(),
    )
    .await?;
    let items = v.get("items").cloned();
    let mut out = verified(v, &sample);
    // Item lines are checked one by one.
    if let (Value::Object(map), Some(Value::Array(items))) = (&mut out, items) {
        let kept: Vec<Value> = items
            .into_iter()
            .filter(|i| {
                i["price"]
                    .as_str()
                    .is_some_and(|p| digits(p).len() >= 2 && digits(&sample).contains(&digits(p)))
            })
            .take(100)
            .collect();
        if kept.is_empty() {
            map.remove("items");
        } else {
            map.insert("items".into(), json!(kept));
        }
    }
    Ok(out)
}

/// A summary of a long text: chunk summaries, then one summary of those.
/// `lang` is the language to write in.
pub async fn summarize(model: &dyn ChatModel, text: &str, lang: &str) -> anyhow::Result<String> {
    let chars: Vec<char> = text.chars().collect();
    let chunks: Vec<String> = chars
        .chunks(CHUNK_CHARS)
        .take(MAX_CHUNKS)
        .map(|c| c.iter().collect())
        .collect();
    let truncated = chars.len() > CHUNK_CHARS * MAX_CHUNKS;
    let mut partial = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let resp = model
            .complete(ChatRequest {
                messages: vec![
                    Message::system(format!("Summarise this part of a document in 5-8 bullet points, keeping every number, date, name and amount exactly. Language: {lang}.")),
                    Message::user(format!("Part {} of {}:\n{chunk}", i + 1, chunks.len())),
                ],
                temperature: Some(0.2),
                max_tokens: Some(700),
                purpose: "share_summary",
                ..ChatRequest::default()
            })
            .await?;
        partial.push(resp.message.text().trim().to_owned());
    }
    if partial.len() == 1 {
        return Ok(partial.remove(0));
    }
    let resp = model
        .complete(ChatRequest {
            messages: vec![
                Message::system(format!("Merge these part summaries into one summary of the whole document: what it is, then the key points (at most 12 bullets), keeping numbers, dates and amounts exactly. Language: {lang}.")),
                Message::user(partial.join("\n\n")),
            ],
            temperature: Some(0.2),
            max_tokens: Some(900),
            purpose: "share_summary",
            ..ChatRequest::default()
        })
        .await?;
    let mut out = resp.message.text().trim().to_owned();
    if truncated {
        out.push_str(&format!(
            "\n(Summary covers the first {} characters of {}.)",
            CHUNK_CHARS * MAX_CHUNKS,
            chars.len()
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invoice_words() {
        assert!(looks_like_invoice("FATTURA n. 12 — Totale da pagare € 84,30 — Scadenza 15/11/2026"));
        assert!(looks_like_invoice("Faktura 991. At betale: 1.250,00 kr. Forfald 01.12.2026"));
        assert!(!looks_like_invoice("Meeting notes: the invoice tool is broken"));
    }

    #[test]
    fn hallucinated_fields_are_dropped() {
        let source = "ENEL Energia S.p.A. Fattura n. 0042-2026 del 20/10/2026. Totale da pagare € 84,30. Scadenza 15/11/2026. IBAN IT60X0542811101000000123456";
        let v = verified(
            json!({"issuer": "ENEL Energia", "number": "0042-2026", "date": "2026-10-20", "due_date": "15/11/2026",
                   "total": "84.30", "currency": "EUR", "iban": "IT60X0542811101000000123456", "payment_ref": "RF99 1234",
                   "extra": ""}),
            source,
        );
        assert_eq!(v["issuer"], "ENEL Energia");
        assert_eq!(v["total"], "84.30");
        assert_eq!(v["date"], "2026-10-20");
        assert_eq!(v["due_date"], "15/11/2026");
        assert_eq!(v["iban"], "IT60X0542811101000000123456");
        assert_eq!(v["currency"], "EUR");
        assert!(v.get("payment_ref").is_none(), "not in the text");
        assert!(v.get("extra").is_none());
        assert_eq!(verified(json!({"total": "99,99"}), source), Value::Null);
    }
}
