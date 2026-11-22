//! Checks that the index's memory savings and the search's shortcuts still
//! hold, on a volume of millions of files built as gozlerd's bootstrap leaves
//! one. A speed check counts the CPU cycles of the calling thread for a
//! shortcut and for the work it replaced, in the same run, so the machine's
//! speed and load change both alike, and fails when the shortcut saves less
//! than its share. A memory check compares what the index holds with what its
//! layout needs.
//!
//! The checks are ignored, because building the volume takes seconds, and run
//! in an optimized build only when the owner asks for them:
//! `pwsh ./build.ps1 -Timing optimizations`.

use super::*;
use crate::index::NTFS_ROOT_FRN;
use crate::index::store::NameRef;
use crate::links::FolderLink;
use crate::query::parse::{ParseOptions, parse_query, parse_query_with};
use crate::usn::record::{
    FILE_ATTRIBUTE_DIRECTORY, ParsedUsnRecord, USN_REASON_FILE_CREATE, USN_REASON_FILE_DELETE,
    USN_REASON_RENAME_NEW_NAME,
};
use std::cell::RefCell;
use std::collections::HashSet;
use std::hint::black_box;
use std::sync::LazyLock;

const MOUNT: &str = r"C:\";

/// The volume most checks read, built once for all of them.
static VOLUME: LazyLock<Volume> = LazyLock::new(|| Volume::build(200));

/// Folders in each subfolder of a top folder of a [`Volume`].
const GROUPS: u64 = 10;
/// Folders in each group folder.
const BINS: u64 = 10;
/// Files in each bin folder.
const FILES_PER_FOLDER: u64 = 50;

/// An index with two top folders, each holding half the files.
struct Volume {
    index: VolumeIndex,
    /// The private bytes the process gained while it built the index.
    private_bytes: u64,
    /// One past the largest file record number.
    records: u64,
}

impl Volume {
    /// Builds a volume of two top folders of `subfolders` folders each, below
    /// each of those [`GROUPS`] folders of [`BINS`] folders of
    /// [`FILES_PER_FOLDER`] files, and finishes it as gozlerd's bootstrap
    /// does.
    fn build(subfolders: u64) -> Self {
        let before = private_bytes();
        let mut index = VolumeIndex::new(NTFS_ROOT_FRN);
        let mut next = 1_000;
        let mut add = |index: &mut VolumeIndex, parent: Frn, name: &str, is_dir: bool| {
            let frn = Frn::ntfs(next, 1);
            next += 1;
            index.insert_enum(&usn_record(
                frn,
                parent,
                name,
                is_dir,
                USN_REASON_FILE_CREATE,
            ));
            frn
        };
        let mut file = 0;
        for a in 0..2 {
            let top = add(&mut index, NTFS_ROOT_FRN, &top_folder(a), true);
            for b in 0..subfolders {
                let sub = add(&mut index, top, &subfolder(b), true);
                for c in 0..GROUPS {
                    let group = add(&mut index, sub, &group_folder(c), true);
                    for d in 0..BINS {
                        let bin = add(&mut index, group, &bin_folder(d), true);
                        for _ in 0..FILES_PER_FOLDER {
                            let frn = add(&mut index, bin, &file_name(file), false);
                            // Scattered sizes, so a size sort compares real keys.
                            let mix = file.wrapping_mul(0x9E37_79B9_7F4A_7C15);
                            index.set_stat(frn, mix % 10_000_000, (mix >> 24) as i64);
                            file += 1;
                        }
                    }
                }
            }
        }
        index.compute_folder_sizes();
        index.optimize_storage();
        index.rank_names();
        Volume {
            index,
            private_bytes: private_bytes().saturating_sub(before),
            records: next,
        }
    }

    /// The first top folder, which holds half the files.
    fn big_folder(&self) -> EntryIdx {
        resolve_scope(&self.index, top_folder(0).as_bytes(), false).expect("the volume has it")
    }

    /// The first bin folder, which holds [`FILES_PER_FOLDER`] files.
    fn small_folder(&self) -> EntryIdx {
        let path = format!(
            r"{}\{}\{}\{}",
            top_folder(0),
            subfolder(0),
            group_folder(0),
            bin_folder(0)
        );
        resolve_scope(&self.index, path.as_bytes(), false).expect("the volume has it")
    }
}

