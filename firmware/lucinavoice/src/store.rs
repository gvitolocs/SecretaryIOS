//! Settings in the first sector of the `nvs` partition, in the
//! [`lucinavoice_core::settings`] format (not ESP-IDF NVS: nothing else on
//! this firmware uses NVS).

use esp_bootloader_esp_idf::partitions::{
    self, DataPartitionSubType, PARTITION_TABLE_MAX_LEN, PartitionType,
};
use esp_storage::FlashStorage;
use lucinavoice_core::settings::{MAX_LEN, Settings};

/// Flash write failed, or the record read back differs.
#[derive(Debug)]
pub struct StoreError;

pub struct Store {
    flash: FlashStorage<'static>,
    offset: u32,
}

impl Store {
    /// `None` if the partition table has no `nvs` partition.
    pub fn new(mut flash: FlashStorage<'static>) -> Option<Self> {
        let mut table = [0u8; PARTITION_TABLE_MAX_LEN];
        let pt = partitions::read_partition_table(&mut flash, &mut table).ok()?;
        let nvs = pt
            .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))
            .ok()??;
        let offset = nvs.offset();
        Some(Self { flash, offset })
    }

    /// The saved settings, or empty ones.
    pub fn load(&mut self) -> Settings {
        let mut raw = [0u8; MAX_LEN];
        match self.flash.read(self.offset, &mut raw) {
            Ok(()) => Settings::decode(&raw).unwrap_or_default(),
            Err(_) => Settings::default(),
        }
    }

    pub fn save(&mut self, settings: &Settings) -> Result<(), StoreError> {
        let mut raw = [0xFFu8; MAX_LEN + 3];
        let mut record = [0u8; MAX_LEN];
        let n = settings.encode(&mut record);
        raw[..n].copy_from_slice(&record[..n]);
        // Flash writes are in 4-byte words.
        let len = (n + 3) & !3;
        self.flash
            .write(self.offset, &raw[..len])
            .map_err(|_| StoreError)?;
        (self.load() == *settings).then_some(()).ok_or(StoreError)
    }

    pub fn erase(&mut self) -> Result<(), StoreError> {
        self.flash
            .write(self.offset, &[0xFF; MAX_LEN + 3][..(MAX_LEN + 3) & !3])
            .map_err(|_| StoreError)
    }
}
