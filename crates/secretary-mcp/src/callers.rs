//! `caller_screen`: Jev's verdict on phone numbers that called the owner
//! (secretary-server `/v1/callers/screen` → Lucina's `/api/secretary/callers/screen`).
//!
//! Read-only, scope `read`; an agent-scoped token may call it too
//! (`crate::tools::also_allowed`). Lucina reads the public caller-ID page
//! (180.dk) first, then asks Jev (P(spam), who is calling, P(pick up)) on facts
//! about the number only; this module checks the arguments and
//! returns her table, then the raw JSON. It never touches a call.

use reqwest::Method;
use rmcp::model::{ContentBlock, JsonObject};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::computer::caller_headers;
use crate::tools::{ToolError, Tools};

/// Footer of every answer.
pub const SOURCE: &str = "Source: Jev (TypeSafe) through Lucina, from facts about each number only (country, line type, his contacts and calls with it, public caller-ID reports from 180.dk, quoted with their URL). Show the table as is.";

/// Reply language of the labels and the table.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    En,
    It,
    Da,
}

/// Arguments of `caller_screen`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScreenArgs {
    /// 1-20 phone numbers as written, e.g. "+45 70 40 52 05" (8 digits without a prefix are Danish).
    pub numbers: Vec<String>,
    /// Language of labels and table: en, it or da (default en).
    #[serde(default)]
    pub lang: Option<Lang>,
}

fn bad(msg: impl Into<String>) -> ToolError {
    ToolError(msg.into())
}

/// The request body for secretary-server, or why the arguments are wrong.
pub fn body(args: &JsonObject) -> Result<Value, ToolError> {
    let a: ScreenArgs = serde_json::from_value(Value::Object(args.clone()))
        .map_err(|e| bad(format!("Invalid arguments: {e}")))?;
    let numbers: Vec<String> = a.numbers.iter().map(|n| n.trim().to_owned()).collect();
    if numbers.is_empty()
        || numbers.len() > 20
        || numbers
            .iter()
            .any(|n| n.is_empty() || n.chars().count() > 40)
    {
        return Err(bad(
            "`numbers` must hold 1-20 phone numbers of 1-40 characters",
        ));
    }
    let lang = match a.lang.unwrap_or(Lang::En) {
        Lang::En => "en",
        Lang::It => "it",
        Lang::Da => "da",
    };
    Ok(json!({"numbers": numbers, "lang": lang}))
}

impl Tools<'_> {
    /// `caller_screen`.
    pub async fn callers(&self, args: &JsonObject) -> Result<Vec<ContentBlock>, ToolError> {
        let v = self
            .app
            .backend
            .send_device_query(
                Method::POST,
                "/v1/callers/screen",
                &[],
                Some(&body(args)?),
                caller_headers(&self.caller.name),
            )
            .await
            .map_err(|e| match e.code.as_str() {
                "callers_unavailable" => {
                    bad("Caller screening is unreachable right now (Lucina is down); do not guess.")
                }
                _ => e.into(),
            })?;
        let table = v["table"].as_str().unwrap_or("").to_owned();
        Ok(vec![
            ContentBlock::text(format!("{table}\n\n{SOURCE}")),
            ContentBlock::text(serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(v: Value) -> Result<Value, ToolError> {
        body(v.as_object().unwrap())
    }

    #[test]
    fn arguments() {
        assert_eq!(
            b(json!({"numbers": [" +45 70 40 52 05 "], "lang": "da"})).unwrap(),
            json!({"numbers": ["+45 70 40 52 05"], "lang": "da"})
        );
        assert_eq!(b(json!({"numbers": ["1"]})).unwrap()["lang"], "en");
        for bad in [
            json!({}),
            json!({"numbers": []}),
            json!({"numbers": [" "]}),
            json!({"numbers": ["1".repeat(41)]}),
            json!({"numbers": vec!["1"; 21]}),
            json!({"numbers": ["1"], "lang": "fr"}),
            json!({"numbers": ["1"], "call": {"source": "android"}}),
        ] {
            assert!(b(bad.clone()).is_err(), "{bad}");
        }
    }
}
