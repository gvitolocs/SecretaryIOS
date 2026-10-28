use crate::{binary::Reader, invalid, Error, Result};
pub struct Texture {
    pub name: String,
    pub version: i32,
    pub encoded: Vec<u8>,
    pub mime: &'static str,
}
impl Texture {
    pub fn read(bytes: &[u8]) -> Result<Self> {
        let mut r = Reader::new(bytes);
        if r.string()? != "CM3D2_TEX" {
            return Err(invalid("texture signature"));
        }
        let version = r.i32()?;
        if ![1000, 1010, 1011].contains(&version) {
            return Err(Error::Unsupported(format!("texture version {version}")));
        }
        let name = r.string()?;
        if version >= 1011 {
            let n = r.count(100000)?;
            for _ in 0..n {
                r.array::<4>()?;
            }
        }
        if version >= 1010 {
            let w = r.i32()?;
            let h = r.i32()?;
            let _format = r.i32()?;
            if !(1..=16384).contains(&w) || !(1..=16384).contains(&h) {
                return Err(invalid("texture dimensions"));
            }
        }
        let n = r.count(128 * 1024 * 1024)?;
        let encoded = r.take(n)?.to_vec();
        let mime = if encoded.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else if encoded.starts_with(&[255, 216, 255]) {
            "image/jpeg"
        } else {
            return Err(Error::Unsupported(
                "texture payload is not PNG/JPEG; compressed texture decoder pending".into(),
            ));
        };
        if r.left() != 0 {
            return Err(invalid("trailing texture data"));
        }
        Ok(Self {
            name,
            version,
            encoded,
            mime,
        })
    }
}
