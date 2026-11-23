//! The held wear request: while a warning of Settings > SSD wear is on, the
//! first window holds one `Wear` request, which the service answers when a
//! folder is copied into itself or something crosses a limit of the wear
//! alarm.

use crate::core::work::Replacement;
use crate::win::wake::OpenWaker;
use crate::win::work::{OffThread, Task};
use gozler_client::{CancelToken, Client, ClientError};
use gozler_core::proto::{PIPE_NAME, ProtoError, RepeatsAfter, WearLimits, WearReply, WearRequest};

/// Why a wear request ended without a reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum Stop {
    /// The service did not answer or stopped; the window tries again later.
    Failed,
    /// The service is older than the window and cannot read the request.
    Refused,
}

pub(in crate::app) struct Watch {
    /// The request that the service holds; `None` while the watch is off.
    task: Option<Task<WearRequest, Result<WearReply, Stop>>>,
    /// What the window asks for, with what it has seen of the service's
    /// events in `after`.
    request: WearRequest,
    waker: OpenWaker,
}

impl Watch {
    pub(super) fn new(waker: OpenWaker) -> Self {
        Self {
            task: None,
            request: WearRequest {
                after: RepeatsAfter { run: 0, number: 0 },
                copies: u32::MAX,
                limits: WearLimits::default(),
            },
            waker,
        }
    }

    /// Starts the watch from now, for folders copied into themselves `copies`
    /// times and for `limits`: run 0 asks the service for its run id and
    /// highest number only, so nothing from before warns. A request that
    /// still runs is dropped, never waited for, because a cancel that
    /// arrives just before its read starts leaves that read blocked until
    /// the service sends something; the new request has its own connection
    /// and its own reply channel, so the old one's reply never arrives.
    pub(in crate::app) fn start(&mut self, copies: u32, limits: WearLimits) {
        self.request = WearRequest {
            after: RepeatsAfter { run: 0, number: 0 },
            copies,
            limits,
        };
        let mut task = Task::new(self.waker.clone(), Replacement::Cancel, wait).held();
        task.replace(self.request);
        self.task = Some(task);
    }

    /// Ends the watch; the request is cancelled and not waited for.
    pub(in crate::app) fn stop(&mut self) {
        self.task = None;
    }

    /// Sends the request again after it failed, with the events the window
    /// has seen. Before the first reply that is run 0, which warns about
    /// nothing from earlier, so a copy made while the first request failed
    /// does not warn.
    pub(in crate::app) fn retry(&mut self) {
        if let Some(task) = &mut self.task
            && !task.is_running()
        {
            task.replace(self.request);
        }
    }

    /// The answer, once it arrives. After a reply, the next request is
    /// already sent, so an event made meanwhile answers it at once.
    pub(in crate::app) fn take(&mut self) -> Option<Result<WearReply, Stop>> {
        let task = self.task.as_mut()?;
        let (_, answer) = task.take()?;
        if let Ok(reply) = &answer {
            self.request.after = RepeatsAfter {
                run: reply.run,
                number: reply.highest,
            };
            task.replace(self.request);
        }
        Some(answer)
    }
}

fn wait(
    request: WearRequest,
    _worker: &OffThread,
    cancel: &CancelToken,
) -> Result<WearReply, Stop> {
    Client::connect(PIPE_NAME, true, cancel.clone())
        .and_then(|mut client| client.wear(request))
        .map_err(|error| match error {
            ClientError::TooOld
            | ClientError::Refused {
                code: ProtoError::UnknownRequest,
                ..
            } => Stop::Refused,
            _ => Stop::Failed,
        })
}