fn top_folder(a: u64) -> String {
    format!("amd64_microsoft-windows-component-{a:03}_31bf3856ad364e35_10.0.26100.1_none_deadbeef")
}

fn subfolder(b: u64) -> String {
    format!("subcomponent_{b:03}_none_cafef00dbaadf00d")
}

fn group_folder(c: u64) -> String {
    format!("module_group_{c:03}")
}

fn bin_folder(d: u64) -> String {
    format!("bin_x64_{d:02}")
}

/// The name of file number `file`, in lowercase like every name of a
/// [`Volume`]. One file in 10,000 is a rare `kernel32_*.dll` and one a rare
/// `ntoskrnl_*.sys`. The others share 625,000 names, so on the volume most
/// checks read about a third of the names are distinct, as on a real volume.
fn file_name(file: u64) -> String {
    match file % 10_000 {
        0 => format!("kernel32_{file}.dll"),
        5_000 => format!("ntoskrnl_{file}.sys"),
        _ => {
            let n = file % 625_000;
            let stems = [
                "document", "report", "setup", "readme", "invoice", "photo", "notes", "build",
            ];
            let extensions = ["txt", "log", "dat", "png"];
            let stem = stems[(n % 8) as usize];
            let extension = extensions[(n / 8 % 4) as usize];
            format!("{stem}_{n}.{extension}")
        }
    }
}

/// A small volume after every third file was renamed and every seventh
/// deleted, so its arenas hold names no entry bears and it needs a
/// compaction.
fn dirty_volume() -> VolumeIndex {
    let mut index = Volume::build(20).index;
    let files: Vec<Frn> = index
        .live_entries()
        .filter(|&idx| !index.is_synthetic(idx) && !index.entry(idx).is_dir())
        .map(|idx| index.entry(idx).frn())
        .collect();
    let churn: Vec<ParsedUsnRecord> = files
        .iter()
        .enumerate()
        .filter_map(|(i, &frn)| {
            if i.is_multiple_of(3) {
                let name = format!("renamed_{i}.txt");
                Some(usn_record(
                    frn,
                    NTFS_ROOT_FRN,
                    &name,
                    false,
                    USN_REASON_RENAME_NEW_NAME,
                ))
            } else if i.is_multiple_of(7) {
                Some(usn_record(
                    frn,
                    NTFS_ROOT_FRN,
                    "",
                    false,
                    USN_REASON_FILE_DELETE,
                ))
            } else {
                None
            }
        })
        .collect();
    for batch in churn.chunks(4096) {
        index.apply_batch(batch);
    }
    index
}

fn usn_record(frn: Frn, parent: Frn, name: &str, is_dir: bool, reason: u32) -> ParsedUsnRecord {
    ParsedUsnRecord {
        frn,
        parent_frn: parent,
        reason,
        attributes: if is_dir { FILE_ATTRIBUTE_DIRECTORY } else { 0 },
        name: name.as_bytes().to_vec(),
    }
}

fn private_bytes() -> u64 {
    gozler_winfs::self_memory()
        .expect("the process reads its own memory counters")
        .private_bytes
}

fn compiled(text: &str) -> CompiledQuery {
    CompiledQuery::new(&parse_query(text).expect("a valid query")).expect("the query compiles")
}

fn compiled_with_match_path(text: &str) -> CompiledQuery {
    let options = ParseOptions {
        match_path: true,
        ..ParseOptions::default()
    };
    let parsed = parse_query_with(text, options).expect("a valid query");
    CompiledQuery::new(&parsed).expect("the query compiles")
}

/// How many times [`alternate`] runs each of its two pieces of work.
const RUNS: usize = 7;

/// `work`'s result and the CPU cycles the calling thread spent on it.
fn timed<T>(work: impl FnOnce() -> T) -> (T, u64) {
    let start = gozler_winfs::thread_cycles();
    let output = black_box(work());
    (output, gozler_winfs::thread_cycles() - start)
}

/// What [`alternate`] measured.
struct Alternated<T, U> {
    first: T,
    second: U,
    /// The fewest cycles of one run of `first`.
    first_cycles: u64,
    /// The fewest cycles of one run of `second`.
    second_cycles: u64,
    /// The median, over the pairs of runs, of `second`'s cycles divided by
    /// `first`'s.
    ratio: f64,
}

