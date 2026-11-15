//! The one matching method of Compare by content (DESIGN.md: Compare by
//! content): histogram diff over two sequences of keys, which are lines or
//! the words of a line. The keys common to both that occur least often
//! anchor the match, so keys that repeat everywhere, such as blank lines and
//! braces, never pull the sequences out of line. A part whose common keys
//! all repeat too often to anchor it is matched with the fewest insertions
//! and deletions instead, as in Git.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::ops::Range;

/// A key that occurs more often than this in a part of the first sequence
/// never anchors that part, as in Git's histogram diff, which bounds the
/// work of each part.
const MAX_CHAIN: usize = 64;

/// The steps that one diff gives [`fewest_edits`], a fraction of a second:
/// past them, a part without an anchor is one hunk.
const EDIT_STEPS: usize = 1 << 26;

/// The most insertions and deletions that [`middle`] follows on each side
/// of the middle, which bounds its memory.
const MAX_EDITS: isize = 1 << 14;

/// A part of the two sequences that differs: the keys `a` of the first are
/// replaced by the keys `b` of the second. One of the two can be empty.
/// The parts between two hunks are equal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hunk {
    pub(crate) a: Range<usize>,
    pub(crate) b: Range<usize>,
}

/// The hunks that turn `a` into `b`, in order; `None` when `cancelled`
/// says so, which it is asked between parts. The work runs on an explicit
/// stack, because a part can split into one key and the rest many times
/// over.
pub(crate) fn diff(a: &[u32], b: &[u32], cancelled: &dyn Fn() -> bool) -> Option<Vec<Hunk>> {
    let mut hunks: Vec<Hunk> = Vec::new();
    // Parts still to match; the last one pushed is the leftmost, so the
    // hunks come out in order.
    let mut parts = vec![(0..a.len(), 0..b.len())];
    let mut steps = 0usize;
    let mut edit_steps = EDIT_STEPS;
    while let Some((mut ra, mut rb)) = parts.pop() {
        steps += 1;
        if steps.is_multiple_of(1024) && cancelled() {
            return None;
        }
        trim(a, b, &mut ra, &mut rb);
        if ra.is_empty() && rb.is_empty() {
            continue;
        }
        if ra.is_empty() || rb.is_empty() {
            push(&mut hunks, ra, rb);
            continue;
        }
        match anchor(a, b, ra.clone(), rb.clone()) {
            Found::Anchor(Anchor {
                a: matched_a,
                b: matched_b,
            }) => {
                parts.push((matched_a.end..ra.end, matched_b.end..rb.end));
                parts.push((ra.start..matched_a.start, rb.start..matched_b.start));
            }
            Found::Frequent => fewest_edits(a, b, ra, rb, &mut hunks, &mut edit_steps, cancelled)?,
            Found::Nothing => push(&mut hunks, ra, rb),
        }
    }
    Some(hunks)
}

/// Matches the equal keys at both ends of the parts `ra` of `a` and `rb`
/// of `b`, and leaves the parts between them.
fn trim(a: &[u32], b: &[u32], ra: &mut Range<usize>, rb: &mut Range<usize>) {
    while ra.start < ra.end && rb.start < rb.end && a[ra.start] == b[rb.start] {
        ra.start += 1;
        rb.start += 1;
    }
    while ra.start < ra.end && rb.start < rb.end && a[ra.end - 1] == b[rb.end - 1] {
        ra.end -= 1;
        rb.end -= 1;
    }
}

/// Matches the parts `ra` of `a` and `rb` of `b` with the fewest keys
/// inserted and deleted (Myers' diff), for parts whose common keys all
/// occur too often to anchor them, as Git's histogram diff falls back to
/// its classic diff: pairing them in order would pull them out of line at
/// their first insertion. A part that takes more than the `steps` left is
/// one hunk. `None` when `cancelled` says so.
fn fewest_edits(
    a: &[u32],
    b: &[u32],
    ra: Range<usize>,
    rb: Range<usize>,
    hunks: &mut Vec<Hunk>,
    steps: &mut usize,
    cancelled: &dyn Fn() -> bool,
) -> Option<()> {
    // As in `diff`, the last part pushed is the leftmost.
    let mut parts = vec![(ra, rb)];
    while let Some((mut ra, mut rb)) = parts.pop() {
        if cancelled() {
            return None;
        }
        trim(a, b, &mut ra, &mut rb);
        if ra.is_empty() && rb.is_empty() {
            continue;
        }
        let split = (!ra.is_empty() && !rb.is_empty())
            .then(|| middle(&a[ra.clone()], &b[rb.clone()], steps))
            .flatten();
        match split {
            Some((x, y)) => {
                parts.push((ra.start + x..ra.end, rb.start + y..rb.end));
                parts.push((ra.start..ra.start + x, rb.start..rb.start + y));
            }
            None => push(hunks, ra, rb),
        }
    }
    Some(())
}

