//! Timings of edit mode's work on the window's thread, on synthetic texts,
//! without a window. Each prints the fewest CPU cycles of the calling thread
//! that one run took, with that run's wall time and its share of a 60 Hz
//! frame, and asserts nothing: the numbers decide whether [`Text`] needs a
//! representation that is cheap to copy and to change. Each test is
//! ignored, because only an optimized build gives meaningful numbers:
//! `pwsh ./build.ps1 -Timing compare::timing`.

use super::{
    Change, EDIT_LIMIT, EditedRows, Form, LINE_COUNT_LIMIT, Pane, SHOWN_LIMIT, Shown, Text,
    split_lines,
};
use crate::core::panel::Side;
use crate::core::viewer::Encoding;
use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

const FRAME: Duration = Duration::from_micros(16_667);

/// How many times each piece of work runs.
const RUNS: usize = 7;

/// The lines of a text of [`EDIT_LIMIT`] bytes whose lines are as long as
/// source code's.
const SOURCE_LINES: usize = 100_000;

/// Runs `work` [`RUNS`] times, each on a new input from `prepare`, and prints
/// the run with the fewest cycles. The input and the output are made and
/// dropped outside the count.
fn measure<I, R>(label: &str, mut prepare: impl FnMut() -> I, mut work: impl FnMut(&mut I) -> R) {
    let mut fewest = (u64::MAX, Duration::ZERO);
    for _ in 0..RUNS {
        let mut input = prepare();
        let (start, clock) = (gozler_winfs::thread_cycles(), Instant::now());
        let output = black_box(work(&mut input));
        let run = (gozler_winfs::thread_cycles() - start, clock.elapsed());
        drop(output);
        drop(input);
        if run.0 < fewest.0 {
            fewest = run;
        }
    }
    let (cycles, time) = fewest;
    println!(
        "{label:<62} {cycles:>14} cycles {:>10.3} ms {:>7.1}% of a frame",
        time.as_secs_f64() * 1e3,
        time.as_secs_f64() / FRAME.as_secs_f64() * 100.0
    );
}

/// A text of about [`EDIT_LIMIT`] bytes in `count` lines of equal length,
/// split as a file is read.
fn text(count: usize) -> Text {
    let line = "x".repeat((EDIT_LIMIT as usize / count).saturating_sub(2).max(1));
    let mut file = String::with_capacity((line.len() + 2) * count);
    for _ in 0..count {
        file.push_str(&line);
        file.push_str("\r\n");
    }
    let (lines, line_end, final_line_end) = split_lines(&file);
    Text {
        lines,
        form: Form {
            encoding: Encoding::Utf8,
            byte_order_mark: false,
            line_end,
            final_line_end,
        },
        editable: true,
    }
}

/// Issue #160: the first key typed while the worker holds a side's text, to
/// build the rows, find or encode a save, copies the whole text on the
/// window's thread through `Arc::make_mut`. A text of [`EDIT_LIMIT`] bytes in
/// [`LINE_COUNT_LIMIT`] lines is the worst case, and one in
/// [`SOURCE_LINES`] lines a source file's. It asserts nothing: its numbers
/// decide whether `Text::lines` is split into shared chunks.
#[test]
#[ignore = "timing; run in release"]
fn timing_make_mut_of_a_shared_text() {
    for count in [LINE_COUNT_LIMIT, SOURCE_LINES] {
        let base = Arc::new(text(count));
        measure(
            &format!("Arc::make_mut of a shared text, {count} lines"),
            || (Arc::clone(&base), Arc::clone(&base)),
            |(edited, _held): &mut (Arc<Text>, Arc<Text>)| Arc::make_mut(edited).lines.len(),
        );
    }
}

/// Issue #167: `Change::apply` of Enter near the start of a text of
/// [`LINE_COUNT_LIMIT`] lines moves every line after it. Right after an
/// `Arc::make_mut` copy the lines have no spare room, so the first Enter
/// also moves them to a larger allocation. It asserts nothing: its numbers
/// decide whether a line edit needs a representation that moves fewer
/// lines.
#[test]
#[ignore = "timing; run in release"]
fn timing_enter_near_the_start_of_a_long_text() {
    let lines = text(LINE_COUNT_LIMIT).lines;
    let enter = Change {
        side: Side::Left,
        at: 1,
        removed: vec![lines[1].clone()],
        inserted: vec![lines[1].clone(), String::new()],
    };
    measure(
        &format!("Change::apply of Enter at line 2, {LINE_COUNT_LIMIT} lines"),
        || {
            let mut spare = Vec::with_capacity(lines.len() + 1);
            spare.extend_from_slice(&lines);
            spare
        },
        |spare| enter.apply(spare),
    );
    measure(
        "the same, on lines just copied by Arc::make_mut",
        || lines.clone(),
        |copied| enter.apply(copied),
    );
}

/// Issue #167: `Pane::edited` of Enter at the end of the second row of a
/// pane of [`SHOWN_LIMIT`] rows shifts the line of every row after it and
/// moves those rows. It asserts nothing: its numbers decide whether the
/// pane needs a representation that shifts fewer rows.
#[test]
#[ignore = "timing; run in release"]
fn timing_enter_near_the_start_of_a_long_pane() {
    let lines = text(SHOWN_LIMIT).lines;
    let shown: Vec<Shown> = (0..SHOWN_LIMIT as u64)
        .map(|line| Shown::Row {
            left: Some(line),
            right: Some(line),
            differs: false,
        })
        .collect();
    let pane = Pane::new(&shown, Side::Left);
    // What the window reads after Enter at the end of the second row: that
    // row's text and the new empty row below it, with the caret there.
    let mut typed: Vec<u16> = lines[1].encode_utf16().collect();
    typed.push(0x0D);
    let enter = EditedRows {
        first: 1,
        last: 2,
        count: SHOWN_LIMIT + 1,
        caret: typed.len(),
        text: typed,
    };
    measure(
        &format!("Pane::edited of Enter at row 2, {SHOWN_LIMIT} rows"),
        || (pane.clone(), enter.clone()),
        |(pane, enter): &mut (Pane, EditedRows)| pane.edited(Side::Left, &lines, enter.clone()),
    );
}