/// Runs `first` and `second` alternately, [`RUNS`] times each, so the two
/// runs of a pair meet the same load on the machine, and the few pairs that
/// other work disturbed do not decide the median ratio. Each returns its
/// result and its [`timed`] cycles, so it can prepare its input outside the
/// count.
fn alternate<T, U>(
    mut first: impl FnMut() -> (T, u64),
    mut second: impl FnMut() -> (U, u64),
) -> Alternated<T, U> {
    let mut ratios = Vec::with_capacity(RUNS);
    let (mut first_cycles, mut second_cycles) = (u64::MAX, u64::MAX);
    let mut results = None;
    for _ in 0..RUNS {
        let (a, a_cycles) = first();
        let (b, b_cycles) = second();
        first_cycles = first_cycles.min(a_cycles);
        second_cycles = second_cycles.min(b_cycles);
        ratios.push(b_cycles as f64 / a_cycles.max(1) as f64);
        // The previous pair's results are dropped here, outside the count.
        results = Some((a, b));
    }
    ratios.sort_by(f64::total_cmp);
    let (first, second) = results.expect("RUNS is above 0");
    Alternated {
        first,
        second,
        first_cycles,
        second_cycles,
        ratio: ratios[RUNS / 2],
    }
}

/// Asserts that an optimization's `shortcut` takes at most `1 / times` of the
/// cycles of the work it `replaced`, as [`alternate`] measures them, prints
/// both, and returns their results.
fn assert_saves<T, U>(
    what: &str,
    shortcut: impl FnMut() -> (T, u64),
    replaced: impl FnMut() -> (U, u64),
    times: f64,
) -> (T, U) {
    let measured = alternate(shortcut, replaced);
    let ratio = measured.ratio;
    eprintln!(
        "{what}: {} cycles, {} without the optimization, {ratio:.1} times fewer",
        measured.first_cycles, measured.second_cycles
    );
    assert!(
        ratio >= times,
        "{what}: {ratio:.1} times fewer cycles than without the optimization, where at least \
         {times} was expected"
    );
    (measured.first, measured.second)
}

/// How many entries of `index` match `query`, below `scope` when one is
/// given, in a search with every shortcut that applies.
fn search(index: &VolumeIndex, query: &CompiledQuery, scope: Option<EntryIdx>) -> usize {
    let seen = SeenBy::Everything;
    let never = AtomicBool::new(false);
    scan(index, MOUNT, query, scope, &seen, &never)
        .records
        .len()
}

/// [`search`] with no shortcut: every entry tested, and each one's folders
/// walked up to `scope`.
fn test_every_entry(index: &VolumeIndex, query: &CompiledQuery, scope: Option<EntryIdx>) -> usize {
    let seen = SeenBy::Everything;
    let never = AtomicBool::new(false);
    let mut scan = Scan::new(index, MOUNT, query, &seen, &never);
    for idx in index.live_entries() {
        if !index.is_synthetic(idx) {
            scan.visit(idx, scope);
        }
    }
    scan.records.len()
}

/// [`search`] in `folder` as its scope walk runs without a depth bound:
/// every entry below the folder tested.
fn walk_whole_folder(index: &VolumeIndex, query: &CompiledQuery, folder: EntryIdx) -> usize {
    let seen = SeenBy::Everything;
    let never = AtomicBool::new(false);
    let mut scan = Scan::new(index, MOUNT, query, &seen, &never);
    for idx in index.subtree_entries(folder) {
        if !index.is_synthetic(idx) {
            scan.visit(idx, None);
        }
    }
    scan.records.len()
}

/// [`search`] with the name scan as its only shortcut: each entry bearing a
/// name that holds one of the query's literals tested, and its folders walked
/// up to `scope`.
fn scan_every_name(index: &VolumeIndex, query: &CompiledQuery, scope: Option<EntryIdx>) -> usize {
    let seen = SeenBy::Everything;
    let never = AtomicBool::new(false);
    let mut scan = Scan::new(index, MOUNT, query, &seen, &never);
    for_each_name_containing_any(index, &query.prefilter, |id| {
        for idx in index.name_chain(id) {
            if !index.is_synthetic(idx) {
                scan.visit(idx, scope);
            }
        }
        true
    });
    scan.records.len()
}

