use super::{PendingWrite, STEP_TICKS, Writes, Written};
use crate::types::Frn;
use crate::usn::ParsedUsnRecord;
use crate::usn::record::{
    USN_REASON_BASIC_INFO_CHANGE, USN_REASON_CLOSE, USN_REASON_DATA_EXTEND,
    USN_REASON_DATA_OVERWRITE, USN_REASON_DATA_TRUNCATION, USN_REASON_FILE_CREATE,
    USN_REASON_FILE_DELETE, USN_REASON_RENAME_NEW_NAME,
};

/// The start of a step.
const T0: i64 = 44_600_000 * STEP_TICKS;
const MB: u64 = 1 << 20;

const WRITE: u32 = USN_REASON_DATA_EXTEND | USN_REASON_CLOSE;
const CREATE: u32 = USN_REASON_FILE_CREATE | USN_REASON_CLOSE;
const DELETE: u32 = USN_REASON_FILE_DELETE | USN_REASON_CLOSE;
const RENAME: u32 = USN_REASON_RENAME_NEW_NAME | USN_REASON_CLOSE;

fn record(frn: u128, folder: u128, name: &str, reason: u32) -> ParsedUsnRecord {
    ParsedUsnRecord {
        frn: Frn(frn),
        parent_frn: Frn(folder),
        reason,
        attributes: 0,
        name: name.as_bytes().to_vec(),
        time: T0,
    }
}

/// A table that a build installed, counting from `T0`.
fn table() -> Writes {
    let mut writes = Writes::for_entries(0);
    writes.keep(&mut Writes::none(), T0);
    writes
}

/// Counts `records` as one batch.
fn apply(writes: &mut Writes, records: &[ParsedUsnRecord]) -> Vec<PendingWrite> {
    writes.begin_batch();
    let mut pending = Vec::new();
    for (rec, &name) in records.iter().zip(&super::name_keys(records)) {
        writes.count(rec, name, &mut pending);
    }
    writes.add_writes(&pending);
    pending
}

fn name_writes(writes: &Writes, folder: u128, name: &str) -> Option<u32> {
    writes
        .of_name(Frn(folder), name.as_bytes())
        .map(|v| v.writes)
}

fn folder_writes(writes: &Writes, folder: u128) -> Option<u32> {
    writes.of_folder(Frn(folder)).map(|v| v.writes)
}

/// What counts as a write. Without it the count is of journal
/// records, of opens, or of renames, and a move of 10,000 files reads as
/// wear.
#[test]
fn only_a_closed_write_of_data_or_a_create_counts() {
    let mut w = table();
    apply(&mut w, &[record(200, 100, "log.txt", WRITE)]);
    assert_eq!(name_writes(&w, 100, "log.txt"), Some(1));
    assert_eq!(folder_writes(&w, 100), Some(1));

    apply(&mut w, &[record(201, 100, "new.txt", CREATE)]);
    assert_eq!(name_writes(&w, 100, "new.txt"), None);
    assert_eq!(folder_writes(&w, 100), Some(2));

    let dates = USN_REASON_BASIC_INFO_CHANGE | USN_REASON_CLOSE;
    apply(&mut w, &[record(200, 100, "log.txt", dates)]);
    apply(
        &mut w,
        &[record(200, 100, "log.txt", USN_REASON_DATA_EXTEND)],
    );
    assert_eq!(name_writes(&w, 100, "log.txt"), Some(1));
    assert_eq!(folder_writes(&w, 100), Some(2));

    // A rename that replaced a file, the delete first and then the rename.
    apply(
        &mut w,
        &[
            record(300, 100, "index", DELETE),
            record(301, 100, "index", RENAME),
        ],
    );
    assert_eq!(name_writes(&w, 100, "index"), Some(1));
    assert_eq!(w.of_name(Frn(100), b"index").map(|v| v.frn), Some(Frn(301)));
    // The rename first and then the delete.
    apply(
        &mut w,
        &[
            record(302, 100, "index", RENAME),
            record(301, 100, "index", DELETE),
        ],
    );
    assert_eq!(name_writes(&w, 100, "index"), Some(2));
    assert_eq!(w.of_name(Frn(100), b"index").map(|v| v.frn), Some(Frn(302)));
    assert_eq!(folder_writes(&w, 100), Some(2), "a rename moves no data");

    // A move replaced nothing.
    apply(&mut w, &[record(400, 100, "moved.txt", RENAME)]);
    assert_eq!(name_writes(&w, 100, "moved.txt"), None);

    // A pair is two files: a file renamed and deleted in one open replaced
    // nothing, and a file renamed to `y` and then deleted leaves the delete
    // for the next file renamed to `y`, and leaves no rename of its own to
    // pair with a later file's delete: the name then holds the last file.
    let both = USN_REASON_RENAME_NEW_NAME | USN_REASON_FILE_DELETE | USN_REASON_CLOSE;
    apply(&mut w, &[record(600, 100, "update.tmp", both)]);
    assert_eq!(name_writes(&w, 100, "update.tmp"), None);
    apply(
        &mut w,
        &[
            record(601, 100, "y", RENAME),
            record(601, 100, "y", DELETE),
            record(602, 100, "y", RENAME),
            record(602, 100, "y", DELETE),
            record(603, 100, "y", RENAME),
        ],
    );
    assert_eq!(
        w.of_name(Frn(100), b"y").map(|v| (v.writes, v.frn)),
        Some((2, Frn(603)))
    );
}

