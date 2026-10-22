//! Built-in tools.

pub mod avatar;
pub mod calculator;
pub mod notes;
pub mod places;
pub mod presence;
pub mod reminders;
pub mod shopping;
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
        Arc::new(notes::NotesTool),
        Arc::new(reminders::RemindersTool),
        Arc::new(shopping::ShoppingTool),
        Arc::new(presence::PresenceTool),
        Arc::new(avatar::AvatarOutfitTool),
    ]
}

/// Parse validated arguments into the tool's typed struct.
pub(crate) fn args<T: serde::de::DeserializeOwned>(
    v: serde_json::Value,
) -> Result<T, crate::ToolError> {
    serde_json::from_value(v).map_err(|e| crate::ToolError::new(format!("invalid arguments: {e}")))
}