/// The records of every entry of `index`, unkeyed and in entry order.
fn every_record(index: &VolumeIndex) -> Vec<SortRecord> {
    let seen = SeenBy::Everything;
    let never = AtomicBool::new(false);
    scan(index, MOUNT, &compiled(""), None, &seen, &never).records
}

/// Asserts that `index` holds each distinct name of its entries once, and no
/// name that no entry bears. A name already in its folded form, as every file
/// and folder name of a [`Volume`] is, has no raw copy. Returns how many
/// distinct names the entries bear.
fn assert_each_name_held_once(index: &VolumeIndex) -> usize {
    let names: HashSet<&[u8]> = index
        .live_entries()
        .map(|idx| index.entry(idx).name())
        .collect();
    let mut folded_bytes = 0;
    let mut raw_bytes = 0;
    for name in &names {
        let folded = fold_accents(name);
        folded_bytes += folded.len() as u64 + 1;
        if folded != *name {
            raw_bytes += name.len() as u64 + 1;
        }
    }
    let memory = index.memory();
    assert_eq!(index.name_pairs().len(), names.len(), "names held");
    assert_eq!(
        memory.arena_folded.used, folded_bytes,
        "folded name bytes, each name with its NUL"
    );
    assert_eq!(
        memory.arena_raw.used, raw_bytes,
        "raw name bytes, each name with its NUL"
    );
    names.len()
}

/// The entry columns ([`crate::index::store::EntryTable`]) cost 8 bytes of
/// file ID and 63 bytes of the other columns an entry, 8 of them a folder's
/// counts of files and folders. Each byte more costs 10 MB on a volume of
/// 10 million files.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn an_entry_costs_71_bytes_of_columns() {
    let index = &VOLUME.index;
    let bytes = index.memory().entries.used;
    let entries = index.len() as u64;
    eprintln!(
        "entry columns: {bytes} bytes for {entries} entries, {:.2} an entry",
        bytes as f64 / entries as f64
    );
    assert!(bytes <= 71 * entries);
}

/// Each distinct name is interned once, its raw form shares its folded bytes
/// when they are equal, and the tables for each name cost 37 bytes: its pair
/// 16, its reference count 4, its chain head 4, its rank 4, its place in the
/// rank order 4, and its intern table slot 5.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn each_name_is_held_once() {
    let index = &VOLUME.index;
    let names = assert_each_name_held_once(index) as u64;
    let tables = index.memory().name_tables.used;
    eprintln!(
        "name tables: {tables} bytes for {names} names, {:.2} a name",
        tables as f64 / names as f64
    );
    assert!(tables <= 37 * names);
}

/// After [`VolumeIndex::optimize_storage`], the file ID map is a list
/// indexed by record number, 4 bytes a record, instead of a hash map.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn the_file_id_map_costs_4_bytes_a_record() {
    let memory = VOLUME.index.memory();
    eprintln!(
        "file ID map: {} bytes for {} record numbers",
        memory.frn_map.used, VOLUME.records
    );
    assert_eq!(memory.frn_map_kind, "dense");
    assert!(memory.frn_map.used <= 4 * VOLUME.records);
}

/// Everything building the index kept, its columns, names, tables, maps and
/// the allocator's overhead, costs at most this many private bytes an entry,
/// so this check also counts memory that [`VolumeIndex::memory`] misses.
const PRIVATE_BYTES_PER_ENTRY: u64 = 100;

#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn the_index_keeps_its_private_bytes_budget() {
    let volume = &*VOLUME;
    let entries = volume.index.len() as u64;
    let memory = volume.index.memory();
    let counted = memory.entries.allocated
        + memory.arena_raw.allocated
        + memory.arena_folded.allocated
        + memory.frn_map.allocated
        + memory.name_tables.allocated
        + memory.dir_children.allocated
        + memory.descriptors.allocated;
    eprintln!(
        "index: {} private bytes, {:.2} an entry; the index counts {counted}, {:.2} an entry",
        volume.private_bytes,
        volume.private_bytes as f64 / entries as f64,
        counted as f64 / entries as f64
    );
    assert!(volume.private_bytes <= PRIVATE_BYTES_PER_ENTRY * entries);
}

