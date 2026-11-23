//! What a disk reports about itself, read from the raw buffers that
//! `gozler-winfs` returns for `IOCTL_STORAGE_QUERY_PROPERTY`: the device
//! descriptor, for the disk's model, serial number and bus, and an NVMe
//! disk's SMART / Health Information log, for its own wear estimate and the
//! bytes written over its life.
//!
//! Layouts, all little-endian:
//!
//! - `STORAGE_DEVICE_DESCRIPTOR` and `STORAGE_PROTOCOL_DATA_DESCRIPTOR` from
//!   `winioctl.h`, as the `windows-sys` 0.61 structs declare them.
//! - The health log from `NVME_HEALTH_INFO_LOG` in `nvme.h`, which follows
//!   the NVMe Base Specification's log page 02h.
//! - How the protocol data is found and checked, from Microsoft's
//!   "Working with NVMe drives"
//!   (<https://learn.microsoft.com/en-us/windows/win32/fileio/working-with-nvme-devices>):
//!   `ProtocolDataOffset` counts from the start of `ProtocolSpecificData`,
//!   not from the start of the buffer.
//!
//! A driver and a disk fill these buffers, so a length or an offset in them
//! can be anything: each one is checked, and a buffer whose parts do not fit
//! together is [`Malformed`].

use thiserror::Error;
use zerocopy::little_endian::{I32, U32};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

/// `STORAGE_BUS_TYPE`'s `BusTypeNvme`.
pub const BUS_NVME: i32 = 17;

/// The bytes of one NVMe health log (`NVME_HEALTH_INFO_LOG`).
const HEALTH_LOG_BYTES: usize = 512;

/// The bytes one NVMe data unit stands for: 1,000 units of 512 bytes. The
/// disk rounds up, so the count of a disk that wrote anything is at least 1.
const DATA_UNIT_BYTES: u128 = 512_000;

/// Where `ProtocolSpecificData` starts in a `STORAGE_PROTOCOL_DATA_DESCRIPTOR`,
/// after its `Version` and `Size`.
const SPECIFIC_DATA_AT: usize = 8;

/// The bytes of `STORAGE_PROTOCOL_SPECIFIC_DATA`.
const SPECIFIC_DATA_BYTES: usize = 40;

/// The parts of a disk's buffer do not fit together; the text says which.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("malformed disk data: {0}")]
pub struct Malformed(&'static str);

/// What a disk's device descriptor says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// `STORAGE_BUS_TYPE`, such as [`BUS_NVME`] or 7 for USB.
    pub bus: i32,
    /// The product id without its padding; `None` when the descriptor holds
    /// none.
    pub model: Option<String>,
    /// The serial number without its padding, which tells the disk from
    /// another of its model; `None` when the descriptor holds none.
    pub serial: Option<String>,
}

impl Device {
    pub fn is_nvme(&self) -> bool {
        self.bus == BUS_NVME
    }
}

/// What an NVMe disk's health log says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NvmeHealth {
    /// Percentage Used: the disk's own estimate of how much of its rated
    /// life it has used. It passes 100 on a disk used past its rating, and
    /// 255 means 255 or more.
    pub used: u8,
    /// The bytes the host wrote over the disk's life; `None` when the count
    /// does not fit in 64 bits, which only a broken disk reports.
    pub written: Option<u64>,
}

/// `STORAGE_DEVICE_DESCRIPTOR`'s fixed part up to `BusType`.
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawDeviceDescriptor {
    /// `Version`, `Size`, the four byte fields and `VendorIdOffset`.
    #[allow(dead_code)]
    head: [u8; 16],
    product_id_offset: U32,
    /// `ProductRevisionOffset`.
    #[allow(dead_code)]
    revision_offset: U32,
    serial_number_offset: U32,
    bus_type: I32,
}