/// Where a shortest path of insertions and deletions from the start of `a`
/// and `b` to their ends crosses its middle (Myers' middle snake), so the
/// keys before it and after it can be matched apart, as in Myers' paper
/// and Google's diff-match-patch; `None` when that takes more than the
/// `steps` left or more than [`MAX_EDITS`] edits on each side. The point
/// is never a corner, so both halves are smaller than the whole.
fn middle(a: &[u32], b: &[u32], steps: &mut usize) -> Option<(usize, usize)> {
    let (n, m) = (
        isize::try_from(a.len()).ok()?,
        isize::try_from(b.len()).ok()?,
    );
    // The diagonals that each direction reaches, at most, and the arrays
    // of the furthest place reached on each, indexed from `-most`.
    let most = ((n + m + 1) / 2).min(MAX_EDITS);
    let at = |k: isize| (most + k) as usize;
    let mut forward = vec![-1isize; at(most) + 2];
    let mut backward = forward.clone();
    forward[at(1)] = 0;
    backward[at(1)] = 0;
    let delta = n - m;
    let odd = delta % 2 != 0;
    // Diagonals that left the grid on one side are not followed again.
    let (mut forward_start, mut forward_end) = (0, 0);
    let (mut backward_start, mut backward_end) = (0, 0);
    // The point where the paths meet, inside the grid and not a corner.
    let split = |x: isize, y: isize| {
        ((0..=n).contains(&x) && (0..=m).contains(&y) && (x, y) != (0, 0) && (x, y) != (n, m))
            .then_some((x as usize, y as usize))
    };
    let reached = |paths: &[isize], k: isize| {
        let place = *usize::try_from(most + delta - k)
            .ok()
            .and_then(|index| paths.get(index))?;
        (place != -1).then_some(place)
    };
    for d in 0..most {
        let mut k = forward_start - d;
        while k <= d - forward_end {
            let mut x = if k == -d || (k != d && forward[at(k - 1)] < forward[at(k + 1)]) {
                forward[at(k + 1)]
            } else {
                forward[at(k - 1)] + 1
            };
            let mut y = x - k;
            let from = x;
            while x < n && y < m && x >= 0 && y >= 0 && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            *steps = steps.checked_sub(1 + (x - from).unsigned_abs())?;
            forward[at(k)] = x;
            if x > n {
                forward_end += 2;
            } else if y > m {
                forward_start += 2;
            } else if odd
                && let Some(back) = reached(&backward, k)
                && x >= n - back
            {
                return split(x, y);
            }
            k += 2;
        }
        let mut k = backward_start - d;
        while k <= d - backward_end {
            let mut x = if k == -d || (k != d && backward[at(k - 1)] < backward[at(k + 1)]) {
                backward[at(k + 1)]
            } else {
                backward[at(k - 1)] + 1
            };
            let mut y = x - k;
            let from = x;
            while x < n
                && y < m
                && x >= 0
                && y >= 0
                && a[(n - x - 1) as usize] == b[(m - y - 1) as usize]
            {
                x += 1;
                y += 1;
            }
            *steps = steps.checked_sub(1 + (x - from).unsigned_abs())?;
            backward[at(k)] = x;
            if x > n {
                backward_end += 2;
            } else if y > m {
                backward_start += 2;
            } else if !odd
                && let Some(front) = reached(&forward, k)
                && front >= n - x
            {
                return split(front, front - (delta - k));
            }
            k += 2;
        }
    }
    None
}

/// Adds a hunk, joined to the last one when the two touch.
fn push(hunks: &mut Vec<Hunk>, a: Range<usize>, b: Range<usize>) {
    if let Some(last) = hunks.last_mut()
        && last.a.end == a.start
        && last.b.end == b.start
    {
        last.a.end = a.end;
        last.b.end = b.end;
        return;
    }
    hunks.push(Hunk { a, b });
}

/// How [`anchor`] orders the runs, the lowest first: the least count of a
/// key, the longest run, and the distance from the middles of the two
/// parts, each doubled to stay whole.
type Rank = (usize, Reverse<usize>, usize);

/// Equal runs of the two parts that the match keeps.
struct Anchor {
    a: Range<usize>,
    b: Range<usize>,
}

/// What [`anchor`] finds in two parts.
enum Found {
    Anchor(Anchor),
    /// Keys common to both parts, each too frequent to anchor them.
    Frequent,
    /// No key common to both parts, which differ whole.
    Nothing,
}