/// A compaction drops the names that renames and deletes left in the
/// arenas with no entry bearing them.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn compaction_drops_the_names_of_removed_files() {
    let mut index = dirty_volume();
    let plan = index
        .plan_compaction()
        .expect("the churn leaves enough dead names for a compaction");
    index.apply_compaction(plan);
    assert_each_name_held_once(&index);
}

/// The empty search, which a full export runs, reserves a record for each
/// entry once, because a list that grows holds its old and new buffers
/// together. A search that tests every entry for a filter grows its list
/// instead, so a selective one does not hold a record for every entry.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn the_empty_search_reserves_its_records_once() {
    let index = &VOLUME.index;
    let seen = SeenBy::Everything;
    let never = AtomicBool::new(false);
    let every = scan(index, MOUNT, &compiled(""), None, &seen, &never).records;
    assert!(
        every.capacity() <= index.len(),
        "room for {} records for {} entries",
        every.capacity(),
        index.len()
    );
    let filter = scan(
        index,
        MOUNT,
        &compiled("size:>9999000"),
        None,
        &seen,
        &never,
    )
    .records;
    assert!(
        filter.capacity() <= 4 * filter.len().max(64),
        "room for {} records for {} matches",
        filter.capacity(),
        filter.len()
    );
}

/// A row that a reply or a result snapshot keeps for a match costs 16 bytes
/// on NTFS: its entry and its file ID, not its name or its path.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn a_row_costs_16_bytes() {
    let index = &VOLUME.index;
    let seen = SeenBy::Everything;
    let never = AtomicBool::new(false);
    let query = compiled("");
    let matches = find_matches(
        index,
        MOUNT,
        &query,
        None,
        SortSpec::default(),
        None,
        false,
        &seen,
        &never,
    );
    let rows = matches
        .order()
        .read_rows(index)
        .expect("nothing changes the index");
    eprintln!("rows: {} bytes for {} entries", rows.bytes(), index.len());
    assert!(rows.bytes() <= 16 * index.len() as u64);
}

/// A term, an OR clause, an `ext:` term, a wildcard and a regular expression
/// each give literals that one of their matches' names holds, so the search
/// scans the distinct names for them ([`for_each_name_containing_any`])
/// instead of testing every entry.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn the_name_scan_beats_testing_every_entry() {
    let index = &VOLUME.index;
    for (kind, text) in [
        ("a term", "kernel32"),
        ("an OR clause", "kernel32|ntoskrnl"),
        ("an ext: term", "ext:dll;sys"),
        ("a wildcard", "*.sys"),
        ("a regular expression", "regex:^kernel32"),
    ] {
        let query = compiled(text);
        assert!(
            !query.prefilter.is_empty(),
            "{kind} `{text}` gives the name scan no literal"
        );
        let (found, expected) = assert_saves(
            &format!("{kind} `{text}`"),
            || timed(|| search(index, &query, None)),
            || timed(|| test_every_entry(index, &query, None)),
            10.0,
        );
        assert_eq!(found, expected, "`{text}`");
    }
}

/// A name that holds the needle many times costs the name scan what a name
/// that holds it once costs, because [`for_each_name_containing`] moves on
/// to the next name after a hit instead of finding each occurrence.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn the_name_scan_moves_to_the_next_name_after_a_hit() {
    let flat = |name: fn(u64) -> String| {
        let mut index = VolumeIndex::new(NTFS_ROOT_FRN);
        for n in 0..200_000 {
            let frn = Frn::ntfs(1_000 + n, 1);
            index.insert_enum(&usn_record(
                frn,
                NTFS_ROOT_FRN,
                &name(n),
                false,
                USN_REASON_FILE_CREATE,
            ));
        }
        index
    };
    let hits = |index: &VolumeIndex| {
        let mut count = 0;
        for_each_name_containing(index, b"e", |_| {
            count += 1;
            true
        });
        count
    };
    let many = flat(|n| format!("{}_{n}", "e".repeat(24)));
    let once = flat(|n| format!("e{}_{n}", "x".repeat(23)));
    let measured = alternate(|| timed(|| hits(&many)), || timed(|| hits(&once)));
    assert_eq!(measured.first, measured.second);
    eprintln!(
        "names holding `e` 24 times: {} cycles, holding it once: {}",
        measured.first_cycles, measured.second_cycles
    );
    assert!(
        measured.ratio >= 0.5,
        "names holding `e` 24 times cost {:.1} times what names holding it once cost",
        1.0 / measured.ratio
    );
}

