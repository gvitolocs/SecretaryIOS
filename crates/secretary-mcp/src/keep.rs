//! Keep features on the owner's notes (colours, labels, checklists): the pure
//! parts, shared by `note_save`, `note_update` and the formatting code.
//!
//! Keep notes live in secretary-server's note store. Labels are the notes'
//! `tags`; a list note carries `items` (`{id, text, checked, indent}`) and its
//! `content` is derived from them on the server.

use serde_json::{json, Value};

/// Keep's palette as secretary-server stores it (Takeout's names, lower-case).
pub const COLORS: [&str; 12] = [
    "default", "red", "orange", "yellow", "green", "teal", "blue", "cerulean", "purple", "pink",
    "brown", "gray",
];

/// A Keep colour name (case-insensitive; "grey" is accepted for "gray").
pub fn color(s: &str) -> Result<String, String> {
    let c = s.trim().to_lowercase();
    let c = if c == "grey" { "gray".to_owned() } else { c };
    if COLORS.contains(&c.as_str()) {
        Ok(c)
    } else {
        Err(format!(
            "`color` must be one of {} (got {s:?})",
            COLORS.join(", ")
        ))
    }
}

/// A label name as typed: trimmed, inner whitespace collapsed, a leading '#'
/// dropped. Keeps the case (it is the display name).
pub fn label_name(s: &str) -> Result<String, String> {
    let t = s.trim().trim_start_matches('#');
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() {
        return Err("a label name must not be empty".into());
    }
    if t.chars().count() > 50 {
        return Err(format!("label {t:?} is longer than 50 characters"));
    }
    if t.chars().any(|c| c.is_control() || c == ',') {
        return Err(format!(
            "label {t:?} contains a comma or a control character"
        ));
    }
    Ok(t)
}

/// The stored form of a label (secretary-server's `tags`): lower-case.
pub fn label_tag(name: &str) -> String {
    name.trim()
        .trim_start_matches('#')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The `labels` to send in a PATCH: the note's current tags minus `remove`
/// (matched by stored form), plus `add` (display names as typed).
pub fn merge_labels(tags: &[String], add: &[String], remove: &[String]) -> Vec<String> {
    let removed: Vec<String> = remove.iter().map(|r| label_tag(r)).collect();
    let mut out: Vec<String> = tags
        .iter()
        .filter(|t| !removed.contains(&label_tag(t)))
        .cloned()
        .collect();
    for a in add {
        let tag = label_tag(a);
        if !out.iter().any(|t| label_tag(t) == tag) {
            out.push(a.clone());
        }
    }
    out
}

/// A reference to a checklist item: its number (as `note_get` shows it, from
/// 1) or its exact text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemRef {
    Number(usize),
    Text(String),
}

/// A checklist item to add: text, state, indent (0 or 1).
pub fn new_item(text: &str, checked: bool) -> Value {
    json!({"text": text, "checked": checked, "indent": 0})
}

fn item_text(it: &Value) -> &str {
    it.get("text").and_then(Value::as_str).unwrap_or("")
}

/// What identifies an item across a refetch: its id, else its text.
fn item_key(it: &Value) -> String {
    it.get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| item_text(it).to_owned())
}