fn written(w: &Writes, folder: u128, name: Option<&str>) -> Option<(u32, Written)> {
    let values = match name {
        Some(name) => w.of_name(Frn(folder), name.as_bytes()),
        None => w.of_folder(Frn(folder)),
    };
    values.map(|v| (v.writes, Written::of(v.written, v.marks)))
}

/// Written counts only the bytes gozler knows. It watches a write inside
/// a file reported as the file's size, which once showed 2.7 GB for two
/// writes inside a 1.36 GB database; a cut taken as a whole-file save,
/// which reads a 500 MB file's size for an append of a few bytes; a cut
/// and an append shown as an exact 0; a return to writes times size for
/// data added to the end; and a loss of the growth that a long write moves
/// before its close.
#[test]
fn written_counts_only_the_bytes_it_knows() {
    let mut w = table();
    let p = apply(&mut w, &[record(200, 100, "log.txt", WRITE)]);
    w.add_sizes(&p, |_| (Some(10 * MB), Some(12 * MB)));
    assert_eq!(
        written(&w, 100, Some("log.txt")),
        Some((1, Written::Exact(2 * MB)))
    );
    assert_eq!(written(&w, 100, None), Some((1, Written::Exact(2 * MB))));

    let overwrite = USN_REASON_DATA_OVERWRITE | USN_REASON_CLOSE;
    let p = apply(&mut w, &[record(201, 101, "data.bin", overwrite)]);
    w.add_sizes(&p, |_| (Some(30 * MB), Some(30 * MB)));
    assert_eq!(
        written(&w, 101, Some("data.bin")),
        Some((1, Written::Unknown))
    );
    assert_eq!(written(&w, 101, None), Some((1, Written::Unknown)));

    // The folder keeps the known bytes of its other file as a minimum.
    let p = apply(&mut w, &[record(209, 101, "data.log", WRITE)]);
    w.add_sizes(&p, |_| (Some(MB), Some(3 * MB)));
    assert_eq!(written(&w, 101, None), Some((2, Written::AtLeast(2 * MB))));

    // A save, as NTFS writes it: the cut, then the data, then the close.
    // A cut is no proof that the whole file was written again, so only the
    // growth is known, and once counted, not with the size too.
    let cut = USN_REASON_DATA_TRUNCATION;
    let p = apply(
        &mut w,
        &[
            record(205, 105, "doc.txt", cut),
            record(205, 105, "doc.txt", cut | USN_REASON_DATA_EXTEND),
            record(205, 105, "doc.txt", cut | WRITE),
        ],
    );
    w.add_sizes(&p, |_| (Some(10 * MB), Some(12 * MB)));
    assert_eq!(
        written(&w, 105, Some("doc.txt")),
        Some((1, Written::AtLeast(2 * MB)))
    );

    // A cut and then an append by another open in one batch, 10 MB to 0
    // to 3 MB: no growth from the size before, so nothing is known, not
    // an exact 0.
    let p = apply(
        &mut w,
        &[
            record(206, 106, "list.json", cut | USN_REASON_CLOSE),
            record(206, 106, "list.json", WRITE),
        ],
    );
    w.add_sizes(&p, |_| (Some(10 * MB), Some(3 * MB)));
    assert_eq!(
        written(&w, 106, Some("list.json")),
        Some((2, Written::Unknown))
    );

    // A cut of a 500 MB file's last bytes and an append, as a program that
    // cuts a JSON array's `]` does: the growth, not 500 MB.
    let p = apply(&mut w, &[record(207, 107, "big.json", cut | WRITE)]);
    w.add_sizes(&p, |_| (Some(500 * MB), Some(500 * MB + 100)));
    assert_eq!(
        written(&w, 107, Some("big.json")),
        Some((1, Written::AtLeast(1024)))
    );

    let p = apply(&mut w, &[record(202, 102, "new.bin", CREATE | WRITE)]);
    w.add_sizes(&p, |_| (None, Some(5 * MB)));
    assert_eq!(written(&w, 102, None), Some((1, Written::Exact(5 * MB))));
    assert_eq!(written(&w, 102, Some("new.bin")), None);

    let p = apply(&mut w, &[record(203, 103, "gone.tmp", CREATE)]);
    w.add_sizes(&p, |_| (None, None));
    assert_eq!(written(&w, 103, None), Some((1, Written::Unknown)));

    // No CLOSE yet: Written grows, the writes do not.
    let p = apply(
        &mut w,
        &[record(204, 104, "big.log", USN_REASON_DATA_EXTEND)],
    );
    w.add_sizes(&p, |_| (Some(MB), Some(3 * MB)));
    assert_eq!(
        written(&w, 104, Some("big.log")),
        Some((0, Written::Exact(2 * MB)))
    );
    assert_eq!(written(&w, 104, None), Some((0, Written::Exact(2 * MB))));
}