/// A rare literal's hits lie far apart in the folded names, and [`name_at`]
/// gallops to each hit's name instead of stepping through every name
/// between two hits.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn the_name_scan_gallops_to_a_far_hit() {
    const NAMES: u32 = 4_000_000;
    const APART: u32 = 10_000;
    let pairs: Vec<NamePair> = (0..NAMES)
        .map(|id| NamePair {
            raw: NameRef::default(),
            folded: NameRef {
                off: id * 10,
                len: 9,
            },
        })
        .collect();
    let hits: Vec<u32> = (0..NAMES / APART)
        .map(|k| (k * APART + APART / 2) * 10 + 4)
        .collect();
    let (galloped, stepped) = assert_saves(
        "hits 10,000 names apart",
        || timed(|| hits.iter().fold(0, |name, &pos| name_at(&pairs, name, pos))),
        || {
            timed(|| {
                hits.iter().fold(0, |mut name, &pos| {
                    while name + 1 < pairs.len() && pairs[name + 1].folded.off <= pos {
                        name += 1;
                    }
                    name
                })
            })
        },
        10.0,
    );
    assert_eq!(galloped, stepped);
}

/// A search in a folder walks the folder's entries
/// ([`VolumeIndex::subtree_entries`]) instead of testing each entry whose
/// name matches and walking up its folders to the scope.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn a_small_folder_walks_only_its_entries() {
    let volume = &*VOLUME;
    let query = compiled("document");
    let folder = Some(volume.small_folder());
    let (found, expected) = assert_saves(
        "`document` in a folder of 50 files",
        || timed(|| search(&volume.index, &query, folder)),
        || timed(|| scan_every_name(&volume.index, &query, folder)),
        100.0,
    );
    assert_eq!(found, expected);
}

/// A rare term in a folder bigger than [`SUBTREE_WALK_MAX`] scans the names
/// once the walk has passed that many entries, instead of walking the whole
/// folder.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn a_big_folder_scans_the_names_for_a_rare_term() {
    let volume = &*VOLUME;
    let query = compiled("kernel32");
    let folder = Some(volume.big_folder());
    let (found, expected) = assert_saves(
        "`kernel32` in half the volume",
        || timed(|| search(&volume.index, &query, folder)),
        || {
            timed(|| {
                let seen = SeenBy::Everything;
                let never = AtomicBool::new(false);
                let walk = scan_with(
                    &volume.index,
                    MOUNT,
                    &query,
                    folder,
                    &seen,
                    &never,
                    usize::MAX,
                );
                walk.records.len()
            })
        },
        2.0,
    );
    assert_eq!(found, expected);
}

/// A broad term in a folder bigger than [`SUBTREE_WALK_MAX`] keeps walking
/// the folder while the walk costs less than the name scan, which would test
/// more entries and walk up each one's folders to the scope. The walk saves
/// less than the other shortcuts, but a walk that gave up would cost the name
/// scan and a part of the walk, more than the name scan alone.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn a_big_folder_walks_for_a_broad_term() {
    let volume = &*VOLUME;
    let query = compiled("e");
    let folder = Some(volume.big_folder());
    let (found, expected) = assert_saves(
        "`e` in half the volume",
        || timed(|| search(&volume.index, &query, folder)),
        || timed(|| scan_every_name(&volume.index, &query, folder)),
        1.2,
    );
    assert_eq!(found, expected);
}

