//! The named-pipe query server.
//!
//! A tokio multi-thread runtime accepts connections on `\\.\pipe\gozler-v1`,
//! frames requests/responses with the shared sans-io codec from
//! `gozler-core::proto`, and answers each search through `gozler-core`'s
//! [`ResultRows`] and [`search_totals`], which read each volume through its
//! [`VolumeState`]. A search can also keep its rows as a result snapshot,
//! which later requests read by its id from one daemon-wide table
//! ([`crate::result_snapshots`]). Indexing itself (bootstrap + tail) stays
//! on dedicated OS threads; tokio only serves the pipe.
//!
//! Each request frame is answered for the client that sent it
//! ([`crate::access`]): a request that names files reads only what that
//! client may see, and one from a client gozlerd cannot identify is refused.

use std::ops::Deref;
use std::os::windows::io::AsHandle;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gozler_core::include::IndexSettings;
use gozler_core::index::{PathStatus, VolumeIndex};
use gozler_core::links::{FolderLink, Links};
use gozler_core::proto::{
    CallerSees, DaemonStatus, DecodeError, Decoded, MAX_CLIENT_FRAME, MemPair,
    OLDEST_PROTO_VERSION, PIPE_NAME, PROTO_VERSION, ProtoError, Request, Response,
    ResultSnapshotId, Returns, Rows, SentFolders, SnapshotRowsRequest, SortSnapshotRequest,
    VolumeMemory, VolumeStatus, Want, decode_request, encode_page, encode_response_frame,
    encode_snapshot_page,
};
use gozler_core::query::{
    CompiledQuery, HeldVolume, IntoSnapshotError, ResultRows, ResultSnapshot, RowsSpec,
    SortAgainError, TotalsError, Viewer, search_totals,
};
use gozler_core::scope::{Scope, VolumeMounts};
use gozler_winfs::{PipeSecurity, build_pipe_security};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{Semaphore, watch};

use crate::access::{self, Access, Refusal};
use crate::include::Include;
use crate::result_snapshots::{Kept, ResultSnapshots, live_volumes};
use crate::volume_state::{VolumeRead, VolumeRegistry, VolumeSet, VolumeState};

/// The result snapshots of every client, each with its volumes held weakly.
type Snapshots = ResultSnapshots<Weak<VolumeState>>;

/// Cap on concurrently-served pipe connections. A silent/hostile local client
/// cannot pin more than this many tasks + read buffers + pipe instances.
const MAX_CONNECTIONS: usize = 64;
/// Pages in flight between the query encoder and the pipe writer. Each page
/// holds [`gozler_core::proto::PAGE_ROWS`] rows, so the backlog bounds a full
/// export's buffered bytes instead of buffering the entire response.
const QUERY_PAGE_BACKLOG: usize = 4;
/// Idle/half-open connections are dropped after this long with no request, so a
/// connected-but-silent client cannot block a task forever. The one-shot CLI
/// client always writes immediately after connecting, well within this window.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// A connection whose client takes none of a reply for this long is dropped,
/// so a client that stops reading cannot hold its result set and its
/// connection slot forever. It is long because a person who pauses the
/// console (a selection in the window, `| more`) also stops the client
/// reading, and waiting only keeps those resources a while longer.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Pause after a failed pipe connect before listening again. When connects
/// keep failing (a broken pipe, or a local user who connects and closes in a
/// loop), it holds the accept loop to a bounded rate instead of a busy
/// loop. A client that connects during the pause waits for it, not
/// longer: the next instance already exists, and its connect completes at once.
const CONNECT_RETRY_PAUSE: Duration = Duration::from_millis(10);
/// At most one log line per this period for failed connects, and one for
/// clients dropped at [`MAX_CONNECTIONS`] (see [`Throttle`]).
const WARN_EVERY: Duration = Duration::from_secs(60);
/// How long after a request ends gozlerd returns its freed memory: just past
/// mimalloc's default purge delay of 1 s (see `run::return_freed_memory`).
const RETURN_MEMORY_AFTER: Duration = Duration::from_millis(1_100);
/// How often to try the pipe name again while another process holds it.
const CLAIM_RETRY_EVERY: Duration = Duration::from_secs(2);

/// Runs the pipe server until stopped. Blocks on a tokio runtime.
///
/// Stops on either Ctrl+C (console mode) or `shutdown` flipping to `true` (the
/// service-control handler's Stop/Shutdown path). Keep at least one
/// `watch::Sender` alive for the duration in console mode: if every sender
/// drops, `changed()` resolves and would end the loop prematurely.
pub(crate) fn run(
    volumes: Arc<VolumeRegistry>,
    include: Arc<Include>,
    shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let security = build_pipe_security().context("building pipe security descriptor")?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;
    let snapshots = Arc::new(ResultSnapshots::new());
    rt.block_on(accept_loop(volumes, include, snapshots, security, shutdown))
}

async fn accept_loop(
    volumes: Arc<VolumeRegistry>,
    include: Arc<Include>,
    snapshots: Arc<Snapshots>,
    security: PipeSecurity,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let Some(mut server) = claim_first_instance(&security, &mut shutdown).await? else {
        return Ok(());
    };
    let conn_limit = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    tracing::info!(pipe = PIPE_NAME, "query server listening");
    let mut connect_failures = Throttle::default();
    let mut dropped_clients = Throttle::default();
    // A line is due at a burst's first event; this logs the rest of it.
    let mut flush = tokio::time::interval(WARN_EVERY);
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            r = server.connect() => {
                if let Err(e) = r {
                    // One client's failed connect (for example a client that
                    // closed the pipe before the server saw it) ends only that
                    // connection. The instance is replaced, the new one first,
                    // so the pipe always has a listener.
                    if let Some(failures) = connect_failures.hit(Instant::now()) {
                        tracing::warn!(error = %e, failures, "pipe connect failed; still listening");
                    }
                    server = create_instance(&security, false)?;
                    tokio::time::sleep(CONNECT_RETRY_PAUSE).await;
                    continue;
                }
                let mut connected = server;
                // Create the next instance before handling, so there is never
                // a window with no listener for a client to race into.
                server = create_instance(&security, false)?;
                // Bound concurrent connections. try_acquire (non-blocking) keeps
                // the accept loop responsive to shutdown under load: at the cap
                // we drop the newest connection rather than pin an extra one.
                match Arc::clone(&conn_limit).try_acquire_owned() {
                    Ok(permit) => {
                        let vols = volumes.clone();
                        let include = include.clone();
                        let snapshots = snapshots.clone();
                        tokio::spawn(async move {
                            let _permit = permit; // held for the connection's lifetime
                            if let Err(e) = handle_connection(&mut connected, vols, include, snapshots).await {
                                tracing::debug!(error = %e, "connection ended");
                            }
                            // Dropping the instance cancels only reads and
                            // connects, so a page the client never took would
                            // keep the handle and its buffer alive outside
                            // MAX_CONNECTIONS until the client reads or
                            // closes. Disconnecting fails that write and frees
                            // both.
                            let _ = connected.disconnect();
                        });
                    }
                    Err(_) => {
                        if let Some(dropped) = dropped_clients.hit(Instant::now()) {
                            tracing::warn!(
                                max = MAX_CONNECTIONS,
                                dropped,
                                "connection limit reached; dropping clients"
                            );
                        }
                        drop(connected);
                    }
                }
            }
            _ = flush.tick() => log_counted(&mut connect_failures, &mut dropped_clients),
            _ = tokio::signal::ctrl_c() => {
                log_counted(&mut connect_failures, &mut dropped_clients);
                tracing::info!("Ctrl+C received; shutting down");
                return Ok(());
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    log_counted(&mut connect_failures, &mut dropped_clients);
                    tracing::info!("stop requested; shutting down");
                    return Ok(());
                }
            }
        }
    }
}

/// Counts a kind of event that a local user can repeat at will, such as a
/// failed connect, and lets at most one log line for it through each
/// [`WARN_EVERY`], so the service log cannot grow with them.
#[derive(Default)]
struct Throttle {
    /// Events since the last line.
    count: u64,
    last_line: Option<Instant>,
}

impl Throttle {
    /// Counts one event at `now`, and returns the count to log when a line
    /// is due: at a burst's first event, then at most once a period.
    fn hit(&mut self, now: Instant) -> Option<u64> {
        self.count += 1;
        let due = self
            .last_line
            .is_none_or(|last| now.duration_since(last) >= WARN_EVERY);
        due.then(|| self.take(now))
    }

    /// The events counted since the last line, if any, starting a new period.
    fn flush(&mut self, now: Instant) -> Option<u64> {
        (self.count > 0).then(|| self.take(now))
    }

    fn take(&mut self, now: Instant) -> u64 {
        self.last_line = Some(now);
        std::mem::take(&mut self.count)
    }
}

/// Logs the failed connects and the dropped clients counted since their last
/// line.
fn log_counted(connect_failures: &mut Throttle, dropped_clients: &mut Throttle) {
    let now = Instant::now();
    if let Some(failures) = connect_failures.flush(now) {
        tracing::warn!(failures, "pipe connects failed; still listening");
    }
    if let Some(dropped) = dropped_clients.flush(now) {
        tracing::warn!(
            max = MAX_CONNECTIONS,
            dropped,
            "connection limit reached; dropped clients"
        );
    }
}

/// Creates the first pipe instance, waiting while another process holds the
/// name.
///
/// Any local user can create [`PIPE_NAME`] before gozlerd claims it (see
/// `gozler_winfs::pipe_trust`), and the first instance is then refused.
/// Failing here would make the service manager restart the service, which
/// reads every volume again, about once a minute for as long as the name is
/// held. So the built indexes wait instead, and the server starts once the
/// name is free. Any other error is returned. `None` when a stop arrives
/// first.
async fn claim_first_instance(
    security: &PipeSecurity,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Option<NamedPipeServer>> {
    let mut waiting = false;
    loop {
        match create_instance(security, true) {
            Ok(server) => {
                if waiting {
                    tracing::info!(pipe = PIPE_NAME, "pipe name is free again");
                }
                return Ok(Some(server));
            }
            Err(e) if name_taken(&e) => {
                if !waiting {
                    tracing::warn!(
                        pipe = PIPE_NAME,
                        error = %e,
                        "another process holds the pipe name; waiting for it"
                    );
                    waiting = true;
                }
            }
            Err(e) => return Err(e),
        }
        tokio::select! {
            () = tokio::time::sleep(CLAIM_RETRY_EVERY) => {}
            _ = tokio::signal::ctrl_c() => return Ok(None),
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(None);
                }
            }
        }
    }
}

