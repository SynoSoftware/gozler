use std::time::{Duration, Instant};

use super::captions::JobCaption;
use super::layout::Px;
use super::status_transfer::Caption;
use super::work::{BUSY, SLOW};

pub(crate) fn work_widths(room: Px, wanted: &[Px], minimum: &[Px], gap: Px) -> Vec<Px> {
    let available = (room.0 - gap.0 * wanted.len().saturating_sub(1) as i32).max(0);
    let minimum: Vec<i32> = wanted
        .iter()
        .zip(minimum)
        .map(|(wanted, minimum)| wanted.0.min(minimum.0))
        .collect();
    let total_minimum: i32 = minimum.iter().sum();
    let extra: i32 = wanted
        .iter()
        .zip(&minimum)
        .map(|(wanted, minimum)| wanted.0 - minimum)
        .sum();
    let spare = (available - total_minimum).max(0).min(extra);
    wanted
        .iter()
        .zip(minimum)
        .map(|(wanted, minimum)| {
            Px(if total_minimum > available {
                (i64::from(minimum) * i64::from(available) / i64::from(total_minimum)) as i32
            } else {
                minimum
                    + (i64::from(wanted.0 - minimum) * i64::from(spare) / i64::from(extra.max(1)))
                        as i32
            })
        })
        .collect()
}

const NOTICE_DURATION: Duration = Duration::from_secs(5);

/// Whether a job's entry shows in the middle group. A job that ends within
/// [`BUSY`] of its start shows only its result, and Cancel joins its
/// progress at [`SLOW`], so nothing flashes.
#[derive(Clone, Copy, Default)]
pub(crate) enum Notice {
    /// The job started and no refresh has looked at it yet.
    #[default]
    Started,
    /// The job runs since this moment, and shows nothing yet.
    Hidden(Instant),
    /// The job runs since this moment, and shows its progress without
    /// Cancel.
    Shown(Instant),
    Persistent,
    Until(Instant),
    Expired,
}

impl Notice {
    pub(crate) fn update(&mut self, running: bool, transient: bool, now: Instant) {
        if running {
            *self = match *self {
                Self::Started => Self::Hidden(now),
                Self::Hidden(since) | Self::Shown(since) => {
                    match now.saturating_duration_since(since) {
                        ran if ran < BUSY => Self::Hidden(since),
                        ran if ran < SLOW => Self::Shown(since),
                        _ => Self::Persistent,
                    }
                }
                Self::Persistent | Self::Until(_) | Self::Expired => Self::Persistent,
            };
            return;
        }
        if !transient {
            *self = Self::Persistent;
            return;
        }
        match *self {
            Self::Started | Self::Hidden(_) | Self::Shown(_) | Self::Persistent => {
                *self = Self::Until(now + NOTICE_DURATION);
            }
            Self::Until(until) if now >= until => *self = Self::Expired,
            Self::Until(_) | Self::Expired => {}
        }
    }

    pub(crate) fn visible(self) -> bool {
        !matches!(self, Self::Started | Self::Hidden(_) | Self::Expired)
    }

    /// Whether a job that started shows nothing of its own yet, so its
    /// entry keeps what it showed before ([`Kept`]).
    pub(crate) fn waits(self) -> bool {
        matches!(self, Self::Started | Self::Hidden(_))
    }

    /// Whether the entry offers Cancel, which a running job gets at
    /// [`SLOW`].
    pub(crate) fn cancel_shown(self) -> bool {
        matches!(self, Self::Persistent)
    }

    pub(crate) fn remaining(self, now: Instant) -> Option<Duration> {
        match self {
            Self::Hidden(since) => Some((since + BUSY).saturating_duration_since(now)),
            Self::Shown(since) => Some((since + SLOW).saturating_duration_since(now)),
            Self::Until(until) => Some(until.saturating_duration_since(now)),
            Self::Started | Self::Persistent | Self::Expired => None,
        }
    }
}

/// What a job's entry showed, which it keeps while a newer job of that kind
/// [`Notice::waits`], so the start of a job never hides the result before
/// it, as [`TransferNotice`] keeps the summary.
#[derive(Default)]
pub(crate) struct Kept(Option<JobCaption>);

impl Kept {
    /// What the entry shows of `caption`, the job's own; `waits` keeps the
    /// earlier one, without its Cancel, which the new job offers at
    /// [`SLOW`].
    pub(crate) fn shown(&mut self, caption: Option<JobCaption>, waits: bool) -> Option<JobCaption> {
        if waits {
            return self.0.clone().map(|kept| JobCaption {
                cancel: false,
                ..kept
            });
        }
        self.0.clone_from(&caption);
        caption
    }
}

/// What the transfer summary shows (DESIGN.md: Status bar). A transfer that
/// starts keeps the summary of the transfers that ended before it, or none,
/// until it has run [`BUSY`]; its Pause and Cancel show from [`SLOW`]. So a
/// fast copy shows only its result.
#[derive(Default)]
pub(crate) struct TransferNotice {
    notice: Notice,
    running: bool,
    /// The summary of the transfers that ended last, as it was built, which
    /// shows while the next transfer is hidden.
    settled: Option<Caption>,
}

