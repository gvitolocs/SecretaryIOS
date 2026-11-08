use crate::{
    binary::Reader,
    invalid,
    model::{self, Material},
    Result,
};
pub fn read(bytes: &[u8]) -> Result<Material> {
    let mut r = Reader::new(bytes);
    if r.string()? != "CM3D2_MATERIAL" {
        return Err(invalid("material signature"));
    }
    let _version = r.i32()?;
    let _name = r.string()?;
    let m = model::material(&mut r)?;
    if r.left() != 0 {
        return Err(invalid("trailing material bytes"));
    }
    Ok(m)
}