/// Whether creating an instance failed because another process holds the
/// name: the first instance is refused with access denied, and a holder that
/// allowed one instance reports every instance busy.
fn name_taken(e: &anyhow::Error) -> bool {
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_PIPE_BUSY: i32 = 231;
    e.downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::raw_os_error)
        .is_some_and(|code| code == ERROR_ACCESS_DENIED || code == ERROR_PIPE_BUSY)
}

fn create_instance(security: &PipeSecurity, first: bool) -> Result<NamedPipeServer> {
    let mut opts = ServerOptions::new();
    opts.first_pipe_instance(first);
    opts.reject_remote_clients(true);
    // SAFETY: `security.attributes_ptr()` points at a live SECURITY_ATTRIBUTES
    // owned by `security`, which outlives this call and every returned instance.
    let server =
        unsafe { opts.create_with_security_attributes_raw(PIPE_NAME, security.attributes_ptr()) }
            .with_context(|| format!("creating pipe instance {PIPE_NAME}"))?;
    Ok(server)
}

async fn handle_connection(
    server: &mut NamedPipeServer,
    volumes: Arc<VolumeRegistry>,
    include: Arc<Include>,
    snapshots: Arc<Snapshots>,
) -> Result<()> {
    // The user a result snapshot counts against. A client whose user cannot
    // be read, such as one that already exited, counts as one user with
    // every other such client.
    let user: Arc<[u8]> = gozler_winfs::pipe_client_user(server.as_handle())
        .unwrap_or_else(|e| {
            tracing::debug!(error = %e, "could not read the client's user");
            Vec::new()
        })
        .into();
    let mut decoder = gozler_core::proto::FrameDecoder::new(MAX_CLIENT_FRAME);
    let mut read_buf = vec![0u8; 16 * 1024];
    let mut unidentified_logged = false;
    loop {
        // Reap idle/half-open connections: a connected client that never sends a
        // request is dropped after IDLE_TIMEOUT, freeing its task/buffer/pipe.
        let n = match tokio::time::timeout(IDLE_TIMEOUT, server.read(&mut read_buf)).await {
            Ok(r) => r?,
            Err(_) => return Ok(()),
        };
        if n == 0 {
            return Ok(());
        }
        decoder.feed(&read_buf[..n]);
        loop {
            let frame = match decoder.next_frame() {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(e) => return Err(anyhow::anyhow!("frame error: {e}")),
            };
            // Taken before the reply reads the pipe again, because the pipe
            // gives the token of the last message read, and on the blocking
            // pool, because it reads the registry. Asked for each frame, so
            // a change to the stored list applies at the next search.
            let pipe = server
                .as_handle()
                .try_clone_to_owned()
                .context("duplicating the pipe handle")?;
            let access =
                tokio::task::spawn_blocking(move || access::of_pipe_client(pipe.as_handle()))
                    .await
                    .context("access task panicked")?;
            // Query execution plus result serialization is the heaviest CPU
            // work in the process and holds a read lock, so every request runs
            // on the blocking pool rather than stalling an async worker (and
            // thus the accept loop). Replies stream through a bounded channel:
            // a large export never exists as one buffer, at most
            // QUERY_PAGE_BACKLOG pages are in flight, and encoding overlaps the
            // pipe writes instead of preceding them.
            let vols = volumes.current();
            let include = include.clone();
            let snapshots = snapshots.clone();
            let user = user.clone();
            let stop = Arc::new(AtomicBool::new(false));
            let stopped = stop.clone();
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(QUERY_PAGE_BACKLOG);
            let encoder = tokio::task::spawn_blocking(move || {
                // A failed send means the receiver is gone (client vanished or
                // write error): stop encoding.
                serve(
                    &vols,
                    &include,
                    &snapshots,
                    &user,
                    &access,
                    &frame,
                    &stopped,
                    &mut |reply| tx.blocking_send(reply).is_ok(),
                );
                match access {
                    Access::Refused(Refusal::Unidentified(error)) => Some(error),
                    _ => None,
                }
            });
            // While the search runs, the pipe is read too, so a client that
            // closes it stops the search instead of leaving it to scan for no
            // one. One read is enough: a client that sends its next request
            // is still there, and the decoder keeps those bytes for it.
            let mut watching = true;
            let mut client_left = false;
            let mut write_err: Option<std::io::Error> = None;
            loop {
                tokio::select! {
                    reply = rx.recv() => match reply {
                        Some(reply) => {
                            if let Err(e) = write_reply(server, &reply).await {
                                write_err = Some(e);
                                break;
                            }
                        }
                        None => break,
                    },
                    read = server.read(&mut read_buf), if watching => match read {
                        Ok(0) | Err(_) => {
                            client_left = true;
                            break;
                        }
                        Ok(n) => {
                            decoder.feed(&read_buf[..n]);
                            watching = false;
                        }
                    },
                }
            }
            // Stops a search still scanning, and closing the channel stops a
            // still-encoding task at its next emit.
            stop.store(true, Ordering::Relaxed);
            drop(rx);
            let unidentified = encoder.await.context("request task panicked")?;
            // The request's buffers are free now; mimalloc returns them once
            // its purge delay has passed and something collects.
            tokio::spawn(async {
                tokio::time::sleep(RETURN_MEMORY_AFTER).await;
                crate::run::return_freed_memory();
            });
            if let Some(error) = unidentified
                && !unidentified_logged
            {
                tracing::warn!(%error, "could not tell which user sent a request; its searches are refused");
                unidentified_logged = true;
            }
            if let Some(e) = write_err {
                return Err(e.into());
            }
            if client_left {
                return Ok(());
            }
        }
    }
}

/// Writes reply bytes to the client, failing with `TimedOut` when the client
/// has not taken them within [`WRITE_TIMEOUT`].
async fn write_reply(server: &mut NamedPipeServer, bytes: &[u8]) -> std::io::Result<()> {
    tokio::time::timeout(WRITE_TIMEOUT, server.write_all(bytes))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "client stopped reading"))?
}

/// Answers one request frame from a client running as `user`, with what
/// `access` lets it see, handing each framed reply to `emit` as soon as it is
/// encoded. `emit` returning `false` means the client is gone; encoding
/// stops. `stop` set means the same, and stops a search before its first
/// reply.
///
/// `Hello`, `Status` and `SettingsSaved` answer every client, because they
/// show no name; `Status` also tells the client what it sees. A request
/// that reads the index's entries is refused when gozlerd cannot tell who
/// sent it, and the Include count answers only a user who sees every file:
/// its folder and name rules over the whole index would let any other user
/// count hidden entries until the counts spell their names.
///
/// A request from a newer client may hold parts this daemon cannot read
/// ([`decode_request`]): the reply names the skipped parts in
/// [`Response::Skipped`] before the answer, or refuses the request with
/// [`ProtoError::UnknownRequest`].
#[allow(clippy::too_many_arguments)]
fn serve(
    volumes: &VolumeSet,
    include: &Include,
    snapshots: &Snapshots,
    user: &[u8],
    access: &Access,
    frame: &[u8],
    stop: &AtomicBool,
    emit: &mut dyn FnMut(Vec<u8>) -> bool,
) {
    let Decoded { request, skipped } = match decode_request(frame) {
        Ok(decoded) => decoded,
        Err(DecodeError::Unknown(part)) => {
            return reply(
                emit,
                Response::Error {
                    code: ProtoError::UnknownRequest,
                    message: part,
                },
            );
        }
        Err(error) => return reply(emit, refusal(error.to_string())),
    };
    let request = match request {
        Request::Hello {
            proto_min,
            proto_max,
            ..
        } => return reply(emit, hello(volumes, proto_min, proto_max)),
        Request::Status { links_held } => {
            let mut status = build_status(volumes, include, snapshots, access.caller_sees());
            if let (Some(held), Access::Sees(visibility)) = (links_held, access) {
                let links = folder_links(volumes, visibility.viewer());
                status.links = (links.fingerprint() != held).then_some(links);
            }
            return reply(emit, Response::Status(status));
        }
        Request::SettingsSaved => {
            include.reload();
            return reply(emit, Response::SettingsApplied);
        }
        request => request,
    };
    let viewer = match access {
        Access::Sees(visibility) => visibility.viewer(),
        Access::Refused(refused) => return reply(emit, unsupported(refused.message())),
    };
    if !skipped.is_empty() {
        reply(emit, Response::Skipped { parts: skipped });
    }
    let search = match request {
        Request::Search(search) => search,
        Request::CountIncluded(settings) => {
            return reply(
                emit,
                match viewer {
                    Viewer::Everything => Response::IncludedCount {
                        entries: count_included(volumes, &settings),
                    },
                    Viewer::Filtered(_) => unsupported(
                        "only a user who sees every file can count what the Include settings keep",
                    ),
                },
            );
        }
        Request::ResultSnapshotRows(read) => {
            return read_snapshot(volumes, snapshots, &read, viewer, emit);
        }
        Request::SortResultSnapshot(sort) => {
            return sort_snapshot(volumes, snapshots, user, &sort, viewer, stop, emit);
        }
        Request::Hello { .. } | Request::Status { .. } | Request::SettingsSaved => {
            unreachable!("answered before the client's access is taken")
        }
    };
    if let Err(off) = include.applied().allows(&search.query) {
        return reply(emit, refusal(off.to_string()));
    }
    let scope = search.scope.as_deref();
    let links = folder_links(volumes, viewer);
    let query = match CompiledQuery::with_links(&search.query, &links, scope) {
        Ok(query) => query,
        Err(e) => return reply(emit, refusal(e.to_string())),
    };
    // In `VolumeSet` order, the order every request takes the volumes' locks
    // in.
    let targets = volumes.to_vec();
    match search.returns {
        Returns::Rows(wanted) => {
            match ResultRows::search(targets, &query, scope, wanted.spec(), viewer, stop) {
                Ok(rows) => stream_rows(rows, &wanted, emit),
                Err(not_indexed) => reply(emit, refusal(not_indexed.to_string())),
            }
        }
        Returns::Totals => reply(emit, totals(targets, &query, scope, viewer, stop)),
        Returns::ResultSnapshot { rows, replaces } => {
            let every_row = RowsSpec {
                offset: 0,
                limit: None,
                ..rows.spec()
            };
            match ResultRows::search(targets, &query, scope, every_row, viewer, stop) {
                // A stopped search is short and no one reads it, so it must
                // not take the place of the user's last result snapshot.
                Ok(_) if stop.load(Ordering::Relaxed) => {}
                Ok(found) => {
                    let made = Made {
                        user,
                        replaces,
                        cap: snapshot_cap(volumes),
                    };
                    make_snapshot(snapshots, made, found, &rows, viewer, stop, emit);
                }
                Err(not_indexed) => reply(emit, refusal(not_indexed.to_string())),
            }
        }
    }
}

