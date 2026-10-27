//! App jobs (`src/lucina-async-actions.js`, `startJob` in
//! `src/secretary-api.js`): long planner actions run in the background and
//! report on the timeline and `GET /jobs/:id`.
//!
//! - `cursor_task` and `spreadsheet_update_task` run on the DeepSeek
//!   Harness through dsh-remote (`src/dsh-remote-client.js`), as on
//!   lucina-nez. `LUCINA_JOBS=false` (shadow mode) makes Harness "not
//!   ready", so these answer with Node's blocker text and start nothing.
//! - `generate_image`: ComfyUI is not configured on this host (Node blocker).
//! - `oracle_monitor_task`: disabled on nezopt (Node's fixed reply).
//! - `graph_task` and `social_post_task` stay with the Node Lucina (rclone,
//!   Poko tokens); the job fails with a clear reason (port plan Q3/Q4).

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

pub const ASYNC_ACTIONS: [&str; 6] = [
    "cursor_task",
    "oracle_monitor_task",
    "spreadsheet_update_task",
    "graph_task",
    "generate_image",
    "social_post_task",
];

/// `asyncActionLabel`.
pub fn label(action: &str) -> &'static str {
    match action {
        "cursor_task" => "Harness task",
        "oracle_monitor_task" => "Oracle monitor task",
        "spreadsheet_update_task" => "spreadsheet update",
        "graph_task" => "graph task",
        "generate_image" => "image generation",
        "social_post_task" => "social post",
        _ => "Hermes action",
    }
}

#[derive(Debug, Clone)]
pub struct DshConfig {
    pub enabled: bool,
    pub url: String,
    pub token: Option<String>,
    pub token_file: PathBuf,
    pub owner_id: String,
    pub workspace: String,
    pub timeout: Duration,
    pub wait_seconds: u64,
    pub poll: Duration,
    pub rpc_timeout: Duration,
    /// `LUCINA_RUNTIME_HOST` label for blocker texts.
    pub host_label: String,
}

impl DshConfig {
    pub fn from_lookup(
        get: &impl Fn(&str) -> Option<String>,
        jobs_enabled: bool,
        host_label: &str,
    ) -> Self {
        let num = |k: &str, d: u64| get(k).and_then(|v| v.trim().parse().ok()).unwrap_or(d);
        Self {
            enabled: jobs_enabled && get("DSH_REMOTE_ENABLED").as_deref() != Some("false"),
            url: get("DSH_REMOTE_URL")
                .unwrap_or_else(|| "http://127.0.0.1:8765".into())
                .trim_end_matches('/')
                .to_owned(),
            token: get("DSH_REMOTE_TOKEN"),
            token_file: get("DSH_REMOTE_TOKEN_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/home/nez/.dsh-remote/token")),
            // Node's harness passes `config.dshRemote.ownerId || "lucina"`.
            owner_id: get("DSH_REMOTE_OWNER_ID").unwrap_or_else(|| "flareon".into()),
            workspace: get("DSH_REMOTE_WORKSPACE")
                .or_else(|| get("HERMES_HARNESS_WORKSPACE"))
                .unwrap_or_else(|| "/home/nez/Projects/Lucina/SecretaryIOS".into()),
            timeout: Duration::from_millis(num("DSH_REMOTE_TIMEOUT_MS", 900_000)),
            wait_seconds: num("DSH_REMOTE_WAIT_SECONDS", 0),
            poll: Duration::from_millis(num("DSH_REMOTE_POLL_MS", 5000).max(500)),
            rpc_timeout: Duration::from_millis(num("DSH_REMOTE_RPC_TIMEOUT_MS", 30_000)),
            host_label: host_label.to_owned(),
        }
    }

    fn read_token(&self) -> Option<String> {
        self.token
            .clone()
            .or_else(|| std::fs::read_to_string(&self.token_file).ok())
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
    }

    /// `isHarnessReady` (dsh-remote enabled and a token is readable).
    pub fn ready(&self) -> bool {
        self.enabled && self.read_token().is_some()
    }

    fn host_phrase(&self) -> String {
        match self.host_label.as_str() {
            "nezopt" => "su nezopt (server AI)".into(),
            "peer1" => "su peer1 (Oracle, Poko only)".into(),
            "pi-home" => "su pi-home (storage services only)".into(),
            "nespc" => "su nespc (retired AI host)".into(),
            h => format!("su {h}"),
        }
    }

    /// `projectWorkSkillName`.
    pub fn skill_name(&self) -> &'static str {
        if self.ready() {
            "Harness project work"
        } else {
            "Cursor project work"
        }
    }
}

