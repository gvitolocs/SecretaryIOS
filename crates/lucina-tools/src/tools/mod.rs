//! Built-in tools.

pub mod computer;
pub mod au;
pub mod avatar;
pub mod calculator;
pub mod callers;
pub mod calendar;
pub mod delegate;
pub mod diagnostics;
pub mod drive;
pub mod media;
pub mod memos;
pub mod meshy;
pub mod notes;
pub mod notify;
pub mod flaw;
pub mod ops;
pub mod places;
pub mod plugins;
pub mod presence;
pub mod reach;
pub mod reminders;
pub mod root_exec;
pub mod secret_request;
pub mod secrets;
pub mod shopping;
pub mod tasks;
pub mod time;
pub mod units;
pub mod weather;

use std::sync::Arc;

use crate::Tool;

/// Every built-in tool, in prompt order.
pub fn builtin() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(time::TimeTool),
        Arc::new(weather::WeatherTool),
        Arc::new(calculator::CalculatorTool),
        Arc::new(units::UnitConvertTool),
        Arc::new(places::PlacesTool),
        Arc::new(au::AuPeopleSearchTool),
        Arc::new(au::AuBuildingTool),
        Arc::new(au::AuCampusMapTool),
        Arc::new(callers::CallerScreenTool),
        Arc::new(notes::NotesTool),
        Arc::new(reminders::RemindersTool),
        Arc::new(calendar::CalendarTool),
        Arc::new(tasks::TasksTool),
        Arc::new(memos::MemosTool),
        Arc::new(diagnostics::DiagnosticsTool),
        Arc::new(media::MediaTool::new()),
        Arc::new(shopping::ShoppingTool),
        Arc::new(presence::PresenceTool),
        Arc::new(avatar::AvatarOutfitTool),
        Arc::new(notify::NotifyTool),
        Arc::new(secrets::SecretsStatusTool),
        Arc::new(secrets::SecretsAuditTool),
        Arc::new(delegate::AskClaudeTool),
        Arc::new(delegate::AskGrokTool),
        Arc::new(delegate::AskChatgptTool),
        Arc::new(delegate::DelegateTool),
        Arc::new(plugins::InstagramProfileTool),
        Arc::new(plugins::InstagramStoriesTool),
        Arc::new(plugins::InstagramWatchlistTool),
        Arc::new(plugins::InstagramWatchTool),
        Arc::new(plugins::InstagramUnwatchTool),
        Arc::new(plugins::InstagramNewTool),
        Arc::new(plugins::InstagramSaveLatestTool),
        Arc::new(plugins::PixivSearchTool),
        Arc::new(plugins::PixivRankingTool),
        Arc::new(plugins::SaveToGalleryTool),
        Arc::new(reach::MessageClaudeTool),
        Arc::new(reach::MessageGrokTool),
        Arc::new(reach::LucinaAgentsTool),
        Arc::new(reach::ReachThreadsTool),
    ]
}

/// Parse validated arguments into the tool's typed struct.
pub(crate) fn args<T: serde::de::DeserializeOwned>(
    v: serde_json::Value,
) -> Result<T, crate::ToolError> {
    serde_json::from_value(v).map_err(|e| crate::ToolError::new(format!("invalid arguments: {e}")))
}
