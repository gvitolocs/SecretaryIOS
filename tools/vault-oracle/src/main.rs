//! `vault-oracle <debug port> <out.json> [script.js]`: real Obsidian is the source of truth
//! for how a vault reads (docs/porting-guidelines.md, section 5). Attach to a
//! running copy, wait for its metadata cache, and dump what it decided. Link
//! targets are also asked one by one through `getFirstLinkpathDest`. A script replaces
//! the dump and its returned JSON string is the output.
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::TcpStream,
    time::{Duration, Instant},
};
use tungstenite::{connect, Message};

const DUMP: &str = r#"(async () => {
  const c = app.metadataCache;
  const files = app.vault.getFiles().map(f => f.path).sort();
  const notes = {};
  for (const f of app.vault.getMarkdownFiles()) {
    const m = c.getFileCache(f) || {};
    const pick = l => (l || []).map(x => ({
      link: x.link, original: x.original, display: x.displayText,
      line: x.position ? x.position.start.line : null,
      dest: (c.getFirstLinkpathDest(x.link.split('#')[0], f.path) || {}).path || null,
    }));
    notes[f.path] = {
      links: pick(m.links), embeds: pick(m.embeds),
      frontmatterLinks: (m.frontmatterLinks || []).map(x => ({key: x.key, link: x.link, original: x.original,
        dest: (c.getFirstLinkpathDest(x.link.split('#')[0], f.path) || {}).path || null})),
      tags: (m.tags || []).map(t => ({tag: t.tag, line: t.position.start.line})),
      frontmatter: m.frontmatter || null,
      headings: (m.headings || []).map(h => ({heading: h.heading, level: h.level, line: h.position.start.line})),
      blocks: Object.keys(m.blocks || {}).sort(),
      sections: (m.sections || []).map(s => ({type: s.type, start: s.position.start.line, end: s.position.end.line})),
      listItems: (m.listItems || []).map(l => ({line: l.position.start.line, task: l.task === undefined ? null : l.task})),
    };
  }
  return JSON.stringify({
    obsidian: electron.ipcRenderer.sendSync('version'),
    files, notes,
    resolved: c.resolvedLinks, unresolved: c.unresolvedLinks, tags: c.getTags(),
    config: {attachmentFolderPath: app.vault.getConfig('attachmentFolderPath'),
             newFileLocation: app.vault.getConfig('newFileLocation'),
             newFileFolderPath: app.vault.getConfig('newFileFolderPath')},
  });
})()"#;

fn http_get(port: u16, path: &str) -> Option<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut out = Vec::new();
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    // The debug server may reset instead of closing: what arrived counts.
    let _ = s.read_to_end(&mut out);
    let out = String::from_utf8_lossy(&out);
    out.split_once("\r\n\r\n").map(|(_, b)| b.to_owned())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args[1].parse().expect("port");
    let started = Instant::now();
    let page = loop {
        let found = http_get(port, "/json")
            .and_then(|b| serde_json::from_str::<Value>(&b).ok())
            .and_then(|v| {
                v.as_array()?
                    .iter()
                    .find(|t| t["type"] == "page")
                    .and_then(|t| t["webSocketDebuggerUrl"].as_str().map(str::to_owned))
            });
        if let Some(url) = found {
            break url;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "no page on the debug port"
        );
        std::thread::sleep(Duration::from_secs(1));
    };
    let (mut ws, _) = connect(page.as_str()).expect("debugger socket");
    let mut id = 0;
    let mut eval = |expr: &str| -> Value {
        id += 1;
        let msg = json!({"id": id, "method": "Runtime.evaluate",
            "params": {"expression": expr, "awaitPromise": true, "returnByValue": true}});
        ws.send(Message::text(msg.to_string())).expect("send");
        loop {
            let text = match ws.read().expect("read") {
                Message::Text(t) => t.to_string(),
                _ => continue,
            };
            let v: Value = serde_json::from_str(&text).expect("json");
            if v["id"] == id {
                return v["result"].clone();
            }
        }
    };
    // Ready when the app exists and the cache has taken in every file: a count that stops moving.
    let mut last = -1i64;
    let mut still = 0;
    loop {
        let n = eval("typeof app === 'undefined' || !app.metadataCache ? -1 : Object.keys(app.metadataCache.resolvedLinks).length");
        let n = n["result"]["value"].as_i64().unwrap_or(-1);
        still = if n >= 0 && n == last { still + 1 } else { 0 };
        last = n;
        if still >= 5 {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(600),
            "cache never settled"
        );
        std::thread::sleep(Duration::from_secs(2));
    }
    let script = args
        .get(3)
        .map(|p| std::fs::read_to_string(p).expect("script"));
    let r = eval(script.as_deref().unwrap_or(DUMP));
    let text = r["result"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("dump failed: {r}"));
    let v: Value = serde_json::from_str(text).expect("dump json");
    std::fs::write(
        &args[2],
        serde_json::to_string_pretty(&v).expect("pretty") + "\n",
    )
    .expect("write");
    println!(
        "obsidian {} files {} notes {} after {:?}",
        v["obsidian"],
        v["files"].as_array().map_or(0, Vec::len),
        v["notes"].as_object().map_or(0, |m| m.len()),
        started.elapsed()
    );
}