/// A search whose clauses bound the depth walks a folder only down to that
/// depth, instead of every entry below it. A scope that a link inside it
/// leads out of becomes a path test of the whole volume, whose candidate
/// walk stops at the same depth instead of giving up for a scan of every
/// entry.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn a_depth_bound_walks_only_its_levels() {
    let volume = &*VOLUME;
    let index = &volume.index;
    let parsed = parse_query("folder: depth:<=3").expect("a valid query");
    let query = CompiledQuery::new(&parsed).expect("the query compiles");
    let folder = volume.big_folder();
    let (found, expected) = assert_saves(
        "`folder: depth:<=3` in half the volume",
        || timed(|| search(index, &query, Some(folder))),
        || timed(|| walk_whole_folder(index, &query, folder)),
        10.0,
    );
    assert_eq!(found, expected);

    let scope = format!(r"C:\{}", top_folder(0));
    let links = Links::new(vec![FolderLink {
        link: format!(r"{scope}\out"),
        target: format!(r"C:\{}", top_folder(1)),
    }]);
    let query =
        CompiledQuery::with_links(&parsed, &links, Some(&scope)).expect("the query compiles");
    let (found, expected) = assert_saves(
        "`folder: depth:<=3` in half the volume, with a link out of it",
        || timed(|| search(index, &query, None)),
        || timed(|| test_every_entry(index, &query, None)),
        10.0,
    );
    assert_eq!(found, expected);
}

/// The volume's root as the scope is no scope, so a search below it walks
/// no folders up to it. As a scope, the root would also cost a part of a
/// walk of the volume before the name scan.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn the_volume_root_is_no_scope() {
    let index = &VOLUME.index;
    let query = compiled("document");
    let root = Some(index.root());
    let (found, expected) = assert_saves(
        "`document` below the root",
        || timed(|| search(index, &query, root)),
        || timed(|| scan_every_name(index, &query, root)),
        1.2,
    );
    assert_eq!(found, expected);
}

/// A path term with a separator walks the folders whose names hold its last
/// component's literal ([`Scan::visit_path_candidates`]) instead of testing
/// every entry's full path, also when a link leads into the volume.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn a_path_term_walks_only_matching_folders() {
    let index = &VOLUME.index;
    let parsed = parse_query(r"module_group_007\bin_x64_03").expect("a valid query");
    let link = FolderLink {
        link: r"C:\elsewhere".into(),
        target: format!(
            r"C:\{}\{}\{}\{}",
            top_folder(0),
            subfolder(0),
            group_folder(0),
            bin_folder(0)
        ),
    };
    let links = Links::new(vec![link]);
    for (kind, query) in [
        ("a path term", CompiledQuery::new(&parsed)),
        (
            "a path term through a link",
            CompiledQuery::with_links(&parsed, &links, None),
        ),
    ] {
        let query = query.expect("the query compiles");
        let (found, expected) = assert_saves(
            kind,
            || timed(|| search(index, &query, None)),
            || timed(|| test_every_entry(index, &query, None)),
            3.0,
        );
        assert_eq!(found, expected, "{kind}");
    }
}

/// With Match path, a term without a separator walks the folders whose names
/// hold it ([`Scan::visit_components`]) instead of testing every entry's
/// path.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn a_match_path_term_walks_only_matching_folders() {
    let index = &VOLUME.index;
    let query = compiled_with_match_path("module_group_007");
    assert!(query.component_literal.is_some());
    let (found, expected) = assert_saves(
        "Match path `module_group_007`",
        || timed(|| search(index, &query, None)),
        || timed(|| test_every_entry(index, &query, None)),
        4.0,
    );
    assert_eq!(found, expected);
}

/// With Match path, a start that the volume root's name fails leaves the
/// volume unscanned ([`CompiledQuery::root_fails`]).
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn a_start_the_root_fails_scans_nothing() {
    let index = &VOLUME.index;
    let query = compiled_with_match_path("startwith:kernel32");
    let answers = assert_saves(
        "Match path `startwith:kernel32`",
        || timed(|| search(index, &query, None)),
        || timed(|| test_every_entry(index, &query, None)),
        100.0,
    );
    assert_eq!(answers, (0, 0));
}

/// A search whose client left tests no more entries ([`Scan::stopped`]), on
/// the full pass and on the name scan.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn a_stopped_search_tests_no_more_entries() {
    let index = &VOLUME.index;
    let seen = SeenBy::Everything;
    let stopped = AtomicBool::new(true);
    for text in ["", "e"] {
        let query = compiled(text);
        let (found, _) = assert_saves(
            &format!("stopped `{text}`"),
            || timed(|| scan(index, MOUNT, &query, None, &seen, &stopped).records),
            || timed(|| search(index, &query, None)),
            100.0,
        );
        assert_eq!(found.len(), 0, "`{text}`");
    }
}