fn numbered(items: &[Value]) -> String {
    items
        .iter()
        .enumerate()
        .take(40)
        .map(|(i, it)| format!("{}. {}", i + 1, item_text(it)))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Resolves a reference against the items as the caller saw them.
pub fn resolve(items: &[Value], r: &ItemRef) -> Result<usize, String> {
    match r {
        ItemRef::Number(n) => {
            if *n >= 1 && *n <= items.len() {
                Ok(*n - 1)
            } else {
                Err(format!(
                    "No item number {n}: the list has {} item(s). Items: {}",
                    items.len(),
                    numbered(items)
                ))
            }
        }
        ItemRef::Text(t) => {
            let want = t.trim().to_lowercase();
            let hits: Vec<usize> = items
                .iter()
                .enumerate()
                .filter(|(_, it)| item_text(it).trim().to_lowercase() == want)
                .map(|(i, _)| i)
                .collect();
            match hits.as_slice() {
                [i] => Ok(*i),
                [] => Err(format!(
                    "No item with the text {t:?}. Items: {}",
                    numbered(items)
                )),
                many => Err(format!(
                    "{} items have the text {t:?} (numbers {}); use the item number.",
                    many.len(),
                    many.iter()
                        .map(|i| (i + 1).to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
        }
    }
}

/// Checklist edits, resolved once against the items the caller saw so that a
/// retry after a 409 applies them to the same items (by id) in the new version.
#[derive(Debug, Default, Clone)]
pub struct Plan {
    pub check: Vec<String>,
    pub uncheck: Vec<String>,
    pub remove: Vec<String>,
    pub add: Vec<Value>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.check.is_empty()
            && self.uncheck.is_empty()
            && self.remove.is_empty()
            && self.add.is_empty()
    }

    /// Resolves every reference against `items`; the first bad one is the error.
    pub fn new(
        items: &[Value],
        check: &[ItemRef],
        uncheck: &[ItemRef],
        remove: &[ItemRef],
        add: Vec<Value>,
    ) -> Result<Self, String> {
        let keys = |refs: &[ItemRef]| -> Result<Vec<String>, String> {
            refs.iter()
                .map(|r| resolve(items, r).map(|i| item_key(&items[i])))
                .collect()
        };
        Ok(Self {
            check: keys(check)?,
            uncheck: keys(uncheck)?,
            remove: keys(remove)?,
            add,
        })
    }

    /// The new items array: existing items kept as they are (ids included),
    /// edited, then the new ones at the end.
    pub fn apply(&self, items: &[Value]) -> Vec<Value> {
        let mut out = Vec::with_capacity(items.len() + self.add.len());
        for it in items {
            let key = item_key(it);
            if self.remove.contains(&key) {
                continue;
            }
            let mut it = it.clone();
            if self.check.contains(&key) {
                it["checked"] = json!(true);
            }
            if self.uncheck.contains(&key) {
                it["checked"] = json!(false);
            }
            out.push(it);
        }
        out.extend(self.add.iter().cloned());
        out
    }

    /// "checked 2, unchecked 1, removed 1, added 3".
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        for (n, what) in [
            (self.check.len(), "checked"),
            (self.uncheck.len(), "unchecked"),
            (self.remove.len(), "removed"),
            (self.add.len(), "added"),
        ] {
            if n > 0 {
                parts.push(format!("{what} {n} item{}", if n == 1 { "" } else { "s" }));
            }
        }
        parts.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<Value> {
        vec![
            json!({"id": "i_a", "text": "Milk", "checked": false, "indent": 0}),
            json!({"id": "i_b", "text": "Bread", "checked": true, "indent": 0}),
            json!({"id": "i_c", "text": "Milk", "checked": false, "indent": 1}),
        ]
    }

    #[test]
    fn colours() {
        assert_eq!(color(" Blue ").unwrap(), "blue");
        assert_eq!(color("grey").unwrap(), "gray");
        assert!(color("mauve").unwrap_err().contains("cerulean"));
    }

    #[test]
    fn labels() {
        assert_eq!(label_name("  #Fisica   2 ").unwrap(), "Fisica 2");
        assert!(label_name("a,b").is_err());
        assert_eq!(label_tag("#Fisica  2"), "fisica 2");
        let tags = vec!["godlevel".to_owned(), "fisica 2".to_owned()];
        let out = merge_labels(
            &tags,
            &["Work".into(), "GodLevel".into()],
            &["Fisica 2".into()],
        );
        assert_eq!(out, vec!["godlevel".to_owned(), "Work".to_owned()]);
    }

    #[test]
    fn references() {
        let it = items();
        assert_eq!(resolve(&it, &ItemRef::Number(2)).unwrap(), 1);
        assert!(resolve(&it, &ItemRef::Number(0)).is_err());
        assert!(resolve(&it, &ItemRef::Number(4))
            .unwrap_err()
            .contains("3 item(s)"));
        assert_eq!(resolve(&it, &ItemRef::Text(" bread ".into())).unwrap(), 1);
        let e = resolve(&it, &ItemRef::Text("milk".into())).unwrap_err();
        assert!(e.contains("numbers 1, 3"), "{e}");
        let e = resolve(&it, &ItemRef::Text("eggs".into())).unwrap_err();
        assert!(e.contains("1. Milk; 2. Bread"), "{e}");
    }

    #[test]
    fn plan_applies_by_id_to_a_newer_version() {
        let it = items();
        let plan = Plan::new(
            &it,
            &[ItemRef::Number(1)],
            &[ItemRef::Text("bread".into())],
            &[ItemRef::Number(3)],
            vec![new_item("Eggs", false)],
        )
        .unwrap();
        assert_eq!(
            plan.summary(),
            "checked 1 item, unchecked 1 item, removed 1 item, added 1 item"
        );
        // A newer version where somebody inserted an item at the top and removed Bread.
        let newer = vec![
            json!({"id": "i_z", "text": "Wine", "checked": false, "indent": 0}),
            it[0].clone(),
            it[2].clone(),
        ];
        let out = plan.apply(&newer);
        let texts: Vec<&str> = out.iter().map(item_text).collect();
        assert_eq!(texts, ["Wine", "Milk", "Eggs"]);
        assert_eq!(out[1]["checked"], true);
        assert_eq!(out[1]["id"], "i_a", "ids are kept");
        assert!(
            out[2].get("id").is_none(),
            "new items get their id from the server"
        );
    }
}
