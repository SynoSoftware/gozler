//! One Viewer's file and notification lifetime stay on its reader worker.
//! The mailbox holds only the latest command and one bounded block.

use super::ViewerEvent;
use crate::core::clipboard::{Publication, RETRY_AFTER};
use crate::core::open::Entries;
use crate::core::viewer::find::{Pattern, Query};
use crate::core::viewer::{LINE_BYTES, Mode, found_in_text};
use crate::core::viewer_window::{BLOCK_BYTES, CopyText, FindIn, Found};
use crate::win::clipboard::{ClipboardBlock, CopyAttempt, CopyFailure};
use crate::win::folder::{Register, reason, unregister, verbatim_wide};
use crate::win::locale::{code_page_bytes, code_page_leads, code_page_text};
use crate::win::wake::OpenWaker;
use crate::win::work::OffThread;
use gozler_client::CancelToken;
use gozler_winfs::{Access, Entry, Reparse, WinError, open_entry};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use windows::Win32::Foundation::{E_OUTOFMEMORY, ERROR_INVALID_HANDLE};

/// The bytes Find reads at once, so a search of a file of any size holds
/// one part of it in memory and checks for Cancel between parts.
const FIND_PART: u64 = 1 << 20;

pub(crate) enum Command {
    /// The files among `entries`, or `focused`, a file, when there are
    /// none; which shows first: `focused` when it is one of them.
    Files {
        entries: Entries,
        focused: Option<Vec<u16>>,
    },
    Open(Vec<u16>),
    Read(u64),
    Copy(CopyText, Publication),
    Find(Query, FindIn),
    Cancel,
    Release {
        keep_registration: bool,
    },
}

#[derive(Default)]
struct Pending {
    generation: u64,
    command: Option<Command>,
    answer: Option<(u64, ViewerEvent)>,
    registration: Option<(u64, Register)>,
    closed: bool,
}

#[derive(Default)]
pub(crate) struct Mailbox {
    pending: Mutex<Pending>,
    changed: Condvar,
    notification: Arc<AtomicIsize>,
    active: AtomicBool,
}

impl Mailbox {
    pub(crate) fn post(&self, generation: u64, command: Command) {
        self.replace(generation, Some(command));
    }

    pub(crate) fn invalidate(&self, generation: u64) {
        self.replace(generation, None);
    }

    fn replace(&self, generation: u64, command: Option<Command>) {
        let (replaced, abandoned) = {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            if pending.closed {
                return;
            }
            pending.generation = generation;
            (
                std::mem::replace(&mut pending.command, command),
                pending.registration.take(),
            )
        };
        drop(replaced);
        drop(abandoned);
        self.changed.notify_one();
    }

    pub(crate) fn take(&self, generation: u64) -> Option<ViewerEvent> {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        if pending.closed || pending.answer.as_ref()?.0 != generation {
            return None;
        }
        pending.answer.take().map(|(_, event)| event)
    }

    pub(crate) fn is_registration(&self, notification: isize) -> bool {
        notification != 0 && self.notification.load(Ordering::Acquire) == notification
    }

    pub(crate) fn released(&self) -> bool {
        !self.active.load(Ordering::Acquire)
    }

    fn opening(&self, generation: u64) -> bool {
        let pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        if pending.closed || pending.generation != generation {
            return false;
        }
        // Query invalidation uses this same lock before checking release.
        self.active.store(true, Ordering::Release);
        true
    }

    pub(crate) fn take_registration(&self, generation: u64) -> Option<Register> {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        if pending.closed || pending.registration.as_ref()?.0 != generation {
            return None;
        }
        pending.registration.take().map(|(_, request)| request)
    }

    pub(crate) fn close(&self) {
        let abandoned = {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            pending.closed = true;
            pending.registration.take()
        };
        drop(abandoned);
        self.changed.notify_one();
    }