/// `asyncActionBlocker`: why the action cannot start, or `None`.
pub fn blocker(plan: &Value, dsh: &DshConfig) -> Option<String> {
    match plan["action"].as_str().unwrap_or("") {
        "cursor_task" | "spreadsheet_update_task" if !dsh.ready() => Some(format!(
            "{} is not ready {}, so I cannot run that action yet.",
            dsh.skill_name(),
            dsh.host_phrase()
        )),
        "generate_image" => Some(
            "ComfyUI is not configured or running, so I cannot generate the image right now."
                .into(),
        ),
        _ => None,
    }
}

// ---------------------------------------------------------------- dsh-remote

/// Hidden characters that can disguise instructions to a shell agent
/// (`src/invisible-unicode.js`, simplified): tags and bidi controls refuse
/// the prompt; zero-width characters are stripped.
pub fn sanitize_task_prompt(prompt: &str) -> Result<String, String> {
    let smuggling =
        |c: char| matches!(c as u32, 0xE0000..=0xE007F | 0x202A..=0x202E | 0x2066..=0x2069);
    let benign = |c: char| matches!(c as u32, 0x200B..=0x200D | 0x2060 | 0xFEFF | 0x00AD);
    let bad: Vec<String> = prompt
        .chars()
        .filter(|c| smuggling(*c))
        .map(|c| format!("U+{:04X}", c as u32))
        .take(5)
        .collect();
    if !bad.is_empty() {
        return Err(format!("Refused: the task text contains hidden Unicode characters that can disguise instructions ({}). Retype the request without copy/pasted hidden characters.", bad.join(", ")));
    }
    Ok(prompt.chars().filter(|c| !benign(*c)).collect())
}

async fn rpc(
    http: &reqwest::Client,
    cfg: &DshConfig,
    name: &str,
    args: Value,
) -> Result<Value, String> {
    let token = cfg
        .read_token()
        .ok_or("DSH_REMOTE_TOKEN or DSH_REMOTE_TOKEN_FILE is required")?;
    let id = uuid::Uuid::new_v4().to_string();
    let resp = http
        .post(format!("{}/", cfg.url))
        .bearer_auth(token)
        .json(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": args}}))
        .timeout(cfg.rpc_timeout)
        .send()
        .await
        .map_err(|e| format!("dsh-remote unreachable: {}", e.without_url()))?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(format!(
            "dsh-remote HTTP {}: {}",
            status.as_u16(),
            body.to_string().chars().take(400).collect::<String>()
        ));
    }
    if !body["error"].is_null() {
        return Err(format!(
            "dsh-remote RPC error: {}",
            body["error"]["message"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| body["error"].to_string())
        ));
    }
    let result = &body["result"];
    if result.is_null() {
        return Err("dsh-remote returned empty result".into());
    }
    let text = result["content"][0]["text"].as_str();
    if result["isError"].as_bool().unwrap_or(false) {
        return Err(format!(
            "dsh-remote tool error: {}",
            text.unwrap_or("unknown")
        ));
    }
    Ok(match text {
        Some(t) => {
            serde_json::from_str(t).unwrap_or_else(|_| json!({"raw": t, "status": "completed"}))
        }
        None => result.clone(),
    })
}

/// `runDshRemoteTask`: queue the prompt and wait until it is idle.
pub async fn run_harness(
    http: &reqwest::Client,
    cfg: &DshConfig,
    prompt: &str,
) -> Result<String, String> {
    let task = sanitize_task_prompt(prompt)?.trim().to_owned();
    if task.is_empty() {
        return Err("Harness task prompt is empty".into());
    }
    let rid = format!(
        "lucina-hospital-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        &uuid::Uuid::new_v4().to_string()[..8]
    );
    let mut result = rpc(
        http,
        cfg,
        "dsh_prompt",
        json!({
            "prompt": task, "workspace": cfg.workspace, "owner_id": cfg.owner_id, "request_id": rid,
            "wait_seconds": cfg.wait_seconds, "new_session": false, "mode": "queue",
        }),
    )
    .await?;
    let started = std::time::Instant::now();
    let busy = |r: &Value| matches!(r["status"].as_str(), Some("running" | "pending"));
    while busy(&result) && started.elapsed() < cfg.timeout {
        tokio::time::sleep(cfg.poll).await;
        let req_id = result["requestId"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| rid.clone());
        result = rpc(http, cfg, "dsh_output", json!({"request_id": req_id, "session_id": result["sessionId"], "include_events": false})).await?;
    }
    if busy(&result) {
        return Err(format!(
            "dsh-remote task still {} after {}ms (request_id={rid})",
            result["status"].as_str().unwrap_or(""),
            cfg.timeout.as_millis()
        ));
    }
    let parts: Vec<&str> = ["output", "message", "summary", "text"]
        .iter()
        .filter_map(|k| result[*k].as_str())
        .filter(|s| !s.trim().is_empty())
        .collect();
    Ok(if parts.is_empty() {
        result.to_string()
    } else {
        parts.join("\n\n")
    })
}