/// `STORAGE_PROTOCOL_DATA_DESCRIPTOR`: `Version` and `Size`, then
/// `STORAGE_PROTOCOL_SPECIFIC_DATA`.
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawProtocolDescriptor {
    version: U32,
    size: U32,
    /// `ProtocolType`, `DataType`, `ProtocolDataRequestValue` and
    /// `ProtocolDataRequestSubValue`.
    #[allow(dead_code)]
    request: [U32; 4],
    protocol_data_offset: U32,
    protocol_data_length: U32,
    /// `FixedProtocolReturnData` and `ProtocolDataRequestSubValue2` to 4.
    #[allow(dead_code)]
    rest: [U32; 4],
}

/// `NVME_HEALTH_INFO_LOG` up to `DataUnitWritten`.
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct RawHealthLog {
    /// `CriticalWarning`, `Temperature`, `AvailableSpare` and
    /// `AvailableSpareThreshold`.
    #[allow(dead_code)]
    head: [u8; 5],
    percentage_used: u8,
    /// `Reserved0` and `DataUnitRead`.
    #[allow(dead_code)]
    middle: [u8; 42],
    data_unit_written: [u8; 16],
}

/// Reads a disk's device descriptor from `bytes`, what
/// `IOCTL_STORAGE_QUERY_PROPERTY` returned for `StorageDeviceProperty`.
///
/// # Errors
///
/// `bytes` is shorter than the descriptor's fixed part.
pub fn device(bytes: &[u8]) -> Result<Device, Malformed> {
    let (descriptor, _) = RawDeviceDescriptor::read_from_prefix(bytes)
        .map_err(|_| Malformed("the device descriptor is too short"))?;
    Ok(Device {
        bus: descriptor.bus_type.get(),
        model: text(bytes, descriptor.product_id_offset.get()),
        serial: text(bytes, descriptor.serial_number_offset.get()),
    })
}

