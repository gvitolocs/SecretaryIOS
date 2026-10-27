//! The Cua stdio MCP proxy: `cua-mcp.sh` for the `lucina-workspace` desktop
//! and `android-cua-mcp.sh` for the `lucina-android` container.
//!
//! Both scripts refuse to start unless the Cua daemon's socket exists, and
//! they expose only the sandbox named on the command line, so every call this
//! module makes lands inside a container. The host paths in [`HOST_PATHS`]
//! are refused before the call is made, and the sandbox has none of them
//! anyway (`check-cua-mcp.sh` asserts it).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

use crate::Error;

/// The workspace desktop's MCP proxy.
pub const WORKSPACE_SANDBOX_SHELL: &str = "/home/nez/Projects/Lucina/lucina-cua/scripts/cua-mcp.sh";
/// The Android container's MCP proxy.
pub const ANDROID_SANDBOX_SHELL: &str =
    "/home/nez/Projects/Lucina/lucina-cua/scripts/android-cua-mcp.sh";
/// `local:lucina-workspace`: the Linux desktop container.
pub const WORKSPACE_SPACE: &str = "local:lucina-workspace";
/// `direct:127.0.0.1:32792`: the Android container.
pub const ANDROID_SPACE: &str = "direct:127.0.0.1:32792";

/// Tool calls reach a container, not the host: these are never allowed.
pub const HOST_PATHS: [&str; 5] = [
    "/home/nez",
    "/var/run/docker.sock",
    "/run/docker.sock",
    "/proc/1/root",
    "/root/.ssh",
];

/// Longest a single proxied call may take (the driver kills its own command
/// at 60 s by default; a desktop click waits on a screenshot).
const CALL_TIMEOUT: Duration = Duration::from_secs(90);

/// An absolute path inside a sandbox. Host paths and traversal are refused,
/// so a `computer_write_file` can never name something on nezopt even before
/// the container's own filesystem says no.
pub fn guest_path(path: &str) -> Result<String, Error> {
    let p = path.trim();
    if !p.starts_with('/') {
        return Err(Error::Sandbox(
            "path must be absolute inside the sandbox".into(),
        ));
    }
    if p.split('/').any(|s| s == "..") {
        return Err(Error::Sandbox("path must not contain '..'".into()));
    }
    if p.chars().any(|c| c.is_control()) {
        return Err(Error::Sandbox("path contains control characters".into()));
    }
    for host in HOST_PATHS {
        if p == host || p.starts_with(&format!("{host}/")) {
            return Err(Error::Sandbox(format!(
                "{host} is a host path; the sandbox has its own filesystem"
            )));
        }
    }
    Ok(p.to_owned())
}

/// The text of an MCP tool result (`content: [{type: "text", text}]`).
pub fn text_of(result: &Value) -> String {
    let mut out = String::new();
    if let Some(items) = result.get("content").and_then(Value::as_array) {
        for item in items {
            if item.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(t) = item.get("text").and_then(Value::as_str) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(t);
                }
            }
        }
    }
    // Driver browser tools put target/tab IDs and DOM refs in structuredContent,
    // alongside a short text summary. Preserve both for the next tool call.
    if let Some(s) = result.get("structuredContent") {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&s.to_string());
    }
    out
}

/// A screenshot or other image block: `(mime_type, base64)`.
pub fn image_of(result: &Value) -> Option<(String, String)> {
    result
        .get("content")?
        .as_array()?
        .iter()
        .find(|i| i.get("type").and_then(Value::as_str) == Some("image"))
        .and_then(|i| {
            let data = i.get("data").and_then(Value::as_str)?;
            let mime = i
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("image/png");
            Some((mime.to_owned(), data.to_owned()))
        })
}

