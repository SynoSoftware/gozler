use super::OffThread;
use crate::core::present::Frame;
use crate::core::work::watchdog::{HEARTBEAT, LOG_BYTES, Watchdog, retain_log};
use crate::win::watchdog::Delivery;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM};
use windows::Win32::System::Threading::{OpenThread, THREAD_QUERY_LIMITED_INFORMATION};
use windows::Win32::System::WindowsProgramming::QueryThreadCycleTime;
use windows::Win32::UI::WindowsAndMessaging::{EnumThreadWindows, IsIconic, IsWindowVisible};
use windows::core::BOOL;

/// The work owner retains this worker and drops it at close, without joining.
/// [`crate::win::watchdog::Endpoint`] closes delivery before destroying its HWND.
pub(crate) struct Thread {
    stop: Option<SyncSender<()>>,
    _thread: JoinHandle<()>,
}

/// How many records of `ui-frames.log` can wait for the watchdog's thread to
/// take them, at most one [`HEARTBEAT`] later; the presenter counts more as
/// dropped.
const FRAMES: usize = 256;

impl Thread {
    /// Starts the worker. A debug build, except under test, also writes
    /// `path` and `ui-frames.log` beside it, and gives the presenter of the
    /// calling window thread its channel.
    pub(crate) fn start(delivery: Delivery, path: PathBuf) -> Self {
        let (stop, stopped) = sync_channel(1);
        let logs = Logs::start(path);
        let thread = super::spawn(move |worker| run(delivery, logs, stopped, worker));
        Self {
            stop: Some(stop),
            _thread: thread,
        }
    }
}

impl Drop for Thread {
    fn drop(&mut self) {
        drop(self.stop.take());
    }
}

struct UiThread(HANDLE);

impl UiThread {
    fn open(id: u32, _worker: &OffThread) -> Option<Self> {
        // SAFETY: only querying a thread's cycle counter; this owned handle
        // remains valid even after its original numeric thread ID is reused.
        unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION, false, id) }
            .ok()
            .map(Self)
    }

    fn cycles(&self, _worker: &OffThread) -> Option<u64> {
        let mut cycles = 0;
        // SAFETY: the owned thread handle and writable counter outlive the call.
        unsafe { QueryThreadCycleTime(self.0, &mut cycles) }
            .ok()
            .map(|()| cycles)
    }
}

