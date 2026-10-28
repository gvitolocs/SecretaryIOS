//! Agent titles (Paseo `create-agent-title.js`, `agent-title-limits.js`).

use crate::jsstr;

/// Paseo `MAX_EXPLICIT_AGENT_TITLE_CHARS`.
pub const MAX_EXPLICIT_TITLE_CHARS: usize = 200;
/// Paseo `MAX_INITIAL_AGENT_TITLE_CHARS` (`min(60, 200)`).
pub const MAX_INITIAL_TITLE_CHARS: usize = 60;

/// Paseo `deriveInitialAgentTitle`: the first non-blank line, whitespace collapsed,
/// cut to 60 UTF-16 units and trimmed.
pub fn initial_title(prompt: &str) -> Option<String> {
    let first = prompt
        .split('\n')
        .map(|line| jsstr::trim(line.strip_suffix('\r').unwrap_or(line)))
        .find(|line| !line.is_empty())?;
    let normalized = jsstr::collapse_spaces(first);
    let normalized = jsstr::trim(&normalized);
    if normalized.is_empty() {
        return None;
    }
    let clamped = jsstr::slice(normalized, 0, MAX_INITIAL_TITLE_CHARS);
    let clamped = jsstr::trim(&clamped);
    (!clamped.is_empty()).then(|| clamped.to_string())
}

/// Paseo `resolveCreateAgentTitles`: `(explicit, provisional)`. An explicit title
/// wins; otherwise the provisional title comes from the trimmed prompt.
pub fn resolve_titles(
    config_title: Option<&str>,
    initial_prompt: Option<&str>,
) -> (Option<String>, Option<String>) {
    let explicit = config_title
        .map(jsstr::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    let trimmed_prompt = initial_prompt.map(jsstr::trim).filter(|p| !p.is_empty());
    let provisional = explicit
        .clone()
        .or_else(|| trimmed_prompt.and_then(initial_title));
    (explicit, provisional)
}