/// Split shell commands into words and command segments, respecting quotes and
/// escapes so a plain mention (including quoted separators) stays ordinary data.
fn shell_segments(command: &str) -> Vec<Vec<String>> {
    let mut segments = Vec::new();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    for c in command.chars() {
        if escaped {
            word.push(c);
            escaped = false;
        } else if c == '\\' && quote != Some('\'') {
            escaped = true;
        } else if Some(c) == quote {
            quote = None;
        } else if quote.is_some() {
            word.push(c);
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c.is_whitespace() || matches!(c, ';' | '&' | '|') {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            if matches!(c, ';' | '&' | '|' | '\n') && !words.is_empty() {
                segments.push(std::mem::take(&mut words));
            }
        } else {
            word.push(c);
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    if !words.is_empty() {
        segments.push(words);
    }
    segments
}

fn firefox_program(word: &str) -> bool {
    matches!(word.rsplit('/').next(), Some("firefox" | "firefox-esr"))
}

fn firefox_package(word: &str) -> bool {
    // Debian version/architecture suffixes, snap channels and Flatpak IDs.
    word.to_ascii_lowercase()
        .split(['/', ':', '=', '.'])
        .any(|p| p == "firefox" || p == "firefox-esr")
}

fn forbidden_browser_command(command: &str) -> bool {
    shell_segments(command).iter().any(|words| {
        let mut i = 0;
        // Common launcher wrappers, their options, and environment assignments.
        while let Some(word) = words.get(i) {
            let name = word.rsplit('/').next().unwrap_or(word);
            if matches!(name, "env" | "nohup" | "setsid" | "exec" | "sudo") {
                i += 1;
                while words.get(i).is_some_and(|s| s.starts_with('-')) {
                    let option = words[i].as_str();
                    i += 1;
                    if matches!(
                        option,
                        "-u" | "-g"
                            | "-h"
                            | "-p"
                            | "-C"
                            | "--user"
                            | "--group"
                            | "--unset"
                            | "--chdir"
                    ) {
                        i += 1;
                    }
                }
            } else if word.split_once('=').is_some_and(|(name, _)| {
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            }) {
                i += 1;
            } else {
                break;
            }
        }
        let Some(program) = words.get(i) else {
            return false;
        };
        if firefox_program(program) {
            return true;
        }
        let name = program.rsplit('/').next().unwrap_or(program);
        matches!(name, "apt" | "apt-get" | "snap" | "flatpak")
            && words[i + 1..].iter().any(|w| w == "install")
            && words[i + 1..].iter().any(|w| firefox_package(w))
    })
}

fn names_firefox(value: &Value) -> bool {
    match value {
        Value::String(s) => s.to_ascii_lowercase().contains("firefox"),
        Value::Array(items) => items.iter().any(names_firefox),
        Value::Object(items) => items.values().any(names_firefox),
        _ => false,
    }
}

/// One sandbox, reached through its stdio MCP proxy.
#[derive(Debug, Clone)]
pub struct Sandbox {
    script: PathBuf,
    space: String,
    call_timeout: Duration,
}

impl Sandbox {
    /// The `lucina-workspace` Linux desktop.
    pub fn workspace() -> Self {
        Self {
            script: PathBuf::from(WORKSPACE_SANDBOX_SHELL),
            space: WORKSPACE_SPACE.into(),
            call_timeout: CALL_TIMEOUT,
        }
    }

    /// The `lucina-android` container.
    pub fn android() -> Self {
        Self {
            script: PathBuf::from(ANDROID_SANDBOX_SHELL),
            space: ANDROID_SPACE.into(),
            call_timeout: CALL_TIMEOUT,
        }
    }

    pub fn with_script(mut self, script: impl Into<PathBuf>) -> Self {
        self.script = script.into();
        self
    }

    pub fn with_timeout(mut self, d: Duration) -> Self {
        self.call_timeout = d;
        self
    }

    pub fn space(&self) -> &str {
        &self.space
    }

    pub fn script(&self) -> &std::path::Path {
        &self.script
    }

    /// `list_spaces` as this proxy sees it.
    pub async fn spaces(&self) -> Result<Value, Error> {
        self.call("list_spaces", json!({})).await
    }

    /// `space_bash`: `/bin/sh -c` inside the container.
    pub async fn bash(&self, command: &str, seconds: Option<u64>) -> Result<Value, Error> {
        if command.trim().is_empty() {
            return Err(Error::Sandbox("command must not be empty".into()));
        }
        self.call(
            "space_bash",
            json!({"command": command, "timeout": seconds.unwrap_or(60)}),
        )
        .await
    }

    /// `space_write`: literal bytes at an absolute guest path.
    pub async fn write(&self, path: &str, content: &str) -> Result<Value, Error> {
        let path = guest_path(path)?;
        self.call("space_write", json!({"path": path, "content": content}))
            .await
    }

    /// `list_tools`: the driver tools the desktop exposes (`screenshot`,
    /// `click`, `type`, `open_url`, …).
    pub async fn tools(&self, name: Option<&str>) -> Result<Value, Error> {
        let mut args = json!({});
        if let Some(n) = name {
            args["name"] = json!(n);
        }
        self.call("list_tools", args).await
    }

    /// `call_tool`: one driver tool (`screenshot`, `click`, `type`, …).
    pub async fn call_tool(&self, tool: &str, arguments: Value) -> Result<Value, Error> {
        if tool.trim().is_empty() {
            return Err(Error::Sandbox("tool must not be empty".into()));
        }
        self.call("call_tool", json!({"tool": tool, "arguments": arguments}))
            .await
    }

    /// Enforce at the lowest shared entry point, including raw `call` users.
    fn check_browser_policy(&self, tool: &str, args: &Value) -> Result<(), Error> {
        if self.space != WORKSPACE_SPACE {
            return Ok(());
        }
        let denied = (tool == "space_bash"
            && args
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(forbidden_browser_command))
            || (tool.eq_ignore_ascii_case("launch_app") && names_firefox(args))
            || (tool == "call_tool"
                && args
                    .get("tool")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.eq_ignore_ascii_case("launch_app"))
                && args.get("arguments").is_some_and(names_firefox));
        if denied {
            Err(Error::BrowserPolicy)
        } else {
            Ok(())
        }
    }

    /// One MCP `tools/call` over a fresh stdio session; the proxy is
    /// stateless, so the session ends with the call.
    pub async fn call(&self, tool: &str, mut args: Value) -> Result<Value, Error> {
        self.check_browser_policy(tool, &args)?;
        if !self.script.exists() {
            return Err(Error::Sandbox(format!(
                "{} is missing",
                self.script.display()
            )));
        }
        if !args.is_object() {
            return Err(Error::Sandbox("arguments must be an object".into()));
        }
        if tool != "list_spaces" {
            args["space"] = json!(self.space);
        }
        let mut cmd = Command::new(&self.script);
        cmd.env_clear()
            .env("HOME", "/home/nez")
            .env("PATH", "/home/nez/.local/bin:/usr/local/bin:/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .current_dir("/tmp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Sandbox(format!("{}: {e}", self.script.display())))?;
        let Some(mut stdin) = child.stdin.take() else {
            return Err(Error::Sandbox("no stdin to the proxy".into()));
        };
        let Some(stdout) = child.stdout.take() else {
            return Err(Error::Sandbox("no stdout from the proxy".into()));
        };
        let requests = [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                              "clientInfo": {"name": "lucina-rs", "version": crate::VERSION}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                   "params": {"name": tool, "arguments": args}}),
        ];
        let mut body = String::new();
        for r in requests {
            body.push_str(&r.to_string());
            body.push('\n');
        }
        let write = async {
            stdin.write_all(body.as_bytes()).await?;
            stdin.shutdown().await
        };
        if write.await.is_err() {
            return Err(Error::Sandbox("the proxy closed its stdin".into()));
        }
        let mut lines = BufReader::new(stdout).lines();
        let reply = timeout(self.call_timeout, async {
            while let Some(line) = lines.next_line().await.ok().flatten() {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if v.get("id").and_then(Value::as_u64) == Some(2) {
                    return Some(v);
                }
            }
            None
        })
        .await;
        let _ = child.start_kill();
        let reply = match reply {
            Ok(Some(v)) => v,
            Ok(None) => return Err(Error::Sandbox("the proxy answered nothing".into())),
            Err(_) => return Err(Error::Sandbox("the proxy timed out".into())),
        };
        if let Some(err) = reply.get("error") {
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("MCP error");
            return Err(Error::Sandbox(message.to_owned()));
        }
        let result = reply.get("result").cloned().unwrap_or(Value::Null);
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            let text = text_of(&result);
            return Err(Error::Sandbox(if text.is_empty() {
                format!("{tool} failed inside the sandbox")
            } else {
                text
            }));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn workspace_refuses_firefox_before_contacting_proxy() {
        let sb = Sandbox::workspace().with_script("/nonexistent/cua-mcp.sh");
        for command in [
            "firefox https://example.com &",
            "firefox-esr",
            "/usr/bin/firefox",
            "/usr/lib/firefox/firefox",
            "true; firefox",
            "true && firefox",
            "false || firefox",
            "echo hello | firefox",
            "true & firefox",
            "true\nfirefox",
            "env DISPLAY=:1 nohup setsid exec sudo -u cua firefox",
            "DISPLAY=:1 /usr/bin/firefox",
            "'firefox'",
            "fire\\fox",
            "sudo apt-get install -y firefox",
            "apt install firefox-esr:amd64",
            "snap install firefox",
            "flatpak install flathub org.mozilla.firefox",
        ] {
            let e = sb.bash(command, None).await.unwrap_err();
            assert_eq!(e, Error::BrowserPolicy, "{command}");
            assert_eq!(e.code(), "bad_request");
            assert_eq!(e.to_string(), "Lucina's computer uses Chromium only: use the browser_* tools (start_session first) or launch Chromium.");
            assert_eq!(
                sb.call("space_bash", json!({"command":command}))
                    .await
                    .unwrap_err(),
                e
            );
        }
        for args in [
            json!({"app":"Firefox"}),
            json!({"command":["/usr/bin/FIREFOX-ESR"]}),
        ] {
            assert_eq!(
                sb.call_tool("launch_app", args.clone()).await.unwrap_err(),
                Error::BrowserPolicy
            );
            assert_eq!(
                sb.call("launch_app", args).await.unwrap_err(),
                Error::BrowserPolicy
            );
        }
    }

    #[test]
    fn chromium_mentions_and_android_are_allowed() {
        let sb = Sandbox::workspace();
        for command in [
            "chromium https://example.com &",
            "echo firefox",
            "echo 'hello; firefox'",
            "printf '%s' firefox",
            "apt-get purge -y firefox",
            "apt install chromium",
            "snap list firefox",
            "which firefox",
        ] {
            assert!(!forbidden_browser_command(command), "{command}");
            assert!(sb
                .check_browser_policy("space_bash", &json!({"command":command}))
                .is_ok());
        }
        for tool in ["start_session", "browser_navigate", "click"] {
            assert!(sb
                .check_browser_policy(
                    "call_tool",
                    &json!({"tool":tool,"arguments":{"url":"https://example.com"}})
                )
                .is_ok());
        }
        assert!(sb
            .check_browser_policy(
                "call_tool",
                &json!({"tool":"launch_app","arguments":{"app":"chromium"}})
            )
            .is_ok());
        for (tool, args) in [
            ("space_bash", json!({"command":"firefox"})),
            (
                "call_tool",
                json!({"tool":"launch_app","arguments":{"app":"firefox"}}),
            ),
        ] {
            assert!(Sandbox::android().check_browser_policy(tool, &args).is_ok());
        }
    }

    #[test]
    fn host_paths_are_refused_before_the_call() {
        assert_eq!(
            guest_path("/home/cua/notes.txt").unwrap(),
            "/home/cua/notes.txt"
        );
        assert_eq!(guest_path("/tmp/a b.txt").unwrap(), "/tmp/a b.txt");
        for host in [
            "/home/nez",
            "/home/nez/Projects/Lucina/SecretaryIOS/Cargo.toml",
            "/var/run/docker.sock",
            "/proc/1/root/etc/shadow",
        ] {
            let e = guest_path(host).unwrap_err();
            assert!(
                matches!(e, Error::Sandbox(_)) && e.to_string().contains("host"),
                "{host}: {e}"
            );
        }
        assert!(guest_path("relative/path").is_err());
        assert!(guest_path("/home/cua/../../nez/x").is_err());
    }

    #[tokio::test]
    async fn a_missing_proxy_is_an_unavailable_sandbox() {
        let sandbox = Sandbox::workspace().with_script("/nonexistent/cua-mcp.sh");
        let e = sandbox.spaces().await.unwrap_err();
        assert_eq!(e.code(), "sandbox_unavailable");
        let e = sandbox
            .write("/home/nez/.ssh/authorized_keys", "x")
            .await
            .unwrap_err();
        assert!(e.to_string().contains("host"));
    }

    #[test]
    fn result_shapes() {
        let text = json!({"content": [{"type": "text", "text": "hello"},
                                      {"type": "text", "text": "world"}]});
        assert_eq!(text_of(&text), "hello\nworld");
        let bound = json!({"content":[{"type":"text","text":"bound target"}],
            "structuredContent":{"target_id":"bt-test","tabs":[{"tab_id":"tab-test"}]}});
        let bound_text = text_of(&bound);
        assert!(bound_text.starts_with("bound target\n"));
        assert!(bound_text.contains("bt-test") && bound_text.contains("tab-test"));
        let image = json!({"content": [{"type": "image", "mimeType": "image/png", "data": "AAA"}]});
        assert_eq!(
            image_of(&image),
            Some(("image/png".to_string(), "AAA".to_string()))
        );
        assert_eq!(image_of(&text), None);
    }
}
