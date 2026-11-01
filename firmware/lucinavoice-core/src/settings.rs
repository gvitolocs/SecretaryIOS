//! The settings record kept in the first sector of the `nvs` partition.
//!
//! Layout: `b"LVS1"`, then three length-prefixed (u8) UTF-8 fields (SSID,
//! Wi-Fi password, device token), then a CRC-32 of everything before it.
//! An erased sector (all `0xFF`), a wrong magic or a bad CRC reads as "no
//! settings". Fields are bounded by what Wi-Fi and the server allow.

pub const MAGIC: &[u8; 4] = b"LVS1";
pub const MAX_SSID: usize = 32;
pub const MAX_PASSWORD: usize = 64;
pub const MAX_TOKEN: usize = 160;
/// Largest encoded record.
pub const MAX_LEN: usize = 4 + 3 + MAX_SSID + MAX_PASSWORD + MAX_TOKEN + 4;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    pub ssid: Field<MAX_SSID>,
    pub password: Field<MAX_PASSWORD>,
    pub token: Field<MAX_TOKEN>,
}

/// A bounded UTF-8 string without an allocator.
#[derive(Clone, PartialEq, Eq)]
pub struct Field<const N: usize> {
    len: u8,
    buf: [u8; N],
}

impl<const N: usize> Default for Field<N> {
    fn default() -> Self {
        Self {
            len: 0,
            buf: [0; N],
        }
    }
}

impl<const N: usize> core::fmt::Debug for Field<N> {
    /// Never prints the value: fields hold the Wi-Fi password and the token.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Field({} bytes)", self.len)
    }
}

impl<const N: usize> Field<N> {
    /// `None` if `s` is longer than `N` bytes.
    pub fn new(s: &str) -> Option<Self> {
        let mut f = Self::default();
        f.set(s).then_some(f)
    }

    pub fn set(&mut self, s: &str) -> bool {
        if s.len() > N {
            return false;
        }
        self.buf[..s.len()].copy_from_slice(s.as_bytes());
        self.len = s.len() as u8;
        true
    }

    pub fn as_str(&self) -> &str {
        // Only `set` and `decode` (which checks UTF-8) write the buffer.
        core::str::from_utf8(&self.buf[..self.len as usize]).unwrap_or("")
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Settings {
    pub fn has_wifi(&self) -> bool {
        !self.ssid.is_empty()
    }

    pub fn is_paired(&self) -> bool {
        !self.token.is_empty()
    }

    /// Writes the record into `out` and returns its length.
    pub fn encode(&self, out: &mut [u8; MAX_LEN]) -> usize {
        let mut n = 0;
        out[..4].copy_from_slice(MAGIC);
        n += 4;
        for field in [
            self.ssid.as_str(),
            self.password.as_str(),
            self.token.as_str(),
        ] {
            out[n] = field.len() as u8;
            n += 1;
            out[n..n + field.len()].copy_from_slice(field.as_bytes());
            n += field.len();
        }
        let crc = crc32(&out[..n]);
        out[n..n + 4].copy_from_slice(&crc.to_le_bytes());
        n + 4
    }

    /// `None` for an erased, foreign or damaged record.
    pub fn decode(raw: &[u8]) -> Option<Self> {
        if raw.get(..4)? != MAGIC {
            return None;
        }
        let mut n = 4;
        let mut s = Settings::default();
        for i in 0..3 {
            let len = *raw.get(n)? as usize;
            n += 1;
            let text = core::str::from_utf8(raw.get(n..n + len)?).ok()?;
            n += len;
            let ok = match i {
                0 => s.ssid.set(text),
                1 => s.password.set(text),
                _ => s.token.set(text),
            };
            if !ok {
                return None;
            }
        }
        let stored = u32::from_le_bytes(raw.get(n..n + 4)?.try_into().ok()?);
        (crc32(&raw[..n]) == stored).then_some(s)
    }
}

/// CRC-32 (IEEE, reflected), bitwise: the record is read once at boot.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    fn sample() -> Settings {
        Settings {
            ssid: Field::new("home").unwrap(),
            password: Field::new("pässword").unwrap(),
            token: Field::new("sec_dev_abc").unwrap(),
        }
    }

    #[test]
    fn round_trips() {
        let mut buf = [0u8; MAX_LEN];
        let n = sample().encode(&mut buf);
        assert_eq!(Settings::decode(&buf[..n]), Some(sample()));
    }

    #[test]
    fn rejects_erased_flash_and_corruption() {
        assert_eq!(Settings::decode(&[0xFF; 64]), None);
        let mut buf = [0u8; MAX_LEN];
        let n = sample().encode(&mut buf);
        buf[6] ^= 1;
        assert_eq!(Settings::decode(&buf[..n]), None);
        assert_eq!(Settings::decode(&buf[..n - 1]), None);
    }

    #[test]
    fn fields_are_bounded_and_never_printed() {
        assert!(Field::<4>::new("12345").is_none());
        let f = Field::<64>::new("secret").unwrap();
        assert_eq!(std::format!("{f:?}"), "Field(6 bytes)");
    }

    #[test]
    fn max_record_fits() {
        let s = Settings {
            ssid: Field::new(&"s".repeat(MAX_SSID)).unwrap(),
            password: Field::new(&"p".repeat(MAX_PASSWORD)).unwrap(),
            token: Field::new(&"t".repeat(MAX_TOKEN)).unwrap(),
        };
        let mut buf = [0u8; MAX_LEN];
        assert_eq!(s.encode(&mut buf), MAX_LEN);
        assert_eq!(Settings::decode(&buf), Some(s));
    }

    #[test]
    fn crc_matches_reference() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
