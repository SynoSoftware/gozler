//! Each Viewer admits one persistent native reader. Reopening replaces its
//! pending command, so a blocked read cannot leave additional readers behind.

use crate::core::clipboard::Publication;
use crate::core::open::Entries;
use crate::core::viewer::find::Query;
use crate::core::viewer_window::{CopyText, FindIn};
use crate::core::work::{Interest, Replacement, Waiting};
use crate::win::Hwnd;
use crate::win::folder::Register;
use crate::win::viewer::ViewerEvent;
use crate::win::viewer::work::{self, Command, Mailbox, Session};
use crate::win::wake::OpenWaker;
use crate::win::work::{Task, spawn};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Copy)]
enum Read {
    Files,
    Open,
    Block,
    Copy,
    Find,
}

pub(in crate::app) struct Reader {
    task: Task<Session, ()>,
    mailbox: Arc<Mailbox>,
    generation: u64,
    /// The command the reading thread runs, and since when, until its
    /// answer, Cancel or Release ends it.
    pending: Option<(Read, Instant)>,
    vetoed: bool,
}

impl Reader {
    pub(in crate::app) fn new(waker: OpenWaker) -> Self {
        let mailbox = Arc::new(Mailbox::default());
        let mut task = Task::new(waker.clone(), Replacement::Cancel, work::run);
        task.replace(Session {
            mailbox: Arc::clone(&mailbox),
            waker,
        });
        Self {
            task,
            mailbox,
            generation: 0,
            pending: None,
            vetoed: false,
        }
    }

    /// Finds the files among `entries`, which `F3` acts on, off the
    /// window's thread, because a panel's selection can hold many entries.
    pub(in crate::app) fn files(&mut self, entries: Entries, focused: Option<Vec<u16>>) {
        self.request(Command::Files { entries, focused });
    }

    /// Opens `path`. The file open before stays open until this one has
    /// opened, so the page of a Reload stays readable when it is cancelled.
    pub(in crate::app) fn open(&mut self, path: Vec<u16>) {
        self.vetoed = false;
        self.request(Command::Open(path));
    }

    /// Reads the block `index`, once no other command runs.
    pub(in crate::app) fn read(&mut self, index: u64) {
        self.request(Command::Read(index));
    }

    /// Reads the bytes of `copy` and puts their text on the clipboard, when
    /// [`Reader::can_copy_or_find`].
    pub(in crate::app) fn copy(&mut self, copy: CopyText, publication: Publication) {
        self.request(Command::Copy(copy, publication));
    }

    /// Looks for `query` in the file from where `at` says, in parts, until
    /// it finds it or reaches the file's end or start, when
    /// [`Reader::can_copy_or_find`].
    pub(in crate::app) fn find(&mut self, query: Query, at: FindIn) {
        self.request(Command::Find(query, at));
    }

    /// Whether Copy or Find can start: no file opens, which they read, and
    /// no Copy or Find runs, which they would end before it answers. A
    /// block read gives way to them, and the page asks for it again. The
    /// window keeps Copy and Find until then.
    pub(in crate::app) fn can_copy_or_find(&self) -> bool {
        matches!(self.pending, None | Some((Read::Block, _)))
    }

    /// Posts `command` to the reading thread, as the command that runs
    /// allows: a block read waits until no command runs, because the page
    /// asks for it again after each answer, and one block at a time keeps a
    /// fast drag of the scroll bar from reading every block it passes; Copy
    /// and Find wait as [`Reader::can_copy_or_find`] says. Any other command
    /// replaces the one that runs.
    fn request(&mut self, command: Command) {
        let (read, interrupt) = match &command {
            Command::Read(_) if self.pending.is_some() => return,
            Command::Copy(..) | Command::Find(..) if !self.can_copy_or_find() => return,
            Command::Files { .. } => (Some(Read::Files), true),
            Command::Open(_) => (Some(Read::Open), true),
            Command::Read(_) => (Some(Read::Block), false),
            Command::Copy(..) => (Some(Read::Copy), false),
            Command::Find(..) => (Some(Read::Find), false),
            Command::Cancel | Command::Release { .. } => (None, true),
        };
        self.pending = read.map(|read| (read, Instant::now()));
        self.command(command, interrupt);
    }

    pub(in crate::app) fn take(&mut self) -> Option<ViewerEvent> {
        let event = self.mailbox.take(self.generation)?;
        self.pending = None;
        Some(event)
    }

    pub(in crate::app) fn take_registration(&mut self) -> Option<Register> {
        let request = self.mailbox.take_registration(self.generation)?;
        if let Some((_, since)) = self.pending.as_mut() {
            *since = Instant::now();
        }
        Some(request)
    }

    pub(in crate::app) fn waiting(&self, now: Instant) -> Option<Waiting> {
        let (_, since) = self.pending?;
        Some(Interest::Awaited.after(now.saturating_duration_since(since)))
    }

    /// Cancels the command that runs; `false` when none runs. A cancelled
    /// open keeps the file open before it.
    pub(in crate::app) fn cancel(&mut self) -> bool {
        if self.pending.is_none() {
            return false;
        }
        self.request(Command::Cancel);
        true
    }

    pub(in crate::app) fn is_registration(&self, notification: isize) -> bool {
        self.mailbox.is_registration(notification)
    }

    pub(in crate::app) fn release(&mut self, keep_registration: bool) {
        self.request(Command::Release { keep_registration });
    }

    pub(in crate::app) fn query_remove(&mut self) -> bool {
        self.release(true);
        let allowed = self.mailbox.released();
        self.vetoed = !allowed;
        allowed
    }

    pub(in crate::app) fn removal_failed(&self) -> bool {
        !self.vetoed
    }

    fn command(&mut self, command: Command, interrupt: bool) {
        self.generation = self.generation.wrapping_add(1);
        if interrupt {
            // Reject the old answer without admitting replacement I/O yet.
            self.mailbox.invalidate(self.generation);
            self.task.interrupt_io();
        }
        self.mailbox.post(self.generation, command);
    }
}

pub(in crate::app) fn register(request: Register, window: Hwnd) {
    let retired = request.complete(window);
    if !retired.is_empty() {
        spawn(move |_| drop(retired));
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.mailbox.close();
        self.task.cancel();
    }
}
