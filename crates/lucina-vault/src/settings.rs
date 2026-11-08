//! What the vault's own `.obsidian/app.json` says about where new files go. Read,
//! never written. A broken or missing file means Obsidian's defaults.
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// `""` or `/` is the root, `./x` is relative to the note.
    pub attachment_folder_path: String,
    /// `root`, `current` or `folder`.
    pub new_file_location: String,
    pub new_file_folder_path: String,
}

impl Settings {
    pub fn load(root: &Path) -> Self {
        std::fs::read(root.join(".obsidian/app.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Folder for a new note, `current` being the folder of the open note.
    pub fn new_note_folder(&self, current: &str) -> String {
        match self.new_file_location.as_str() {
            "current" => current.to_owned(),
            "folder" => self.new_file_folder_path.trim_matches('/').to_owned(),
            _ => String::new(),
        }
    }

    /// Folder for an attachment added to the note at `note`.
    pub fn attachment_folder(&self, note: &str) -> String {
        let p = self.attachment_folder_path.as_str();
        let here = note.rsplit_once('/').map_or("", |(f, _)| f);
        if let Some(rel) = p.strip_prefix("./") {
            return [here, rel]
                .iter()
                .filter(|c| !c.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join("/");
        }
        if p == "." {
            return here.to_owned();
        }
        p.trim_matches('/').to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folders() {
        let s = Settings {
            attachment_folder_path: "./att".into(),
            new_file_location: "folder".into(),
            new_file_folder_path: "/Inbox/".into(),
        };
        assert_eq!(s.attachment_folder("a/b.md"), "a/att");
        assert_eq!(s.attachment_folder("b.md"), "att");
        assert_eq!(s.new_note_folder("x"), "Inbox");
        assert_eq!(Settings::default().new_note_folder("x"), "");
        assert_eq!(Settings::default().attachment_folder("a/b.md"), "");
    }
}
