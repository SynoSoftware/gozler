//! The service's side of the wear watch ([`Request::Wear`]): the run id, the
//! sequence counter that numbers every event of every volume, and the wait of
//! a held request.
//!
//! A window holds one wear request, and the service answers it when it has
//! something newer than the request's `after`, so neither side runs a timer
//! while nothing happens. A held request waits as an async task on the pipe
//! server, not on the blocking pool, where it would park a pool thread for
//! each window. It registers for [`wake`] before it reads the lists, so an
//! event made between its read and its wait still wakes it.
//!
//! [`Request::Wear`]: gozler_core::proto::Request::Wear

use std::os::windows::io::AsHandle;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context;
use gozler_core::proto::{
    MAX_SERVER_FRAME, RepeatsAfter, Response, WearReply, WearRequest, encode_response_frame,
};
use gozler_core::query::Viewer;
use gozler_winfs::WinError;
use tokio::io::AsyncReadExt;
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::sync::Notify;

use crate::access::{self, Access, Visibility};
use crate::volume_state::{VolumeRegistry, VolumeSet};

/// The most path bytes one reply holds. It is half of [`MAX_SERVER_FRAME`],
/// which leaves room for the escaped backslashes and the other fields, so a
/// reply stays a frame the client reads.
const REPLY_PATH_BYTES: usize = MAX_SERVER_FRAME as usize / 2;

/// The service's sequence counter, at 0 when the service starts: each event
/// that a volume's list makes or updates takes the next number, so one
/// number orders the events of every volume.
pub(crate) static NUMBERS: AtomicU64 = AtomicU64::new(0);

/// Wakes the held wear requests.
static WAKE: Notify = Notify::const_new();

/// The id of this run of the service ([`run_id`]).
static RUN: OnceLock<u64> = OnceLock::new();

/// Wakes every held wear request, so each reads the lists again. The tail
/// calls it after a batch that made or updated an event, from its own
/// thread.
pub(crate) fn wake() {
    WAKE.notify_waiters();
}

/// The id of this run of the service: 64 random bits that are not 0, made
/// at the first wear request. The events and the counter are not saved, so
/// the id is what tells a client that the numbers started again.
///
/// # Errors
///
/// The random bits could not be read; nothing is kept, so a later request
/// tries again.
fn run_id() -> Result<u64, WinError> {
    if let Some(&run) = RUN.get() {
        return Ok(run);
    }
    let run = loop {
        let bits = gozler_winfs::random_u128()? as u64;
        if bits != 0 {
            break bits;
        }
    };
    // Two first requests at once keep the same id: the one set first.
    Ok(*RUN.get_or_init(|| run))
}

/// Which events a reply to a request holds, from the request's `after`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wanted {
    /// None, answered at once: the client's first request, which learns the
    /// run id and the highest number and warns about nothing older.
    Nothing,
    /// The events numbered above this; the request waits while there is
    /// none.
    Above(u64),
    /// Every event, answered at once: the service started
    /// again after the client's last reply, so each is newer than what the
    /// client has seen.
    Every,
}

fn wanted(after: RepeatsAfter, run: u64) -> Wanted {
    match after.run {
        0 => Wanted::Nothing,
        seen if seen == run => Wanted::Above(after.number),
        _ => Wanted::Every,
    }
}

