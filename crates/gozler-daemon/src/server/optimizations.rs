//! Checks that gozlerd's memory saving still holds. They run with
//! `gozler_core`'s optimization checks, in an optimized build, only when the
//! owner asks for them: `pwsh ./build.ps1 -Timing optimizations`.

use crate::run::{allocator_committed, return_freed_memory};
use std::sync::mpsc;

/// What a search frees on its own thread returns to Windows when the server
/// collects, [`super::RETURN_MEMORY_AFTER`] after the request. If mimalloc
/// kept freed memory longer than that wait, an idle gozlerd would hold the
/// buffers of its last search until the next request.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn a_search_returns_its_memory_after_the_request() {
    const SEARCH_BYTES: u64 = 256 << 20;
    const BUFFER_BYTES: usize = 64 << 10;
    let (freed, searched) = mpsc::channel();
    let (finish, finished) = mpsc::channel::<()>();
    // The search thread stays alive past the collect, as gozlerd's blocking
    // threads do.
    let search = std::thread::spawn(move || {
        let buffers: Vec<Vec<u8>> = (0..SEARCH_BYTES / BUFFER_BYTES as u64)
            .map(|_| vec![1u8; BUFFER_BYTES])
            .collect();
        let held = allocator_committed();
        drop(buffers);
        freed.send(held).expect("the test waits for it");
        finished.recv().ok();
    });
    let held = searched.recv().expect("the search thread sends it");
    std::thread::sleep(super::RETURN_MEMORY_AFTER);
    return_freed_memory();
    let idle = allocator_committed();
    finish.send(()).expect("the search thread waits for it");
    search.join().expect("the search thread ends");
    eprintln!("allocator: {held} bytes while searching, {idle} once collected");
    assert!(idle + SEARCH_BYTES / 2 <= held);
}