    /// Has the window register `handle` for its drive's removal into
    /// `notification`, and waits until it did, or until a newer command or
    /// close dropped the request.
    fn register(
        &self,
        generation: u64,
        handle: isize,
        notification: &Arc<AtomicIsize>,
        waker: &OpenWaker,
    ) {
        let (request, acknowledged) = Register::new(handle, Arc::clone(notification));
        {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            if pending.closed || pending.generation != generation {
                return;
            }
            pending.registration = Some((generation, request));
        }
        waker.deliver(|| true);
        // A claimed UI request keeps the file valid even if close arrives meanwhile.
        let _ = acknowledged.recv();
    }

    fn next(&self) -> Option<(u64, Command)> {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if pending.closed {
                return None;
            }
            if let Some(command) = pending.command.take() {
                return Some((pending.generation, command));
            }
            pending = self
                .changed
                .wait(pending)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn current(&self, generation: u64) -> bool {
        let pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        !pending.closed && pending.generation == generation
    }

    fn deliver(&self, generation: u64, event: ViewerEvent, waker: &OpenWaker) {
        let replaced = {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            if pending.closed || pending.generation != generation {
                return;
            }
            pending.answer.replace((generation, event))
        };
        drop(replaced);
        waker.deliver(|| true);
    }
}

pub(crate) struct Session {
    pub(crate) mailbox: Arc<Mailbox>,
    pub(crate) waker: OpenWaker,
}

struct Reading {
    file: Option<Entry>,
    failed: Option<(bool, String)>,
    len: u64,
    mailbox: Arc<Mailbox>,
    /// The clipboard's sequence number after this Viewer's last copy.
    copied: AtomicU32,
}

impl Reading {
    fn release(&mut self, keep_registration: bool) {
        drop(self.file.take());
        self.mailbox.active.store(false, Ordering::Release);
        if !keep_registration {
            self.clear();
        }
    }

    fn clear(&mut self) {
        drop(self.file.take());
        self.mailbox.active.store(false, Ordering::Release);
        self.failed = None;
        let notification = self.mailbox.notification.swap(0, Ordering::AcqRel);
        unregister((notification != 0).then_some(notification));
    }

    /// Opens `path` and reads its first block. The file open before stays
    /// open, with its removal registration, until this one has opened and
    /// registered, so a Reload that is cancelled or replaced leaves the page
    /// readable, and a removable drive stays removable.
    fn open(
        &mut self,
        path: &[u16],
        generation: u64,
        current: impl Fn() -> bool,
        register: impl Fn(&Entry, &Arc<AtomicIsize>),
    ) -> Result<Option<ViewerEvent>, WinError> {
        if !current() || !self.mailbox.opening(generation) {
            return Ok(None);
        }
        let file = open_entry(&verbatim_wide(path), Access::Read, Reparse::Follow)?;
        let len = file.stat()?.size;
        let first = file_bytes(&file, len, 0..BLOCK_BYTES as u64)?;
        if !current() {
            return Ok(None);
        }
        // The window registers the new file into its own slot. A newer
        // command meanwhile drops the request, or finds the registration
        // done there, and the old file and its registration stay.
        let registered = Arc::new(AtomicIsize::new(0));
        register(&file, &registered);
        let registered = registered.swap(0, Ordering::AcqRel);
        if !current() {
            unregister((registered != 0).then_some(registered));
            return Ok(None);
        }
        // `opening` marked a file as held, so a removal query while the old
        // file gives way to the new one is vetoed.
        let old = self.mailbox.notification.swap(registered, Ordering::AcqRel);
        unregister((old != 0).then_some(old));
        self.failed = None;
        self.file = Some(file);
        self.len = len;
        Ok(Some(ViewerEvent::Opened { len, first }))
    }

    fn bytes(&self, index: u64) -> Result<Vec<u8>, WinError> {
        let start = index.saturating_mul(BLOCK_BYTES as u64);
        self.read_range(start..start.saturating_add(BLOCK_BYTES as u64))
    }

    fn read_range(&self, range: Range<u64>) -> Result<Vec<u8>, WinError> {
        let file = self
            .file
            .as_ref()
            .expect("only an open reading requests bytes");
        file_bytes(file, self.len, range)
    }