/// The reply to `request` from `volumes` as `viewer` sees them, in this run
/// `run`; `None` while the request waits.
fn answer(
    volumes: &VolumeSet,
    request: WearRequest,
    viewer: Viewer<'_>,
    run: u64,
) -> Option<WearReply> {
    // Read before any list: an event takes its number inside its volume's
    // write hold, so every event up to this number is in a list by the time
    // a read hold reads it.
    let mut highest = NUMBERS.load(Ordering::Acquire);
    let wanted = wanted(request.after, run);
    let above = match wanted {
        Wanted::Nothing => None,
        Wanted::Above(number) => Some(number),
        Wanted::Every => Some(0),
    };
    let may_list = match viewer {
        Viewer::Everything => None,
        Viewer::Filtered(may_list) => Some(may_list),
    };
    let mut events = Vec::new();
    let mut dropped = 0;
    for volume in volumes.iter() {
        let read = volume.read();
        let index = read.index();
        let list = index.repeats();
        dropped += list.dropped();
        let reaches = |event: &gozler_core::index::repeats::RepeatEvent| {
            above.is_some_and(|above| event.number > above)
                && event.number <= highest
                && event.copies >= request.copies
        };
        events.extend(list.reported(index, volume.mount_prefix(), reaches, may_list));
    }
    if matches!(wanted, Wanted::Above(_)) && events.is_empty() {
        return None;
    }
    events.sort_unstable_by_key(|event| event.number);
    // A reply over the byte budget ends at the last event that fits, and
    // `highest` names it, so the client's next request asks for the rest.
    let mut bytes = 0;
    let fits = events
        .iter()
        .take_while(|event| {
            bytes += event.top.len() + event.deepest.as_deref().map_or(0, str::len);
            bytes <= REPLY_PATH_BYTES
        })
        .count();
    if fits < events.len() {
        events.truncate(fits);
        highest = events.last().map_or(highest, |event| event.number);
    }
    Some(WearReply {
        run,
        highest,
        events,
        dropped,
    })
}

/// How a held wear request ended.
#[derive(Debug)]
pub(crate) enum Held {
    /// The reply is sent.
    Answered,
    /// The client closed the pipe, or wrote to it while it waited, which the
    /// protocol does not allow, so the connection closes.
    Left,
}

/// Signals each time a held request starts to wait, after it registered for
/// [`wake`] and read the lists, so a test acts only on a waiting request.
#[cfg(test)]
static WAITING: Notify = Notify::const_new();

