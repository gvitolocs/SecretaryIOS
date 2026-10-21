//! Catalog: offline product-name lookup over a JSON asset.
//!
//! The asset is OUR data file (`assets/catalog.example.json` in the repo, or
//! any bundle the operator points LUCINA_CATALOG / Catalog::open at). The
//! reference client's bundled catalog is proprietary and is NOT shipped in
//! this repo — point `open` at such a file locally if you have the rights.

use crate::shopping::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub name: String,
    #[serde(default)]
    pub section: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogFile {
    pub entries: Vec<CatalogEntry>,
}

pub struct Catalog {
    entries: Vec<CatalogEntry>,
}

impl Catalog {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        let file: CatalogFile = serde_json::from_str(&raw)?;
        Ok(Self {
            entries: file.entries,
        })
    }

    pub fn from_entries(entries: Vec<CatalogEntry>) -> Self {
        Self { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Case-insensitive substring search, best (shortest) match first.
    pub fn search(&self, query: &str, limit: usize) -> Vec<&CatalogEntry> {
        let q = query.to_lowercase();
        let mut hits: Vec<&CatalogEntry> = self
            .entries
            .iter()
            .filter(|e| e.name.to_lowercase().contains(&q))
            .collect();
        hits.sort_by_key(|e| e.name.len());
        hits.truncate(limit);
        hits
    }

    /// Section guess for a free-typed item (used when the user invents a name).
    pub fn section_for(&self, name: &str) -> Option<String> {
        self.search(name, 1).first().and_then(|e| e.section.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat() -> Catalog {
        Catalog::from_entries(vec![
            CatalogEntry {
                name: "Milk 3.5%".into(),
                section: Some("Cooling".into()),
            },
            CatalogEntry {
                name: "Milk chocolate".into(),
                section: Some("Sweets".into()),
            },
            CatalogEntry {
                name: "Butter".into(),
                section: None,
            },
        ])
    }

    #[test]
    fn search_prefers_shortest() {
        let cat = cat();
        let hits = cat.search("milk", 10);
        assert_eq!(hits[0].name, "Milk 3.5%");
    }

    #[test]
    fn section_for_unknown_is_none() {
        let cat = cat();
        assert!(cat.section_for("xylophone").is_none());
        assert_eq!(cat.section_for("butter"), None);
        assert_eq!(cat.section_for("chocolate").as_deref(), Some("Sweets"));
    }
}
