//! JSON in, JSON out: the one interface `secretary-ffi` and the Android bridge expose,
//! so Swift and Kotlin hold no vault logic. Every answer has `ok`; a failure adds `code`
//! and `error`, and a conflict adds `current`. docs/lucina-vault.md lists the operations.
use crate::{
    graph::GraphOptions,
    markdown,
    vault::{note_name, Options, Vault},
    Error,
};
use serde_json::{json, Value};

pub struct Session {
    vault: Vault,
}

fn s<'a>(v: &'a Value, key: &str) -> Result<&'a str, Error> {
    v[key]
        .as_str()
        .ok_or_else(|| Error::Invalid(format!("missing {key}")))
}

impl Session {
    pub fn open(root: &str) -> Result<Self, Error> {
        Ok(Session {
            vault: Vault::open(root, Options::default())?,
        })
    }
    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    pub fn request(&mut self, input: &str) -> String {
        let answer = serde_json::from_str::<Value>(input)
            .map_err(|e| Error::Invalid(format!("bad JSON: {e}")))
            .and_then(|v| self.handle(&v));
        match answer {
            Ok(mut v) => {
                v["ok"] = json!(true);
                v.to_string()
            }
            Err(e) => {
                let code = match &e {
                    Error::Invalid(_) => "invalid",
                    Error::ReadOnly => "read_only",
                    Error::NotFound(_) => "not_found",
                    Error::Conflict { .. } => "conflict",
                    Error::Exists(_) => "exists",
                    Error::Io(_) => "io",
                };
                let mut out = json!({"ok": false, "code": code, "error": e.to_string()});
                if let Error::Conflict { current, .. } = &e {
                    out["current"] = json!(current);
                }
                out.to_string()
            }
        }
    }

    fn note_json(&self, path: &str) -> Result<Value, Error> {
        let n = self
            .vault
            .note(path)
            .ok_or_else(|| Error::NotFound(path.to_owned()))?;
        let t = self.vault.read(path)?;
        let outgoing: Vec<Value> = self
            .vault
            .outgoing(path)
            .into_iter()
            .map(|(l, d)| {
                json!({"link": l.link, "display": l.display, "embed": l.embed, "line": l.line, "dest": d})
            })
            .collect();
        let properties: Vec<Value> = n
            .scan
            .properties
            .iter()
            .map(|(k, v)| json!({"key": k, "value": v}))
            .collect();
        Ok(json!({
            "path": path,
            "name": n.name(),
            "text": t.text,
            "revision": t.revision,
            "blocks": markdown::blocks(&t.text),
            "tags": n.scan.tags,
            "aliases": n.scan.aliases,
            "headings": n.scan.headings,
            "properties": properties,
            "outgoing": outgoing,
            "backlinks": self.vault.backlinks(path),
        }))
    }