/// Who makes a result snapshot, and what makes room for it.
struct Made<'a> {
    user: &'a [u8],
    replaces: Option<ResultSnapshotId>,
    /// The most rows all result snapshots hold.
    cap: u64,
}

/// The most rows all result snapshots hold: twice the entries of every
/// index, two empty searches.
fn snapshot_cap(volumes: &VolumeSet) -> u64 {
    2 * volumes.iter().map(|v| v.index().len() as u64).sum::<u64>()
}

/// Keeps `found`, a search for every row, as a result snapshot, answers its
/// id, and streams the rows that `rows`' offset and limit ask for, as
/// `viewer` sees them.
fn make_snapshot(
    snapshots: &Snapshots,
    made: Made<'_>,
    found: ResultRows<Arc<VolumeState>>,
    rows: &Rows,
    viewer: Viewer<'_>,
    stop: &AtomicBool,
    emit: &mut dyn FnMut(Vec<u8>) -> bool,
) {
    let (snapshot, volumes) = match found.into_snapshot(stop) {
        Ok(kept) => kept,
        Err(IntoSnapshotError::TooManyVolumes(too_many)) => {
            return reply(emit, refusal(too_many.to_string()));
        }
        // The client left: nothing is kept, so no slot is taken and the
        // snapshot this one replaces stays.
        Err(IntoSnapshotError::Stopped(_)) => return,
    };
    let id = match gozler_winfs::random_u128() {
        Ok(bits) => ResultSnapshotId(bits),
        Err(e) => return reply(emit, refusal(format!("no id for a result snapshot: {e}"))),
    };
    let snapshot = Arc::new(snapshot);
    snapshots.keep(
        Kept {
            id,
            user: made.user.to_vec(),
            last_read: Instant::now(),
            snapshot: snapshot.clone(),
            volumes: volumes.iter().map(Arc::downgrade).collect(),
        },
        made.replaces,
        made.cap,
    );
    let total = snapshot.total();
    let mut made_frame = Vec::new();
    encode_response_frame(&Response::ResultSnapshot { id, total }, &mut made_frame);
    if !emit(made_frame) {
        return;
    }
    let read = SnapshotRowsRequest {
        id,
        offset: rows.offset,
        limit: rows.limit,
        wants: rows.wants.clone(),
    };
    stream_snapshot(&snapshot, volumes, &read, viewer, emit);
}

/// Streams the rows of the result snapshot that `read` names, as `viewer`
/// sees them, or says it is gone.
fn read_snapshot(
    volumes: &VolumeSet,
    snapshots: &Snapshots,
    read: &SnapshotRowsRequest,
    viewer: Viewer<'_>,
    emit: &mut dyn FnMut(Vec<u8>) -> bool,
) {
    let found = snapshots.get(read.id, Instant::now());
    let Some((snapshot, held)) = found else {
        return reply(emit, snapshot_gone());
    };
    let Some(held) = live_volumes(&held, volumes) else {
        return reply(emit, snapshot_gone());
    };
    stream_snapshot(&snapshot, held, read, viewer, emit);
}

/// Sorts the result snapshot that `sort` names again with no search, as
/// `viewer` sees its rows, keeps the sorted one in its place, answers its id,
/// and streams the rows that `sort.rows`' offset and limit ask for; or says
/// it is gone.
fn sort_snapshot(
    volumes: &VolumeSet,
    snapshots: &Snapshots,
    user: &[u8],
    sort: &SortSnapshotRequest,
    viewer: Viewer<'_>,
    stop: &AtomicBool,
    emit: &mut dyn FnMut(Vec<u8>) -> bool,
) {
    let found = snapshots.get(sort.id, Instant::now());
    let Some((snapshot, held)) = found else {
        return reply(emit, snapshot_gone());
    };
    let Some(held) = live_volumes(&held, volumes) else {
        return reply(emit, snapshot_gone());
    };
    let sorted = match snapshot.sort_again(held, sort.rows.sort, viewer, stop) {
        Ok(sorted) => sorted,
        Err(SortAgainError::Gone(_)) => return reply(emit, snapshot_gone()),
        // The client left: no one reads an answer.
        Err(SortAgainError::Stopped(_)) => return,
    };
    let made = Made {
        user,
        replaces: Some(sort.id),
        cap: snapshot_cap(volumes),
    };
    make_snapshot(snapshots, made, sorted, &sort.rows, viewer, stop, emit);
}

/// Streams the rows of `snapshot` that `read` asks for, from `volumes`, its
/// volumes, as `viewer` sees them and as [`stream_rows`] streams a search's;
/// each page carries the search's sums when `read` asks for them. An index
/// replaced meanwhile ends the reply with the gone error.
fn stream_snapshot(
    snapshot: &ResultSnapshot,
    volumes: Vec<Arc<VolumeState>>,
    read: &SnapshotRowsRequest,
    viewer: Viewer<'_>,
    emit: &mut dyn FnMut(Vec<u8>) -> bool,
) {
    let sums = (read.wants.contains(&Want::Sums))
        .then(|| snapshot.sums())
        .flatten();
    let mut rows = snapshot.read(volumes, read.offset, read.limit, viewer);
    let mut sent = SentFolders::default();
    loop {
        match encode_snapshot_page(&mut rows, &read.wants, sums, &mut sent) {
            Ok((page, more)) => {
                if !emit(page) || !more {
                    return;
                }
            }
            Err(_) => return reply(emit, snapshot_gone()),
        }
    }
}

/// The reply to a read of a result snapshot that no longer answers.
fn snapshot_gone() -> Response {
    Response::Error {
        code: ProtoError::ResultSnapshotGone,
        message: "the result snapshot is gone; search again".into(),
    }
}

/// Encodes one tagged reply frame and hands it to `emit`.
fn reply(emit: &mut dyn FnMut(Vec<u8>) -> bool, response: Response) {
    let mut out = Vec::new();
    encode_response_frame(&response, &mut out);
    let _ = emit(out);
}

/// The reply that refuses a request the daemon cannot run as sent.
fn refusal(message: String) -> Response {
    Response::Error {
        code: ProtoError::BadQuery,
        message,
    }
}

/// The reply that refuses a request this client may not make.
fn unsupported(message: &str) -> Response {
    Response::Error {
        code: ProtoError::Unsupported,
        message: message.into(),
    }
}

/// Streams `rows` as one or more binary results-page frames (paged by
/// [`ResultsPage::push`](gozler_core::proto::ResultsPage::push), `more` set
/// on all but the last). An empty result set still emits one `more: false`
/// page so the client always sees a terminal frame. No full path is built:
/// each page sends the folders its rows need that no earlier page sent, and
/// each row its folder and its name. A page releases the volumes' locks
/// before it is sent, so the pipe and a slow client never hold a lock.
fn stream_rows(
    mut rows: ResultRows<Arc<VolumeState>>,
    wanted: &Rows,
    emit: &mut dyn FnMut(Vec<u8>) -> bool,
) {
    let mut sent = SentFolders::default();
    loop {
        let (page, more) = encode_page(&mut rows, &wanted.wants, &mut sent);
        if !emit(page) || !more {
            return;
        }
    }
}

/// The reply to a [`Returns::Totals`] search, of the matches `viewer` sees.
fn totals(
    volumes: Vec<Arc<VolumeState>>,
    query: &CompiledQuery,
    scope: Option<&str>,
    viewer: Viewer<'_>,
    stop: &AtomicBool,
) -> Response {
    match search_totals(volumes, query, scope, viewer, stop) {
        Ok(totals) => Response::Totals(totals.into()),
        Err(TotalsError::NotIndexed(not_indexed)) => refusal(not_indexed.to_string()),
        Err(TotalsError::Overflow(overflow)) => Response::Error {
            code: ProtoError::Overflow,
            message: overflow.to_string(),
        },
    }
}

/// Answers version negotiation with the highest version both sides speak.
///
/// This daemon serves every version from [`OLDEST_PROTO_VERSION`] to
/// [`PROTO_VERSION`], so a client whose range misses all of them is refused.
/// Answering `PROTO_VERSION.min(proto_max)` without that check would answer
/// `proto: 0` to a `proto_max: 0` client, agreeing on a version neither side
/// implements.
fn hello(volumes: &VolumeSet, proto_min: u16, proto_max: u16) -> Response {
    let proto = PROTO_VERSION.min(proto_max);
    if proto < proto_min || proto < OLDEST_PROTO_VERSION {
        return Response::Error {
            code: ProtoError::Unsupported,
            message: format!(
                "client speaks protocol {proto_min}..={proto_max}; this daemon speaks \
                 {OLDEST_PROTO_VERSION}..={PROTO_VERSION}"
            ),
        };
    }
    Response::Hello {
        proto,
        server: format!("gozlerd {}", gozler_core::escompat::version()),
        ready: all_volumes_live(volumes),
    }
}

/// Whether every volume's index is as complete as gozler can make it, via
/// [`gozler_core::types::VolumePhase::is_complete`]. The same bar
/// [`DaemonStatus::volumes_incomplete`] applies, so `Hello.ready` and a `Status`
/// reply can never disagree about whether the index is whole.
fn all_volumes_live(volumes: &VolumeSet) -> bool {
    volumes.iter().all(|v| v.phase().is_complete())
}

/// How many entries the indexed volumes would hold under `settings`. A
/// volume that is not indexed now adds nothing, and neither does what the
/// applied settings left out, because the index does not hold it.
fn count_included(volumes: &VolumeSet, settings: &IndexSettings) -> u64 {
    volumes
        .iter()
        .map(|v| {
            let rules = settings.rules_for(&v.guid);
            if rules.leaves_out_volume() {
                0
            } else {
                v.read().index().count_kept(&rules)
            }
        })
        .sum()
}

