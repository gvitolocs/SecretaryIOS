//! The real providers, and the registry the daemon builds at startup from the
//! installed CLIs and the owner's lane config.
//!
//! | Provider | Transport |
//! |---|---|
//! | `claude` | Claude Code CLI, `--input-format stream-json --output-format stream-json` |
//! | `codex` | `codex app-server` (JSON-RPC) |
//! | `cursor`, `opencode`, `copilot`, and `[acp.<id>]` entries | ACP over stdio (`cursor-agent acp`, `opencode acp`, `copilot --acp`) |
//! | `pi` | `pi --mode rpc` (JSON lines) |
//! | `zcode` | the ZCode CLI (`zcode.cjs --json`), one process per turn |

pub mod acp;
mod acp_subagents;
pub mod claude;
pub mod claude_subagents;
pub mod codex;
pub mod pi;
pub mod zcode;

use std::sync::Arc;

use crate::lane::LaneConfig;
use crate::process::{resolve_program, Registry};
use crate::provider::Provider;

/// Every provider whose CLI is installed. Models and efforts default to the first
/// lane of the matching kind in the lane config.
pub fn installed(lanes: &LaneConfig, registry: &Registry) -> Vec<Arc<dyn Provider>> {
    let mut out: Vec<Arc<dyn Provider>> = Vec::new();
    let lane = |kind: &str| lanes.first_of_kind(kind).map(|(_, l)| l.clone());
    if let Some(bin) = resolve_program("claude").or_else(|| resolve_program("~/.local/bin/claude")) {
        let l = lane("claude");
        out.push(Arc::new(claude::ClaudeProvider {
            bin,
            default_model: l.as_ref().and_then(|l| l.model.clone()),
            default_effort: l.as_ref().and_then(|l| l.effort.clone()),
            registry: Some(registry.clone()),
        }));
    }
    if let Some(bin) = resolve_program("codex").or_else(|| resolve_program("~/.local/bin/codex")) {
        let l = lane("codex");
        out.push(Arc::new(codex::CodexProvider {
            bin,
            default_model: l.as_ref().and_then(|l| l.model.clone()),
            default_effort: l.as_ref().and_then(|l| l.effort.clone()),
            registry: Some(registry.clone()),
        }));
    }
    for def in acp::builtin() {
        if let Some(bin) = resolve_program(def.bin).or_else(|| resolve_program(&format!("~/.local/bin/{}", def.bin))).or_else(|| resolve_program(&format!("~/bin/{}", def.bin))) {
            let default_model = if def.id == "cursor" { lane("cursor").and_then(|l| l.model) } else { None };
            out.push(Arc::new(acp::AcpProvider::new(def, bin, default_model, Some(registry.clone()))));
        }
    }
    out.extend(acp::custom(registry));
    if let Some(bin) = resolve_program("pi").or_else(|| resolve_program("~/.local/bin/pi")) {
        out.push(Arc::new(pi::PiProvider { bin, registry: Some(registry.clone()) }));
    }
    if let Some(dir) = zcode::find_dir(lane("zcode").and_then(|l| l.dir).as_deref()) {
        if let Some(node) = resolve_program("node") {
            out.push(Arc::new(zcode::ZcodeProvider { node, dir, registry: Some(registry.clone()) }));
        }
    }
    out
}
