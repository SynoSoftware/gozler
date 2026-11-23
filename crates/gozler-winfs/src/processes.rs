//! Each running process's write counter, for the wear alarm's limit on what
//! one program writes.

use core::mem::zeroed;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::ProcessStatus::K32EnumProcesses;
use windows_sys::Win32::System::Threading::{
    GetProcessIoCounters, GetProcessTimes, IO_COUNTERS, OpenProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};

use crate::error::{WinError, last_error};

/// The most process ids one listing reads: 256 KiB of ids.
const MOST_PROCESSES: usize = 1 << 16;

/// The longest full path of a program, in UTF-16 units.
const MAX_IMAGE: usize = 32 * 1024;

/// One running process and its write counter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessWrites {
    pub id: u32,
    /// When the process started, a FILETIME.
    pub created: i64,
    /// The full path of its program.
    pub image: String,
    /// The bytes it asked to write since it started, through write calls to
    /// any handle: files, pipes and devices (`WriteTransferCount`).
    pub written: u64,
}

/// The write counter of each process that the caller may query. A process
/// that exits during the listing, or that the caller may not open, is left
/// out; the service, as SYSTEM, opens all but a few.
///
/// # Errors
///
/// The list of process ids could not be read.
pub fn process_writes() -> Result<Vec<ProcessWrites>, WinError> {
    let mut image = vec![0u16; MAX_IMAGE];
    Ok(process_ids()?
        .into_iter()
        .filter_map(|id| read(id, &mut image))
        .collect())
}

fn process_ids() -> Result<Vec<u32>, WinError> {
    let mut ids = vec![0u32; 1024];
    loop {
        let bytes = u32::try_from(size_of_val(ids.as_slice()))
            .expect("MOST_PROCESSES ids fit in a u32 of bytes");
        let mut returned = 0u32;
        // SAFETY: `ids` holds `bytes` bytes, and the call writes at most that
        // many into it and the count into `returned`.
        if unsafe { K32EnumProcesses(ids.as_mut_ptr(), bytes, &mut returned) } == 0 {
            return Err(last_error("K32EnumProcesses"));
        }
        // A full buffer may have cut the list short.
        if returned < bytes || ids.len() >= MOST_PROCESSES {
            ids.truncate(returned as usize / size_of::<u32>());
            return Ok(ids);
        }
        ids.resize(ids.len() * 2, 0);
    }
}

/// The counter of process `id`, with its path read into `image`; `None`
/// when the process cannot be opened or one of its values read.
fn read(id: u32, image: &mut [u16]) -> Option<ProcessWrites> {
    // SAFETY: plain FFI; a null return is checked before the handle is used.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, id) };
    if raw.is_null() {
        return None;
    }
    // SAFETY: `raw` is a process handle that this call opened and owns; the
    // `OwnedHandle` closes it once.
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    let handle = process.as_raw_handle();
    // Each of the three calls accepts a handle open with
    // PROCESS_QUERY_LIMITED_INFORMATION.
    // SAFETY: IO_COUNTERS is plain integers, for which all zero bytes are
    // a valid value.
    let mut counters: IO_COUNTERS = unsafe { zeroed() };
    // SAFETY: `handle` is open, and `counters` is a live local.
    if unsafe { GetProcessIoCounters(handle, &mut counters) } == 0 {
        return None;
    }
    let [mut created, mut exited, mut kernel, mut user] = [FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    }; 4];
    // SAFETY: `handle` is open, and the four times are live locals.
    if unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return None;
    }
    let mut length = u32::try_from(image.len()).expect("MAX_IMAGE fits in a u32");
    // SAFETY: `handle` is open, and `image` holds `length` units, which the
    // call writes at most, with the count it wrote back in `length`.
    let read = unsafe {
        QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, image.as_mut_ptr(), &mut length)
    };
    if read == 0 {
        return None;
    }
    let created = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    Some(ProcessWrites {
        id,
        created: created as i64,
        image: String::from_utf16_lossy(&image[..length as usize]),
        written: counters.WriteTransferCount,
    })
}