    fn handle(&mut self, v: &Value) -> Result<Value, Error> {
        let limit = v["limit"].as_u64().unwrap_or(100) as usize;
        match s(v, "op")? {
            "status" => Ok(json!({
                "root": self.vault.root().display().to_string(),
                "writable": self.vault.writable(),
                "counts": self.vault.counts(),
                "warnings": self.vault.warnings().iter().take(50).collect::<Vec<_>>(),
            })),
            "set_writable" => {
                self.vault.opts.writable = v["writable"].as_bool().unwrap_or(false);
                Ok(json!({"writable": self.vault.writable()}))
            }
            "refresh" => Ok(json!({"read": self.vault.refresh()?, "counts": self.vault.counts()})),
            "list" => {
                let folder = v["folder"].as_str().unwrap_or("");
                let prefix = if folder.is_empty() {
                    String::new()
                } else {
                    format!("{folder}/")
                };
                let direct = |p: &str| {
                    p.strip_prefix(prefix.as_str())
                        .is_some_and(|r| !r.contains('/'))
                };
                let mut notes: Vec<Value> = self
                    .vault
                    .notes()
                    .filter(|n| direct(&n.path))
                    .map(|n| json!({"path": n.path, "name": n.name(), "mtimeNs": n.mtime_ns, "size": n.size}))
                    .collect();
                notes.sort_by_cached_key(|n| n["name"].as_str().unwrap_or("").to_lowercase());
                let mut files: Vec<&String> = self
                    .vault
                    .attachments()
                    .iter()
                    .filter(|p| direct(p))
                    .collect();
                files.sort_by_cached_key(|p| p.to_lowercase());
                let folders: Vec<String> = self
                    .vault
                    .folders()
                    .into_iter()
                    .filter(|f| direct(f))
                    .collect();
                Ok(json!({"folder": folder, "folders": folders, "notes": notes, "files": files}))
            }
            "note" => self.note_json(s(v, "path")?),
            "resolve" => {
                let from = v["from"].as_str().unwrap_or("");
                let dest = self.vault.resolve(from, s(v, "link")?);
                Ok(
                    json!({"dest": dest, "isNote": dest.is_some_and(|d| self.vault.note(d).is_some())}),
                )
            }
            "file_path" => {
                let p = self.vault.file_path(s(v, "path")?)?;
                Ok(json!({"absolute": p.display().to_string()}))
            }
            "search" => Ok(json!({"hits": self.vault.search(s(v, "query")?, limit)})),
            "quick_switch" => {
                let hits: Vec<Value> = self
                    .vault
                    .quick_switch(s(v, "query")?, limit)
                    .into_iter()
                    .map(|p| json!({"name": note_name(&p), "path": p}))
                    .collect();
                Ok(json!({"hits": hits}))
            }
            "tags" => Ok(json!({"tags": self.vault.tags()})),
            "tag_notes" => {
                let paths: Vec<&str> = self
                    .vault
                    .notes_with_tag(s(v, "tag")?)
                    .into_iter()
                    .map(|n| n.path.as_str())
                    .collect();
                Ok(json!({"paths": paths}))
            }
            "unresolved" => Ok(json!({"unresolved": self.vault.unresolved()})),
            "graph" => {
                let o = GraphOptions {
                    around: v["around"].as_str().map(str::to_owned),
                    depth: v["depth"].as_u64().unwrap_or(1) as u32,
                    with_attachments: v["attachments"].as_bool().unwrap_or(false),
                    with_orphans: v["orphans"].as_bool().unwrap_or(false),
                    tag: v["tag"].as_str().map(str::to_owned),
                };
                Ok(serde_json::to_value(self.vault.graph(&o)).unwrap_or(Value::Null))
            }
            "save" => {
                let revision = self
                    .vault
                    .save(s(v, "path")?, s(v, "text")?, s(v, "revision")?)?;
                Ok(json!({"revision": revision}))
            }
            "conflict_copy" => {
                let p = self.vault.save_conflict_copy(
                    s(v, "path")?,
                    s(v, "text")?,
                    s(v, "date")?,
                    v["device"].as_str().unwrap_or("device"),
                )?;
                Ok(json!({"path": p}))
            }
            "create" => {
                let path = match v["path"].as_str() {
                    Some(p) => p.to_owned(),
                    None => self
                        .vault
                        .new_note_path(s(v, "name")?, v["folder"].as_str().unwrap_or("")),
                };
                let revision = self
                    .vault
                    .create(&path, v["text"].as_str().unwrap_or("").as_bytes())?;
                Ok(json!({"path": path, "revision": revision}))
            }
            "rename" => {
                let plan = self.vault.plan_rename(s(v, "from")?, s(v, "to")?)?;
                let updated = self.vault.rename(&plan)?;
                Ok(json!({"path": plan.to, "updated": updated}))
            }
            "trash" => Ok(json!({"movedTo": self.vault.trash(s(v, "path")?)?})),
            other => Err(Error::Invalid(format!("unknown op: {other}"))),
        }
    }
}