/// Answers `request` on `server`, or holds it until there is something for
/// it, as `visibility` lets the client see. Each time the request wakes, it
/// asks again what the client may see, so a change to the stored list
/// applies to a held request at its next reply. While it waits, it reads the
/// pipe, so a window that closes frees its pipe instance and its connection
/// at once. It has no idle timeout: a window holds its request for as long
/// as it runs.
///
/// # Errors
///
/// The reply could not be written, or the task that reads the lists
/// panicked.
pub(crate) async fn hold(
    server: &mut NamedPipeServer,
    volumes: &VolumeRegistry,
    visibility: Visibility,
    request: WearRequest,
) -> anyhow::Result<Held> {
    let mut read_buf = [0u8; 1];
    let mut access = Access::Sees(visibility);
    let mut woke = false;
    loop {
        let woken = WAKE.notified();
        tokio::pin!(woken);
        woken.as_mut().enable();
        let set = volumes.current();
        let pipe = if woke {
            Some(
                server
                    .as_handle()
                    .try_clone_to_owned()
                    .context("duplicating the pipe handle")?,
            )
        } else {
            None
        };
        let (reply, back) = tokio::task::spawn_blocking(move || {
            let access = match pipe {
                Some(pipe) => access::of_pipe_client(pipe.as_handle(), false),
                None => access,
            };
            let response = match (&access, run_id()) {
                (_, Err(e)) => Some(crate::server::unsupported(&format!(
                    "no id for this run of the service: {e}"
                ))),
                (Access::Refused(refusal), _) => {
                    Some(crate::server::unsupported(refusal.message()))
                }
                (Access::Sees(visibility), Ok(run)) => {
                    answer(&set, request, visibility.viewer(), run).map(Response::Wear)
                }
            };
            // Encoded here: a reply can be megabytes.
            let reply = response.map(|response| {
                let mut out = Vec::new();
                encode_response_frame(&response, &mut out);
                out
            });
            (reply, access)
        })
        .await
        .context("wear task panicked")?;
        access = back;
        woke = true;
        if let Some(reply) = reply {
            crate::server::write_reply(server, &reply).await?;
            return Ok(Held::Answered);
        }
        #[cfg(test)]
        WAITING.notify_one();
        tokio::select! {
            () = woken.as_mut() => {}
            // A close, an error or a byte: each ends the hold.
            _ = server.read(&mut read_buf) => return Ok(Held::Left),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Held, NUMBERS, WAITING, answer, hold, run_id, wake};
    use crate::access::Visibility;
    use crate::volume_state::{Indexed, VolumeRegistry, VolumeSet, VolumeState};
    use gozler_core::index::settle::Batch;
    use gozler_core::index::{NTFS_ROOT_FRN, VolumeIndex};
    use gozler_core::proto::{
        FrameDecoder, MAX_SERVER_FRAME, RepeatsAfter, Response, WearReply, WearRequest,
        decode_response_frame,
    };
    use gozler_core::query::{ResultNames, Viewer};
    use gozler_core::types::{Frn, VolumePhase};
    use gozler_core::usn::record::{
        FILE_ATTRIBUTE_DIRECTORY, ParsedUsnRecord, USN_REASON_CLOSE, USN_REASON_FILE_CREATE,
    };
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };

    /// One test that holds a request at a time, so each one's `WAITING`
    /// signal comes from its own hold.
    static HOLDS: Mutex<()> = Mutex::new(());

    /// An empty volume at `C:\`, installed as `Building::finish` installs
    /// one, so its creates take numbers from the service's counter.
    fn volume() -> Arc<VolumeState> {
        let mut index = VolumeIndex::new(NTFS_ROOT_FRN);
        index.repeats_mut().number_from(&NUMBERS);
        let indexed = Indexed {
            index,
            metadata_pending: false,
            journal_max_bytes: None,
            built: true,
        };
        Arc::new(VolumeState::in_state(r"C:\", indexed, VolumePhase::Live))
    }

    /// Creates the folder `a` at `depth` below the root, inside the one at
    /// `depth - 1`, as the tail applies a journal batch.
    fn create(volume: &VolumeState, depth: u128) {
        let parent = if depth == 0 {
            NTFS_ROOT_FRN
        } else {
            Frn(100 + depth - 1)
        };
        let record = ParsedUsnRecord {
            frn: Frn(100 + depth),
            parent_frn: parent,
            reason: USN_REASON_FILE_CREATE | USN_REASON_CLOSE,
            attributes: FILE_ATTRIBUTE_DIRECTORY,
            name: b"a".to_vec(),
            time: 0,
        };
        volume.index_mut().apply_batch(&Batch::of(&[record]));
    }

    fn request(run: u64, number: u64, copies: u32) -> WearRequest {
        WearRequest {
            after: RepeatsAfter { run, number },
            copies,
        }
    }

    /// A client that keeps its last run id across a restart of the service
    /// must get every event of the new run; a client that waited for number
    /// 12 would otherwise miss the new run's events 1 to 12. A first request
    /// warns about nothing older, and an event below the client's copies
    /// does not answer it.
    #[test]
    fn a_reply_holds_the_events_the_client_has_not_seen_in_this_run() {
        let volume = volume();
        for depth in 0..4 {
            create(&volume, depth);
        }
        let volumes: VolumeSet = Arc::new(vec![volume]);
        let run = 77;
        let reply = |seen, number, copies| {
            answer(
                &volumes,
                request(seen, number, copies),
                Viewer::Everything,
                run,
            )
        };
        let first = reply(0, 0, 4).expect("a first request answers at once");
        assert!(first.events.is_empty());
        let every = reply(76, first.highest, 4).expect("another run answers at once");
        assert_eq!(every.events.len(), 1);
        assert_eq!(every.events[0].top, r"C:\a");
        let number = every.events[0].number;
        let newer = reply(run, number - 1, 4).expect("an event the client has not seen");
        assert_eq!(newer.events, every.events);
        assert_eq!(reply(run, number, 4), None);
        assert_eq!(reply(run, number - 1, 5), None);
    }

    /// Reads one reply from the client's end of the pipe. A minute without
    /// one fails the test instead of holding the run.
    async fn reply_on(pipe: &mut NamedPipeClient) -> WearReply {
        let mut decoder = FrameDecoder::new(MAX_SERVER_FRAME);
        let mut buf = [0u8; 4096];
        loop {
            if let Some(frame) = decoder.next_frame().expect("a whole frame") {
                let response = decode_response_frame(&frame, &mut ResultNames::default())
                    .expect("a reply decodes");
                let Response::Wear(reply) = response else {
                    panic!("a wear reply, not {response:?}");
                };
                return reply;
            }
            let read = tokio::time::timeout(Duration::from_secs(60), pipe.read(&mut buf));
            let n = (read.await)
                .expect("a reply within a minute")
                .expect("the pipe reads");
            assert_ne!(n, 0, "the service closed the pipe");
            decoder.feed(&buf[..n]);
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a tokio runtime")
    }

    /// Both ends of a new pipe of this test process, connected.
    async fn connected(test: &str) -> (NamedPipeServer, NamedPipeClient) {
        let name = format!(r"\\.\pipe\gozlerd-wear-{test}-{}", std::process::id());
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .expect("the test pipe is created");
        let client = ClientOptions::new().open(&name).expect("the client opens");
        server.connect().await.expect("the client connects");
        (server, client)
    }

    /// Returns once the held request waits, after it read the lists.
    async fn waiting() {
        tokio::time::timeout(Duration::from_secs(60), WAITING.notified())
            .await
            .expect("the request waits within a minute");
    }

    /// A held request must answer when a create gives a tree its copies,
    /// and not at an earlier create below them, or the window warns too
    /// early or never; an event made between two requests must answer the
    /// second at once, or the window misses it until the next event.
    #[test]
    fn a_held_request_answers_when_a_create_reaches_its_copies() {
        let _one = HOLDS.lock().unwrap_or_else(PoisonError::into_inner);
        let volume = volume();
        for depth in 0..3 {
            create(&volume, depth);
        }
        let registry = Arc::new(VolumeRegistry::default());
        registry.add(volume.clone());
        runtime().block_on(async move {
            let (mut server, mut client) = connected("answers").await;
            let run = run_id().expect("a run id");
            let seen = NUMBERS.load(Ordering::Acquire);
            let held = {
                let registry = registry.clone();
                tokio::spawn(async move {
                    let everything = Visibility::Everything;
                    let held =
                        hold(&mut server, &registry, everything, request(run, seen, 5)).await;
                    (held.expect("the reply is written"), server)
                })
            };
            waiting().await;
            // 4 copies, below the request's 5: it reads again and waits on.
            create(&volume, 3);
            wake();
            waiting().await;
            create(&volume, 4);
            wake();
            let first = reply_on(&mut client).await;
            assert_eq!(first.events.len(), 1);
            assert_eq!(first.events[0].copies, 5);
            let (held, mut server) = held.await.expect("the held task does not panic");
            assert!(matches!(held, Held::Answered));

            create(&volume, 5);
            let next = request(run, first.highest, 5);
            let again = hold(&mut server, &registry, Visibility::Everything, next).await;
            assert!(matches!(again, Ok(Held::Answered)));
            let second = reply_on(&mut client).await;
            assert_eq!(second.events.len(), 1);
            assert_eq!(second.events[0].copies, 6);
        });
    }

    /// A client that writes while its request is held, then closes, must
    /// free its connection. A hold that stopped reading the pipe would keep
    /// the pipe instance and one of the service's connections until the
    /// service restarts, and 64 such clients would lock everyone out.
    #[test]
    fn a_client_that_writes_during_a_hold_frees_its_connection() {
        let _one = HOLDS.lock().unwrap_or_else(PoisonError::into_inner);
        let registry = Arc::new(VolumeRegistry::default());
        registry.add(volume());
        runtime().block_on(async move {
            let (mut server, mut client) = connected("writes").await;
            let run = run_id().expect("a run id");
            let seen = NUMBERS.load(Ordering::Acquire);
            let held = tokio::spawn(async move {
                let never = request(run, seen, u32::MAX);
                hold(&mut server, &registry, Visibility::Everything, never).await
            });
            waiting().await;
            client.write_all(&[0]).await.expect("the client writes");
            drop(client);
            let held = tokio::time::timeout(Duration::from_secs(60), held)
                .await
                .expect("the hold ends within a minute")
                .expect("the held task does not panic");
            assert!(matches!(held, Ok(Held::Left)));
        });
    }
}
