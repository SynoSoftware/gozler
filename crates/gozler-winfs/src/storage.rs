//! What the copy engine reads about a volume's disks, to choose its workers
//! and which operations wait for each other; what the service reads about a
//! physical disk for its health; and the running Windows build.
//!
//! The volume is opened with no access rights, as BetterCopy opens it, so
//! no administrator right is needed for these queries. A disk's health log
//! is asked for on a disk opened for reading, which needs an administrator.

use core::mem::{offset_of, size_of, zeroed};
use core::ptr;

use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
use windows_sys::Win32::Foundation::{GENERIC_READ, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
    OPEN_EXISTING,
};
use windows_sys::Win32::System::Ioctl::{
    DEVICE_SEEK_PENALTY_DESCRIPTOR, DISK_EXTENT, IOCTL_STORAGE_QUERY_PROPERTY, NVMeDataTypeLogPage,
    PropertyStandardQuery, ProtocolTypeNvme, STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_ID,
    STORAGE_PROPERTY_QUERY, STORAGE_PROTOCOL_DATA_DESCRIPTOR, STORAGE_PROTOCOL_SPECIFIC_DATA,
    StorageDeviceProperty, StorageDeviceProtocolSpecificProperty, StorageDeviceSeekPenaltyProperty,
};
use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

use crate::error::{WinError, last_error};
use crate::file_id::FileHandle;
use crate::ioctl::{ioctl_in_bytes, ioctl_in_place, ioctl_out};

/// What a volume's storage says about itself. Each field is `None` when the
/// storage does not answer its query, as a virtual or remote disk may not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeStorage {
    /// Whether reading at another place costs a seek, as on a hard disk.
    pub seek_penalty: Option<bool>,
    pub device: Option<StorageDevice>,
    /// The physical disks the volume lies on; a volume can span disks.
    pub disks: Option<Vec<u32>>,
}

/// From `STORAGE_DEVICE_DESCRIPTOR`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageDevice {
    /// `STORAGE_BUS_TYPE`: 7 is USB.
    pub bus_type: i32,
    /// Whether the device takes several commands at once. A USB device
    /// without it is a bulk-only device, which serves one file at a time.
    pub command_queueing: bool,
}

/// The most disks a volume's answer may name; a volume on more is taken as
/// one whose disks cannot be read.
const MAX_DISKS: usize = 32;

#[repr(C)]
struct DiskExtents {
    count: u32,
    extents: [DISK_EXTENT; MAX_DISKS],
}

/// Reads the storage under `volume`, a `\\?\Volume{GUID}` path without a
/// trailing backslash. Fails only when the volume cannot be opened.
pub fn volume_storage(volume: &[u16]) -> Result<VolumeStorage, WinError> {
    let path: Vec<u16> = volume.iter().copied().chain(Some(0)).collect();
    let handle = open(&path, 0, "CreateFileW (volume_storage)")?;

    let mut penalty = [0u8; size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>()];
    let seek_penalty = ioctl_in_bytes(
        handle.as_handle(),
        IOCTL_STORAGE_QUERY_PROPERTY,
        &query(StorageDeviceSeekPenaltyProperty),
        &mut penalty,
    )
    .ok()
    .filter(|&bytes| bytes as usize >= size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>())
    .map(|_| penalty[offset_of!(DEVICE_SEEK_PENALTY_DESCRIPTOR, IncursSeekPenalty)] != 0);

    // The descriptor's fixed part; the strings after it are not needed.
    let mut device = [0u8; size_of::<STORAGE_DEVICE_DESCRIPTOR>()];
    let device = ioctl_in_bytes(
        handle.as_handle(),
        IOCTL_STORAGE_QUERY_PROPERTY,
        &query(StorageDeviceProperty),
        &mut device,
    )
    .ok()
    .filter(|&bytes| bytes as usize >= size_of::<STORAGE_DEVICE_DESCRIPTOR>())
    .map(|_| {
        let bus = offset_of!(STORAGE_DEVICE_DESCRIPTOR, BusType);
        StorageDevice {
            bus_type: i32::from_le_bytes(device[bus..bus + 4].try_into().expect("4 bytes")),
            command_queueing: device[offset_of!(STORAGE_DEVICE_DESCRIPTOR, CommandQueueing)] != 0,
        }
    });

    // SAFETY: `DiskExtents` is plain integers, for which zero is valid.
    let mut extents: DiskExtents = unsafe { zeroed() };
    let disks = ioctl_out(
        handle.as_handle(),
        IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
        &mut extents,
    )
    .ok()
    .filter(|_| extents.count as usize <= MAX_DISKS)
    .map(|_| {
        let mut disks: Vec<u32> = extents.extents[..extents.count as usize]
            .iter()
            .map(|extent| extent.DiskNumber)
            .collect();
        disks.sort_unstable();
        disks.dedup();
        disks
    });

    Ok(VolumeStorage {
        seek_penalty,
        device,
        disks,
    })
}

/// Opens the device at `path`, NUL-terminated, with `access`; `context`
/// names the call in the error.
fn open(path: &[u16], access: u32, context: &'static str) -> Result<FileHandle, WinError> {
    // SAFETY: `path` is NUL-terminated; no security attributes and no
    // template are supplied.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(last_error(context));
    }
    Ok(FileHandle(handle))
}

