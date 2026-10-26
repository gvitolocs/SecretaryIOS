use crate::{binary::Reader, invalid, Result};
use serde::Serialize;
#[derive(Debug, Serialize)]
pub struct Menu {
    pub version: i32,
    pub source: String,
    pub name: String,
    pub category: String,
    pub description: String,
    pub commands: Vec<Vec<String>>,
}
impl Menu {
    /// Parse commands as inert data; this never evaluates script text.
    pub fn read(bytes: &[u8]) -> Result<Self> {
        let mut r = Reader::new(bytes);
        if r.string()? != "CM3D2_MENU" {
            return Err(invalid("menu signature"));
        }
        let version = r.i32()?;
        let source = r.string()?;
        let name = r.string()?;
        let category = r.string()?;
        let description = r.string()?;
        let n = r.count(16 * 1024 * 1024)?;
        let mut script = Reader::new(r.take(n)?);
        let mut commands = Vec::new();
        loop {
            let n = script.u8()?;
            if n == 0 {
                break;
            }
            let mut args = Vec::new();
            for _ in 0..n {
                args.push(script.string()?);
            }
            commands.push(args);
            if commands.len() > 100_000 {
                return Err(invalid("menu command limit"));
            }
        }
        if script.left() != 0 || r.left() != 0 {
            return Err(invalid("menu command block length"));
        }
        Ok(Self {
            version,
            source,
            name,
            category,
            description,
            commands,
        })
    }
}