/// `buildSpreadsheetUpdatePrompt` (verbatim).
pub fn spreadsheet_prompt(
    spreadsheet_name: &str,
    instruction: &str,
    user_text: &str,
    memory: &str,
) -> String {
    let name = spreadsheet_name.trim();
    let file = if name.is_empty() {
        String::new()
    } else if name.to_lowercase().ends_with(".xlsx") {
        name.to_owned()
    } else {
        format!("{name}.xlsx")
    };
    let target = if file.is_empty() {
        "No target spreadsheet was fixed by the planner. Choose the best Drive workbook after inspection.".to_owned()
    } else {
        format!("Planner-suggested target spreadsheet file on Drive: drive:{file}")
    };
    let section = |title: &str, v: &str| {
        if v.trim().is_empty() {
            String::new()
        } else {
            format!("\n\n{title}:\n{}", v.trim())
        }
    };
    let lines: Vec<String> = vec![
        "You are executing a Hermes production skill: update a Google Drive spreadsheet, then verify the update.".into(),
        String::new(),
        "Use the existing Google Drive access through root rclone. Do not ask the user for confirmation if the requested row/value is clear.".into(),
        "Do not only explain what should be done. Actually perform the spreadsheet update and verify it.".into(),
        String::new(),
        target,
        String::new(),
        "Required workflow:".into(),
        "1. Create a temporary working directory.".into(),
        "2. Inventory candidate spreadsheets on Drive with rclone (for example: rclone lsf drive: --max-depth 2) and filter relevant .xlsx files by the user request, memory, filenames, and visible content.".into(),
        "3. If the planner suggested a file, still verify it is semantically appropriate by inspecting sheet names and headers. If another Drive workbook is clearly better, use the better one and mention why in the result.".into(),
        "4. Download the selected spreadsheet with rclone copyto.".into(),
        "5. Before editing, create a timestamped backup copy under /opt/hermes/data/drive-backups and inspect workbook sheets and headers.".into(),
        "6. Choose the sheet/table by domain, sheet names, and headers. Create a new sheet only when the domain is clear and no suitable sheet/table exists.".into(),
        "7. Apply the requested update with the narrowest possible edit. For append tasks, append only the new row; do not rebuild the sheet from JSON/arrays.".into(),
        "8. Preserve unrelated sheets, formulas, dates, formatting, workbook XML, and existing rows. Treat any changed historical cell as a failed update.".into(),
        "9. Upload the edited file back with rclone copyto.".into(),
        "10. Mandatory verification: download the uploaded workbook again from Drive into a fresh verification path, reopen it, compare the actual workbook/sheet/row/cells against the expected modification, and confirm historical rows/cells still match the backup.".into(),
        "11. Return a concise execution report containing: Selected workbook, selected sheet, backup path, expected modification, actual verified cells, historical-row preservation result, and exactly one line starting with \"Verification: PASS\" or \"Verification: FAIL\".".into(),
        "12. If anything fails or verification is FAIL, include a \"Diagnosis\" section with: expected state, actual state, likely cause, memory/context that was wrong or useful, and the concrete next rule to use next time.".into(),
        String::new(),
        "Selection rules:".into(),
        "- Reason about the correct workbook and sheet. Do not rely blindly on a default filename.".into(),
        "- Prefer a workbook whose filename, sheets, and headers match the domain of the request.".into(),
        "- If multiple candidates are plausible and no existing memory/user instruction disambiguates them, stop and ask for clarification instead of writing to the wrong file.".into(),
        "- Known current Drive workbooks include: weight_tracking_strong_2026-05-15.xlsx for body weight logs, and patrimony.xlsx for patrimony/finance tracking.".into(),
        String::new(),
        "Update rules:".into(),
        "- If the task is about body weight from a scale photo, use the visible scale reading and the user preferences in memory. If memory says to subtract phone weight, apply that rule.".into(),
        "- For body weight logs, prefer the workbook whose filename/sheets/headers indicate weight tracking. The current known weight workbook is weight_tracking_strong_2026-05-15.xlsx.".into(),
        "- The existing weight workbook has a \"Raw Data\" sheet with columns: Date, Measurement Type, Value, Unit, Source.".into(),
        "- For a scale photo with phone correction, append one row to \"Raw Data\" with current date/time, Measurement Type \"Weight\", Value equal to the corrected final kg, Unit \"kg\", and Source \"Telegram scale photo\". Use python3 scripts/append-weight-log.py for this append when available.".into(),
        "- If \"Raw Data\" is missing in the weight workbook, create it with exactly these columns: Date, Measurement Type, Value, Unit, Source.".into(),
        "- Preserve \"Summary\" and \"Charts\" sheets. Do not write weight entries into patrimony.xlsx merely because it is a fallback; use patrimony.xlsx only when the request is about patrimony/finance or the user explicitly names it.".into(),
        "- If the user asks to log a receipt/expense, infer category/date/amount only from visible evidence and user instructions.".into(),
        "- Receipt/expense logging requires more reasoning than weight logging. Extract at least merchant/vendor, transaction date, total amount, and currency from the receipt image or user text. If any required field is unclear, stop and ask/return Verification: FAIL; do not guess.".into(),
        "- For receipt/expense appends, first find a workbook and sheet with ledger headers such as Date, Merchant/Vendor, Total/Amount, Currency, Category, Payment Method, Tax, Source, and Notes. Use python3 scripts/append-receipt-log.py for the final append when such a ledger exists.".into(),
        "- Do not write receipts into patrimony.xlsx sheets named \"Daglige bidrag\", \"Gæld\", \"Aktivallokering\", or \"Grafer\" unless the user explicitly says that exact sheet is the receipt ledger and its headers match the receipt fields. Those sheets are finance/debt/chart sheets, not receipt ledgers.".into(),
        "- If no receipt ledger exists, do not create one inside an existing finance workbook without explicit user approval. Return Verification: FAIL with a diagnosis and propose the exact ledger schema to create next time.".into(),
        "- Bank/account statement CSV uploads are expense import tasks, not receipt-photo tasks. If the user explicitly asks to create an expenses sheet or add statement data to the expenses sheet, create or use a dedicated expense ledger sheet after inspecting headers and sample rows.".into(),
        "- For account statement imports, parse the CSV with a real CSV parser or a careful delimiter/quote-aware parser. Preserve original transaction date, description/payee, amount, currency, and source file name when available. Do not import duplicate rows if the ledger already contains the same date/description/amount/source combination.".into(),
        "- For newly created expense ledgers from bank/account statements, prefer columns: Date, Description, Amount, Currency, Category, Account, Source, Notes. Use negative amounts for expenses when the source statement indicates debits; keep income/credits distinguishable if present.".into(),
        "- Prefer appending a new row to the matching sheet/table unless the user clearly asks to edit an existing row.".into(),
        "- Never use XLSX.utils.json_to_sheet, aoa_to_sheet, or book_new to rewrite the weight workbook or receipt ledger for a simple append. Those operations can erase existing dates/formulas. If a library rewrite would be needed, stop and return Verification: FAIL with diagnosis instead.".into(),
        "- A successful append must increase the target sheet row count by exactly one and leave every pre-existing row/cell equal to the backup. If old dates become blank, numeric serials, or otherwise changed, restore the backup and return Verification: FAIL.".into(),
        "- If columns or target sheet are ambiguous for non-weight tasks, inspect likely sheets first. If still ambiguous, stop and report the blocker instead of fabricating a write.".into(),
        "- Do not treat local save or upload as proof. Verification must read the post-upload Drive copy.".into(),
        "- If verification does not find the expected row/cells, return \"Verification: FAIL\" and explain the mismatch/blocker. Do not report success.".into(),
        "- On failure, reason from current memory and observed evidence. If memory pointed to the wrong workbook/sheet, say so and state the corrected workbook/sheet choice.".into(),
        "- Make the failure useful for learning: include a stable rule Hermes should remember for the next attempt, such as \"weight logs use workbook X sheet Y\".".into(),
        "- Report only factual changes made: file, sheet, row/cells, values, and verification result.".into(),
        "- Do not commit code or make unrelated repository changes.".into(),
        section("User request", user_text),
        section("Planner instruction", instruction),
        section("Relevant memory context", memory),
    ];
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blockers_and_sanitizer() {
        let get = |k: &str| (k == "DSH_REMOTE_TOKEN").then(|| "t".to_owned());
        let off = DshConfig::from_lookup(&get, false, "nezopt");
        assert_eq!(
            blocker(&json!({"action": "cursor_task"}), &off).unwrap(),
            "Cursor project work is not ready su nezopt (server AI), so I cannot run that action yet."
        );
        let on = DshConfig::from_lookup(&get, true, "nezopt");
        assert!(blocker(&json!({"action": "cursor_task"}), &on).is_none());
        assert!(blocker(&json!({"action": "generate_image"}), &on)
            .unwrap()
            .starts_with("ComfyUI"));
        assert_eq!(sanitize_task_prompt("a\u{200B}b").unwrap(), "ab");
        assert!(sanitize_task_prompt("a\u{202E}b").is_err());
        assert!(spreadsheet_prompt("patrimony", "x", "y", "").contains("drive:patrimony.xlsx"));
    }
}
