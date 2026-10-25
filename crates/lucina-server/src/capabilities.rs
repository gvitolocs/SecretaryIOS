//! Lucina's self-description from the runtime's flags
//! (`src/lucina-capabilities.js`): the "Runtime capabilities" block heads the
//! operator brief and answers "cosa sai fare?". Texts are Node's, verbatim;
//! the flags describe the whole Lucina system (Telegram and Discord stay in
//! the Node process, see [`Capabilities`]).

use lucina_intent::mcp;

/// The flags the block reads, with Node's env names.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Capabilities {
    /// `LUCINA_DEEPSEEK_ONLY`.
    pub deepseek_only: bool,
    /// `DEEPSEEK_MODEL` (default `deepseek-flash`).
    pub deepseek_model: String,
    /// A Lucina Telegram / Discord bot token is configured in the system
    /// (`LUCINA_SYSTEM_TELEGRAM` / `LUCINA_SYSTEM_DISCORD`, written by
    /// `env-from-node` from the Node container without the tokens).
    pub telegram: bool,
    pub discord: bool,
    pub secretary: bool,
    /// `ALEXA_LUCINA_ENABLED`.
    pub alexa: bool,
    pub reminders: bool,
    pub app_notify: bool,
    /// `LUCINA_PROACTIVE_CHECKIN_ENABLED`, `_DAY_START`, `_DAY_END`.
    pub proactive: bool,
    pub proactive_start: u32,
    pub proactive_end: u32,
    pub notes: bool,
    /// `LUCINA_TASKS`.
    pub tasks: bool,
    pub calendar: bool,
    pub grocery: bool,
    /// `LUCINA_PRESENCE_CONTEXT=true` exactly (Node's env check).
    pub presence: bool,
    pub outfit: bool,
    pub secretary_server: bool,
    /// `HERMES_HARNESS_ENABLED` or `DSH_REMOTE_ENABLED` (default true).
    pub project_work: bool,
    /// `COMFYUI_BASE_URL` set.
    pub images: bool,
    /// `LUCINA_WEB_CRAWL_ENABLED` (default true).
    pub web: bool,
    pub oracle_monitor: bool,
    /// `LUCINA_MCP_TRUSTED`: MCP callers with the owner's token act like him.
    pub mcp_trusted: bool,
    /// `LUCINA_DRIVE`: Lucina's own Google Drive (read-only).
    pub drive: bool,
}

