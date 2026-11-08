//! Power and frontlight traits. No implementation yet: on the PW3 these map to sysfs nodes
//! (battery gauge, `fl_intensity`-style frontlight control) that have not been inspected on the device.
use crate::Result;

pub trait Power {
    /// Battery charge 0..=100.
    fn battery_percent(&mut self) -> Result<u8>;
    fn is_charging(&mut self) -> Result<bool>;
    /// Keep the stock screensaver/suspend from triggering while the reader is in the foreground.
    fn inhibit_suspend(&mut self, inhibit: bool) -> Result<()>;
}

pub trait Frontlight {
    /// Highest supported level (0 = off).
    fn max_level(&self) -> u32;
    fn level(&mut self) -> Result<u32>;
    fn set_level(&mut self, level: u32) -> Result<()>;
}
