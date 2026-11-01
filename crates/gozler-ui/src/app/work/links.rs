//! Reads whether the reparse points that result rows show are junctions or
//! symbolic links, which the index does not record, so the list shows
//! Windows' link arrow on them (DESIGN.md: Results list).

use crate::core::work::{Replacement, Retirement, Waiting};
use crate::win::folder;
use crate::win::wake::OpenWaker;
use crate::win::work::{OffThread, Task};
use gozler_client::CancelToken;
use std::time::Instant;

/// Each path checked, with whether it is a link.
pub(in crate::app) type Checked = Vec<(String, bool)>;

pub(in crate::app) struct Work {
    task: Task<Vec<String>, Checked>,
}

impl Work {
    pub(super) fn new(waker: OpenWaker) -> Self {
        Self {
            task: Task::new(waker, Replacement::Finish, check)
                .with_size(Vec::len)
                .background(),
        }
    }

    /// Checks `paths` after the paths already asked for.
    pub(in crate::app) fn check(&mut self, paths: Vec<String>) {
        self.task.submit(paths, |mut asked, mut paths| {
            asked.append(&mut paths);
            asked
        });
    }

    pub(in crate::app) fn take(&mut self) -> Option<Checked> {
        self.task.take().map(|(_, checked)| checked)
    }

    pub(in crate::app) fn take_retired(&mut self) -> Retirement {
        self.task.take_retired()
    }

    pub(super) fn tick(&mut self, now: Instant) -> bool {
        self.task
            .tick(now)
            .is_some_and(|waiting| waiting != Waiting::Expired)
    }

    pub(super) fn close(&mut self) -> Retirement {
        self.task.cancel();
        self.task.close_delivery()
    }
}

fn check(paths: Vec<String>, _worker: &OffThread, cancel: &CancelToken) -> Checked {
    paths
        .into_iter()
        .take_while(|_| !cancel.is_cancelled())
        .map(|path| {
            let link = folder::is_link(&path.encode_utf16().collect::<Vec<_>>());
            (path, link)
        })
        .collect()
}
