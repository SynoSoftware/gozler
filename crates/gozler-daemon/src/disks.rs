//! The service's answer to [`Request::DiskHealth`]: for each physical disk
//! under a fixed volume, its model, the mount points on it, and the wear and
//! the lifetime bytes written that the disk reports itself.
//!
//! Only an NVMe disk's health is read. Microsoft documents no meaning of the
//! request values for an ATA log page through `IOCTL_STORAGE_QUERY_PROPERTY`,
//! and a SATA disk's SMART attributes differ by vendor, so a SATA disk, like
//! a USB or virtual one, is [`Unread::UnsupportedBus`]. The bus comes from
//! the device descriptor, a query that does not read the media.
//!
//! Each disk is read on its own, so a disk that fails gives its reason and
//! every other disk its values. The wear alarm's disk limit reads the disks
//! too (`crate::alarms`). Each caller joins the read that runs, if one does
//! ([`read_shared`]), so a disk that does not answer holds one thread, not
//! one for each request and sample.
//!
//! [`Request::DiskHealth`]: gozler_core::proto::Request::DiskHealth

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use gozler_core::disk::{self, Device, NvmeHealth};
use gozler_core::proto::{Disk, Response, Unread};
use gozler_winfs::{ERROR_ACCESS_DENIED, WinError};

/// The Win32 errors with which a disk or its driver says that it does not
/// answer the query.
const ERROR_INVALID_FUNCTION: u32 = 1;
const ERROR_NOT_SUPPORTED: u32 = 50;

/// The reply to [`Request::DiskHealth`]: every physical disk under a fixed
/// volume, in the order of the disk numbers. It waits on each disk, so it
/// runs on the blocking pool.
///
/// [`Request::DiskHealth`]: gozler_core::proto::Request::DiskHealth
pub(crate) fn health() -> Response {
    match &*read_shared() {
        Ok(disks) => Response::DiskHealth {
            disks: disks.iter().map(|read| read.disk.clone()).collect(),
        },
        Err(e) => crate::server::unsupported(&format!("could not list the volumes: {e}")),
    }
}

/// What a read of every disk gave.
type Read = Result<Vec<DiskRead>, WinError>;

/// The read of every disk that runs now; `None` while none does.
static RUNNING: Mutex<Option<Arc<Shared>>> = Mutex::new(None);

/// One read of every disk, which the callers that join it wait for.
#[derive(Default)]
struct Shared {
    read: Mutex<Option<Arc<Read>>>,
    done: Condvar,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Every disk as [`read_all`] reads it: by a read of its own, or by the
/// read that runs, which it waits for. It waits on each disk, so it runs
/// off the async runtime.
pub(crate) fn read_shared() -> Arc<Read> {
    let (shared, reads) = {
        let mut running = lock(&RUNNING);
        match &*running {
            Some(shared) => (Arc::clone(shared), false),
            None => {
                let shared = Arc::new(Shared::default());
                *running = Some(Arc::clone(&shared));
                (shared, true)
            }
        }
    };
    if reads {
        let read = Arc::new(read_all());
        *lock(&shared.read) = Some(Arc::clone(&read));
        *lock(&RUNNING) = None;
        shared.done.notify_all();
        return read;
    }
    let mut read = lock(&shared.read);
    loop {
        if let Some(read) = &*read {
            return Arc::clone(read);
        }
        read = (shared.done.wait(read)).unwrap_or_else(PoisonError::into_inner);
    }
}

/// A disk as the service read it.
pub(crate) struct DiskRead {
    pub(crate) disk: Disk,
    /// The serial number, which tells the disk from another of its model;
    /// `None` when its descriptor holds none or could not be read.
    pub(crate) serial: Option<String>,
}

/// Every physical disk under a fixed volume, in the order of the disk
/// numbers.
///
/// # Errors
///
/// The volumes could not be listed.
fn read_all() -> Read {
    let volumes = gozler_winfs::enumerate_volumes()?;
    let mut disks: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for volume in volumes.into_iter().filter(|volume| volume.is_fixed) {
        let guid = volume.guid_path.trim_end_matches('\\');
        let path: Vec<u16> = guid.encode_utf16().collect();
        // A volume whose disks cannot be read names no disk.
        let Some(numbers) = gozler_winfs::volume_storage(&path)
            .ok()
            .and_then(|storage| storage.disks)
        else {
            continue;
        };
        for number in numbers {
            let mounts = disks.entry(number).or_default();
            mounts.extend_from_slice(&volume.mounts);
        }
    }
    Ok(disks
        .into_iter()
        .map(|(number, mounts)| read(number, mounts))
        .collect())
}

/// What physical disk `number`, which holds `mounts`, reports.
fn read(number: u32, mut mounts: Vec<String>) -> DiskRead {
    mounts.sort_unstable();
    let (device, health) = match device(number) {
        Ok(device) if device.is_nvme() => (Some(device), nvme_health(number)),
        Ok(device) => (Some(device), Err(Unread::UnsupportedBus)),
        Err(unread) => (None, Err(unread)),
    };
    let (used, written, unread) = match health {
        Ok(health) => (
            Some(health.used),
            health.written,
            health.written.is_none().then_some(Unread::NotReported),
        ),
        Err(unread) => (None, None, Some(unread)),
    };
    let (bus, model, serial) = device.map_or((None, None, None), |device| {
        (Some(device.bus), device.model, device.serial)
    });
    let disk = Disk {
        number,
        model,
        mounts,
        bus,
        used,
        written,
        unread,
    };
    DiskRead { disk, serial }
}

fn device(number: u32) -> Result<Device, Unread> {
    let bytes = gozler_winfs::disk_descriptor(number).map_err(|e| failed(number, &e))?;
    disk::device(&bytes).map_err(|e| {
        tracing::debug!(disk = number, error = %e, "could not read the disk's descriptor");
        Unread::NotReported
    })
}

fn nvme_health(number: u32) -> Result<NvmeHealth, Unread> {
    let bytes = gozler_winfs::nvme_health_log(number).map_err(|e| failed(number, &e))?;
    disk::nvme_health(&bytes).map_err(|e| {
        tracing::debug!(disk = number, error = %e, "could not read the disk's health log");
        Unread::NotReported
    })
}

/// Why `error`, from a query of disk `number`, leaves its health unread.
fn failed(number: u32, error: &WinError) -> Unread {
    tracing::debug!(disk = number, %error, "a disk query failed");
    match error.code {
        ERROR_ACCESS_DENIED => Unread::NoRights,
        ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED => Unread::NotReported,
        code => Unread::Failed { code },
    }
}