impl Drop for UiThread {
    fn drop(&mut self) {
        // SAFETY: this worker is the only owner of the query handle.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// How long frame records wait in memory for one append, unless a quarter
/// of [`LOG_BYTES`] gathers first, so a debug build writes the file a few
/// hundred times an hour, below the SSD wear alarm's default limit for one
/// file.
const FRAMES_EVERY: Duration = Duration::from_secs(10);

/// The diagnostic logs of a debug build, which a release build and the
/// tests never open.
struct Logs {
    stalls: Log,
    frames: Log,
    records: Receiver<Frame>,
    ticks: i64,
    /// Frame lines not yet appended, since `appended`.
    waiting: String,
    appended: Instant,
}

impl Logs {
    fn start(path: PathBuf) -> Option<Self> {
        // A test's window keeps its settings file in the temporary folder,
        // where nobody reads these logs and nothing removes them.
        if !cfg!(debug_assertions) || cfg!(test) {
            return None;
        }
        let (frames, records) = sync_channel(FRAMES);
        crate::win::present::log_frames(frames);
        Some(Self {
            frames: Log {
                path: path.with_file_name("ui-frames.log"),
            },
            stalls: Log { path },
            records,
            ticks: crate::win::present::ticks_per_second(),
            waiting: String::new(),
            appended: Instant::now(),
        })
    }

    /// Takes the channel's records each heartbeat, so no more are dropped,
    /// and appends them after [`FRAMES_EVERY`], or at once when `now`.
    fn write_frames(&mut self, now: bool, worker: &OffThread) {
        let ticks = self.ticks;
        self.waiting
            .extend(self.records.try_iter().map(|frame| frame.line(ticks)));
        let due =
            now || self.waiting.len() >= LOG_BYTES / 4 || self.appended.elapsed() >= FRAMES_EVERY;
        if due && !self.waiting.is_empty() {
            let _ = self.frames.append(self.waiting.as_bytes(), worker);
            self.waiting.clear();
            self.appended = Instant::now();
        }
    }
}

fn run(delivery: Delivery, mut logs: Option<Logs>, stopped: Receiver<()>, worker: &OffThread) {
    let thread = UiThread::open(delivery.thread_id, worker);
    let origin = Instant::now();
    let mut watchdog = Watchdog::default();
    loop {
        if let Some(logs) = &mut logs {
            logs.write_frames(false, worker);
        }
        if !matches!(
            stopped.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ) {
            break;
        }
        if any_visible(delivery.thread_id, worker) {
            watchdog.acknowledge(delivery.acknowledged());
            let context = delivery.context();
            let cycles = thread.as_ref().and_then(|thread| thread.cycles(worker));
            let now = origin.elapsed();
            let sequence = watchdog.heartbeat(now, cycles);
            if !delivery.notify(sequence, worker) {
                watchdog.reset();
            } else {
                watchdog.acknowledge(delivery.acknowledged());
                if let (Some(stall), Some(logs)) = (watchdog.overdue(now, cycles, context), &logs) {
                    let _ = logs.stalls.append(stall.line().as_bytes(), worker);
                }
            }
        } else {
            watchdog.reset();
        }
        match stopped.recv_timeout(HEARTBEAT) {
            Err(RecvTimeoutError::Timeout) => {}
            _ => break,
        }
    }
    if let Some(logs) = &mut logs {
        logs.write_frames(true, worker);
    }
}

fn any_visible(thread_id: u32, _worker: &OffThread) -> bool {
    unsafe extern "system" fn visit(window: HWND, data: LPARAM) -> BOOL {
        // SAFETY: synchronous enumeration retains the caller's local; this
        // callback never exposes its borrow outside this call.
        let visible = unsafe { &mut *(data.0 as *mut bool) };
        // SAFETY: only native flags are queried; no Rust pointer or control
        // message crosses the UI-thread boundary.
        *visible = unsafe { IsWindowVisible(window).as_bool() && !IsIconic(window).as_bool() };
        BOOL::from(!*visible)
    }
    let mut visible = false;
    // SAFETY: the local outlives this synchronous enumeration of the captured
    // UI thread's nonchild windows, including native system dialogs and menus.
    let _ = unsafe {
        EnumThreadWindows(
            thread_id,
            Some(visit),
            LPARAM(std::ptr::from_mut(&mut visible) as isize),
        )
    };
    visible
}

struct Log {
    path: PathBuf,
}

impl Log {
    fn append(&self, line: &[u8], _worker: &OffThread) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&self.path)?;
        // All processes append under one lock, which closing this handle
        // releases on both successful and failed updates.
        file.lock()?;
        let length = file.seek(SeekFrom::End(0))?;
        if length + line.len() as u64 <= 2 * LOG_BYTES as u64 {
            return file.write_all(line);
        }
        // The file outgrew twice its bound: trim it to the newest bytes, so
        // the rewrite happens once per `LOG_BYTES` of new records.
        file.seek(SeekFrom::Start(length.saturating_sub(LOG_BYTES as u64)))?;
        let mut bytes = Vec::with_capacity(LOG_BYTES);
        Read::by_ref(&mut file)
            .take(LOG_BYTES as u64)
            .read_to_end(&mut bytes)?;
        retain_log(&mut bytes, line);
        file.rewind()?;
        file.write_all(&bytes)?;
        file.set_len(bytes.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;

    #[test]
    fn independent_watchdogs_preserve_interleaved_diagnostics() {
        let bytes = super::super::spawn(|worker| {
            let path = std::env::temp_dir()
                .join(format!("gozler-watchdog-log-{}.txt", std::process::id()));
            std::fs::write(&path, vec![b'x'; 2 * LOG_BYTES + 7]).unwrap();
            let first = Log { path: path.clone() };
            let second = Log { path: path.clone() };
            first.append(b"window A first\n", worker).unwrap();
            second.append(b"window B\n", worker).unwrap();
            let reader = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ.0)
                .open(&path)
                .unwrap();
            assert!(first.append(b"failed append\n", worker).is_err());
            drop(reader);
            first.append(b"window A last\n", worker).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            std::fs::remove_file(path).unwrap();
            bytes
        })
        .join()
        .unwrap();
        let lines = b"window A first\nwindow B\nwindow A last\n";
        // The first append trims the file to `LOG_BYTES`; the rest append.
        assert_eq!(bytes.len(), LOG_BYTES + lines.len() - 15);
        assert!(bytes.ends_with(lines));
        assert!(
            bytes[..bytes.len() - lines.len()]
                .iter()
                .all(|&byte| byte == b'x')
        );
    }
}