fn build_status(
    volumes: &VolumeSet,
    include: &Include,
    snapshots: &Snapshots,
    caller_sees: CallerSees,
) -> DaemonStatus {
    let mut incomplete = false;
    let vols = volumes
        .iter()
        .map(|v| {
            let read = v.read();
            if !read.phase().is_complete() {
                incomplete = true;
            }
            let index = read.index();
            let stats = index.stats();
            let mem = index.memory();
            let pair = |c: gozler_core::index::ComponentBytes| MemPair {
                used: c.used,
                alloc: c.allocated,
            };
            VolumeStatus {
                guid: v.guid.clone(),
                mounts: v.mounts.clone(),
                phase: read.phase().clone(),
                progress: read.progress,
                entries: index.len() as u64,
                generation: index.generation(),
                metadata_pending: read.metadata_pending(),
                journal_max_bytes: read.journal_max_bytes(),
                placeholders_created: stats.placeholders_created,
                delete_of_unknown: stats.delete_of_unknown,
                stale_slots: stats.stale_slots,
                link_reconciles_dropped: stats.link_reconciles_dropped,
                unread_folders: read.unread_folders(),
                memory: Some(VolumeMemory {
                    entries: pair(mem.entries),
                    arena_raw: pair(mem.arena_raw),
                    arena_folded: pair(mem.arena_folded),
                    frn_map: pair(mem.frn_map),
                    name_tables: pair(mem.name_tables),
                    dir_children: pair(mem.dir_children),
                    descriptors: pair(mem.descriptors),
                    frn_map_kind: mem.frn_map_kind.to_string(),
                }),
            }
        })
        .collect();
    let self_mem = gozler_winfs::self_memory().unwrap_or_default();
    DaemonStatus {
        volumes: vols,
        volumes_incomplete: incomplete,
        process_working_set: self_mem.working_set,
        process_private_bytes: self_mem.private_bytes,
        allocator_committed: crate::run::allocator_committed(),
        result_snapshots_bytes: snapshots.bytes(Instant::now()),
        include: Some((*include.applied()).clone()),
        left_out: include.left_out(),
        caller_sees,
        links: None,
    }
}

/// The folder links on `volumes` whose names `viewer` sees
/// ([`VolumeIndex::folder_links`]), and each mount of a volume after its
/// first, as a link to the mount its paths print with.
fn folder_links(volumes: &VolumeSet, viewer: Viewer<'_>) -> Links {
    let may_list = match viewer {
        Viewer::Everything => None,
        Viewer::Filtered(may_list) => Some(may_list),
    };
    let mut links = Vec::new();
    for volume in volumes.iter() {
        let prefix = volume.mount_prefix();
        links.extend(volume.read().index().folder_links(prefix, may_list));
        links.extend(volume.mounts.iter().skip(1).map(|mount| FolderLink {
            link: mount.trim_end_matches('\\').to_owned(),
            target: prefix.trim_end_matches('\\').to_owned(),
        }));
    }
    let mut links = Links::new(links).into_vec();
    let volumes: Vec<_> = volumes.iter().collect();
    let mut pending = vec![Vec::new(); volumes.len()];
    for (number, link) in links.iter().enumerate() {
        if let Scope::Folder { volume, folder } =
            Scope::of(Some(&link.target), volumes.iter().map(|v| v.mounted()))
        {
            pending[volume].push((number, folder.to_owned()));
        }
    }
    let mut path = Vec::new();
    for (volume, pending) in volumes.iter().zip(pending) {
        let held = volume.read();
        for (number, folder) in pending {
            let index = held.index();
            let Some(folder) =
                gozler_core::query::engine::resolve_scope(index, folder.as_bytes(), false)
            else {
                continue;
            };
            path.clear();
            if index.path_of(folder, &mut path) == PathStatus::Ok {
                let prefix = volume.mount_prefix().trim_end_matches('\\');
                links[number].target = if path.is_empty() {
                    prefix.to_owned()
                } else {
                    format!("{prefix}\\{}", gozler_core::wtf8::to_string_lossy(&path))
                };
            }
        }
    }
    links.into()
}

/// A volume's index, held through [`VolumeState::read`] for a search.
pub(crate) struct HeldIndex<'a>(VolumeRead<'a>);

impl Deref for HeldIndex<'_> {
    type Target = VolumeIndex;

    fn deref(&self) -> &VolumeIndex {
        self.0.index()
    }
}

impl gozler_core::query::IndexedVolume for VolumeState {
    type Index<'a> = HeldIndex<'a>;

    fn read(&self) -> HeldVolume<HeldIndex<'_>> {
        let read = VolumeState::read(self);
        HeldVolume {
            complete: read.phase().is_complete(),
            metadata_pending: read.metadata_pending(),
            index: HeldIndex(read),
        }
    }

    fn mounts(&self) -> VolumeMounts<'_> {
        self.mounted()
    }
}

#[cfg(test)]
mod optimizations;

#[cfg(test)]
mod tests {
    use super::{Snapshots, build_status, serve};
    use crate::access::{Access, Refusal, Visibility};
    use crate::include::Include;
    use crate::result_snapshots::ResultSnapshots;
    use crate::volume_state::{Indexed, VolumeSet, VolumeState};
    use gozler_core::include::{DriveChoice, FolderChoice, IndexSettings};
    use gozler_core::index::{NTFS_ROOT_FRN, VolumeIndex};
    use gozler_core::proto::{
        CallerSees, DaemonStatus, FileIdentity, FrameDecoder, MAX_SERVER_FRAME, PAGE_ROWS,
        PROTO_VERSION, ProtoError, Request, Response, Returns, Rows, SearchRequest, Want,
        decode_response_frame, encode_request,
    };
    use gozler_core::query::{ParseOptions, ResultNames, parse_query_with};
    use gozler_core::types::{Frn, SortKey, SortSpec, VolumePhase};
    use gozler_core::usn::record::{
        FILE_ATTRIBUTE_DIRECTORY, ParsedUsnRecord, USN_REASON_FILE_CREATE,
        USN_REASON_RENAME_NEW_NAME,
    };
    use std::sync::atomic::AtomicBool;

    /// A taken pipe name must read as taken through the context
    /// `create_instance` adds, or `claim_first_instance` returns the error and
    /// the service manager restarts gozlerd, which reads every volume again,
    /// once a minute for as long as another process holds the name.
    #[test]
    fn a_held_pipe_name_is_recognized_through_its_context() {
        use anyhow::Context;
        let failed = |code| {
            Err::<(), _>(std::io::Error::from_raw_os_error(code))
                .context("creating pipe instance")
                .unwrap_err()
        };
        assert!(super::name_taken(&failed(5)));
        assert!(super::name_taken(&failed(231)));
        assert!(
            !super::name_taken(&failed(2)),
            "file not found is a real failure"
        );
    }
    use std::sync::Arc;

    fn dir_record(frn: u128, parent: u128, name: &str) -> ParsedUsnRecord {
        ParsedUsnRecord {
            frn: Frn(frn),
            parent_frn: Frn(parent),
            reason: USN_REASON_FILE_CREATE,
            attributes: FILE_ATTRIBUTE_DIRECTORY,
            name: name.as_bytes().to_vec(),
        }
    }

    /// `C:\Windows\System32` as an index, built through the public API only.
    fn windows_index() -> VolumeIndex {
        let mut index = VolumeIndex::new(NTFS_ROOT_FRN);
        index.insert_enum(&dir_record(100, NTFS_ROOT_FRN.0, "Windows"));
        index.insert_enum(&dir_record(101, 100, "System32"));
        index
    }

    fn volume(mount: &str, index: VolumeIndex, phase: VolumePhase) -> Arc<VolumeState> {
        let indexed = Indexed {
            index,
            metadata_pending: false,
            journal_max_bytes: None,
            built: true,
        };
        Arc::new(VolumeState::in_state(mount, indexed, phase))
    }

    fn live_volume(mount: &str) -> Arc<VolumeState> {
        volume(mount, windows_index(), VolumePhase::Live)
    }