impl Capabilities {
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Self {
        let is_true = |k: &str| get(k).is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
        let not_false = |k: &str| get(k).is_none_or(|v| v.trim() == "true");
        let num = |k: &str, d: u32| get(k).and_then(|v| v.trim().parse().ok()).unwrap_or(d);
        Self {
            deepseek_only: is_true("LUCINA_DEEPSEEK_ONLY"),
            deepseek_model: get("DEEPSEEK_MODEL")
                .map(|m| m.trim().to_owned())
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| "deepseek-flash".into()),
            telegram: is_true("LUCINA_SYSTEM_TELEGRAM"),
            discord: is_true("LUCINA_SYSTEM_DISCORD"),
            secretary: is_true("SECRETARY_API_ENABLED"),
            alexa: is_true("ALEXA_LUCINA_ENABLED"),
            reminders: get("LUCINA_REMINDERS_ENABLED").is_none_or(|v| v.trim() == "true"),
            app_notify: is_true("LUCINA_APP_NOTIFY"),
            proactive: is_true("LUCINA_PROACTIVE_CHECKIN_ENABLED"),
            proactive_start: num("LUCINA_PROACTIVE_CHECKIN_DAY_START", 8),
            proactive_end: num("LUCINA_PROACTIVE_CHECKIN_DAY_END", 22),
            notes: is_true("LUCINA_NOTES"),
            tasks: is_true("LUCINA_TASKS"),
            calendar: is_true("LUCINA_CALENDAR"),
            grocery: get("GROCERY_DATABASE_URL")
                .or_else(|| get("GROCERY_DATABASE_URL_FILE"))
                .is_some(),
            presence: is_true("LUCINA_PRESENCE_CONTEXT"),
            outfit: is_true("LUCINA_AVATAR_OUTFIT"),
            secretary_server: get("LUCINA_SECRETARY_SERVER_URL")
                .is_some_and(|u| !u.trim().is_empty()),
            project_work: is_true("HERMES_HARNESS_ENABLED") || not_false("DSH_REMOTE_ENABLED"),
            images: get("COMFYUI_BASE_URL").is_some_and(|u| !u.trim().is_empty()),
            web: not_false("LUCINA_WEB_CRAWL_ENABLED"),
            oracle_monitor: is_true("LUCINA_ORACLE_MONITOR_ENABLED"),
            mcp_trusted: is_true("LUCINA_MCP_TRUSTED"),
            drive: is_true("LUCINA_DRIVE"),
        }
    }

    fn mcp_allowed_phrase() -> String {
        let mut parts: Vec<String> = mcp::ALLOWED_ACTIONS
            .iter()
            .map(|a| match *a {
                "reply" => "answer questions".to_owned(),
                "list_reminders" => "list reminders".to_owned(),
                "schedule_reminder" => "create reminders".to_owned(),
                "notes" => "save and read notes".to_owned(),
                "tasks" => "read tasks, add them and mark them done (never delete)".to_owned(),
                "game" => {
                    "ask how Lucina's Pokémon game is going (never start or pause it)".to_owned()
                }
                other => other.replace('_', " "),
            })
            .collect();
        let g = mcp::ALLOWED_GROCERY_INTENTS;
        if g.iter()
            .any(|i| i.starts_with("grocery.shopping_list") || *i == "grocery.purchase")
        {
            parts.push("read, add to and mark bought on the shopping list".into());
        }
        if g.iter().any(|i| {
            ["offer", "price", "expense", "pantries", "analytics"]
                .iter()
                .any(|w| i.contains(w))
        }) {
            parts.push("look up offers, prices and grocery spending".into());
        }
        parts.join("; ")
    }

    /// `describeLucinaCapabilities`: `(reasoning, live, off)`.
    pub fn describe(&self) -> (String, Vec<String>, Vec<String>) {
        let mut live: Vec<String> = Vec::new();
        let mut off: Vec<String> = Vec::new();
        let mut add = |enabled: bool, yes: String, no: Option<&str>| {
            if enabled {
                live.push(yes);
            } else if let Some(n) = no {
                off.push(n.to_owned());
            }
        };
        let reasoning = if self.deepseek_only {
            format!("My reasoning (planning and every reply) runs only on the DeepSeek API (model {}). No local Qwen or Gemma and no Gemini/OpenAI chain answer my chat.", self.deepseek_model)
        } else {
            "My reasoning runs on the planner chain: hosted providers first, local fallbacks after, DeepSeek last.".to_owned()
        };
        add(
            self.telegram,
            "Telegram chat with Giuseppe (text, voice notes, photos with vision).".into(),
            None,
        );
        add(
            self.discord,
            "Discord chat (DMs and servers; server admin tools only on Discord).".into(),
            None,
        );
        add(self.secretary, "Secretary app chat (web PWA and iOS), one timeline shared with Telegram, Discord and Alexa.".into(), Some("Secretary app chat is off on this runtime."));
        add(self.alexa, "Alexa as a voice input surface.".into(), None);
        if self.mcp_trusted {
            add(self.secretary, "Lucina MCP: ChatGPT, Grok, Cursor and Claude Code ask me on Giuseppe's behalf with his token, so I act on their requests as on his own (downloads, project work, delegation), with my own judgement; secrets and money stay out.".into(), None);
        } else {
            add(self.secretary, format!("Lucina MCP: ChatGPT, Grok and Claude Code can ask me on Giuseppe's behalf. MCP guard: they may only {}; anything else waits for Giuseppe's own yes in the app or on Telegram.", Self::mcp_allowed_phrase()), None);
        }
        add(self.drive, "My own Google Drive (mylucina20@gmail.com): list, search, read and write my files, make folders, rename, move and trash (the `drive` tool); the operator moves files in and out with `lucina drive get` / `lucina drive put`.".into(), None);
        add(
            self.reminders,
            format!(
                "Timed reminders (persisted), delivered on Telegram{}.",
                if self.app_notify {
                    " and as Secretary app notifications"
                } else {
                    ""
                }
            ),
            Some("Timed reminders are off on this runtime."),
        );
        add(self.app_notify, "Secretary app notifications are live: web push to the installed PWA, the iOS app and the in-app inbox. Reminders and my proactive messages land there too.".into(), Some("No Secretary app push on this runtime (LUCINA_APP_NOTIFY is off)."));
        add(
            self.proactive,
            format!("Proactive check-ins: I may write first between {}:00 and {}:00, usually at most once a day.", self.proactive_start, self.proactive_end),
            Some("Proactive check-ins are off: I only answer."),
        );
        add(self.notes, "Notes in the Secretary app: I save, append to and read notes (\"salva come nota …\", \"cosa c'è nella nota …\").".into(), Some("Notes from chat are off (LUCINA_NOTES)."));
        add(self.tasks, "Tasks synced with Todoist: I list today's and overdue tasks, add tasks and mark them done (\"cosa devo fare oggi?\", \"aggiungi task …\", \"fatto lo shadowing\"); deleting stays with Giuseppe.".into(), Some("Tasks from chat are off (LUCINA_TASKS)."));
        add(self.calendar, "Google Calendar: I read the agenda and add an event only after Giuseppe's explicit yes.".into(), Some("Calendar from chat is off (LUCINA_CALENDAR)."));
        add(self.grocery, "Shopping lists v2 and grocery accounting: lists, items, purchases, offers, prices, pantry.".into(), None);
        add(self.presence, "I see Giuseppe's Discord presence (online, idle, playing) as context. It describes him, it is not a channel of mine.".into(), None);
        if self.secretary {
            add(
                self.outfit,
                "My 3D avatar in the app has a wardrobe; I change outfit and hair when asked in the app chat.".into(),
                Some("My 3D avatar in the app has a wardrobe Giuseppe uses; changing it from chat is off (LUCINA_AVATAR_OUTFIT)."),
            );
            if !self.outfit {
                live.push("The Secretary app shows my 3D avatar and her wardrobe (outfits and hair picked in the app).".into());
            }
        }
        let mut add = |enabled: bool, yes: &str, no: Option<&str>| {
            if enabled {
                live.push(yes.to_owned());
            } else if let Some(n) = no {
                off.push(n.to_owned());
            }
        };
        add(self.secretary && self.secretary_server, "The app's Links page shows Giuseppe's connected accounts and channels and their status.", None);
        add(
            true,
            "Long-term memory in Honcho across sessions, plus the recent turns of each chat.",
            None,
        );
        add(self.project_work, "Project work on nezopt through DeepSeek Harness: code, logs, checks (runs as a background job).", None);
        add(self.images, "Image generation with ComfyUI.", None);
        add(
            true,
            "Google Drive spreadsheet graphs and updates (background jobs).",
            None,
        );
        add(
            self.web,
            "Reading a public web page or searching for one.",
            None,
        );
        add(
            self.oracle_monitor,
            "Oracle CPU/RAM monitor.",
            Some("The Oracle CPU/RAM monitor is off on this runtime."),
        );
        off.push("No email: the Gmail integration was removed on 2026-09-20.".into());
        (reasoning, live, off)
    }

    /// `lucinaCapabilityBlock()`.
    pub fn block(&self) -> String {
        let (reasoning, live, off) = self.describe();
        let mut lines = vec![
            "Runtime capabilities (generated from this runtime's flags just now; they override memory and earlier chat turns):".to_owned(),
            format!("- Reasoning: {reasoning}"),
            "Live now:".to_owned(),
        ];
        lines.extend(live.iter().map(|l| format!("- {l}")));
        if !off.is_empty() {
            lines.push("Not available on this runtime:".into());
        }
        lines.extend(off.iter().map(|l| format!("- {l}")));
        lines.push("Facts vs hypotheses: when you describe yourself, only what is listed under \"Live now\" is a present fact. Anything else (ideas, plans, things remembered from older chats) is a hypothesis: label it (\"ipotesi\", \"da costruire\", \"not enabled here\"). Earlier replies may be stale; this block wins.".into());
        lines.join("\n")
    }
}

