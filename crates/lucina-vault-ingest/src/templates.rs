//! The guidelines prompt and the note templates: files in the crate
//! (`guidelines/`, `templates/`), compiled in, and replaceable at run time by
//! a directory with the same file names (`LUCINA_VAULT_INGEST_TEMPLATES`).

use std::path::Path;

const GUIDELINES: &str = include_str!("../guidelines/zettelkasten.md");
const PERMANENT: &str = include_str!("../templates/permanent.md");
const EVENT: &str = include_str!("../templates/event.md");
const HUB: &str = include_str!("../templates/hub.md");
const MOC: &str = include_str!("../templates/moc.md");

#[derive(Debug, Clone)]
pub struct Templates {
    /// The system prompt (the owner's Zettelkasten rules).
    pub guidelines: String,
    pub permanent: String,
    pub event: String,
    pub hub: String,
    pub moc: String,
}

impl Default for Templates {
    fn default() -> Self {
        Self {
            guidelines: GUIDELINES.into(),
            permanent: PERMANENT.into(),
            event: EVENT.into(),
            hub: HUB.into(),
            moc: MOC.into(),
        }
    }
}

impl Templates {
    /// The built-in files, each replaced by `<dir>/guidelines/zettelkasten.md`
    /// or `<dir>/templates/<name>.md` when that file exists.
    pub fn load(dir: Option<&Path>) -> Self {
        let mut t = Self::default();
        let Some(dir) = dir else {
            return t;
        };
        for (rel, slot) in [
            ("guidelines/zettelkasten.md", &mut t.guidelines),
            ("templates/permanent.md", &mut t.permanent),
            ("templates/event.md", &mut t.event),
            ("templates/hub.md", &mut t.hub),
            ("templates/moc.md", &mut t.moc),
        ] {
            match std::fs::read_to_string(dir.join(rel)) {
                Ok(s) if !s.trim().is_empty() => *slot = s,
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!(file = rel, error = %e, "vault-ingest: template unreadable; built-in used")
                }
            }
        }
        t
    }
}

/// `{{name}}` → value, in one pass (a value is never expanded again).
pub fn fill(template: &str, vars: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(template.len() + 512);
    let mut rest = template;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        match after.find("}}") {
            Some(j) => {
                let name = &after[..j];
                match vars.iter().find(|(k, _)| *k == name) {
                    Some((_, v)) => out.push_str(v),
                    None => out.push_str(&rest[i..i + 4 + j]),
                }
                rest = &after[j + 2..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filling() {
        assert_eq!(
            fill(
                "a {{x}} b {{y}} {{z}}",
                &[("x", "{{y}}".into()), ("y", "2".into())]
            ),
            "a {{y}} b 2 {{z}}"
        );
        assert_eq!(fill("open {{x", &[("x", "1".into())]), "open {{x");
    }

    #[test]
    fn built_ins_have_their_fields() {
        let t = Templates::default();
        for f in [
            "{{title}}",
            "{{body}}",
            "{{files}}",
            "{{connects_to}}",
            "{{enables}}",
            "{{sources}}",
            "{{event}}",
        ] {
            assert!(t.permanent.contains(f), "{f}");
        }
        for f in ["{{summary}}", "{{outputs}}", "{{source}}", "{{key}}"] {
            assert!(t.event.contains(f), "{f}");
        }
        assert!(t.guidelines.contains("one claim per note"));
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("templates")).unwrap();
        std::fs::write(dir.path().join("templates/event.md"), "E {{title}}").unwrap();
        let l = Templates::load(Some(dir.path()));
        assert_eq!(l.event, "E {{title}}");
        assert_eq!(l.permanent, t.permanent);
    }
}
