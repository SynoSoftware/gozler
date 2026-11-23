//! Query-latency benchmark over a synthetic multi-million-entry index.
//!
//! Builds a tree resembling a real volume (nested directories, varied file
//! names, a known fraction containing a rare token) and measures `run_query`
//! for rare, medium, and wildcard needles, what a user who cannot list half
//! the folders pays for a broad search, then what keeping folder sizes costs
//! over the same tree, and what a sort change in the window asks. Over a
//! volume shaped like a system drive, it measures a folder tree to a depth
//! and the `repeats:` test, and what applying a journal batch costs.
//! Run with `cargo bench -p gozler-core`.

// Measure the effect of a thread-caching allocator on the alloc-heavy broad
// reconstruction. Toggle by commenting this out to compare against the system heap.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod measure;

use criterion::{Criterion, criterion_group, criterion_main};
use gozler_core::index::settle::{Batch, SharedIndex};
use gozler_core::index::volume::{Room, make_room};
use gozler_core::index::writes::Writes;
use gozler_core::index::{NTFS_ROOT_FRN, VolumeIndex};
use gozler_core::links::{FolderLink, Links};
use gozler_core::query::{
    CompiledQuery, HeldVolume, IndexedVolume, ParseOptions, ResultRows, ResultSnapshot, RowsSpec,
    TermMatch, Viewer, parse_query, parse_query_with, run_query,
};
use gozler_core::scope::VolumeMounts;
use gozler_core::types::{Frn, SortDir, SortKey, SortSpec};
use gozler_core::usn::record::{
    FILE_ATTRIBUTE_DIRECTORY, ParsedUsnRecord, USN_REASON_CLOSE, USN_REASON_DATA_EXTEND,
    USN_REASON_FILE_CREATE, USN_REASON_FILE_DELETE, USN_REASON_RENAME_NEW_NAME,
};
use parking_lot::{RwLock, RwLockUpgradableReadGuard};
use std::hint::black_box;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// Deep, long-named tree (WinSxS / node_modules-like) so path reconstruction pays
// a realistic ~6-hop parent walk per file, not the 2-3 hops a flat tree gives.
const L1: u64 = 24;
const L2: u64 = 14;
const L3: u64 = 12;
const L4: u64 = 10;
const FILES_PER_LEAF: u64 = 52; // 24*14*12*10*52 ~= 2.1M files at depth 5+

fn frn(rec: u64) -> Frn {
    Frn::ntfs(rec, 1)
}

fn enum_rec(f: Frn, parent: Frn, name: &str, is_dir: bool) -> ParsedUsnRecord {
    ParsedUsnRecord {
        frn: f,
        parent_frn: parent,
        reason: USN_REASON_FILE_CREATE,
        attributes: if is_dir { FILE_ATTRIBUTE_DIRECTORY } else { 0 },
        name: name.as_bytes().to_vec(),
        time: 0,
    }
}

/// The descriptor of every folder a filtered user may list.
const OPEN: &[u8] = b"open";
/// The descriptor of every other folder: every second leaf folder.
const CLOSED: &[u8] = b"closed";

/// Builds a deep synthetic index (5 directory levels with long WinSxS-style
/// names). Every 10,000th file's name contains the rare token "kernel32" so a
/// rare query returns a realistic ~200 hits. Every second leaf folder has the
/// descriptor [`CLOSED`], and every other folder [`OPEN`].
fn build_index() -> (VolumeIndex, usize) {
    let mut idx = VolumeIndex::new(NTFS_ROOT_FRN);
    idx.set_folder_descriptor(NTFS_ROOT_FRN, OPEN);
    let mut next: u64 = 1000;
    let mut count = 0usize;

    for a in 0..L1 {
        let d1 = frn(next);
        next += 1;
        idx.insert_enum(&enum_rec(d1, NTFS_ROOT_FRN, &format!("amd64_microsoft-windows-component-{a:03}_31bf3856ad364e35_10.0.26100.1_none_deadbeef"), true));
        idx.set_folder_descriptor(d1, OPEN);
        for b in 0..L2 {
            let d2 = frn(next);
            next += 1;
            idx.insert_enum(&enum_rec(
                d2,
                d1,
                &format!("subcomponent_{b:03}_none_cafef00dbaadf00d"),
                true,
            ));
            idx.set_folder_descriptor(d2, OPEN);
            for cc in 0..L3 {
                let d3 = frn(next);
                next += 1;
                idx.insert_enum(&enum_rec(d3, d2, &format!("module_group_{cc:03}"), true));
                idx.set_folder_descriptor(d3, OPEN);
                for e in 0..L4 {
                    let d4 = frn(next);
                    next += 1;
                    idx.insert_enum(&enum_rec(d4, d3, &format!("bin_x64_{e:02}"), true));
                    let leaf = if e % 2 == 0 { OPEN } else { CLOSED };
                    idx.set_folder_descriptor(d4, leaf);
                    for fidx in 0..FILES_PER_LEAF {
                        let file = frn(next);
                        let seq = next;
                        next += 1;
                        let name = if seq.is_multiple_of(10_000) {
                            format!("kernel32_{seq}.dll")
                        } else {
                            format!("document_{seq}_report_v{fidx}.txt")
                        };
                        idx.insert_enum(&enum_rec(file, d4, &name, false));
                        // Scattered sizes and dates, so a size or date sort
                        // compares real keys.
                        let mix = seq.wrapping_mul(0x9E37_79B9_7F4A_7C15);
                        idx.set_stat(file, mix % 10_000_000, (mix >> 24) as i64);
                        count += 1;
                    }
                }
            }
        }
    }
    idx.rank_names();
    (idx, count)
}

