//! Overlay resource lookup for native archives. Referenced textures/materials
//! can live in the base archive while a model or menu lives in a later pack.
use crate::{arc::Archive, invalid, Result};
use std::path::Path;
pub trait ResourceSource {
    fn paths(&self) -> Box<dyn Iterator<Item = &str> + '_>;
    fn read_resource(&mut self, path: &str) -> Result<Vec<u8>>;
    fn resolve(&self, name: &str, near: &str) -> Result<String> {
        let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
        let mut seen = std::collections::HashSet::new();
        let mut choices = self
            .paths()
            .filter(|p| seen.insert(p.to_lowercase()))
            .filter(|p| {
                p.rsplit('/')
                    .next()
                    .is_some_and(|n| n.eq_ignore_ascii_case(base))
            })
            .map(|p| {
                (
                    p.split('/')
                        .zip(near.split('/'))
                        .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
                        .count(),
                    p.to_owned(),
                )
            })
            .collect::<Vec<_>>();
        choices.sort_by_key(|a| std::cmp::Reverse(a.0));
        choices.dedup_by(|a, b| a.1.eq_ignore_ascii_case(&b.1));
        if choices.is_empty() {
            return Err(invalid(format!("asset not found: {name}")));
        }
        if choices.get(1).is_some_and(|p| p.0 == choices[0].0) {
            return Err(invalid(format!("ambiguous asset: {name}")));
        }
        Ok(choices.remove(0).1)
    }
}
impl ResourceSource for Archive {
    fn paths(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(self.entries.iter().map(|e| e.path.as_str()))
    }
    fn read_resource(&mut self, path: &str) -> Result<Vec<u8>> {
        self.read(path)
    }
}
pub struct ArchiveSet {
    pub archives: Vec<Archive>,
}
impl ArchiveSet {
    pub fn open(paths: &[String]) -> Result<Self> {
        Ok(Self {
            archives: paths
                .iter()
                .map(|p| Archive::open(Path::new(p)))
                .collect::<Result<Vec<_>>>()?,
        })
    }
}
impl ResourceSource for ArchiveSet {
    fn paths(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(
            self.archives
                .iter()
                .rev()
                .flat_map(|a| a.entries.iter().map(|e| e.path.as_str())),
        )
    }
    fn read_resource(&mut self, path: &str) -> Result<Vec<u8>> {
        for a in self.archives.iter_mut().rev() {
            if a.entries.iter().any(|e| e.path.eq_ignore_ascii_case(path)) {
                return a.read(path);
            }
        }
        Err(invalid(format!("resource missing: {path}")))
    }
}