/// The NUL-terminated text at `offset` in `bytes`, without the spaces that
/// pad it. `None` for an offset of 0, which the descriptor uses for no text,
/// for text cut off by the end of `bytes`, and for empty text.
fn text(bytes: &[u8], offset: u32) -> Option<String> {
    let start = usize::try_from(offset).ok().filter(|&start| start != 0)?;
    let rest = bytes.get(start..)?;
    let end = memchr::memchr(0, rest)?;
    let text = String::from_utf8_lossy(&rest[..end]);
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Reads an NVMe disk's health from `bytes`, what
/// `IOCTL_STORAGE_QUERY_PROPERTY` returned for its health log.
///
/// # Errors
///
/// The descriptor is short or of another version, or the offset and length
/// it gives do not hold a whole health log inside `bytes`.
pub fn nvme_health(bytes: &[u8]) -> Result<NvmeHealth, Malformed> {
    let (descriptor, _) = RawProtocolDescriptor::read_from_prefix(bytes)
        .map_err(|_| Malformed("the protocol data descriptor is too short"))?;
    let own_size = size_of::<RawProtocolDescriptor>() as u32;
    if descriptor.version.get() != own_size || descriptor.size.get() != own_size {
        return Err(Malformed("the descriptor is of another version"));
    }
    let offset = descriptor.protocol_data_offset.get() as usize;
    let length = descriptor.protocol_data_length.get() as usize;
    if offset < SPECIFIC_DATA_BYTES || length < HEALTH_LOG_BYTES {
        return Err(Malformed("the health log's offset or length is too small"));
    }
    let log = SPECIFIC_DATA_AT
        .checked_add(offset)
        .and_then(|start| bytes.get(start..start.checked_add(HEALTH_LOG_BYTES)?))
        .ok_or(Malformed("the health log ends past the buffer"))?;
    let (log, _) =
        RawHealthLog::read_from_prefix(log).expect("a whole health log is longer than its head");
    let written = u128::from_le_bytes(log.data_unit_written)
        .checked_mul(DATA_UNIT_BYTES)
        .and_then(|bytes| u64::try_from(bytes).ok());
    Ok(NvmeHealth {
        used: log.percentage_used,
        written,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A protocol data descriptor whose data starts `gap` bytes after
    /// `ProtocolSpecificData`'s end, then a health log with `used` and
    /// `units` written.
    fn health_reply(gap: usize, used: u8, units: u128) -> Vec<u8> {
        let mut bytes = vec![0u8; SPECIFIC_DATA_AT + SPECIFIC_DATA_BYTES + gap];
        bytes[0..4].copy_from_slice(&48u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&48u32.to_le_bytes());
        let offset = (SPECIFIC_DATA_BYTES + gap) as u32;
        bytes[24..28].copy_from_slice(&offset.to_le_bytes());
        bytes[28..32].copy_from_slice(&512u32.to_le_bytes());
        let mut log = [0u8; HEALTH_LOG_BYTES];
        log[5] = used;
        log[48..64].copy_from_slice(&units.to_le_bytes());
        bytes.extend_from_slice(&log);
        bytes
    }

    /// The offset counts from `ProtocolSpecificData`, 8 bytes into the
    /// buffer; a parser that counted from the buffer's start would read the
    /// log 8 bytes early and show another disk's wear. The gap makes such a
    /// misreading land on bytes that are not the values.
    #[test]
    fn a_health_log_gives_the_percentage_used_and_the_bytes_written() {
        let health = nvme_health(&health_reply(16, 7, 75_000_000)).expect("a whole log");
        assert_eq!(
            health,
            NvmeHealth {
                used: 7,
                written: Some(38_400_000_000_000),
            }
        );
    }

    /// A count that overflows 64 bits must not hide the percentage the same
    /// log holds.
    #[test]
    fn written_bytes_past_64_bits_are_unknown_and_the_percentage_stays() {
        let health = nvme_health(&health_reply(0, 120, u128::MAX)).expect("a whole log");
        assert_eq!(
            health,
            NvmeHealth {
                used: 120,
                written: None,
            }
        );
    }

    #[test]
    fn a_health_log_cut_short_is_malformed() {
        let mut bytes = health_reply(0, 7, 1);
        bytes.pop();
        assert!(nvme_health(&bytes).is_err());
    }

    /// A descriptor as an NVMe disk's driver returns it: the product id
    /// padded with spaces, and the vendor id left out.
    #[test]
    fn the_model_is_the_product_id_without_its_padding() {
        let mut bytes = vec![0u8; 40];
        bytes[16..20].copy_from_slice(&40u32.to_le_bytes());
        bytes[28..32].copy_from_slice(&BUS_NVME.to_le_bytes());
        bytes.extend_from_slice(b"KXG50ZNV512G NVMe TOSHIBA 512GB   \0");
        let found = device(&bytes).expect("a whole descriptor");
        assert!(found.is_nvme());
        let model = "KXG50ZNV512G NVMe TOSHIBA 512GB";
        assert_eq!(found.model.as_deref(), Some(model));
        // Text that the end of the buffer cuts off is no model.
        bytes.pop();
        assert_eq!(device(&bytes).expect("a whole descriptor").model, None);
    }

    proptest! {
        /// A driver's buffer can hold anything; reading it returns an error
        /// or values, and never panics. The header of the right version and
        /// any offset and length, up to `u32::MAX`, reach the offset checks,
        /// which random bytes alone would rarely pass.
        #[test]
        fn any_bytes_read_without_a_panic(
            mut bytes in proptest::collection::vec(any::<u8>(), 0..700),
            offset in any::<u32>(),
            length in any::<u32>(),
        ) {
            prop_assert_eq!(device(&bytes).is_ok(), bytes.len() >= 32);
            let _ = nvme_health(&bytes);
            if bytes.len() >= 32 {
                bytes[0..4].copy_from_slice(&48u32.to_le_bytes());
                bytes[4..8].copy_from_slice(&48u32.to_le_bytes());
                bytes[24..28].copy_from_slice(&offset.to_le_bytes());
                bytes[28..32].copy_from_slice(&length.to_le_bytes());
                let _ = nvme_health(&bytes);
            }
        }
    }
}