fn compiled(query: &str) -> CompiledQuery {
    CompiledQuery::new(&parse_query(query).unwrap()).unwrap()
}

fn bench_query(c: &mut Criterion<measure::Measure>) {
    let (idx, count) = build_index();
    let total = idx.len();
    eprintln!("built index: {count} files, {total} live entries");

    let mut group = c.benchmark_group("query_2M");
    group.sample_size(20);

    for (target, match_path) in [("names", false), ("paths", true)] {
        for (version, ordered) in [("before", false), ("after", true)] {
            let parsed = parse_query_with(
                "doc rep",
                ParseOptions {
                    match_path,
                    ordered_terms: ordered,
                    term_match: TermMatch::WordStart,
                    ..ParseOptions::default()
                },
            )
            .unwrap();
            let parsed = CompiledQuery::new(&parsed).unwrap();
            let total =
                run_query(&idx, r"C:\", &parsed, None, SortSpec::default(), Some(100)).total;
            assert!(
                total > 2_000_000,
                "the broad comparison must exercise over two million matches"
            );
            eprintln!("ordering/{target}/{version}: {total} matches");
            group.bench_function(format!("ordering/{target}/{version}"), |b| {
                b.iter(|| {
                    let out = run_query(
                        black_box(&idx),
                        r"C:\",
                        black_box(&parsed),
                        None,
                        SortSpec::default(),
                        Some(100),
                    );
                    black_box(out.total)
                })
            });
        }
    }

    for (label, query) in [
        ("rare(kernel32)", "kernel32"),
        ("medium(report)", "report"),
        ("common(document)", "document"),
        ("wildcard(*.dll)", "*.dll"),
        ("two-term(report txt)", "report txt"),
        // No name term to drive the arena scan, so every entry's full path
        // is tested.
        (
            "path-term(group_007\\bin_x64_03)",
            r"module_group_007\bin_x64_03",
        ),
    ] {
        let parsed = compiled(query);
        group.bench_function(label, |b| {
            b.iter(|| {
                let out = run_query(
                    black_box(&idx),
                    r"C:\",
                    black_box(&parsed),
                    None,
                    SortSpec::default(),
                    Some(100),
                );
                black_box(out.total)
            })
        });
    }

    // The path term above through a junction to one leaf folder, as gozlerd
    // compiles it: the entries outside the target must cost what they cost
    // without links.
    let links = Links::new(vec![FolderLink {
        link: r"C:\elsewhere".into(),
        target: r"C:\amd64_microsoft-windows-component-000_31bf3856ad364e35_10.0.26100.1_none_deadbeef\subcomponent_000_none_cafef00dbaadf00d\module_group_000\bin_x64_00".into(),
    }]);
    let parsed = parse_query(r"module_group_007\bin_x64_03").unwrap();
    let parsed = CompiledQuery::with_links(&parsed, &links, None).unwrap();
    group.bench_function("path-term(group_007\\bin_x64_03) one link", |b| {
        b.iter(|| {
            let out = run_query(
                black_box(&idx),
                r"C:\",
                black_box(&parsed),
                None,
                SortSpec::default(),
                Some(100),
            );
            black_box(out.total)
        })
    });

    // With -p every term tests the full path. `kernel32` lies in file names,
    // `module_group_007` in folder names, so their subtrees match too,
    // `bin_x64_03` in folder names that 210,000 files are below, and
    // `report` in nearly every file name. Start of name makes each word a
    // `startwith:` of the full path, which only the root's name decides.
    let match_path = ParseOptions {
        match_path: true,
        ..ParseOptions::default()
    };
    for (label, query) in [
        ("match-path(kernel32)", "kernel32"),
        ("match-path(module_group_007)", "module_group_007"),
        ("match-path(bin_x64_03 kernel32)", "bin_x64_03 kernel32"),
        ("match-path(report)", "report"),
        ("match-path(startwith:kernel32)", "startwith:kernel32"),
    ] {
        let parsed = CompiledQuery::new(&parse_query_with(query, match_path).unwrap()).unwrap();
        group.bench_function(label, |b| {
            b.iter(|| {
                let out = run_query(
                    black_box(&idx),
                    r"C:\",
                    black_box(&parsed),
                    None,
                    SortSpec::default(),
                    Some(100),
                );
                black_box(out.total)
            })
        });
    }

    // A volume root as the `-path` scope does the same work as no scope.
    let parsed = compiled("document");
    group.bench_function("common(document) scope=root", |b| {
        b.iter(|| {
            let out = run_query(
                black_box(&idx),
                r"C:\",
                black_box(&parsed),
                Some(r"C:\"),
                SortSpec::default(),
                Some(100),
            );
            black_box(out.total)
        })
    });
    // Ordering reads each compared candidate's size from the index.
    group.bench_function("common(document) sort=size", |b| {
        b.iter(|| {
            let out = run_query(
                black_box(&idx),
                r"C:\",
                black_box(&parsed),
                None,
                SortSpec::default_for(SortKey::Size),
                Some(100),
            );
            black_box(out.total)
        })
    });
    group.finish();

    // Broad limit=None: reconstructs and sorts the ENTIRE match set (~2M paths),
    // the "dump everything" workload (export / `gozler foo | wc`) where path
    // reconstruction dominates.
    let mut broad = c.benchmark_group("query_2M_broad_none");
    broad.sample_size(10);
    for (label, query) in [
        ("common(document)", "document"),
        ("wildcard(*.txt)", "*.txt"),
    ] {
        let parsed = compiled(query);
        broad.bench_function(label, |b| {
            b.iter(|| {
                let out = run_query(
                    black_box(&idx),
                    r"C:\",
                    black_box(&parsed),
                    None,
                    SortSpec::default(),
                    None,
                );
                black_box(out.rows.len())
            })
        });
    }

    // The same search, read as gozlerd reads it, for a user who sees every
    // file and for one who cannot list half the leaf folders, who pays one
    // check per descriptor and one parent lookup per candidate.
    let parsed = compiled("document");
    let may_list = |descriptor: &[u8]| descriptor == OPEN;
    for (label, viewer) in [
        ("search(document)", Viewer::Everything),
        ("search(document) caller(half)", Viewer::Filtered(&may_list)),
    ] {
        broad.bench_function(label, |b| {
            b.iter(|| black_box(search_rows(&idx, &parsed, None, viewer)))
        });
    }

    broad.finish();
}

fn bench_linked_release(c: &mut Criterion<measure::Measure>) {
    let (mut index, _) = build_index();
    for (id, parent, name, folder) in [
        (3_000_000, NTFS_ROOT_FRN, "artifacts", true),
        (3_000_001, frn(3_000_000), "TinyTorrent2", true),
        (3_000_002, frn(3_000_001), "release", true),
    ] {
        index.insert_enum(&enum_rec(frn(id), parent, name, folder));
    }
    for n in 0..17 {
        index.insert_enum(&enum_rec(
            frn(3_000_003 + n),
            frn(3_000_002),
            &format!("TinyTorrent-{n}.exe"),
            false,
        ));
    }
    index.rank_names();
    let links = vec![FolderLink {
        link: r"C:\SynoSoftware\TinyTorrent2\artifacts".into(),
        target: r"V:\artifacts\TinyTorrent2".into(),
    }];
    let options = ParseOptions {
        match_path: true,
        ..ParseOptions::default()
    };
    let mut group = c.benchmark_group("linked_release_2M");
    group.sample_size(20);
    for (name, text) in [
        ("relative", r"artifacts\release\"),
        (
            "absolute",
            r"C:\SynoSoftware\TinyTorrent2\artifacts\release\",
        ),
        (
            "wildcard",
            r"C:\SynoSoftware\TinyTorrent2\artifacts\release\*.exe",
        ),
    ] {
        let parsed = parse_query_with(text, options).unwrap();
        group.bench_function(name, |b| {
            b.iter(|| {
                let table = Links::new(links.clone());
                let query = CompiledQuery::with_links(&parsed, &table, None).unwrap();
                let answer = run_query(
                    black_box(&index),
                    r"V:\",
                    &query,
                    None,
                    SortSpec::default(),
                    Some(100),
                );
                assert_eq!(answer.total, 17);
                black_box(answer.total)
            })
        });
    }
    group.finish();
}

/// Reads every row of `query` below `scope` over `idx` as `viewer` sees it,
/// and returns how many rows the search found.
fn search_rows(
    idx: &VolumeIndex,
    query: &CompiledQuery,
    scope: Option<&str>,
    viewer: Viewer<'_>,
) -> u64 {
    let volume = BenchVolume {
        index: idx,
        mounts: [r"C:\".to_string()],
    };
    let spec = RowsSpec {
        sort: SortSpec::default(),
        offset: 0,
        limit: None,
        sums: false,
    };
    let rows = ResultRows::search(
        vec![volume],
        query,
        scope,
        spec,
        viewer,
        &AtomicBool::new(false),
    )
    .expect("the scope is on the volume");
    let total = rows.total();
    read_pages(rows);
    total
}

/// The bench's index, searched as gozlerd searches a volume.
struct BenchVolume<'a> {
    index: &'a VolumeIndex,
    mounts: [String; 1],
}

impl IndexedVolume for BenchVolume<'_> {
    type Index<'h>
        = &'h VolumeIndex
    where
        Self: 'h;

    fn read(&self) -> HeldVolume<&VolumeIndex> {
        HeldVolume {
            index: self.index,
            complete: true,
            metadata_pending: false,
        }
    }

    fn mounts(&self) -> VolumeMounts<'_> {
        VolumeMounts {
            mounts: &self.mounts,
            guid: "",
        }
    }
}

/// The rows of the window's first chunk.
const CHUNK_ROWS: u32 = 16_384;

/// Reads every page of `rows`, as gozlerd sends them.
fn read_pages<V: IndexedVolume>(mut rows: ResultRows<V>) -> u64 {
    let mut read = 0u64;
    while rows.page(4096, |row, _| {
        read += black_box(row.name.len()) as u64;
        true
    }) {}
    read
}

/// What a sort change asks of gozlerd while the list reads no result
/// snapshot: a search for the first chunk in the new sort, with the sums.
#[inline(never)]
fn sort_by_search(volume: &Arc<BenchVolume>, query: &CompiledQuery, sort: SortSpec) -> u64 {
    let spec = RowsSpec {
        sort,
        offset: 0,
        limit: Some(CHUNK_ROWS),
        sums: true,
    };
    let rows = ResultRows::search(
        vec![volume.clone()],
        query,
        None,
        spec,
        Viewer::Everything,
        &AtomicBool::new(false),
    )
    .expect("no scope");
    read_pages(rows)
}

/// What a sort change asks once the list reads a result snapshot: that one
/// sorted again, kept as a new one, and its first chunk read.
#[inline(never)]
fn sort_snapshot_again(
    snapshot: &ResultSnapshot,
    volume: &Arc<BenchVolume>,
    sort: SortSpec,
) -> u64 {
    let sorted = snapshot
        .sort_again(
            vec![volume.clone()],
            sort,
            Viewer::Everything,
            &AtomicBool::new(false),
        )
        .expect("the index stays");
    let (sorted, volumes) = sorted
        .into_snapshot(&AtomicBool::new(false))
        .expect("one volume");
    let mut rows = sorted.read(volumes, 0, Some(CHUNK_ROWS), Viewer::Everything);
    let mut read = 0u64;
    while rows
        .page(4096, |_, _| {
            read += 1;
            true
        })
        .expect("the index stays")
    {}
    read
}

/// The two requests of a sort change in the window, over the empty search,
/// which matches every entry.
fn bench_sort_change(c: &mut Criterion<measure::Measure>) {
    let (idx, _) = build_index();
    let volume = Arc::new(BenchVolume {
        index: &idx,
        mounts: [r"C:\".to_string()],
    });
    let query = compiled("");
    let by_name = RowsSpec {
        sort: SortSpec::default(),
        offset: 0,
        limit: None,
        sums: false,
    };
    let (snapshot, _) = ResultRows::search(
        vec![volume.clone()],
        &query,
        None,
        by_name,
        Viewer::Everything,
        &AtomicBool::new(false),
    )
    .expect("no scope")
    .into_snapshot(&AtomicBool::new(false))
    .expect("one volume");
    let by_size = SortSpec {
        key: SortKey::Size,
        dir: SortDir::Desc,
    };
    let mut group = c.benchmark_group("sort_change_2M");
    group.sample_size(10);
    group.bench_function("search by size", |b| {
        b.iter(|| sort_by_search(&volume, &query, by_size))
    });
    group.bench_function("result snapshot sorted by size", |b| {
        b.iter(|| sort_snapshot_again(&snapshot, &volume, by_size))
    });
    group.finish();
}

/// What folder sizes cost: summing every folder once, when a build's size and
/// date pass ends, and a file's size change while the folder sizes are still
/// unknown and once they are kept, which is the extra each journal change
/// pays.
fn bench_folder_sizes(c: &mut Criterion<measure::Measure>) {
    let (mut idx, _) = build_index();
    // The first file `build_index` makes, with five folders above it, the
    // root included, as every file of the index has.
    let file = frn(1004);
    let mut size = 0u64;
    let mut group = c.benchmark_group("folder_sizes_2M");
    group.sample_size(10);
    group.bench_function("set_stat, sizes unknown", |b| {
        b.iter(|| {
            size ^= 1;
            idx.set_stat(black_box(file), size, 0);
        })
    });
    group.bench_function("compute_folder_sizes", |b| {
        b.iter(|| idx.compute_folder_sizes())
    });
    group.bench_function("set_stat, sizes kept", |b| {
        b.iter(|| {
            size ^= 1;
            idx.set_stat(black_box(file), size, 0);
        })
    });
    group.finish();
}

/// An index that [`build_system_volume`] fills, and the next file record
/// number it gives.
struct TreeBuilder {
    idx: VolumeIndex,
    next: u64,
}

impl TreeBuilder {
    /// Adds a folder named `name` in `parent`.
    fn folder(&mut self, parent: Frn, name: &str) -> Frn {
        let folder = frn(self.next);
        self.next += 1;
        self.idx.insert_enum(&enum_rec(folder, parent, name, true));
        folder
    }

    /// Adds below `parent` `widths[0]` folders, in each of them `widths[1]`
    /// folders, and so on, and `files` files in each folder of the last
    /// level. Returns how many folders it added.
    fn tree(&mut self, parent: Frn, widths: &[u64], files: u64) -> u64 {
        let Some((&width, deeper)) = widths.split_first() else {
            for _ in 0..files {
                let name = format!("file_{}.dat", self.next);
                self.idx
                    .insert_enum(&enum_rec(frn(self.next), parent, &name, false));
                self.next += 1;
            }
            return 0;
        };
        let mut folders = 0;
        for n in 0..width {
            let folder = self.folder(parent, &format!("folder_{n:02}"));
            folders += 1 + self.tree(folder, deeper, files);
        }
        folders
    }
}

/// Builds a volume shaped like a system drive: a big `C:\Windows`, a small
/// `C:\ProgramData`, and `C:\Users` with the link folders `All Users` and
/// `Default User` and profiles that are several folders deep, with files in
/// each deepest folder. `C:\Users` holds so much of the volume that a path
/// term's candidate walk below it gives up without a depth bound. Returns
/// the index and how many folders `C:\Users` holds.
fn build_system_volume() -> (VolumeIndex, u64) {
    let mut volume = TreeBuilder {
        idx: VolumeIndex::new(NTFS_ROOT_FRN),
        next: 1000,
    };
    let windows = volume.folder(NTFS_ROOT_FRN, "Windows");
    volume.tree(windows, &[50, 40], 860);
    let program_data = volume.folder(NTFS_ROOT_FRN, "ProgramData");
    volume.tree(program_data, &[30, 20], 19);
    let users = volume.folder(NTFS_ROOT_FRN, "Users");
    let mut folders = 0;
    for link in ["All Users", "Default User"] {
        volume.folder(users, link);
        folders += 1;
    }
    for profile in ["owner", "Public", "Default", "defaultuser0"] {
        let profile = volume.folder(users, profile);
        folders += 1 + volume.tree(profile, &[24, 12, 10, 8], 8);
    }
    volume.idx.rank_names();
    (volume.idx, folders)
}

/// A folder tree to a depth with every row read, as TinyTorrent's Library
/// folders setting asks for it: `gozler -path C:\Users /ad depth:<=3 -n all`.
/// On a real system drive `C:\Users\All Users` leads to `C:\ProgramData`, so
/// the search reads the whole volume and also finds what is below that
/// link. Each case prints its wall time as well, at best of a few runs.
fn bench_folder_tree(c: &mut Criterion<measure::Measure>) {
    let (idx, folders_in_users) = build_system_volume();
    eprintln!("built system volume: {} live entries", idx.len());
    let scope = r"C:\Users";
    let all_users = Links::new(vec![FolderLink {
        link: r"C:\Users\All Users".into(),
        target: r"C:\ProgramData".into(),
    }]);
    let mut group = c.benchmark_group("folder_tree_2.6M");
    group.sample_size(10);
    // Folders to depth 3: the two link folders, the profiles and the 24
    // folders in each. Through the link: `C:\ProgramData` itself, its 30
    // folders and the 20 folders in each.
    for (label, text, links, rows) in [
        ("depth<=3", "folder: depth:<=3", Links::default(), 102),
        (
            "depth<=3 link out",
            "folder: depth:<=3",
            all_users,
            102 + 1 + 30 + 600,
        ),
        ("every depth", "folder:", Links::default(), folders_in_users),
    ] {
        let parsed = parse_query(text).unwrap();
        let query = CompiledQuery::with_links(&parsed, &links, Some(scope)).unwrap();
        let search = || search_rows(&idx, &query, Some(scope), Viewer::Everything);
        let mut best = Duration::MAX;
        for _ in 0..5 {
            let start = Instant::now();
            let found = search();
            best = best.min(start.elapsed());
            assert_eq!(found, rows, "{label}");
        }
        eprintln!(
            "folder_tree_2.6M/{label}: {rows} rows in {:.3} ms",
            best.as_secs_f64() * 1000.0
        );
        group.bench_function(label, |b| b.iter(|| black_box(search())));
    }
    group.finish();
}

/// `repeats:` over the same volume. Its generated folders repeat their
/// names at each level, as nested `node_modules` folders do, so the test's
/// second pass runs on many folders, and the profiles hold folders whose
/// name repeats 4 times.
fn bench_repeats(c: &mut Criterion<measure::Measure>) {
    let (idx, _) = build_system_volume();
    let mut group = c.benchmark_group("repeats_2.6M");
    group.sample_size(10);
    // `folder:` alone is the scan that the two others pay for too.
    for text in ["folder:", "folder: repeats:4", "folder: repeats:"] {
        let query = compiled(text);
        let search = || search_rows(&idx, &query, None, Viewer::Everything);
        eprintln!("repeats_2.6M/{text}: {} rows", search());
        group.bench_function(text, |b| b.iter(|| black_box(search())));
    }
    group.finish();
}

/// What one journal batch costs to apply: 5,000 records over the index of
/// 2.1 million entries. A batch of saves saves 1,250 names in one folder as
/// a program does through a temporary file, in groups of 4 records with
/// `CLOSE`: a new file is created and written, the file that holds the name
/// is deleted, and the new file is renamed to the name. The next batch saves
/// again with the two files of each name swapped, so the index keeps its
/// size. The two batches are built before the timing, as `settle_batch`
/// builds a batch before it takes the write lock, so the timing is the
/// lock's hold. The batches run first with no table of writes and then with
/// the table a build installs; the difference, divided by 5,000, is what
/// counting costs a record.
fn bench_journal_batch(c: &mut Criterion<measure::Measure>) {
    let (mut idx, _) = build_index();
    let folder = frn(1000);
    let record = |file: u64, name: String, reason: u32| ParsedUsnRecord {
        frn: frn(file),
        parent_frn: folder,
        reason: reason | USN_REASON_CLOSE,
        attributes: 0,
        name: name.into_bytes(),
        time: 0,
    };
    let (first, second) = (50_000_000, 50_100_000);
    let holders: Vec<ParsedUsnRecord> = (0..1_250)
        .map(|n| record(first + n, format!("saved_{n}.dat"), USN_REASON_FILE_CREATE))
        .collect();
    idx.apply_batch(&Batch::of(&holders));
    let save = |old: u64, new: u64| {
        (0..1_250)
            .flat_map(|n| {
                let (temporary, name) = (format!("saved_{n}.tmp"), format!("saved_{n}.dat"));
                [
                    record(new + n, temporary.clone(), USN_REASON_FILE_CREATE),
                    record(new + n, temporary, USN_REASON_DATA_EXTEND),
                    record(old + n, name.clone(), USN_REASON_FILE_DELETE),
                    record(new + n, name, USN_REASON_RENAME_NEW_NAME),
                ]
            })
            .collect::<Vec<_>>()
    };
    let records = [save(first, second), save(second, first)];
    let batches = [Batch::of(&records[0]), Batch::of(&records[1])];
    // The two batches in turn, from the first, which replaces the holders.
    let mut saved = 0;
    let mut apply = |idx: &mut VolumeIndex| {
        saved += 1;
        idx.apply_batch(&batches[(saved - 1) % 2])
    };
    let mut group = c.benchmark_group("journal_batch_2.1M");
    group.sample_size(20);
    group.bench_function("5000 saves", |b| b.iter(|| black_box(apply(&mut idx))));
    let mut table = Writes::for_entries(idx.len());
    table.keep(idx.writes_mut(), 0);
    *idx.writes_mut() = table;
    group.bench_function("5000 saves counted", |b| {
        b.iter(|| black_box(apply(&mut idx)))
    });
    group.finish();
}

/// The longest write lock a journal batch holds when it grows the index's
/// storage, measured in matched pairs over the same batches: "in lock" grows
/// the buffers in the batch's hold, as `apply_batch` alone does, and "room"
/// runs `make_room` first, as `settle_batch` does. Each batch also reports
/// how far the commit charge rose above where it started. Runs only when the
/// filter names `growth`, because it builds a volume of 2.6 million entries
/// for each run and prints its own table: three pairs, then one pair with
/// three threads searching the volume, which reports how long a search
/// waits for the lock.
fn bench_growth_hold(_: &mut Criterion<measure::Measure>) {
    if !std::env::args().any(|arg| arg.contains("growth")) {
        return;
    }
    let commit = CommitPeak::start();
    for run in 0..3 {
        for room in [false, true] {
            growth_run(run, room, 0, &commit);
        }
    }
    for room in [false, true] {
        growth_run(3, room, 3, &commit);
    }
}

/// One run of [`bench_growth_hold`]: 24 batches of 5,000 new files, with
/// `make_room` before each batch when `room`, while `readers` threads search.
fn growth_run(run: usize, room: bool, readers: u64, commit: &CommitPeak) {
    const BATCH: u64 = 5_000;
    const BATCHES: u64 = 24;
    let (mut idx, _) = build_system_volume();
    idx.optimize_storage();
    let entries = idx.len();
    let first = entries as u64 + 1_100;
    let folder = frn(1000);
    let locked = Locked {
        index: RwLock::new(idx),
        longest_swap: AtomicU64::new(0),
    };
    let (stop, waits) = (AtomicBool::new(false), Mutex::new(Vec::new()));
    let series = std::thread::scope(|scope| {
        for reader in 0..readers {
            let (locked, stop, waits) = (&locked, &stop, &waits);
            scope.spawn(move || {
                // A rare needle scans the whole folded arena, as a search does.
                let query = compiled("zqxjv");
                std::thread::sleep(Duration::from_millis(reader * 3));
                let mut mine = Vec::new();
                while !stop.load(Relaxed) {
                    let asked = Instant::now();
                    let index = locked.index.read();
                    mine.push(asked.elapsed());
                    black_box(search_rows(&index, &query, None, Viewer::Everything));
                }
                waits.lock().expect("no reader panics").extend(mine);
            });
        }
        let mut series = Vec::new();
        for batch in 0..BATCHES {
            let records: Vec<ParsedUsnRecord> = (0..BATCH)
                .map(|n| {
                    let file = batch * BATCH + n;
                    ParsedUsnRecord {
                        frn: frn(first + file),
                        parent_frn: folder,
                        reason: USN_REASON_FILE_CREATE | USN_REASON_CLOSE,
                        attributes: 0,
                        name: format!("grown_{file}.log").into_bytes(),
                        time: 0,
                    }
                })
                .collect();
            let start = commit.reset();
            if room {
                make_room(&locked, &Room::of(&records));
            }
            let batch = Batch::of(&records);
            let hold = locked.write(|i| {
                let cycles = measure::thread_cycles();
                black_box(i.apply_batch(&batch));
                measure::thread_cycles() - cycles
            });
            series.push((hold, commit.peak().saturating_sub(start)));
        }
        stop.store(true, Relaxed);
        series
    });
    let mut holds: Vec<u64> = series.iter().map(|s| s.0).collect();
    holds.sort_unstable();
    let (normal, worst) = (holds[holds.len() / 2], holds[holds.len() - 1]);
    let commit_rise = series.iter().map(|s| s.1).max().expect("batches ran");
    let path = if room { "room" } else { "in lock" };
    eprintln!(
        "growth run {run} {path}: {entries} entries, median batch {:.1} M cycles, worst {:.1} M = {:.2}x, longest swap {:.0} K cycles, highest commit rise {:.0} MB",
        normal as f64 / 1e6,
        worst as f64 / 1e6,
        worst as f64 / normal as f64,
        locked.longest_swap.load(Relaxed) as f64 / 1e3,
        commit_rise as f64 / 1e6,
    );
    eprintln!(
        "  per batch (M cycles / commit rise MB): {}",
        series
            .iter()
            .map(|(c, m)| format!("{:.0}/{:.0}", *c as f64 / 1e6, *m as f64 / 1e6))
            .collect::<Vec<_>>()
            .join("  ")
    );
    let waits = waits.into_inner().expect("no reader panics");
    if let Some(longest) = waits.iter().max() {
        let over = waits
            .iter()
            .filter(|w| **w > Duration::from_millis(1))
            .count();
        eprintln!(
            "  {readers} readers: {} searches, longest wait for the lock {:.1} ms, {over} waits over 1 ms",
            waits.len(),
            longest.as_secs_f64() * 1e3
        );
    }
}

/// The index behind the daemon's lock, with `replace` as `VolumeState` has
/// it, and the longest swap that `replace` held the exclusive lock for.
struct Locked {
    index: RwLock<VolumeIndex>,
    longest_swap: AtomicU64,
}

impl SharedIndex for Locked {
    fn read<R>(&self, f: impl FnOnce(&VolumeIndex) -> R) -> R {
        f(&self.index.read())
    }

    fn write<R>(&self, f: impl FnOnce(&mut VolumeIndex) -> R) -> R {
        f(&mut self.index.write())
    }

    fn replace<P, O>(
        &self,
        plan: impl FnOnce(&VolumeIndex) -> Option<P>,
        install: impl FnOnce(&mut VolumeIndex, P) -> O,
    ) -> bool {
        let read = self.index.upgradable_read();
        let Some(new) = plan(&read) else {
            return false;
        };
        let mut index = RwLockUpgradableReadGuard::upgrade(read);
        let cycles = measure::thread_cycles();
        let old = install(&mut index, new);
        self.longest_swap
            .fetch_max(measure::thread_cycles() - cycles, Relaxed);
        drop(index);
        drop(old);
        true
    }
}

/// The process's highest commit charge since the last [`Self::reset`], which
/// a thread samples as fast as it can, because a batch's peak lasts only
/// while it copies.
struct CommitPeak {
    peak: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    sampler: Option<std::thread::JoinHandle<()>>,
}

impl CommitPeak {
    fn start() -> Self {
        let (peak, stop) = (
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicBool::new(false)),
        );
        let sampler = {
            let (peak, stop) = (Arc::clone(&peak), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !stop.load(Relaxed) {
                    peak.fetch_max(commit_now(), Relaxed);
                }
            })
        };
        CommitPeak {
            peak,
            stop,
            sampler: Some(sampler),
        }
    }

    /// Starts a new peak at the commit charge now, and returns it.
    fn reset(&self) -> u64 {
        let now = commit_now();
        self.peak.store(now, Relaxed);
        now
    }

    fn peak(&self) -> u64 {
        self.peak.load(Relaxed).max(commit_now())
    }
}

impl Drop for CommitPeak {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(sampler) = self.sampler.take() {
            sampler.join().expect("the sampler does not panic");
        }
    }
}

fn commit_now() -> u64 {
    measure::private_bytes()
}

criterion_group! {
    name = benches;
    config = measure::criterion();
    targets = bench_query, bench_linked_release, bench_folder_sizes, bench_sort_change,
        bench_folder_tree, bench_repeats, bench_journal_batch, bench_growth_hold
}
criterion_main!(benches);