/// The run of equal keys that anchors the parts `ra` of `a` and `rb` of
/// `b`: of the runs around each key of `rb` that occurs in `ra` at most
/// [`MAX_CHAIN`] times, the one whose least frequent key occurs least often
/// in `ra`, of those the longest, and of those the nearest to the middles
/// of `ra` and `rb`. The middles keep the parts it splits into of even
/// size, so files that differ in every other line take time in proportion
/// to n log n, not n², and of the places of a repeated run they pair the
/// ones that face each other.
fn anchor(a: &[u32], b: &[u32], ra: Range<usize>, rb: Range<usize>) -> Found {
    // Where each key occurs in `ra`, up to one more than the chain allows,
    // which marks it as too frequent.
    let mut index: HashMap<u32, Vec<usize>> = HashMap::new();
    for i in ra.clone() {
        let places = index.entry(a[i]).or_default();
        if places.len() <= MAX_CHAIN {
            places.push(i);
        }
    }
    let count = |key: u32| index.get(&key).map_or(usize::MAX, Vec::len);
    let mut best: Option<(Rank, Anchor)> = None;
    let mut frequent = false;
    let (middle_a, middle_b) = (ra.start + ra.end, rb.start + rb.end);
    let mut j = rb.start;
    while j < rb.end {
        let Some(places) = index.get(&b[j]) else {
            j += 1;
            continue;
        };
        if places.len() > MAX_CHAIN {
            frequent = true;
            j += 1;
            continue;
        }
        let mut next = j + 1;
        for &i in places {
            let (mut start_a, mut start_b) = (i, j);
            while start_a > ra.start && start_b > rb.start && a[start_a - 1] == b[start_b - 1] {
                start_a -= 1;
                start_b -= 1;
            }
            let (mut end_a, mut end_b) = (i + 1, j + 1);
            while end_a < ra.end && end_b < rb.end && a[end_a] == b[end_b] {
                end_a += 1;
                end_b += 1;
            }
            let least = (start_a..end_a)
                .map(|k| count(a[k]))
                .min()
                .unwrap_or(usize::MAX);
            let rank = (
                least,
                Reverse(end_a - start_a),
                (start_a + end_a).abs_diff(middle_a) + (start_b + end_b).abs_diff(middle_b),
            );
            if best.as_ref().is_none_or(|(best, _)| rank < *best) {
                let anchor = Anchor {
                    a: start_a..end_a,
                    b: start_b..end_b,
                };
                best = Some((rank, anchor));
            }
            next = next.max(end_b);
        }
        j = next;
    }
    match best {
        Some((_, anchor)) => Found::Anchor(anchor),
        None if frequent => Found::Frequent,
        None => Found::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never() -> bool {
        false
    }

    fn hunks(a: &[u32], b: &[u32]) -> Vec<(Range<usize>, Range<usize>)> {
        diff(a, b, &never)
            .expect("nothing cancels")
            .into_iter()
            .map(|hunk| (hunk.a, hunk.b))
            .collect()
    }

    /// An inserted block keeps the lines around it paired, and blank lines
    /// (key 0), which occur everywhere, never anchor a wrong match.
    #[test]
    fn an_inserted_block_keeps_the_lines_around_it_paired() {
        assert!(hunks(&[1, 2, 3], &[1, 2, 3]).is_empty());
        assert_eq!(hunks(&[1, 2, 3, 4], &[1, 2, 9, 9, 3, 4]), [(2..2, 2..4)]);
        assert_eq!(hunks(&[1, 2, 3], &[1, 3]), [(1..2, 1..1)]);
        let a = [5, 0, 6, 0, 7, 0, 8];
        let b = [5, 0, 9, 0, 0, 6, 0, 7, 0, 8];
        assert_eq!(hunks(&a, &b), [(2..2, 2..5)]);
        assert_eq!(hunks(&[1, 2], &[3, 4]), [(0..2, 0..2)]);
    }

    /// Lines that all repeat, fewer times than the chain allows and more,
    /// still pair line for line: each inserted line is its own hunk
    /// (reported defect: the whole part was one difference).
    #[test]
    fn repeated_lines_keep_their_pairs() {
        for times in [20, 100] {
            let a: Vec<u32> = [1, 2].repeat(times);
            let b: Vec<u32> = [1, 3, 2].repeat(times);
            let inserted: Vec<_> = (0..times)
                .map(|k| (2 * k + 1..2 * k + 1, 3 * k + 1..3 * k + 2))
                .collect();
            assert_eq!(hunks(&a, &b), inserted, "{times} times");
        }
    }

    /// Short sequences, whose parts anchor, and long ones of three keys,
    /// whose keys repeat past the chain and leave parts without an anchor.
    fn sequence() -> impl proptest::strategy::Strategy<Value = Vec<u32>> {
        proptest::prop_oneof![
            proptest::collection::vec(0u32..6, 0..40),
            proptest::collection::vec(0u32..3, 150..300),
        ]
    }

    proptest::proptest! {
        /// The hunks are in order, never overlap, and the parts between
        /// them are equal, so they turn `a` into `b` exactly.
        #[test]
        fn the_hunks_turn_one_sequence_into_the_other(a in sequence(), b in sequence()) {
            let found = diff(&a, &b, &never).expect("nothing cancels");
            let mut rebuilt = Vec::new();
            let (mut i, mut j) = (0, 0);
            for hunk in &found {
                proptest::prop_assert!(hunk.a.start >= i && hunk.b.start >= j);
                proptest::prop_assert_eq!(&a[i..hunk.a.start], &b[j..hunk.b.start]);
                rebuilt.extend_from_slice(&a[i..hunk.a.start]);
                rebuilt.extend_from_slice(&b[hunk.b.clone()]);
                (i, j) = (hunk.a.end, hunk.b.end);
            }
            proptest::prop_assert_eq!(&a[i..], &b[j..]);
            rebuilt.extend_from_slice(&a[i..]);
            proptest::prop_assert_eq!(rebuilt, b);
        }
    }
}