    /// Looks for `query` from where `at` says, one part of the file at a
    /// time; `None` when a newer command replaced it.
    fn find(
        &self,
        query: &Query,
        at: FindIn,
        current: impl Fn() -> bool,
    ) -> Result<Option<ViewerEvent>, WinError> {
        let encoding = at.detected.encoding;
        let answer = |found| {
            Ok(Some(ViewerEvent::Found {
                encoding,
                looked_for: query.text.clone(),
                found,
            }))
        };
        // Hex bytes match at any byte, the byte order mark included.
        let text_start = if query.hex { 0 } else { at.start };
        let Some(pattern) = Pattern::new(
            query,
            encoding,
            text_start,
            &code_page_bytes,
            &code_page_leads,
        ) else {
            return answer(None);
        };
        let len = at.len.min(self.len);
        let (reach, context) = (pattern.reach(), pattern.back());
        let mut found = None;
        if at.backward {
            let mut end = at.from.min(len);
            while found.is_none() && end > text_start {
                if !current() {
                    return Ok(None);
                }
                let start = end.saturating_sub(FIND_PART).max(text_start);
                let base = start.saturating_sub(context).max(text_start);
                let bytes = self.read_range(base..end.saturating_add(reach).min(len))?;
                found = pattern.last(&bytes, base, start..end, &code_page_text);
                end = start;
            }
        } else {
            let mut start = at.from.max(text_start);
            while found.is_none() && start < len {
                if !current() {
                    return Ok(None);
                }
                let end = start.saturating_add(FIND_PART).min(len);
                let base = start.saturating_sub(context).max(text_start);
                let bytes = self.read_range(base..end.saturating_add(reach).min(len))?;
                found = pattern.first(&bytes, base, start..end, &code_page_text);
                start = end;
            }
        }
        let Some(bytes) = found else {
            return answer(None);
        };
        let text = match at.mode {
            Mode::Text => {
                let data_start = at.detected.data_start;
                let base = bytes
                    .start
                    .saturating_sub(LINE_BYTES as u64)
                    .max(data_start.min(bytes.start));
                let around = self.read_range(base..bytes.end)?;
                let places =
                    found_in_text(encoding, data_start, &around, base, &bytes, &code_page_text);
                Some(places)
            }
            Mode::Hex => None,
        };
        answer(Some(Found { bytes, text }))
    }

    fn read(&self, index: u64) -> Result<ViewerEvent, WinError> {
        self.bytes(index)
            .map(|bytes| ViewerEvent::Block { index, bytes })
    }