fn query(property: STORAGE_PROPERTY_ID) -> STORAGE_PROPERTY_QUERY {
    STORAGE_PROPERTY_QUERY {
        PropertyId: property,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    }
}

/// `\\.\PhysicalDrive{disk}`, NUL-terminated.
fn disk_path(disk: u32) -> Vec<u16> {
    format!(r"\\.\PhysicalDrive{disk}")
        .encode_utf16()
        .chain(Some(0))
        .collect()
}

/// The most bytes of a device descriptor that [`disk_descriptor`] reads:
/// the fixed part and the strings after it, which name a model in tens of
/// bytes.
const DESCRIPTOR_BYTES: usize = 1024;

/// The device descriptor of physical disk `disk`, as the raw bytes
/// `IOCTL_STORAGE_QUERY_PROPERTY` returns for `StorageDeviceProperty`, which
/// `gozler_core::disk::device` reads. The disk is opened with no access
/// rights, so the query needs no administrator right and does not touch the
/// media.
///
/// # Errors
///
/// The disk could not be opened, or the query failed.
pub fn disk_descriptor(disk: u32) -> Result<Vec<u8>, WinError> {
    let handle = open(&disk_path(disk), 0, "CreateFileW (disk_descriptor)")?;
    let mut bytes = vec![0u8; DESCRIPTOR_BYTES];
    let returned = ioctl_in_bytes(
        handle.as_handle(),
        IOCTL_STORAGE_QUERY_PROPERTY,
        &query(StorageDeviceProperty),
        &mut bytes,
    )
    .map_err(|code| WinError {
        code,
        context: "IOCTL_STORAGE_QUERY_PROPERTY (disk_descriptor)",
    })?;
    bytes.truncate(returned as usize);
    Ok(bytes)
}

/// `NVME_LOG_PAGE_HEALTH_INFO` in `nvme.h`: the SMART / Health Information
/// log page.
const NVME_HEALTH_LOG_PAGE: u32 = 2;

/// The bytes of `NVME_HEALTH_INFO_LOG`.
const NVME_HEALTH_LOG_BYTES: u32 = 512;

// `gozler_core::disk::nvme_health` reads the reply with this layout.
const _: () = assert!(size_of::<STORAGE_PROTOCOL_DATA_DESCRIPTOR>() == 48);
const _: () = assert!(size_of::<STORAGE_PROTOCOL_SPECIFIC_DATA>() == 40);

/// The SMART / Health Information log of physical disk `disk`, an NVMe
/// disk, as the raw bytes `IOCTL_STORAGE_QUERY_PROPERTY` returns:
/// a `STORAGE_PROTOCOL_DATA_DESCRIPTOR`, then the log, which
/// `gozler_core::disk::nvme_health` reads. The query is the one in
/// Microsoft's "Working with NVMe drives". The disk is opened for reading,
/// which needs administrator rights.
///
/// # Errors
///
/// The disk could not be opened, as without administrator rights
/// (`ERROR_ACCESS_DENIED`), or the disk or its driver refused the query.
pub fn nvme_health_log(disk: u32) -> Result<Vec<u8>, WinError> {
    type Query = STORAGE_PROPERTY_QUERY;
    type Data = STORAGE_PROTOCOL_SPECIFIC_DATA;
    let path = disk_path(disk);
    let handle = open(&path, GENERIC_READ, "CreateFileW (nvme_health_log)")?;
    // The query's `Data` starts at its `AdditionalParameters`; the disk's
    // reply overwrites the same buffer.
    let data = offset_of!(Query, AdditionalParameters);
    let fields = [
        (
            offset_of!(Query, PropertyId),
            StorageDeviceProtocolSpecificProperty as u32,
        ),
        (offset_of!(Query, QueryType), PropertyStandardQuery as u32),
        (
            data + offset_of!(Data, ProtocolType),
            ProtocolTypeNvme as u32,
        ),
        (
            data + offset_of!(Data, DataType),
            NVMeDataTypeLogPage as u32,
        ),
        (
            data + offset_of!(Data, ProtocolDataRequestValue),
            NVME_HEALTH_LOG_PAGE,
        ),
        (
            data + offset_of!(Data, ProtocolDataOffset),
            size_of::<Data>() as u32,
        ),
        (
            data + offset_of!(Data, ProtocolDataLength),
            NVME_HEALTH_LOG_BYTES,
        ),
    ];
    let mut bytes = vec![0u8; data + size_of::<Data>() + NVME_HEALTH_LOG_BYTES as usize];
    for (at, value) in fields {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    let returned = ioctl_in_place(handle.as_handle(), IOCTL_STORAGE_QUERY_PROPERTY, &mut bytes)
        .map_err(|code| WinError {
            code,
            context: "IOCTL_STORAGE_QUERY_PROPERTY (nvme_health_log)",
        })?;
    bytes.truncate(returned as usize);
    Ok(bytes)
}

/// The running Windows build, such as 26200. `GetVersionEx` would report
/// the build that the executable's manifest declares instead.
pub fn windows_build() -> u32 {
    // SAFETY: `info` is a live struct whose size field is set, as the call
    // requires; RtlGetVersion always succeeds.
    unsafe {
        let mut info: OSVERSIONINFOW = zeroed();
        info.dwOSVersionInfoSize = size_of::<OSVERSIONINFOW>() as u32;
        RtlGetVersion(&mut info);
        info.dwBuildNumber
    }
}
