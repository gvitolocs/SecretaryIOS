//! The device an app turn comes from, as a fact of the turn (no message text is matched).
//!
//! The timeline channel names the owner's client: `dsi` (the Nintendo DSi's Fortuna shell) and `mac`
//! (the Lucina app on his Mac) join `web` and `ios` ([`secretary_core::channel`]). A message that
//! arrives through one is proof that the device is switched on and connected, so the turn's system
//! prompt says so, and Jev's state carries the same two facts.
//!
//! Her own tools do not reach the DSi or the Mac: what he asks her to do on or with them goes to an
//! agent (`crate::act`), whose brief takes the device notes from [`agent_notes`].

/// The device fact of a turn from the Nintendo DSi (the Fortuna shell).
pub const DSI_SYSTEM: &str = "The owner is talking to you right now through his Nintendo DSi (the Fortuna shell). It is switched on, on his home Wi-Fi and connected to you: this message came from it. Never say the DSi is unreachable, offline or not connected. You have no direct tool that acts on the DSi; when he asks you to do something on it or with it, an agent does it (agent_task).";

/// The device fact of a turn from the Lucina app on his Mac.
pub const MAC_SYSTEM: &str = "The owner is writing to you right now from the Lucina app on his Mac. The Mac is switched on and connected to you: this message came from it. Never say the Mac is unreachable, offline or not connected. You have no direct tool that acts on the Mac; when he asks you to do something on it, an agent does it (agent_task).";

/// What this turn's system prompt says about the device, for the owner's own turns only. Empty for
/// every channel that is not the DSi or the Mac, and for an MCP caller (`mcp`): an agent's prose is
/// not the owner on a device.
pub fn system_fact(channel: &str, mcp: bool) -> &'static str {
    if mcp {
        return "";
    }
    match channel {
        "dsi" => DSI_SYSTEM,
        "mac" => MAC_SYSTEM,
        _ => "",
    }
}

/// Jev's `device` fact for a channel: `nintendo_dsi`, `mac_app`, `iphone_app` or `web_app`; empty
/// when the device is not known. The turn's state also gets `device_connected: true` when this is
/// not empty. The iPhone and web labels exist for callers that want them; the chat route labels only
/// the DSi and the Mac ([`jev_device_of_turn`]), so the states of everyday turns stay as Jev was
/// calibrated on them.
pub fn jev_device(channel: &str) -> &'static str {
    match channel {
        "dsi" => "nintendo_dsi",
        "mac" => "mac_app",
        "ios" => "iphone_app",
        "web" => "web_app",
        _ => "",
    }
}

/// The `device` fact the chat route hands to Jev: the DSi and the Mac only, owner turns only.
pub fn jev_device_of_turn(channel: &str, mcp: bool) -> &'static str {
    if mcp || !matches!(channel, "dsi" | "mac") {
        return "";
    }
    jev_device(channel)
}

/// The Jev state of a turn with the device facts added: `device` and `device_connected`, or the
/// state as it is when `device` is empty.
pub fn with_device(mut state: serde_json::Value, device: &str) -> serde_json::Value {
    if !device.is_empty() {
        state["device"] = serde_json::json!(device);
        state["device_connected"] = serde_json::json!(true);
    }
    state
}

/// What the owner calls the device, for the agent's brief and his ack.
pub fn label(channel: &str) -> &'static str {
    match channel {
        "dsi" => "Nintendo DSi",
        "mac" => "Mac",
        "ios" => "iPhone",
        _ => "web page",
    }
}

/// What an agent needs to know about the device the request came from (and about the ones he
/// names). Facts of the home setup, not instructions about the task.
pub fn agent_notes(channel: &str) -> String {
    let mut notes = String::new();
    match channel {
        "dsi" => notes.push_str(
            "The Nintendo DSi runs the Fortuna shell. It is reachable only from nezopt, on the home LAN: FTP on port 21 without login. \
The helper `~/scratch/fortuna-chat/dsi.sh` has the commands ip, site, shot, get and put. Files go onto its SD card by FTP put. \
`SITE BOOT <sd path>` starts a homebrew .nds. Retail .nds games cannot be started remotely today and must be started by the owner from the shell. \
GBA and GB titles open through their runner's own file list. The docs are in `/home/nez/Projects/Lucina/LucinaOS/fortuna/docs/lucina.md`. \
Keep any activity on the DSi under 2 minutes. Never reboot, reflash or delete anything on it.",
        ),
        "mac" => notes.push_str(
            "The Mac is the owner's MacBook Air with the Lucina app. Nothing on it is reachable from nezopt unless the task says how. Do not change its settings.",
        ),
        _ => {}
    }
    notes
}

/// The owner's other machines, named in a request (same for every brief).
pub const MACHINES: &str = "The owner's own machines: nespc is his Ubuntu PC (`nespc.fritz.box`, user `nes`, on Wi-Fi); nezopt is the home server this agent runs on.";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn device_facts_only_for_the_dsi_and_the_mac_on_the_owners_turns() {
        assert!(system_fact("dsi", false).contains("Nintendo DSi"));
        assert!(system_fact("dsi", false).contains("connected"));
        assert!(system_fact("mac", false).contains("Mac"));
        for channel in ["web", "ios", "telegram", "voice", ""] {
            assert_eq!(system_fact(channel, false), "", "{channel}");
        }
        assert_eq!(
            system_fact("dsi", true),
            "",
            "an MCP caller is not the owner on a device"
        );
    }

    #[test]
    fn device_jev_state_has_the_two_facts_or_neither() {
        let base = json!({"message": "ciao", "platform": "secretary"});
        let dsi = with_device(base.clone(), jev_device_of_turn("dsi", false));
        assert_eq!(dsi["device"], "nintendo_dsi");
        assert_eq!(dsi["device_connected"], true);
        let mac = with_device(base.clone(), jev_device_of_turn("mac", false));
        assert_eq!(mac["device"], "mac_app");
        // Unknown (and the everyday web and iPhone turns, and MCP turns): neither key.
        for (channel, mcp) in [
            ("web", false),
            ("ios", false),
            ("telegram", false),
            ("dsi", true),
        ] {
            let s = with_device(base.clone(), jev_device_of_turn(channel, mcp));
            assert_eq!(s, base, "{channel} {mcp}");
        }
        assert_eq!(jev_device("ios"), "iphone_app");
        assert_eq!(jev_device("web"), "web_app");
        assert_eq!(jev_device("other"), "");
    }

    #[test]
    fn device_notes_for_the_dsi_name_the_helper_and_the_limits() {
        let n = agent_notes("dsi");
        for needle in [
            "dsi.sh",
            "FTP",
            "SITE BOOT",
            "under 2 minutes",
            "Never reboot, reflash or delete",
        ] {
            assert!(n.contains(needle), "{needle}");
        }
        assert_eq!(agent_notes("web"), "");
    }
}