/// A file created and appended to in one batch has the bytes of both writes
/// in its folder, and its name's share of them is unknown.
#[test]
fn a_file_created_and_appended_in_one_batch_counts_its_bytes() {
    let mut w = table();
    let p = apply(
        &mut w,
        &[
            record(
                250,
                100,
                "new.log",
                USN_REASON_FILE_CREATE | USN_REASON_DATA_EXTEND | USN_REASON_CLOSE,
            ),
            record(250, 100, "new.log", WRITE),
        ],
    );
    w.add_sizes(&p, |_| (None, Some(1024)));
    assert_eq!(
        written(&w, 100, Some("new.log")),
        Some((1, Written::Unknown))
    );
    assert_eq!(written(&w, 100, None), Some((2, Written::Exact(1024))));
}

/// A rename over a name moves no data. It watches the size of a file that
/// was only moved, as `move /y big.iso x.iso`, counted as bytes written,
/// while a save through a temporary file still gives the name the bytes
/// that the batch wrote.
#[test]
fn a_rename_over_a_name_counts_only_bytes_written_in_its_batch() {
    let mut w = table();
    let p = apply(
        &mut w,
        &[
            record(300, 100, "x.iso", DELETE),
            record(301, 100, "x.iso", RENAME),
        ],
    );
    w.add_sizes(&p, |_| (Some(4096 * MB), Some(4096 * MB)));
    assert_eq!(written(&w, 100, Some("x.iso")), Some((1, Written::Unknown)));

    let p = apply(
        &mut w,
        &[
            record(311, 100, "doc.tmp", CREATE | WRITE),
            record(310, 100, "doc", DELETE),
            record(311, 100, "doc", RENAME),
        ],
    );
    w.add_sizes(&p, |_| (None, Some(5 * MB)));
    assert_eq!(
        written(&w, 100, Some("doc")),
        Some((1, Written::Exact(5 * MB)))
    );
}

/// Fills `w` with `count` names in folder 100, each written `times` times,
/// from `first` on, in one batch.
fn fill(w: &mut Writes, first: usize, count: usize, times: usize) {
    let mut records = Vec::new();
    for n in first..first + count {
        for _ in 0..times {
            records.push(record(1000 + n as u128, 100, &format!("f{n}"), WRITE));
        }
    }
    apply(w, &records);
}

/// A full table drops its least written rows and says how low the
/// values may be, so a name dropped and written again stays within that
/// bound; a row the current batch counted in is never dropped.
#[test]
fn a_full_table_drops_its_least_written_rows_within_the_bound() {
    let mut w = table();
    // The folder's row and 4,094 names of 2 writes, and `light` of 1.
    fill(&mut w, 0, 4094, 2);
    apply(&mut w, &[record(999, 100, "light", WRITE)]);
    fill(&mut w, 5000, 1, 2);
    assert_eq!(name_writes(&w, 100, "light"), None);
    assert_eq!(w.status().map(|s| s.low_writes), Some(2));

    // Written again, and dropped again: 2 writes, within 0 and 0 + 4.
    apply(&mut w, &[record(999, 100, "light", WRITE)]);
    fill(&mut w, 6000, 510, 2);
    assert_eq!(name_writes(&w, 100, "light"), Some(1));
    fill(&mut w, 7000, 1, 2);
    assert_eq!(name_writes(&w, 100, "light"), None);
    assert_eq!(w.status().map(|s| s.low_writes), Some(4));

    // The batch that counts `light` also fills the table: `light` stays.
    let mut records = vec![record(999, 100, "light", WRITE)];
    for n in 8000..8600 {
        records.push(record(1000 + n, 100, &format!("f{n}"), WRITE));
        records.push(record(1000 + n, 100, &format!("f{n}"), WRITE));
    }
    apply(&mut w, &records);
    assert_eq!(name_writes(&w, 100, "light"), Some(1));
}
