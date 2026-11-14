//! Daemon orchestration shared by console (`gozlerd run`) and service
//! (`gozlerd run --service`) modes: start indexing every volume, then serve
//! queries over the pipe until stopped, while the volumes index.

use anyhow::{Context, Result};
use tokio::sync::watch;

use crate::supervisor::Supervisor;
use crate::{bootstrap, server};

/// Starts indexing every fixed NTFS and ReFS volume and returns before any is indexed.
///
/// The pipe opens right after this, so a client asking during the first
/// bootstrap gets results flagged incomplete, not "the daemon is not running".
/// Each volume is `Bootstrapping` until its tail has built its index.
pub(crate) fn start_engine() -> Result<Supervisor> {
    let enumerated = bootstrap::enumerate_all()?;
    Supervisor::spawn(
        enumerated,
        crate::include::read_saved(),
        crate::include::read_rebuild(),
    )
    .context("starting the volume supervisor")
}

/// Returns to Windows the memory freed more than mimalloc's purge delay ago,
/// for the whole process. The server calls it just past that delay after
/// each request, and the supervisor on every scan for what the tails free.
///
/// mimalloc keeps freed memory for its default delay of 1 s before it
/// returns it, and only an allocation or a collect after that returns it, so
/// an idle gozlerd would keep the buffers of its last search. Purging at once
/// instead (`purge_delay` 0) measured 16 to 28 % more CPU for broad searches
/// repeated over 2 million entries (`query_2M_broad_none`), because each
/// search commits again what the last one gave back; with the delay, the
/// searches a person types in quick succession reuse it.
pub(crate) fn return_freed_memory() {
    // SAFETY: mi_collect takes no pointers and is documented safe to call
    // from any thread at any time.
    unsafe { libmimalloc_sys::mi_collect(false) };
}

/// The bytes mimalloc holds from Windows now, in use or freed and not yet
/// returned, which `--status` reports; 0 when mimalloc does not say.
/// mimalloc counts it in every build, unlike its other statistics.
pub(crate) fn allocator_committed() -> u64 {
    // SAFETY: with no buffer, mimalloc allocates the text with its own
    // allocator and returns it NUL-terminated, or null.
    let text = unsafe { libmimalloc_sys::mi_stats_get_json(0, std::ptr::null_mut()) };
    if text.is_null() {
        return 0;
    }
    // SAFETY: `text` is the NUL-terminated buffer mimalloc just returned,
    // read before it is freed below.
    let json = unsafe { std::ffi::CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: `text` came from mimalloc's allocator and is freed once.
    unsafe { libmimalloc_sys::mi_free(text.cast()) };
    serde_json::from_str::<serde_json::Value>(&json)
        .ok()
        .and_then(|stats| stats["committed"]["current"].as_u64())
        .unwrap_or(0)
}

/// Best-effort post-bootstrap memory settling, which the supervisor runs once
/// no volume is `Bootstrapping`. Never fatal: logs and continues.
///
/// Default: purge the allocator and trim the working set once. Pages return
/// on first touch as soft faults, spread over the first queries, and the
/// process's visible memory drops to what queries actually touch instead of
/// the bootstrap peak.
///
/// `GOZLER_PIN_WORKING_SET=1` opts into the opposite trade: pin the resident set
/// as a hard minimum so a low-memory burst can never page the index out and
/// no query ever pays a soft-fault storm. For latency-sensitive setups.
pub(crate) fn settle_working_set() {
    // Have mimalloc return freed bootstrap memory to the OS first: whichever
    // branch follows, transient bootstrap garbage (enumeration buffers,
    // pre-shrink slack) must not be counted, pinned, or trimmed-and-refaulted.
    // SAFETY: mi_collect takes no pointers and is documented safe to call
    // from any thread at any time.
    unsafe { libmimalloc_sys::mi_collect(true) };
    let pin = std::env::var_os("GOZLER_PIN_WORKING_SET").is_some_and(|v| v == "1");
    if pin {
        match gozler_winfs::pin_working_set() {
            Ok(()) => tracing::info!("working set pinned (index kept resident)"),
            // Best-effort: the memory manager may refuse a large hard minimum.
            // The index still works, so this is a quiet note, not a warning.
            Err(e) => tracing::debug!(error = %e, "working set not pinned (best-effort)"),
        }
    } else {
        match gozler_winfs::trim_working_set() {
            Ok(()) => {
                tracing::info!("working set trimmed once (GOZLER_PIN_WORKING_SET=1 to pin instead)")
            }
            Err(e) => tracing::debug!(error = %e, "working set not trimmed (best-effort)"),
        }
    }
}

/// Console entry: start indexing, serve on the pipe, and stop on Ctrl+C.
pub(crate) fn run_console() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    if !gozler_winfs::is_elevated() {
        tracing::warn!("gozlerd is not elevated; raw volume access will likely fail");
    }

    let supervisor = start_engine()?;
    tracing::info!(
        volumes = supervisor.volumes().current().len(),
        "serving queries while the volumes index (Ctrl+C to stop)"
    );

    // A never-triggered channel: console stop comes from Ctrl+C inside the
    // server. `_stop_tx` is held so the receiver never sees a closed channel.
    let (_stop_tx, stop_rx) = watch::channel(false);
    let result = server::run(supervisor.volumes(), supervisor.include(), stop_rx);

    supervisor.shutdown();
    result
}

#[cfg(test)]
mod tests {
    /// `--status` splits the daemon's memory by what the allocator holds, which
    /// it reads by name from mimalloc's statistics; a mimalloc update that
    /// renames or drops the figure would silently remove that line.
    #[test]
    fn the_allocator_reports_what_it_holds() {
        let _held = vec![0u8; 1 << 20];
        assert!(super::allocator_committed() >= 1 << 20);
    }
}