    #[test]
    fn links_use_indexed_target_case_and_scopes_enter_primary_folder_mounts() {
        let mut host = VolumeIndex::new(NTFS_ROOT_FRN);
        for (frn, name, target) in [
            (100, "alias", r"D:\Windows"),
            (101, "nested", r"D:\Windows\SYSTEM32"),
        ] {
            host.insert_enum(&dir_record(frn, NTFS_ROOT_FRN.0, name));
            host.set_folder_link(
                Frn(frn),
                Some(gozler_core::links::ReparseTarget::Absolute(target.into())),
            );
        }
        let mut data = windows_index();
        let mut file = dir_record(102, 101, "driver.dll");
        file.attributes = 0;
        data.insert_enum(&file);
        let vols = set(&[
            volume(r"C:\", host, VolumePhase::Live),
            volume(r"D:\", data, VolumePhase::Live),
        ]);
        let mut named = query(r"alias\System32\driver");
        named.match_case = true;
        let found = results(&encode_query_binary(&vols, named));
        assert_eq!(found[0].total, 1);
        assert_eq!(found[0].items[0].path, r"D:\Windows\System32\driver.dll");
        let mut wrong_case = query(r"alias\SYSTEM32\driver");
        wrong_case.match_case = true;
        assert_eq!(results(&encode_query_binary(&vols, wrong_case))[0].total, 0);

        let mounted = set(&[live_volume(r"C:\"), live_volume(r"C:\repo\data\")]);
        let mut scoped = query("System32");
        scoped.scope = Some(r"C:\repo".into());
        let found = results(&encode_query_binary(&mounted, scoped));
        assert_eq!(found[0].total, 1);
        assert_eq!(found[0].items[0].path, r"C:\repo\data\Windows\System32");
    }

    /// An index whose name order is the REVERSE of its path order: `a\z.log`
    /// sorts after `z\a.log` by name but before it by path. A fixture where the
    /// two agree would pass even with the name key computed wrongly.
    fn merge_index() -> VolumeIndex {
        let mut index = VolumeIndex::new(NTFS_ROOT_FRN);
        index.insert_enum(&dir_record(100, NTFS_ROOT_FRN.0, "a"));
        index.insert_enum(&dir_record(101, NTFS_ROOT_FRN.0, "z"));
        for (frn, parent, name) in [(102, 100, "z.log"), (103, 101, "a.log")] {
            let mut rec = dir_record(frn, parent, name);
            rec.attributes = 0;
            index.insert_enum(&rec);
        }
        index
    }

    fn set(vols: &[Arc<VolumeState>]) -> VolumeSet {
        Arc::new(vols.to_vec())
    }

    /// The default Include settings, which keep everything.
    fn defaults() -> Include {
        Include::new(IndexSettings::default(), None, Box::new(|| {}))
    }

    /// A Snapshot volume must NOT taint results.
    ///
    /// It has no USN journal and never can (Windows refuses to create one on a
    /// recovery partition), so its index is finished: no waiting improves it and
    /// no action fixes it. Flagging it would put an always-on "results may be
    /// incomplete" warning on every query, which nobody reads when it finally
    /// matters.
    #[test]
    fn snapshot_volumes_do_not_taint_results() {
        let vols = set(&[
            live_volume(r"C:\"),
            volume(r"D:\", windows_index(), VolumePhase::Snapshot),
        ]);
        assert!(
            !build_status(&vols, &defaults(), &Snapshots::new(), CallerSees::EveryFile)
                .volumes_incomplete,
            "a fully indexed journal-less volume must not flag results incomplete"
        );
        assert!(
            !results(&encode_query_binary(&vols, query("System32")))[0].volumes_incomplete,
            "and no query page may flag it either"
        );
        let Response::Hello { ready, .. } =
            answer(&vols, hello_request(PROTO_VERSION, PROTO_VERSION))
        else {
            panic!("must negotiate")
        };
        assert!(ready, "a snapshot volume is ready to serve");
    }

    /// The honesty bit: any volume whose index is NOT as complete as gozler can make
    /// it must mark results incomplete, or the user trusts a partial index.
    #[test]
    fn status_is_incomplete_unless_every_volume_is_live() {
        assert!(
            !build_status(
                &set(&[live_volume(r"C:\")]),
                &defaults(),
                &Snapshots::new(),
                CallerSees::EveryFile
            )
            .volumes_incomplete
        );

        for phase in [
            VolumePhase::Bootstrapping,
            VolumePhase::Rescanning,
            VolumePhase::Offline,
            VolumePhase::Failed {
                reason: "test".into(),
            },
        ] {
            let vols = set(&[live_volume(r"C:\"), volume(r"D:\", windows_index(), phase)]);
            assert!(
                build_status(&vols, &defaults(), &Snapshots::new(), CallerSees::EveryFile)
                    .volumes_incomplete,
                "one non-Live volume must taint the whole status"
            );
        }
    }

    /// The preview counts what each volume would keep: a folder left out
    /// takes everything below it, and a drive left out counts nothing.
    #[test]
    fn counting_included_entries_follows_the_settings() {
        let vols = set(&[live_volume(r"C:\")]);
        let all = vols[0].read().index().len() as u64;
        let count = |settings| match answer(&vols, Request::CountIncluded(settings)) {
            Response::IncludedCount { entries } => entries,
            other => panic!("an entry count, not {other:?}"),
        };
        let drive = |indexed, folders| IndexSettings {
            drives: vec![DriveChoice {
                volume: vols[0].guid.clone(),
                name: String::new(),
                indexed,
                folders,
            }],
            ..IndexSettings::default()
        };
        let windows = FolderChoice {
            path: "Windows".into(),
            indexed: false,
        };

        assert_eq!(count(IndexSettings::default()), all);
        assert_eq!(
            count(drive(true, vec![windows])),
            all - 2,
            "Windows and System32 are left out"
        );
        assert_eq!(count(drive(false, Vec::new())), 0);
    }

    /// The payload of the frame a client sends for `request`.
    fn request_frame(request: &Request) -> Vec<u8> {
        let mut frame = Vec::new();
        encode_request(request, &mut frame);
        frame.split_off(4)
    }

    /// Every frame the daemon replies to `request` with, back to back as the
    /// pipe carries them.
    fn reply_bytes(volumes: &VolumeSet, request: &Request) -> Vec<u8> {
        reply_bytes_in(volumes, &ResultSnapshots::new(), request)
    }

    /// [`reply_bytes`] with the result snapshots of earlier requests.
    fn reply_bytes_in(volumes: &VolumeSet, snapshots: &Snapshots, request: &Request) -> Vec<u8> {
        let everything = Access::Sees(Visibility::Everything);
        reply_bytes_as(volumes, snapshots, &everything, request)
    }

    /// [`reply_bytes_in`] for a client that `access` describes.
    fn reply_bytes_as(
        volumes: &VolumeSet,
        snapshots: &Snapshots,
        access: &Access,
        request: &Request,
    ) -> Vec<u8> {
        frame_reply_bytes(volumes, snapshots, access, &request_frame(request))
    }

    /// The frames the daemon replies to the request payload `frame`.
    fn frame_reply_bytes(
        volumes: &VolumeSet,
        snapshots: &Snapshots,
        access: &Access,
        frame: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        serve(
            volumes,
            &defaults(),
            snapshots,
            b"",
            access,
            frame,
            &AtomicBool::new(false),
            &mut |reply| {
                out.extend_from_slice(&reply);
                true
            },
        );
        out
    }

    /// The one frame the daemon replies to `request` from a client that
    /// `access` describes.
    fn answer_as(volumes: &VolumeSet, access: &Access, request: &Request) -> Response {
        let bytes = reply_bytes_as(volumes, &ResultSnapshots::new(), access, request);
        let mut frames = decode_frames(&bytes);
        assert_eq!(frames.len(), 1, "one reply");
        frames.remove(0)
    }

    /// A client gozlerd cannot identify gets no entry and no count,
    /// while the requests that show no name still answer. Without it, a
    /// change to `serve` could answer an unknown caller unfiltered, which
    /// lets any program bypass the filter.
    #[test]
    fn an_unidentified_client_reads_no_entry() {
        use gozler_core::proto::SnapshotRowsRequest;
        let vols = set(&[live_volume(r"C:\")]);
        let refused = Access::Refused(Refusal::Anonymous);
        let snapshot_read = Request::ResultSnapshotRows(SnapshotRowsRequest {
            id: gozler_core::proto::ResultSnapshotId(1),
            offset: 0,
            limit: None,
            wants: Vec::new(),
        });
        for request in [
            search(query("System32")),
            totals_search("System32", None),
            Request::CountIncluded(IndexSettings::default()),
            snapshot_read,
        ] {
            match answer_as(&vols, &refused, &request) {
                Response::Error {
                    code: ProtoError::Unsupported,
                    message,
                } => assert_eq!(message, Refusal::Anonymous.message()),
                other => panic!("a refusal, not {other:?}"),
            }
        }
        let hello = hello_request(PROTO_VERSION, PROTO_VERSION);
        assert!(matches!(
            answer_as(&vols, &refused, &hello),
            Response::Hello { .. }
        ));
        assert!(matches!(
            answer_as(&vols, &refused, &Request::Status { links_held: None }),
            Response::Status(DaemonStatus {
                caller_sees: CallerSees::Unidentified,
                ..
            })
        ));
        assert!(matches!(
            answer_as(&vols, &refused, &Request::SettingsSaved),
            Response::SettingsApplied
        ));
    }

    /// A filtered user reads only the entries of the folders they may
    /// list, and gets no Include count. Without it, a change to `serve`
    /// could search for a filtered user unfiltered, or give any user the
    /// Include count, whose rules can spell hidden names.
    #[test]
    fn a_filtered_user_reads_only_what_they_may_list() {
        let mut index = windows_index();
        index.insert_enum(&dir_record(102, NTFS_ROOT_FRN.0, "Users"));
        let mut file = dir_record(103, 102, "a.txt");
        file.attributes = 0;
        index.insert_enum(&file);
        for (folder, descriptor) in [
            (NTFS_ROOT_FRN, &b"open"[..]),
            (Frn(100), b"hidden"),
            (Frn(102), b"open"),
        ] {
            index.set_folder_descriptor(folder, descriptor);
        }
        let vols = set(&[volume(r"C:\", index, VolumePhase::Live)]);
        let caller = Access::Sees(Visibility::Filtered(Box::new(|descriptor: &[u8]| {
            descriptor == b"open"
        })));

        let bytes = reply_bytes_as(&vols, &ResultSnapshots::new(), &caller, &search(query("")));
        assert_eq!(
            paths(&bytes),
            [r"C:\Users\a.txt", r"C:\Users", r"C:\Windows"],
            "System32 is in a folder the user cannot list"
        );
        let count = Request::CountIncluded(IndexSettings::default());
        assert!(matches!(
            answer_as(&vols, &caller, &count),
            Response::Error {
                code: ProtoError::Unsupported,
                ..
            }
        ));
    }

    /// A file renamed and moved on its volume keeps its identity in the
    /// answer: its file ID on the volume, whole, and the GUID path that
    /// `--status` names the volume by. A script such as TinyTorrent's Library
    /// knows the file again by them; an answer that named the index's slot,
    /// dropped the sequence number or named another volume would make the
    /// renamed file a new one.
    #[test]
    fn a_renamed_file_keeps_its_identity() {
        let movie = Frn::ntfs(200, 7);
        let mut index = windows_index();
        index.insert_enum(&dir_record(102, NTFS_ROOT_FRN.0, "Movies"));
        let mut file = dir_record(movie.0, 101, "film.mkv");
        file.attributes = 0;
        index.insert_enum(&file);
        let vol = volume(r"C:\", index, VolumePhase::Live);
        let vols = set(&[Arc::clone(&vol)]);
        let found = || {
            let mut request = search(query("film"));
            let Request::Search(SearchRequest {
                returns: Returns::Rows(rows),
                ..
            }) = &mut request
            else {
                unreachable!("search asks for rows")
            };
            rows.wants.push(Want::Identity);
            let pages = results(&reply_bytes(&vols, &request));
            let [item] = &pages[0].items[..] else {
                panic!("one match: {:?}", pages[0].items)
            };
            (item.path.clone(), item.identity.clone())
        };
        let Response::Status(status) = answer(&vols, Request::Status { links_held: None }) else {
            panic!("a status reply")
        };
        let identity = Some(Box::new(FileIdentity {
            volume: status.volumes[0].guid.clone(),
            file_id: movie,
        }));
        assert_eq!(
            found(),
            (
                r"C:\Windows\System32\film.mkv".to_string(),
                identity.clone()
            )
        );

        let mut rename = dir_record(movie.0, 102, "Film (2001).mkv");
        rename.attributes = 0;
        rename.reason = USN_REASON_RENAME_NEW_NAME;
        vol.index_mut().apply_batch(&[rename]);
        assert_eq!(
            found(),
            (r"C:\Movies\Film (2001).mkv".to_string(), identity)
        );
    }

    /// A search as a test writes it: query text, which [`search`] parses as
    /// gozler does, and the rows it asks for.
    struct TextQuery {
        query: String,
        scope: Option<String>,
        sort: SortSpec,
        offset: u32,
        limit: Option<u32>,
        wants: Vec<Want>,
        match_case: bool,
    }

    fn search(q: TextQuery) -> Request {
        let options = ParseOptions {
            match_case: q.match_case,
            ..ParseOptions::default()
        };
        Request::Search(SearchRequest {
            query: parse_query_with(&q.query, options).expect("the test's query parses"),
            scope: q.scope,
            returns: Returns::Rows(Rows {
                sort: q.sort,
                offset: q.offset,
                limit: q.limit,
                wants: q.wants,
            }),
        })
    }

    /// The reply to the search `q`.
    fn encode_query_binary(volumes: &VolumeSet, q: TextQuery) -> Vec<u8> {
        reply_bytes(volumes, &search(q))
    }

    /// The one frame the daemon replies to `request` with.
    fn answer(volumes: &VolumeSet, request: Request) -> Response {
        let mut frames = decode_frames(&reply_bytes(volumes, &request));
        assert_eq!(frames.len(), 1, "one reply");
        frames.remove(0)
    }

    fn query(q: &str) -> TextQuery {
        TextQuery {
            query: q.to_string(),
            scope: None,
            sort: SortSpec::default_for(SortKey::Name),
            offset: 0,
            limit: None,
            wants: vec![Want::Size, Want::DateModified],
            match_case: false,
        }
    }

    /// Splits the daemon's own output for one request back into responses.
    /// The wire is `[len:u32-le][payload]` repeated, and every decoder used
    /// here is public gozler-core API, with one table of folders for the
    /// reply, so the test reads exactly what the CLI would.
    fn decode_frames(bytes: &[u8]) -> Vec<gozler_core::proto::Response> {
        let mut out = Vec::new();
        let mut names = ResultNames::default();
        let mut i = 0;
        while i < bytes.len() {
            let len = u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
            i += 4;
            out.push(decode_response_frame(&bytes[i..i + len], &mut names).expect("decodes"));
            i += len;
        }
        out
    }

    fn results(bytes: &[u8]) -> Vec<gozler_core::proto::QueryResults> {
        decode_frames(bytes)
            .into_iter()
            .map(|r| match r {
                gozler_core::proto::Response::Results(r) => r,
                other => panic!("expected Results, got {other:?}"),
            })
            .collect()
    }

    /// The documented terminal-frame invariant. A client reads until it sees
    /// `more: false`, so a zero-match query that emitted no frame at all would
    /// hang it rather than return nothing.
    #[test]
    fn empty_result_set_still_emits_one_terminal_frame() {
        let vols = set(&[live_volume(r"C:\")]);
        let pages = results(&encode_query_binary(&vols, query("no-such-file-anywhere")));
        assert_eq!(pages.len(), 1, "exactly one frame");
        assert!(!pages[0].more, "the only frame must be terminal");
        assert_eq!(pages[0].items.len(), 0);
        assert_eq!(pages[0].total, 0);
    }

    /// A request that fails to decode must come back as a single tagged JSON
    /// error, never as an empty results page (which would read as "no
    /// matches").
    #[test]
    fn a_malformed_request_returns_an_error_frame_not_an_empty_page() {
        let vols = set(&[live_volume(r"C:\")]);
        let mut replies = Vec::new();
        serve(
            &vols,
            &defaults(),
            &ResultSnapshots::new(),
            b"",
            &Access::Sees(Visibility::Everything),
            br#"{"t":"Search","query":"#,
            &AtomicBool::new(false),
            &mut |reply| {
                replies.extend_from_slice(&reply);
                true
            },
        );
        let frames = decode_frames(&replies);
        assert_eq!(frames.len(), 1);
        assert!(
            matches!(frames[0], gozler_core::proto::Response::Error { .. }),
            "got {:?}",
            frames[0]
        );
    }

    /// What the daemon replies to `request`, a request as JSON that this
    /// build's client cannot make, from a client that sees every file.
    fn replies_to_json(volumes: &VolumeSet, request: &serde_json::Value) -> Vec<Response> {
        let everything = Access::Sees(Visibility::Everything);
        let frame = request.to_string();
        let bytes = frame_reply_bytes(
            volumes,
            &ResultSnapshots::new(),
            &everything,
            frame.as_bytes(),
        );
        decode_frames(&bytes)
    }

    /// A search of `clauses` as a newer client sends it, for its rows with
    /// `wants`.
    fn newer_search(clauses: serde_json::Value, wants: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "t": "Search",
            "query": { "clauses": clauses, "match_case": false },
            "scope": null,
            "returns": { "Rows": {
                "sort": { "key": "Name", "dir": "Asc" },
                "offset": 0,
                "limit": null,
                "wants": wants,
            } },
        })
    }

    /// A newer client's search runs without the term and the want this
    /// daemon cannot read, and the reply names both before the rows.
    /// Without the second decode, the daemon refuses the whole search as
    /// malformed, and the person gets an error instead of the rows it can
    /// find.
    #[test]
    fn a_search_runs_without_the_parts_it_cannot_read_and_names_them() {
        let vols = set(&[live_volume(r"C:\")]);
        let clauses = serde_json::json!([{ "terms": [
            { "negated": false, "test": { "Name": "System32" } },
            { "negated": false, "test": { "Thumbnail": 64 } },
        ] }]);
        let frames = replies_to_json(
            &vols,
            &newer_search(clauses, serde_json::json!(["Size", "Preview"])),
        );
        assert_eq!(
            frames[0],
            Response::Skipped {
                parts: vec!["Thumbnail".into(), "Preview".into()]
            }
        );
        let Some(Response::Results(page)) = frames.get(1) else {
            panic!("the rows after the skipped parts: {frames:?}");
        };
        let paths: Vec<&str> = page.items.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, [r"C:\Windows\System32"]);
    }

    /// A skipped term alone in its clause, negated or not, leaves the clause
    /// empty, which matches nothing, so the search finds no rows. A daemon
    /// that removed the emptied clause would match every entry the other
    /// clauses let through, and Commander mode would act on files the
    /// search never found.
    #[test]
    fn a_skipped_term_alone_in_its_clause_finds_nothing() {
        let vols = set(&[live_volume(r"C:\")]);
        for negated in [false, true] {
            let clauses = serde_json::json!([
                { "terms": [{ "negated": false, "test": { "Name": "Windows" } }] },
                { "terms": [{ "negated": negated, "test": { "Thumbnail": 64 } }] },
            ]);
            let frames = replies_to_json(&vols, &newer_search(clauses, serde_json::json!([])));
            let Some(Response::Results(page)) = frames.get(1) else {
                panic!("the rows after the skipped parts: {frames:?}");
            };
            assert_eq!((page.total, page.items.len()), (0, 0), "negated: {negated}");
        }
    }

    /// A request of a kind this daemon does not know is refused as a
    /// request only a newer daemon reads, named by its kind. As `BadQuery`,
    /// gozler would report a wrong query; as `Unsupported`, an access
    /// refusal.
    #[test]
    fn an_unknown_kind_of_request_is_refused_by_its_name() {
        let vols = set(&[live_volume(r"C:\")]);
        let frames = replies_to_json(&vols, &serde_json::json!({ "t": "Thumbnails", "size": 64 }));
        assert_eq!(
            frames,
            [Response::Error {
                code: ProtoError::UnknownRequest,
                message: "Thumbnails".into(),
            }]
        );
    }

    /// A search for the totals of `query` below `scope`.
    fn totals_search(query: &str, scope: Option<&str>) -> Request {
        Request::Search(SearchRequest {
            query: parse_query_with(query, ParseOptions::default()).expect("the query parses"),
            scope: scope.map(str::to_string),
            returns: Returns::Totals,
        })
    }

    /// A total past 64 bits, which only a corrupt size in the index can make,
    /// answers `Overflow`, which gozler exits by as an internal error, rather
    /// than a wrapped total that looks valid.
    #[test]
    fn a_total_past_64_bits_answers_overflow() {
        use gozler_core::layout::{LayoutFile, LayoutName};
        let mut index = VolumeIndex::new(NTFS_ROOT_FRN);
        for (frn, size) in [(100, u64::MAX - 1), (101, 2)] {
            index.enrich(&LayoutFile {
                frn: Frn(frn),
                attributes: 0x20,
                size: Some(size),
                mtime_ft: Some(5),
                created_ft: None,
                security: None,
                names: vec![LayoutName {
                    parent_frn: NTFS_ROOT_FRN,
                    name: format!("{frn}.bin").into_bytes(),
                    dos_only: false,
                }],
            });
        }
        let vols = set(&[volume(r"C:\", index, VolumePhase::Live)]);
        match answer(&vols, totals_search("", None)) {
            Response::Error { code, .. } => assert_eq!(code, ProtoError::Overflow),
            other => panic!("an overflow must be refused, got {other:?}"),
        }
    }

    /// The sums a window asks for reach it through the pipe, over every
    /// match and not only the page's rows, and a folder inside a matching
    /// folder adds nothing to the folders' bytes.
    #[test]
    fn a_search_that_wants_sums_gets_them_over_the_pipe() {
        use gozler_core::layout::{LayoutFile, LayoutName};
        use gozler_core::query::MatchSums;
        let mut index = windows_index();
        index.enrich(&LayoutFile {
            frn: Frn(102),
            attributes: 0x20,
            size: Some(5),
            mtime_ft: Some(5),
            created_ft: None,
            security: None,
            names: vec![LayoutName {
                parent_frn: Frn(101),
                name: b"kernel32.dll".to_vec(),
                dos_only: false,
            }],
        });
        index.compute_folder_sizes();
        let vols = set(&[volume(r"C:", index, VolumePhase::Live)]);
        let mut request = search(TextQuery {
            limit: Some(1),
            ..query("")
        });
        let Request::Search(SearchRequest {
            returns: Returns::Rows(rows),
            ..
        }) = &mut request
        else {
            unreachable!("search asks for rows")
        };
        rows.wants.push(Want::Sums);
        let pages = results(&reply_bytes(&vols, &request));
        let last = pages.last().expect("one page at least");
        assert_eq!(last.items.len(), 1, "the limit cuts the rows");
        assert_eq!(
            last.sums,
            Some(MatchSums {
                files: 1,
                file_bytes: 5,
                files_without_size: 0,
                folders: 2,
                folder_bytes: 5,
                folder_bytes_at_least: false,
            })
        );
    }

    /// A result snapshot answers its id and the rows asked for, a later
    /// read by that id answers the rows at its offset with the same total,
    /// a sort answers a new one in the new order, which replaces it, and an
    /// id the daemon no longer keeps answers the gone error, which the
    /// window answers with a new search.
    #[test]
    fn a_result_snapshot_is_read_by_its_id_until_it_is_gone() {
        use gozler_core::proto::{SnapshotRowsRequest, SortSnapshotRequest};
        use gozler_core::types::SortDir;
        let vols = set(&[live_volume(r"C:\")]);
        let snapshots = ResultSnapshots::new();
        let made = Request::Search(SearchRequest {
            query: parse_query_with("", ParseOptions::default()).expect("parses"),
            scope: None,
            returns: Returns::ResultSnapshot {
                rows: Rows {
                    sort: SortSpec::default_for(SortKey::Name),
                    offset: 0,
                    limit: Some(1),
                    wants: Vec::new(),
                },
                replaces: None,
            },
        });
        let frames = decode_frames(&reply_bytes_in(&vols, &snapshots, &made));
        let Response::ResultSnapshot { id, total: 2 } = frames[0] else {
            panic!("the id of 2 rows first: {:?}", frames[0])
        };
        let paths = |frame: &Response| match frame {
            Response::Results(page) => (
                page.total,
                page.items
                    .iter()
                    .map(|i| i.path.clone())
                    .collect::<Vec<_>>(),
            ),
            other => panic!("a results page: {other:?}"),
        };
        assert_eq!(frames.len(), 2);
        assert_eq!(
            paths(&frames[1]),
            (2, vec![r"C:\Windows\System32".to_string()])
        );

        let read = |id| {
            Request::ResultSnapshotRows(SnapshotRowsRequest {
                id,
                offset: 1,
                limit: None,
                wants: Vec::new(),
            })
        };
        let frames = decode_frames(&reply_bytes_in(&vols, &snapshots, &read(id)));
        assert_eq!(paths(&frames[0]), (2, vec![r"C:\Windows".to_string()]));

        let sort = Request::SortResultSnapshot(SortSnapshotRequest {
            id,
            rows: Rows {
                sort: SortSpec {
                    key: SortKey::Name,
                    dir: SortDir::Desc,
                },
                offset: 0,
                limit: None,
                wants: Vec::new(),
            },
        });
        let frames = decode_frames(&reply_bytes_in(&vols, &snapshots, &sort));
        let Response::ResultSnapshot {
            id: sorted,
            total: 2,
        } = frames[0]
        else {
            panic!("the id of the sorted rows first: {:?}", frames[0])
        };
        assert_ne!(sorted, id);
        assert_eq!(
            paths(&frames[1]),
            (
                2,
                vec![
                    r"C:\Windows".to_string(),
                    r"C:\Windows\System32".to_string()
                ]
            )
        );
        let frames = decode_frames(&reply_bytes_in(&vols, &snapshots, &read(id)));
        assert!(
            matches!(
                frames[..],
                [Response::Error {
                    code: ProtoError::ResultSnapshotGone,
                    ..
                }]
            ),
            "{frames:?}"
        );
    }

    /// A result snapshot search or sort whose client left keeps nothing, so it
    /// does not replace the result snapshot the window still reads. Without it, a
    /// fast scroll that abandons requests takes the user's snapshot slots
    /// with short answers that no one reads (#137).
    #[test]
    fn a_stopped_search_keeps_no_result_snapshot() {
        use gozler_core::proto::{SnapshotRowsRequest, SortSnapshotRequest};
        let vols = set(&[live_volume(r"C:\")]);
        let snapshots = ResultSnapshots::new();
        let make = |replaces| {
            request_frame(&Request::Search(SearchRequest {
                query: parse_query_with("", ParseOptions::default()).expect("parses"),
                scope: None,
                returns: Returns::ResultSnapshot {
                    rows: Rows {
                        sort: SortSpec::default_for(SortKey::Name),
                        offset: 0,
                        limit: Some(1),
                        wants: Vec::new(),
                    },
                    replaces,
                },
            }))
        };
        let serve_with = |frame: &[u8], stop: bool| {
            let mut out = Vec::new();
            serve(
                &vols,
                &defaults(),
                &snapshots,
                b"",
                &Access::Sees(Visibility::Everything),
                frame,
                &AtomicBool::new(stop),
                &mut |reply| {
                    out.extend_from_slice(&reply);
                    true
                },
            );
            decode_frames(&out)
        };
        let frames = serve_with(&make(None), false);
        let Response::ResultSnapshot { id, .. } = frames[0] else {
            panic!("a result snapshot: {:?}", frames[0])
        };
        assert!(serve_with(&make(Some(id)), true).is_empty());
        let sort = request_frame(&Request::SortResultSnapshot(SortSnapshotRequest {
            id,
            rows: Rows {
                sort: SortSpec::default_for(SortKey::Size),
                offset: 0,
                limit: Some(1),
                wants: Vec::new(),
            },
        }));
        assert!(serve_with(&sort, true).is_empty());
        let read = request_frame(&Request::ResultSnapshotRows(SnapshotRowsRequest {
            id,
            offset: 0,
            limit: None,
            wants: Vec::new(),
        }));
        let frames = serve_with(&read, false);
        assert!(
            matches!(frames[..], [Response::Results(_), ..]),
            "{frames:?}"
        );
    }

    /// A scope on no indexed volume answers `BadQuery`, which gozler exits by
    /// as invalid input, for rows and totals alike, because an empty answer
    /// would read as "no such file".
    #[test]
    fn a_scope_on_no_indexed_volume_answers_bad_query() {
        let vols = set(&[live_volume(r"C:\")]);
        let mut rows = query("System32");
        rows.scope = Some(r"E:\x".to_string());
        for request in [search(rows), totals_search("System32", Some(r"E:\x"))] {
            match answer(&vols, request) {
                Response::Error { code, .. } => assert_eq!(code, ProtoError::BadQuery),
                other => panic!("a scope on no volume must be refused, got {other:?}"),
            }
        }
    }

    /// Paging must set `more` on every frame but the last, and `total` must be
    /// the whole match count on every page, not the page's own length.
    #[test]
    fn pages_carry_more_until_the_last_and_a_stable_total() {
        let n = PAGE_ROWS as u64 + 1;
        let vols = set(&[volume(r"C:\", paging_index(), VolumePhase::Live)]);

        let pages = results(&encode_query_binary(&vols, query("hit")));
        assert_eq!(pages.len(), 2, "PAGE_ROWS + 1 matches must span two frames");
        assert!(pages[0].more, "first page must signal more");
        assert!(!pages[1].more, "last page must be terminal");
        assert_eq!(pages[0].items.len(), PAGE_ROWS as usize);
        assert_eq!(pages[1].items.len(), 1);
        assert_eq!(pages[0].total, n, "total is the whole match count");
        assert_eq!(pages[1].total, n, "total must not drift across pages");
    }

    /// One folder holding [`PAGE_ROWS`] + 1 matches of `hit`, so a reply
    /// takes two pages.
    fn paging_index() -> VolumeIndex {
        let mut index = VolumeIndex::new(NTFS_ROOT_FRN);
        index.insert_enum(&dir_record(100, NTFS_ROOT_FRN.0, "many"));
        for i in 0..=u128::from(PAGE_ROWS) {
            let mut rec = dir_record(1000 + i, 100, &format!("hit{i:05}.log"));
            rec.attributes = 0;
            index.insert_enum(&rec);
        }
        index
    }

    /// A volume whose index gets new entry indexes between two pages sends
    /// no more rows, and the pages after it say the answer is incomplete, or
    /// the client takes the short answer as whole.
    #[test]
    fn an_index_changed_between_pages_marks_the_answer_incomplete() {
        let vol = volume(r"C:\", paging_index(), VolumePhase::Live);
        let vols = set(&[Arc::clone(&vol)]);
        let mut out = Vec::new();
        serve(
            &vols,
            &defaults(),
            &ResultSnapshots::new(),
            b"",
            &Access::Sees(Visibility::Everything),
            &request_frame(&search(query("hit"))),
            &AtomicBool::new(false),
            &mut |reply| {
                if out.is_empty() {
                    let mut index = vol.index_mut();
                    let mut gone = dir_record(1000, 100, "");
                    gone.reason = gozler_core::usn::record::USN_REASON_FILE_DELETE;
                    index.apply_batch(&[gone]);
                    index.optimize_storage();
                }
                out.extend_from_slice(&reply);
                true
            },
        );
        let pages = results(&out);
        assert_eq!(pages.len(), 2);
        assert!(!pages[0].volumes_incomplete);
        assert!(pages[1].volumes_incomplete, "the rows stopped");
        assert!(pages[1].items.is_empty());
        assert_eq!(pages[1].total, u64::from(PAGE_ROWS), "the last row is out");
    }

    /// Every page must fit the 16 MiB frame cap the client decodes with, or
    /// the client refuses it and the search fails with exit 7. A page sends
    /// the folders its rows need, and [`PAGE_ROWS`] rows that each sit in a
    /// folder chain of their own pass that cap; any local user can create
    /// them.
    #[test]
    fn pages_of_long_paths_fit_the_clients_frame_cap() {
        let mut index = VolumeIndex::new(NTFS_ROOT_FRN);
        // Each row sits below 17 folders of its own with 250-character
        // names, so PAGE_ROWS rows would need 18 MB of folders.
        let mut frn = 100;
        for i in 0..PAGE_ROWS as u64 {
            let mut parent = NTFS_ROOT_FRN.0;
            for _ in 0..17 {
                index.insert_enum(&dir_record(frn, parent, &"d".repeat(250)));
                parent = frn;
                frn += 1;
            }
            let mut rec = dir_record(frn, parent, &format!("hit{i:05}.log"));
            rec.attributes = 0;
            index.insert_enum(&rec);
            frn += 1;
        }
        let vols = set(&[volume(r"C:\", index, VolumePhase::Live)]);

        let mut decoder = FrameDecoder::new(MAX_SERVER_FRAME);
        decoder.feed(&encode_query_binary(&vols, query("hit")));
        let mut names = ResultNames::default();
        let mut pages = Vec::new();
        while let Some(frame) = decoder
            .next_frame()
            .expect("every frame fits the client's cap")
        {
            match decode_response_frame(&frame, &mut names).expect("decodes") {
                Response::Results(page) => pages.push(page),
                other => panic!("expected Results, got {other:?}"),
            }
        }
        assert_eq!(
            pages.len(),
            2,
            "the byte cap splits the rows over two pages"
        );
        assert!(pages[0].more, "first page must signal more");
        assert!(!pages[1].more, "last page must be terminal");
        let names: Vec<String> = pages
            .iter()
            .flat_map(|page| &page.items)
            .map(|item| item.path.rsplit('\\').next().unwrap().to_string())
            .collect();
        let expected: Vec<String> = (0..PAGE_ROWS).map(|i| format!("hit{i:05}.log")).collect();
        assert_eq!(names, expected, "every row arrives once, in order");
    }

    /// When the page consumer reports it is gone (`emit` returns false, i.e.
    /// the client vanished or the pipe write failed), the encoder must stop
    /// instead of encoding every remaining page into the void.
    #[test]
    fn streaming_stops_when_the_consumer_goes_away() {
        let mut index = VolumeIndex::new(NTFS_ROOT_FRN);
        index.insert_enum(&dir_record(100, NTFS_ROOT_FRN.0, "many"));
        for i in 0..(PAGE_ROWS as u64 * 3) {
            let mut rec = dir_record(1000 + u128::from(i), 100, &format!("hit{i:05}.log"));
            rec.attributes = 0;
            index.insert_enum(&rec);
        }
        let vols = set(&[volume(r"C:\", index, VolumePhase::Live)]);

        let mut emitted = 0usize;
        serve(
            &vols,
            &defaults(),
            &ResultSnapshots::new(),
            b"",
            &Access::Sees(Visibility::Everything),
            &request_frame(&search(query("hit"))),
            &AtomicBool::new(false),
            &mut |_page| {
                emitted += 1;
                false // consumer gone after the first page
            },
        );
        assert_eq!(
            emitted, 1,
            "a dead consumer must stop the encoder after the rejected page"
        );
    }

    /// A size not asked for must stay out even when the index holds one,
    /// and the mount prefix must be prepended to every emitted path.
    #[test]
    fn flags_suppress_metadata_and_paths_carry_the_mount_prefix() {
        let vols = set(&[live_volume(r"C:\")]);
        let mut q = query("System32");
        q.wants.clear();

        let pages = results(&encode_query_binary(&vols, q));
        let item = &pages[0].items[0];
        assert!(item.size.is_none(), "a size not asked for must stay out");
        assert!(
            item.mtime_ft.is_none(),
            "a date modified not asked for must stay out"
        );
        assert!(
            item.path.starts_with(r"C:\"),
            "path {:?} must carry the mount prefix",
            item.path
        );
    }

    /// A volume that is not Live must taint every page, not just the first, so a
    /// client that only reads the last frame still learns the set is partial.
    #[test]
    fn incomplete_volumes_taint_every_page() {
        let vols = set(&[
            live_volume(r"C:\"),
            volume(r"D:\", windows_index(), VolumePhase::Rescanning),
        ]);
        let pages = results(&encode_query_binary(&vols, query("System32")));
        assert!(
            pages.iter().all(|p| p.volumes_incomplete),
            "every page must report the index is incomplete"
        );
    }

    /// Offset past the end is a legal request and must terminate cleanly rather
    /// than emit nothing.
    #[test]
    fn offset_and_limit_slice_the_result_set() {
        let vols = set(&[live_volume(r"C:\")]);

        let mut q = query("System32");
        q.offset = 500;
        let pages = results(&encode_query_binary(&vols, q));
        assert_eq!(pages.len(), 1);
        assert!(!pages[0].more);
        assert_eq!(
            pages[0].items.len(),
            0,
            "offset past the end yields no items"
        );

        let mut q = query("Windows");
        q.limit = Some(1);
        let pages = results(&encode_query_binary(&vols, q));
        assert_eq!(pages[0].items.len(), 1);
    }

    /// Two volumes' hits merge into one global name order. The fixture's name
    /// order is the reverse of its path order, so a merge keyed on the wrong
    /// thing cannot coincidentally produce the right answer.
    #[test]
    fn name_sort_merges_two_volumes_into_one_global_order() {
        let vols = set(&[
            volume(r"C:\", merge_index(), VolumePhase::Live),
            volume(r"D:\", merge_index(), VolumePhase::Live),
        ]);
        let pages = results(&encode_query_binary(&vols, query("log")));
        let names: Vec<&str> = pages[0]
            .items
            .iter()
            .map(|i| i.path.rsplit('\\').next().unwrap())
            .collect();

        assert_eq!(
            names,
            ["a.log", "a.log", "z.log", "z.log"],
            "merged pages must be globally ordered by name, not by path or by volume"
        );
    }

    /// Every emitted path, in wire order, across all pages.
    fn paths(bytes: &[u8]) -> Vec<String> {
        results(bytes)
            .into_iter()
            .flat_map(|p| p.items.into_iter().map(|i| i.path))
            .collect()
    }

    fn hello_request(proto_min: u16, proto_max: u16) -> Request {
        Request::Hello {
            proto_min,
            proto_max,
            client: "test".to_string(),
        }
    }

    /// Negotiation answers this build's version to a client whose range holds
    /// it, and refuses a client of an older protocol rather than answer a
    /// cheerful `ready: true` and let the mismatch surface as garbled frames
    /// later: gozlerd serves no older protocol.
    #[test]
    fn hello_answers_this_version_or_refuses() {
        let vols = set(&[live_volume(r"C:\")]);

        for range in [(1, PROTO_VERSION), (PROTO_VERSION, PROTO_VERSION)] {
            match answer(&vols, hello_request(range.0, range.1)) {
                Response::Hello { proto, .. } => assert_eq!(proto, PROTO_VERSION, "{range:?}"),
                other => panic!("{range:?} must negotiate, got {other:?}"),
            }
        }

        // An older client: its results pages lay out full paths.
        match answer(&vols, hello_request(PROTO_VERSION - 1, PROTO_VERSION - 1)) {
            Response::Error { code, .. } => assert_eq!(code, ProtoError::Unsupported),
            other => panic!("an older protocol must be refused, got {other:?}"),
        }

        // Floor above us: the client needs a protocol this build cannot speak.
        match answer(&vols, hello_request(PROTO_VERSION + 1, PROTO_VERSION + 2)) {
            Response::Error { code, .. } => assert_eq!(code, ProtoError::Unsupported),
            other => panic!("a floor above this version must be refused, got {other:?}"),
        }

        // Ceiling below us: no overlap either, and `proto: 0` would be a
        // version neither side implements.
        match answer(&vols, hello_request(0, 0)) {
            Response::Error { code, .. } => assert_eq!(code, ProtoError::Unsupported),
            other => panic!("proto_max: 0 must be refused, got {other:?}"),
        }
    }

    /// `Hello.ready` and `Status.volumes_incomplete` answer the same question,
    /// so they must never contradict each other.
    #[test]
    fn hello_ready_never_contradicts_status() {
        for vols in [
            set(&[live_volume(r"C:\")]),
            set(&[
                live_volume(r"C:\"),
                volume(r"D:\", windows_index(), VolumePhase::Bootstrapping),
            ]),
        ] {
            let Response::Hello { ready, .. } =
                answer(&vols, hello_request(PROTO_VERSION, PROTO_VERSION))
            else {
                panic!("a client of this protocol must negotiate")
            };
            assert_eq!(
                ready,
                !build_status(&vols, &defaults(), &Snapshots::new(), CallerSees::EveryFile)
                    .volumes_incomplete,
                "ready must track the same bar as volumes_incomplete"
            );
        }
    }

    /// Every way two hits can tie or nearly tie: es path order against a byte
    /// compare (`a~qx7zv` against `a\qx7zv`), equal sizes and dates, an unknown
    /// size, names equal after folding, and one file with two links of the
    /// same name.
    fn order_index() -> VolumeIndex {
        use gozler_core::layout::{LayoutFile, LayoutName};
        let mut index = VolumeIndex::new(NTFS_ROOT_FRN);
        index.insert_enum(&dir_record(100, NTFS_ROOT_FRN.0, "P"));
        index.insert_enum(&dir_record(101, 100, "a"));
        index.insert_enum(&dir_record(102, 100, "a b"));
        let mut sizeless = dir_record(205, 102, "nosize.qx7zv");
        sizeless.attributes = 0;
        index.insert_enum(&sizeless);
        for (frn, links, size) in [
            (200, &[(100, "a~qx7zv")][..], 10),
            (201, &[(101, "qx7zv")][..], 10),
            (202, &[(102, "QX7ZV")][..], 30),
            (203, &[(101, "link.qx7zv"), (102, "link.qx7zv")][..], 10),
        ] {
            index.enrich(&LayoutFile {
                frn: Frn(frn),
                attributes: 0x20,
                size: Some(size),
                mtime_ft: Some(5),
                created_ft: None,
                security: None,
                names: links
                    .iter()
                    .map(|&(parent, name)| LayoutName {
                        parent_frn: Frn(parent),
                        name: name.as_bytes().to_vec(),
                        dos_only: false,
                    })
                    .collect(),
            });
        }
        index
    }

    /// `-n K` prints the first K rows of the same query without `-n`, under
    /// every sort, on one volume and across two, whose runs are merged.
    #[test]
    fn a_limited_query_is_a_prefix_of_the_unlimited_one() {
        use gozler_core::types::SortDir;
        for vols in [
            set(&[volume(r"C:\", order_index(), VolumePhase::Live)]),
            set(&[
                volume(r"C:\", order_index(), VolumePhase::Live),
                volume(r"C:\mnt\", order_index(), VolumePhase::Live),
            ]),
        ] {
            for key in [
                SortKey::Name,
                SortKey::Path,
                SortKey::Size,
                SortKey::DateModified,
            ] {
                for dir in [SortDir::Asc, SortDir::Desc] {
                    let page = |limit| {
                        let mut q = query("qx7zv");
                        q.sort = SortSpec { key, dir };
                        q.limit = limit;
                        paths(&encode_query_binary(&vols, q))
                    };
                    let unlimited = page(None);
                    assert_eq!(unlimited.len(), 6 * vols.len());
                    for k in 0..=unlimited.len() {
                        assert_eq!(
                            page(Some(k as u32)),
                            unlimited[..k],
                            "{key:?}/{dir:?} on {} volume(s): -n {k}",
                            vols.len()
                        );
                    }
                }
            }
        }
    }

    /// `-case` (the request's `match_case`) makes matching case-sensitive, and
    /// the `-path` scope follows the same value whether it comes from the
    /// switch or from a `case:` directive.
    #[test]
    fn the_case_switch_reaches_the_matcher_and_the_scope() {
        let vols = set(&[live_volume(r"C:\")]);
        let run = |text: &str, match_case: bool, scope: Option<&str>| {
            let mut q = query(text);
            q.match_case = match_case;
            q.scope = scope.map(str::to_string);
            paths(&encode_query_binary(&vols, q))
        };
        assert!(run("system32", true, None).is_empty());
        assert_eq!(run("System32", true, None), [r"C:\Windows\System32"]);
        assert_eq!(run("system32", false, None), [r"C:\Windows\System32"]);
        // `case:0` overrides the switch.
        assert_eq!(run("system32 case:0", true, None), [r"C:\Windows\System32"]);

        assert!(run("case: System32", false, Some(r"C:\windows")).is_empty());
        assert_eq!(
            run("case: System32", false, Some(r"C:\Windows")),
            [r"C:\Windows\System32"]
        );
    }
}