    /// Reads the selection's bytes and puts its text on the clipboard,
    /// retrying while another program holds the clipboard as the main
    /// window's Copy does; `None` when a newer command replaced it.
    fn copy(
        &self,
        copy: CopyText,
        mut publication: Publication,
        current: impl Fn() -> bool,
        cancel: &CancelToken,
        worker: &OffThread,
    ) -> Result<Option<ViewerEvent>, WinError> {
        let block_bytes = BLOCK_BYTES as u64;
        let first = copy.bytes.start / block_bytes;
        let base = first * block_bytes;
        let wanted = copy.bytes.end - base;
        let mut bytes = Vec::with_capacity(usize::try_from(wanted).unwrap_or(0));
        for index in first..copy.bytes.end.div_ceil(block_bytes) {
            if !current() {
                return Ok(None);
            }
            let block = self.bytes(index)?;
            let short = block.len() < BLOCK_BYTES;
            bytes.extend_from_slice(&block);
            if short {
                break;
            }
        }
        bytes.truncate(usize::try_from(wanted).unwrap_or(usize::MAX));
        let skip = usize::try_from(copy.bytes.start - base)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let text = copy.text(&bytes[skip..], &code_page_text);
        drop(bytes);
        let Some(mut block) = ClipboardBlock::text(&text) else {
            let error = windows::core::Error::from_hresult(E_OUTOFMEMORY);
            return Ok(Some(ViewerEvent::Copied(Err(CopyFailure::Native(error)))));
        };
        drop(text);
        let copied = loop {
            match block.set(&publication, &self.copied, cancel, worker) {
                CopyAttempt::Published | CopyAttempt::Superseded => break Ok(()),
                CopyAttempt::Busy(again) if publication.retry() => {
                    if !current() {
                        return Ok(None);
                    }
                    std::thread::sleep(RETRY_AFTER);
                    block = again;
                }
                CopyAttempt::Busy(_) => break Err(CopyFailure::Busy),
                CopyAttempt::Failed(error) => break Err(CopyFailure::Native(error)),
            }
        };
        Ok(Some(ViewerEvent::Copied(copied)))
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        self.clear();
    }
}

/// The bytes of `range` of `file` within its length `len`, fewer when the
/// file ends sooner now.
fn file_bytes(file: &Entry, len: u64, range: Range<u64>) -> Result<Vec<u8>, WinError> {
    let start = range.start;
    let want = range.end.min(len).saturating_sub(start) as usize;
    let mut bytes = vec![0; want];
    let mut filled = 0;
    while filled < want {
        match file.read_at(start + filled as u64, &mut bytes[filled..])? {
            0 => break,
            read => filled += read,
        }
    }
    bytes.truncate(filled);
    Ok(bytes)
}

pub(crate) fn run(session: Session, worker: &OffThread, cancel: &CancelToken) {
    let mut reading = Reading {
        file: None,
        failed: None,
        len: 0,
        mailbox: Arc::clone(&session.mailbox),
        copied: AtomicU32::new(0),
    };
    while let Some((generation, command)) = session.mailbox.next() {
        if cancel.is_cancelled() {
            break;
        }
        let current = || !cancel.is_cancelled() && session.mailbox.current(generation);
        if !current() {
            continue;
        }
        let opens = matches!(command, Command::Open(_));
        let result = match command {
            Command::Files { entries, focused } => {
                let mut paths: Vec<Vec<u16>> = entries
                    .resolve()
                    .into_iter()
                    .filter(|entry| !entry.is_folder)
                    .map(|entry| entry.path)
                    .collect();
                // Only folders are selected: the focused file shows.
                if paths.is_empty() {
                    paths.extend(focused.clone());
                }
                let first = focused
                    .and_then(|focused| paths.iter().position(|path| *path == focused))
                    .unwrap_or(0);
                Ok(Some(ViewerEvent::Files { paths, first }))
            }
            Command::Open(path) => {
                reading.open(&path, generation, current, |file, notification| {
                    session.mailbox.register(
                        generation,
                        file.handle_value(),
                        notification,
                        &session.waker,
                    );
                })
            }
            Command::Read(index) if reading.file.is_some() => reading.read(index).map(Some),
            Command::Copy(copy, publication) if reading.file.is_some() => {
                reading.copy(copy, publication, current, cancel, worker)
            }
            Command::Find(query, at) if reading.file.is_some() => reading.find(&query, at, current),
            // Every command that reads answers, so the window never waits for
            // one: with the failure that closed the file, or as a read of no
            // file, which the window's commands do not ask for.
            Command::Read(_) | Command::Copy(..) | Command::Find(..) => {
                let (opened, reason) = reading.failed.clone().unwrap_or_else(|| {
                    let error = WinError {
                        code: ERROR_INVALID_HANDLE.0,
                        context: "ReadFile",
                    };
                    (true, reason(&error))
                });
                Ok(Some(ViewerEvent::Failed { opened, reason }))
            }
            Command::Cancel => continue,
            Command::Release { keep_registration } => {
                // Keep removal notifications through our own veto's failure event.
                reading.release(keep_registration);
                continue;
            }
        };
        if reading.file.is_none() {
            session.mailbox.active.store(false, Ordering::Release);
        }
        if !session.mailbox.current(generation) || cancel.is_cancelled() {
            continue;
        }
        let event = result.unwrap_or_else(|error| {
            // An open that fails can leave the file open before it.
            let opened = !opens && reading.file.is_some();
            let reason = reason(&error);
            // A removal query can race the preceding generation check.
            // Keep its notification until replacement, completion or close.
            drop(reading.file.take());
            session.mailbox.active.store(false, Ordering::Release);
            // Cancel can discard this answer while the document stays visible.
            reading.failed = Some((opened, reason.clone()));
            Some(ViewerEvent::Failed { opened, reason })
        });
        if let Some(event) = event {
            session.mailbox.deliver(generation, event, &session.waker);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_queued_release_does_not_acknowledge_an_open_file() {
        let path = std::env::temp_dir().join(format!("gozler-release-{}.bin", std::process::id()));
        std::fs::write(&path, "contents").expect("a test file");
        let wide: Vec<_> = path
            .to_str()
            .expect("a Unicode temp folder")
            .encode_utf16()
            .collect();
        crate::win::work::spawn(move |_| {
            let mailbox = Arc::new(Mailbox::default());
            let mut reading = Reading {
                file: None,
                failed: None,
                len: 0,
                mailbox: Arc::clone(&mailbox),
                copied: AtomicU32::new(0),
            };
            assert!(
                reading
                    .open(&wide, 0, || true, |_, _| {})
                    .expect("the file opens")
                    .is_some()
            );
            mailbox.post(
                1,
                Command::Release {
                    keep_registration: true,
                },
            );
            assert!(
                !mailbox.released(),
                "posting cancellation cannot consent to removal"
            );
            assert!(
                !mailbox.opening(0),
                "a superseded open cannot race release consent"
            );
            let (_, Command::Release { keep_registration }) =
                mailbox.next().expect("a release is queued")
            else {
                panic!("the queued command releases the file");
            };
            reading.release(keep_registration);
            assert!(mailbox.released());
            assert!(
                reading.file.is_none(),
                "consent follows actual handle ownership release"
            );
        })
        .join()
        .expect("the reader ends");
        std::fs::remove_file(path).expect("the file was released");
    }

    /// Cancel while a file opens again, also while the window registers the
    /// new file, keeps the file open before it, so the page that Reload
    /// keeps can still read it.
    #[test]
    fn a_cancelled_open_keeps_the_open_file() {
        let path = std::env::temp_dir().join(format!("gozler-reopen-{}.bin", std::process::id()));
        std::fs::write(&path, "contents").expect("a test file");
        let wide: Vec<u16> = path
            .to_str()
            .expect("a Unicode temp folder")
            .encode_utf16()
            .collect();
        crate::win::work::spawn(move |_| {
            let mut reading = Reading {
                file: None,
                failed: None,
                len: 0,
                mailbox: Arc::default(),
                copied: AtomicU32::new(0),
            };
            assert!(
                reading
                    .open(&wide, 0, || true, |_, _| {})
                    .expect("the file opens")
                    .is_some()
            );
            // Cancel comes after the open read the first block, or after it
            // registered the new file.
            for checks_passed in [1, 2] {
                let checks = std::cell::Cell::new(0);
                let current = || {
                    checks.set(checks.get() + 1);
                    checks.get() <= checks_passed
                };
                assert!(
                    reading
                        .open(&wide, 0, current, |_, _| {})
                        .expect("the file opens")
                        .is_none()
                );
                assert!(reading.file.is_some(), "the old file stays open");
            }
        })
        .join()
        .expect("the reader ends");
        std::fs::remove_file(path).expect("the file was released");
    }

    /// A later block 0 must not reset the page or leave its awaited block unresolved.
    #[test]
    fn only_the_opening_reads_block_0_as_opened() {
        let path = std::env::temp_dir().join(format!("gozler-viewer-{}.bin", std::process::id()));
        std::fs::write(&path, vec![b'a'; BLOCK_BYTES + 1]).expect("the temp folder is writable");
        let wide: Vec<u16> = path
            .to_str()
            .expect("the temp path is Unicode")
            .encode_utf16()
            .collect();
        crate::win::work::spawn(move |_| {
            let mut reading = Reading {
                file: None,
                failed: None,
                len: 0,
                mailbox: Arc::default(),
                copied: AtomicU32::new(0),
            };
            assert!(
                matches!(reading.open(&wide, 0, || true, |_, _| {}), Ok(Some(ViewerEvent::Opened { len, .. }))
                if len == BLOCK_BYTES as u64 + 1)
            );
            for index in [1, 0] {
                let event = reading.read(index);
                assert!(matches!(event, Ok(ViewerEvent::Block { index: got, .. }) if got == index));
            }
        })
        .join()
        .expect("the reader finishes");
        let _ = std::fs::remove_file(path);
    }
}