/// Matches sort by number keys packed from their names' ranks
/// ([`key_found`]) instead of by comparing their names.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn matches_sort_by_packed_number_keys() {
    let index = &VOLUME.index;
    let mut records = every_record(index);
    // Below the count at which `Matches::order` sorts in parallel, so both
    // sorts run on this thread.
    records.truncate(90_000);
    let by_rank = |records| {
        let matches = key_found(index, MOUNT, records, None, None, SortSpec::default());
        matches.order().records
    };
    let by_name = |mut records: Vec<SortRecord>| {
        records.sort_unstable_by(|a, b| index.folded_name(a.idx()).cmp(index.folded_name(b.idx())));
        records
    };
    assert_saves(
        "90,000 matches by name",
        || {
            let records = records.clone();
            timed(|| by_rank(records))
        },
        || {
            let records = records.clone();
            timed(|| by_name(records))
        },
        2.0,
    );
}

/// A page selects its rows from the matches ([`Matches::order`]) and sorts
/// only them, instead of sorting every match.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn a_page_sorts_only_its_rows() {
    let index = &VOLUME.index;
    let records = every_record(index);
    let by_size = SortSpec::default_for(SortKey::Size);
    let keyed = || key_found(index, MOUNT, records.clone(), None, None, by_size);
    assert_saves(
        "a page of 100 rows by size",
        || {
            let mut matches = keyed();
            matches.end = 100;
            timed(|| matches.order().records)
        },
        || {
            let mut records = keyed().records;
            timed(|| {
                records.sort_unstable();
                records
            })
        },
        4.0,
    );
}

/// A page that takes nearly every match sorts its rows on rayon's threads
/// ([`Matches::order`]), so the calling thread spends little of a sort.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn a_long_page_sorts_in_parallel() {
    let index = &VOLUME.index;
    let records = every_record(index);
    let by_size = SortSpec::default_for(SortKey::Size);
    let keyed = || key_found(index, MOUNT, records.clone(), None, None, by_size);
    assert_saves(
        "a page of all but 5 rows by size, on the calling thread",
        || {
            let mut matches = keyed();
            matches.end = matches.records.len() - 5;
            timed(|| matches.order().records)
        },
        || {
            let mut records = keyed().records;
            timed(|| {
                records.sort_unstable();
                records
            })
        },
        2.0,
    );
}

/// A compaction copies the live names while searches keep reading
/// ([`VolumeIndex::plan_compaction`]), so the part that holds the write
/// lock ([`VolumeIndex::apply_compaction`]) only installs the copy.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn a_compaction_holds_the_write_lock_briefly() {
    let dirty = dirty_volume();
    assert_saves(
        "installing a compaction, against copying it",
        || {
            let mut index = dirty_volume();
            let plan = index
                .plan_compaction()
                .expect("the churn leaves dead names");
            timed(|| {
                index.apply_compaction(plan);
                index
            })
        },
        || timed(|| dirty.plan_compaction()),
        3.0,
    );
}

/// Once folder sizes are kept, a file's size change updates the folders
/// above it ([`VolumeIndex::set_stat`]) instead of summing every folder
/// again.
#[test]
#[ignore = "optimization check: build.ps1 -Timing optimizations"]
fn a_size_change_updates_only_its_folders() {
    let index = Volume::build(20).index;
    let file = index
        .live_entries()
        .find(|&idx| !index.is_synthetic(idx) && !index.entry(idx).is_dir())
        .map(|idx| index.entry(idx).frn())
        .expect("the volume has files");
    // Both sides change the index, one after the other.
    let index = RefCell::new(index);
    let mut size = 0;
    assert_saves(
        "a file's size change",
        || {
            size ^= 1;
            let mut index = index.borrow_mut();
            timed(|| index.set_stat(file, size, 0))
        },
        || {
            let mut index = index.borrow_mut();
            timed(|| index.compute_folder_sizes())
        },
        1_000.0,
    );
}
