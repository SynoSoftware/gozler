//! Looks up the icon of each extension that rows show, in the shell's
//! apartment, because a file type's icon can come from any program's file or
//! icon handler (DESIGN.md: Results list).

use crate::core::work::{Replacement, Retirement, Waiting};
use crate::win::gdi::ImageList;
use crate::win::shell::Session;
use crate::win::wake::OpenWaker;
use crate::win::work::{OffThread, Task};
use gozler_client::CancelToken;
use std::time::Instant;

/// Each extension looked up, with its index in the system image lists, or
/// `None` when Windows did not give one.
pub(in crate::app) type Found = Vec<(String, Option<i32>)>;

pub(in crate::app) struct Work {
    task: Task<Vec<String>, Found>,
}

impl Work {
    pub(super) fn new(waker: OpenWaker) -> Self {
        Self {
            task: Task::shell(waker, Replacement::Finish, look_up)
                .with_size(Vec::len)
                .background(),
        }
    }

    /// Looks up `extensions` after the extensions already asked for.
    pub(in crate::app) fn look_up(&mut self, extensions: Vec<String>) {
        self.task.submit(extensions, |mut asked, mut extensions| {
            asked.append(&mut extensions);
            asked
        });
    }

    pub(in crate::app) fn take(&mut self) -> Option<Found> {
        self.task.take().map(|(_, found)| found)
    }

    pub(in crate::app) fn take_retired(&mut self) -> Retirement {
        self.task.take_retired()
    }

    /// How the lookups wait; [`Waiting::Expired`] once, when the work gives
    /// up on them, and no answer comes for the extensions asked so far.
    pub(super) fn tick(&mut self, now: Instant) -> Option<Waiting> {
        self.task.tick(now)
    }

    pub(super) fn close(&mut self) -> Retirement {
        self.task.cancel();
        self.task.close_delivery()
    }
}

fn look_up(
    extensions: Vec<String>,
    _session: &Session,
    worker: &OffThread,
    cancel: &CancelToken,
) -> Found {
    extensions
        .into_iter()
        .take_while(|_| !cancel.is_cancelled())
        .map(|extension| {
            let index = ImageList::system_index(&format!("file.{extension}"), worker);
            (extension, index)
        })
        .collect()
}