/// `looksLikeSelfDescriptionRequest`.
pub fn looks_like_self_description(text: &str) -> bool {
    lucina_intent::re!(r"\b(cosa (?:sai|puoi) fare|che (?:cosa )?(?:sai|puoi) fare|cosa supporti|le tue (?:capacit[aà]|funzion\w*)|che modello|quale modello|che llm|su che modello|inventario|what can you do|your (?:capabilities|features|tools)|which model|what model|what are you running|what do you support|hvad kan du)", "i").test(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_from_flags() {
        let env: std::collections::HashMap<&str, &str> = [
            ("LUCINA_DEEPSEEK_ONLY", "true"),
            ("SECRETARY_API_ENABLED", "true"),
            ("LUCINA_APP_NOTIFY", "true"),
            ("LUCINA_NOTES", "true"),
            ("LUCINA_SYSTEM_TELEGRAM", "true"),
        ]
        .into_iter()
        .collect();
        let c = Capabilities::from_lookup(&|k: &str| env.get(k).map(|v| v.to_string()));
        let b = c.block();
        assert!(b.starts_with("Runtime capabilities (generated from this runtime's flags just now"));
        assert!(b.contains("- Reasoning: My reasoning (planning and every reply) runs only on the DeepSeek API (model deepseek-flash)."));
        assert!(b.contains("- Timed reminders (persisted), delivered on Telegram and as Secretary app notifications."));
        assert!(b.contains("MCP guard: they may only answer questions; list reminders; create reminders; save and read notes; read tasks, add them and mark them done (never delete); ask how Lucina's Pokémon game is going (never start or pause it); read, add to and mark bought on the shopping list; look up offers, prices and grocery spending;"));
        assert!(b.contains("- Calendar from chat is off (LUCINA_CALENDAR)."));
        assert!(b.contains("- Tasks from chat are off (LUCINA_TASKS)."));
        assert!(looks_like_self_description("cosa sai fare?"));
        assert!(!looks_like_self_description("ciao"));
    }
}
