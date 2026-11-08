use crate::{invalid, Result};
pub struct Reader<'a> {
    pub bytes: &'a [u8],
    pub pos: usize,
}
impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    pub fn left(&self) -> usize {
        self.bytes.len() - self.pos
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| invalid("offset overflow"))?;
        let data = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| invalid(format!("truncated at {} requesting {n}", self.pos)))?;
        self.pos = end;
        Ok(data)
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn i64(&mut self) -> Result<i64> {
        Ok(self.u64()? as i64)
    }
    pub fn f32(&mut self) -> Result<f32> {
        let v = f32::from_bits(self.u32()?);
        if !v.is_finite() {
            return Err(invalid("non-finite numeric value"));
        }
        Ok(v)
    }
    pub fn array<const N: usize>(&mut self) -> Result<[f32; N]> {
        let mut out = [0.; N];
        for v in &mut out {
            *v = self.f32()?;
        }
        Ok(out)
    }
    pub fn count(&mut self, max: usize) -> Result<usize> {
        let n = self.i32()?;
        if n < 0 || n as usize > max {
            return Err(invalid(format!("count {n} exceeds {max}")));
        }
        Ok(n as usize)
    }
    /// System.IO.BinaryReader's UTF-8 string with a seven-bit byte-length prefix.
    pub fn string(&mut self) -> Result<String> {
        let mut n = 0u32;
        for shift in [0, 7, 14, 21, 28] {
            let b = self.u8()?;
            if shift == 28 && b > 7 {
                return Err(invalid("invalid seven-bit string length"));
            }
            n |= u32::from(b & 127) << shift;
            if b < 128 {
                if n > 1_048_576 {
                    return Err(invalid("string too large"));
                }
                return String::from_utf8(self.take(n as usize)?.to_vec())
                    .map_err(|_| invalid("string UTF-8"));
            }
        }
        Err(invalid("unterminated string length"))
    }
}