impl TransferNotice {
    /// Follows `caption`, the summary that the latest presentation holds.
    pub(crate) fn update(&mut self, caption: Option<&Caption>, now: Instant) {
        let running = caption.is_some_and(Caption::runs);
        if running && !self.running {
            self.notice = Notice::Started;
        }
        self.running = running;
        self.notice.update(running, false, now);
        if !running {
            self.settled = caption.cloned();
        }
    }

    pub(crate) fn remaining(&self, now: Instant) -> Option<Duration> {
        self.notice.remaining(now)
    }

    /// Whether the summary shows a transfer that runs.
    pub(crate) fn shows_running(&self) -> bool {
        self.running && self.notice.visible()
    }

    /// What the status bar shows for `caption`.
    pub(crate) fn shown(&self, mut caption: Caption) -> Option<Caption> {
        if !caption.runs() {
            return Some(caption);
        }
        if !self.notice.visible() {
            return self.settled.clone();
        }
        if !self.notice.cancel_shown() {
            caption.pause = None;
            caption.cancel = None;
        }
        Some(caption)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_work_shares_room_without_losing_its_controls() {
        assert_eq!(
            work_widths(Px(350), &[Px(300), Px(300)], &[Px(150), Px(150)], Px(6)),
            [Px(172), Px(172)]
        );
        assert_eq!(
            work_widths(Px(80), &[Px(300), Px(300)], &[Px(150), Px(150)], Px(6)),
            [Px(37), Px(37)]
        );
    }

    #[test]
    fn repeated_updates_do_not_keep_a_completion_notice_alive() {
        let start = Instant::now();
        let mut notice = Notice::default();
        notice.update(false, true, start);
        notice.update(false, true, start + Duration::from_secs(4));
        assert!(notice.visible());
        assert_eq!(
            notice.remaining(start + Duration::from_secs(4)),
            Some(Duration::from_secs(1))
        );
        notice.update(false, true, start + NOTICE_DURATION);
        assert!(!notice.visible());
        notice.update(false, true, start + Duration::from_secs(30));
        assert!(!notice.visible());
        notice.update(false, false, start + Duration::from_secs(31));
        assert!(notice.visible());
        assert_eq!(notice.remaining(start + Duration::from_secs(31)), None);
    }

    /// A job's progress or Cancel shown for the milliseconds a fast job
    /// takes, and then replaced by its result, flashes (AGENTS.md: The
    /// window).
    #[test]
    fn a_job_shows_its_progress_and_cancel_only_once_it_runs_long() {
        let start = Instant::now();
        let mut fast = Notice::default();
        fast.update(true, false, start);
        assert!(!fast.visible());
        assert_eq!(fast.remaining(start), Some(BUSY));
        fast.update(false, true, start + BUSY / 2);
        assert!(fast.visible());

        let mut slow = Notice::default();
        slow.update(true, false, start);
        slow.update(true, false, start + BUSY);
        assert!(slow.visible() && !slow.cancel_shown());
        assert_eq!(slow.remaining(start + BUSY), Some(SLOW - BUSY));
        slow.update(true, false, start + SLOW);
        assert!(slow.visible() && slow.cancel_shown());
        assert_eq!(slow.remaining(start + SLOW), None);
    }

    /// A job that starts keeps the result its entry shows until it shows
    /// its own, so its start never hides that result for a moment.
    #[test]
    fn a_new_job_keeps_the_result_before_it_until_it_shows() {
        let result = JobCaption {
            text: "Exported".to_owned(),
            ..JobCaption::default()
        };
        let mut kept = Kept::default();
        assert_eq!(
            kept.shown(Some(result.clone()), false),
            Some(result.clone())
        );
        assert_eq!(kept.shown(None, true), Some(result));
        let running = JobCaption {
            text: "Exporting".to_owned(),
            cancel: true,
            ..JobCaption::default()
        };
        assert_eq!(kept.shown(Some(running.clone()), false), Some(running));
    }

    /// The transfer summary follows the job notices, so a fast copy shows
    /// only its result.
    #[test]
    fn a_transfer_keeps_the_last_summary_until_it_runs_long() {
        use super::super::file_operations::QueueAction;
        use super::super::glyph::Glyph;
        use super::super::status_transfer::Action;
        let action = |command| Action {
            text: String::new(),
            glyph: Glyph::Copy,
            command,
        };
        let caption = |id, running: bool| Caption {
            id: gozler_fileops::OperationId::for_tests(id),
            name: String::new(),
            status: String::new(),
            activity: String::new(),
            glyph: None,
            progress: Default::default(),
            paused: false,
            percent: String::new(),
            byte_rate: String::new(),
            file_rate: String::new(),
            pause: running.then(|| action(QueueAction::Pause)),
            cancel: running.then(|| action(QueueAction::Cancel)),
            report: None,
            summary_tip: String::new(),
        };
        let start = Instant::now();
        let mut notice = TransferNotice::default();
        notice.update(Some(&caption(1, false)), start);
        notice.update(Some(&caption(2, true)), start);
        assert_eq!(notice.shown(caption(2, true)), Some(caption(1, false)));

        notice.update(Some(&caption(2, true)), start + BUSY);
        assert_eq!(
            notice.shown(caption(2, true)),
            Some(Caption {
                pause: None,
                cancel: None,
                ..caption(2, true)
            })
        );

        notice.update(Some(&caption(2, true)), start + SLOW);
        assert_eq!(notice.shown(caption(2, true)), Some(caption(2, true)));
    }
}
