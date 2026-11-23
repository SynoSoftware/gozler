//! Folders copied into themselves. A step that copies a folder into a folder
//! inside it, again and again, leaves a path that ends in the same group of
//! folder names again and again, such as `a\a\a\a` or `a\b\c\a\b\c\a\b\c`.
//!
//! [`RepeatTest`] owns the rule "the path ends in the same group of names N
//! times", for a group of any length. It walks the folder's chain of names
//! upward once, counting how often the folder's own name occurs, which ends
//! the test on almost every folder. Only a folder whose name occurs N times
//! runs the Z-function (Gusfield's Z-algorithm) over the chain, which gives
//! every group length in one pass. The journal tail's detector and the
//! `repeats:` search both call it. [`RepeatEvents`] is the list of the trees
//! the detector found.

use std::sync::atomic::{AtomicU64, Ordering};

use super::volume::MAX_PATH_HOPS;
use super::{ComponentBytes, VolumeIndex};
use crate::proto::RepeatingFolder;
use crate::types::{EntryIdx, Frn, NIL};

/// The fewest copies of a group that make an event: gozlerd makes none
/// below it, whatever limit a client asks for.
pub const EVENT_COPIES: u32 = 4;

/// The most events a volume's [`RepeatEvents`] keeps.
const EVENTS: usize = 64;

/// A group of folder names that a folder's path ends in, again and again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Repeat {
    /// How many names the group holds.
    pub period: u32,
    /// How many whole times the path ends in the group.
    pub copies: u32,
    /// The highest folder the repeat reaches, the part of a group above the
    /// whole groups included, so every create of one tree gives the same top.
    pub top: EntryIdx,
}

/// The test's two buffers, the chain of names and the Z values, which the
/// caller keeps and reuses: each is allocated once at the most names a path
/// walk reads, so the test allocates nothing.
pub struct RepeatTest {
    /// The folder, then its parent, and so on up to the root, which it
    /// leaves out.
    chain: Vec<EntryIdx>,
    /// `z[k]`: how many names, from the folder upward, equal the name `k`
    /// levels above them.
    z: Vec<u32>,
}

impl RepeatTest {
    /// Allocates both buffers at their full size. Allocate outside the
    /// index's write lock.
    pub fn allocate() -> Self {
        Self {
            chain: Vec::with_capacity(MAX_PATH_HOPS),
            z: Vec::with_capacity(MAX_PATH_HOPS),
        }
    }

    /// The group that `folder`'s path ends in at least `least` times, of any
    /// length; `None` when no group does. Of several, the one whose repeat
    /// reaches highest, and the shortest on a tie, so `a` 8 times is `a` 8
    /// times, not `a\a` 4 times.
    pub fn highest_run(
        &mut self,
        index: &VolumeIndex,
        folder: EntryIdx,
        least: u32,
    ) -> Option<Repeat> {
        let least = least.max(2);
        if !self.read(index, folder, least) {
            return None;
        }
        let (period, copies, reach) = highest(&self.z, least)?;
        Some(Repeat {
            period,
            copies,
            top: self.chain[reach - 1],
        })
    }

    /// The most whole copies that any group of `folder`'s path ends in, when
    /// that is at least `least`; 1 otherwise, which no range whose lowest
    /// value is `least` accepts, and which a range that holds 1 does.
    pub fn most_copies(&mut self, index: &VolumeIndex, folder: EntryIdx, least: u32) -> u32 {
        let least = least.max(2);
        if !self.read(index, folder, least) {
            return 1;
        }
        let most = (self.z.iter().enumerate().skip(1))
            .map(|(k, &matched)| copies(matched, k))
            .max()
            .unwrap_or(1);
        if most >= least { most } else { 1 }
    }

    /// Reads `folder`'s chain of names, and when the folder's own name
    /// occurs at least `least` times in it, fills the Z values of the group
    /// lengths that can give `least` copies and answers `true`.
    fn read(&mut self, index: &VolumeIndex, folder: EntryIdx, least: u32) -> bool {
        self.chain.clear();
        let own = index.folded_name(folder);
        let root = index.root();
        let mut times = 0u32;
        let mut cur = folder;
        while cur != NIL && cur != root && self.chain.len() < MAX_PATH_HOPS {
            times += u32::from(same(index.folded_name(cur), own));
            self.chain.push(cur);
            cur = index.entry(cur).parent();
        }
        // A walk that the hop cap stopped has no full path: a parent cycle
        // would read as copies, and a deeper tree would give a new top at
        // each create.
        if (cur != NIL && cur != root) || times < least {
            return false;
        }
        let chain = &self.chain;
        let last = chain.len() / least as usize;
        z_values(chain.len(), last, &mut self.z, |i, j| {
            same(index.folded_name(chain[i]), index.folded_name(chain[j]))
        });
        true
    }
}

/// Whether two folded names are equal. Every entry of one name shares that
/// name's folded bytes, so the address alone settles the usual case.
#[inline]
fn same(a: &[u8], b: &[u8]) -> bool {
    std::ptr::eq(a, b) || a == b
}

/// Fills `z[k]` for `k` from 1 to `last`, over a chain of `len` names that
/// `same` compares by place: how many names from the start equal the names
/// `k` places further. At most `2 * len` comparisons.
fn z_values(len: usize, last: usize, z: &mut Vec<u32>, same: impl Fn(usize, usize) -> bool) {
    z.clear();
    z.resize(last + 1, 0);
    // `left..right` is the match that reaches furthest so far.
    let (mut left, mut right) = (0, 0);
    for i in 1..=last {
        let mut n = if i < right {
            (right - i).min(z[i - left] as usize)
        } else {
            0
        };
        while i + n < len && same(n, i + n) {
            n += 1;
        }
        z[i] = u32::try_from(n).expect("a chain holds at most MAX_PATH_HOPS names");
        if i + n > right {
            (left, right) = (i, i + n);
        }
    }
}

/// How many whole times the group of length `k` repeats, whose Z value is
/// `matched`.
fn copies(matched: u32, k: usize) -> u32 {
    1 + matched / k as u32
}

/// Of the group lengths in `z` that repeat at least `least` times, the one
/// whose repeat reaches highest, the shortest on a tie: its length, its
/// copies and how many names its repeat reaches.
fn highest(z: &[u32], least: u32) -> Option<(u32, u32, usize)> {
    let mut best: Option<(u32, u32, usize)> = None;
    for (k, &matched) in z.iter().enumerate().skip(1) {
        let copies = copies(matched, k);
        let reach = k + matched as usize;
        if copies >= least && best.is_none_or(|(_, _, highest)| reach > highest) {
            best = Some((k as u32, copies, reach));
        }
    }
    best
}

/// One folder tree that a copy into itself made: a top and a period.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepeatEvent {
    /// The [`Repeat::top`] of the tree.
    pub top: Frn,
    /// The folder whose create last made or updated the event.
    pub deepest: Frn,
    /// The [`Repeat::period`] of the tree.
    pub period: u32,
    pub copies: u32,
    /// When the event was made, as a FILETIME.
    pub time: i64,
    /// The service's sequence number of the event's last change, which
    /// orders the events of every volume.
    pub number: u64,
}

/// A volume's folder trees copied into themselves, as the journal tail finds
/// them, in a list of fixed size that drops the event changed least recently
/// when it is full. It is allocated at its full size, so recording an event
/// never grows it.
pub struct RepeatEvents {
    events: Vec<RepeatEvent>,
    /// How many events the list dropped.
    dropped: u64,
    /// The service's sequence counter, which gives each change its number.
    /// A list without one records no event, so an index that was never
    /// installed in the service makes none.
    numbers: Option<&'static AtomicU64>,
    test: RepeatTest,
}

impl RepeatEvents {
    /// What the list and its test's buffers hold in memory, for `--status`.
    pub(crate) fn memory(&self) -> ComponentBytes {
        ComponentBytes::of_vec(&self.events)
            + ComponentBytes::of_vec(&self.test.chain)
            + ComponentBytes::of_vec(&self.test.z)
    }

    /// An empty list that holds no memory and records nothing, to stand in
    /// the index while a batch holds its list: the batch's checks need the
    /// index and the list at once, and taking the list with a default would
    /// allocate under the write lock.
    pub const PLACEHOLDER: RepeatEvents = RepeatEvents {
        events: Vec::new(),
        dropped: 0,
        numbers: None,
        test: RepeatTest {
            chain: Vec::new(),
            z: Vec::new(),
        },
    };

    /// Allocates the list and its test at their full size. Allocate outside
    /// the index's write lock.
    pub fn allocate() -> Self {
        Self {
            events: Vec::with_capacity(EVENTS),
            dropped: 0,
            numbers: None,
            test: RepeatTest::allocate(),
        }
    }

    /// Gives the list the service's sequence counter, from which it numbers
    /// every event it makes or updates from now on.
    pub fn number_from(&mut self, numbers: &'static AtomicU64) {
        self.numbers = Some(numbers);
    }

    /// Tests `folder`, which was just created, and records the tree it
    /// completes when its path ends in a group at least [`EVENT_COPIES`] times:
    /// a new event, or an update of the event with the same top and period,
    /// each with the counter's next number. `time` is a FILETIME. Answers
    /// whether it made or updated an event.
    pub fn check(&mut self, index: &VolumeIndex, folder: EntryIdx, time: i64) -> bool {
        let Some(numbers) = self.numbers else {
            return false;
        };
        let Some(repeat) = self.test.highest_run(index, folder, EVENT_COPIES) else {
            return false;
        };
        let top = index.entry(repeat.top).frn();
        let deepest = index.entry(folder).frn();
        let number = numbers.fetch_add(1, Ordering::AcqRel) + 1;
        let same_tree =
            |event: &&mut RepeatEvent| event.top == top && event.period == repeat.period;
        if let Some(event) = self.events.iter_mut().find(same_tree) {
            event.copies = repeat.copies;
            event.deepest = deepest;
            event.number = number;
            return true;
        }
        if self.events.len() == EVENTS {
            let oldest = (0..self.events.len())
                .min_by_key(|&at| self.events[at].number)
                .expect("a full list holds events");
            self.events.swap_remove(oldest);
            self.dropped += 1;
        }
        self.events.push(RepeatEvent {
            top,
            deepest,
            period: repeat.period,
            copies: repeat.copies,
            time,
            number,
        });
        true
    }

    /// The events whose number is above `number`, in no order.
    #[cfg(test)]
    fn since(&self, number: u64) -> impl Iterator<Item = &RepeatEvent> {
        self.events
            .iter()
            .filter(move |event| event.number > number)
    }

    /// The events that `wanted` keeps, as a reply sends them, with the full
    /// paths under `mount_prefix` that their folders have now; an event
    /// whose top is gone or has no full path is left out. With `may_list`, a
    /// filtered user's answer for a folder's descriptor, only the events
    /// whose top is in a folder the user may list are given, and a deepest
    /// folder in a folder they may not list is not named; `None` gives every
    /// event, for a user who sees every file.
    pub fn reported(
        &self,
        index: &VolumeIndex,
        mount_prefix: &str,
        wanted: impl Fn(&RepeatEvent) -> bool,
        may_list: Option<&(impl Fn(&[u8]) -> bool + ?Sized)>,
    ) -> Vec<RepeatingFolder> {
        let mut kept = self.events.iter().filter(|&event| wanted(event)).peekable();
        if kept.peek().is_none() {
            return Vec::new();
        }
        let seen_by = index.seen_by(may_list);
        let mut path = Vec::new();
        let mut path_of = |frn: Frn| index.seen_path(frn, mount_prefix, &seen_by, &mut path);
        kept.filter_map(|event| {
            Some(RepeatingFolder {
                number: event.number,
                top: path_of(event.top)?,
                deepest: path_of(event.deepest),
                period: event.period,
                copies: event.copies,
                time: event.time,
            })
        })
        .collect()
    }

    /// How many events the list dropped since it was made.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    use super::{EVENT_COPIES, Repeat, RepeatEvents, RepeatTest, highest, z_values};
    use crate::index::settle::Batch;
    use crate::index::{NTFS_ROOT_FRN, VolumeIndex};
    use crate::types::{EntryIdx, Frn};
    use crate::usn::record::{
        FILE_ATTRIBUTE_DIRECTORY, ParsedUsnRecord, USN_REASON_BASIC_INFO_CHANGE, USN_REASON_CLOSE,
        USN_REASON_FILE_CREATE,
    };
    use proptest::prelude::*;
    use std::sync::atomic::AtomicU64;

    /// The folders of `path` under the root, each inside the one before, and
    /// their entries, top first. A path's folders get file IDs from `first`.
    fn add_path(index: &mut VolumeIndex, first: u128, path: &str) -> Vec<EntryIdx> {
        let mut parent = NTFS_ROOT_FRN;
        let mut entries = Vec::new();
        for (n, name) in path.split('\\').enumerate() {
            let frn = Frn(first + n as u128);
            index.insert_enum(&ParsedUsnRecord {
                frn,
                parent_frn: parent,
                reason: USN_REASON_FILE_CREATE,
                attributes: FILE_ATTRIBUTE_DIRECTORY,
                name: name.as_bytes().to_vec(),
                time: 0,
            });
            entries.push(index.head_of(frn).expect("the folder was just added"));
            parent = frn;
        }
        entries
    }

    fn tree(path: &str) -> (VolumeIndex, Vec<EntryIdx>) {
        let mut index = VolumeIndex::new(NTFS_ROOT_FRN);
        let entries = add_path(&mut index, 100, path);
        (index, entries)
    }

    /// The test's answer for the deepest folder of `path`, with its top as
    /// the place of the top folder in `path`.
    fn run_of(path: &str, least: u32) -> Option<(u32, u32, usize)> {
        let (index, entries) = tree(path);
        let deepest = *entries.last().expect("a path names a folder");
        let Repeat {
            period,
            copies,
            top,
        } = RepeatTest::allocate().highest_run(&index, deepest, least)?;
        let place = entries
            .iter()
            .position(|&e| e == top)
            .expect("the top is on the path");
        Some((period, copies, place))
    }

    fn most_of(path: &str, least: u32) -> u32 {
        let (index, entries) = tree(path);
        let deepest = *entries.last().expect("a path names a folder");
        RepeatTest::allocate().most_copies(&index, deepest, least)
    }

    fn times(group: &str, n: usize) -> String {
        vec![group; n].join("\\")
    }

    /// The fourth copy is where the detector warns: an off-by-one in the Z
    /// values or the copies arithmetic warns at the third, or never.
    #[test]
    fn a_fourth_copy_of_a_group_of_any_length_repeats_and_a_third_does_not() {
        for group in ["a", r"a\b", r"a\b\c", r"a\b\c\d\e\f\g"] {
            let period = group.split('\\').count() as u32;
            assert_eq!(run_of(&times(group, 4), 4), Some((period, 4, 0)), "{group}");
            assert_eq!(run_of(&times(group, 3), 4), None, "{group}");
        }
    }

    /// Both `a` and `a\a` reach 8 names, and the shorter group names the copy.
    #[test]
    fn eight_names_alike_are_one_name_eight_times() {
        assert_eq!(run_of(&times("a", 8), 4), Some((1, 8, 0)));
    }

    #[test]
    fn copies_ignore_case_and_need_the_parent_to_match() {
        assert_eq!(most_of(r"Src\src", 2), 2);
        assert_eq!(most_of(r"a\b\a\c", 2), 1);
        assert_eq!(most_of(r"a\b\a\c", 4), 1);
    }

    /// Every create of one tree must give the same top, or the detector makes
    /// a new event for each.
    #[test]
    fn the_top_includes_the_part_of_a_group_above_the_whole_groups() {
        let fourth_b = format!(r"r\{}", times(r"a\b", 4));
        assert_eq!(run_of(&fourth_b, 4), Some((2, 4, 1)));
        let fifth_a = format!(r"{fourth_b}\a");
        assert_eq!(run_of(&fifth_a, 4), Some((2, 4, 1)));
    }

    /// `x` copied into `x\a\a\a\a` four times: the group of 5 reaches higher
    /// than the `a` 4 times inside each copy, so it names the tree.
    #[test]
    fn a_group_that_holds_a_repeat_of_its_own_names_the_tree() {
        let four = times(r"x\a\a\a\a", 4);
        assert_eq!(run_of(&four, 4), Some((5, 4, 0)));
        assert_eq!(run_of(&format!(r"{four}\x"), 4), Some((5, 4, 0)));
        assert_eq!(run_of(&format!(r"{four}\x\a"), 4), Some((5, 4, 0)));
    }

    /// A runaway copy past the hop cap must not answer, or the top it gives
    /// moves down at each create and every create makes a new event.
    #[test]
    fn a_path_deeper_than_the_hop_cap_has_no_copies() {
        let path = times("a", super::MAX_PATH_HOPS + 1);
        assert_eq!(run_of(&path, 4), None);
        assert_eq!(most_of(&path, 2), 1);
    }

    /// The plain check: for each group length, compares the names one by one.
    fn plain_highest(names: &[u8], least: u32) -> Option<(u32, u32, usize)> {
        let mut best: Option<(u32, u32, usize)> = None;
        for k in 1..=names.len() / least as usize {
            let mut n = 0;
            while k + n < names.len() && names[n] == names[k + n] {
                n += 1;
            }
            let copies = 1 + (n / k) as u32;
            if copies >= least && best.is_none_or(|(_, _, reach)| k + n > reach) {
                best = Some((k as u32, copies, k + n));
            }
        }
        best
    }

    proptest! {
        /// Chains of 3 names repeat often, so they reach every branch of the
        /// Z-function's reuse of earlier values.
        #[test]
        fn the_z_function_agrees_with_a_plain_check(
            names in proptest::collection::vec(0u8..3, 1..48),
            least in 2u32..6,
        ) {
            let mut z = Vec::new();
            z_values(names.len(), names.len() / least as usize, &mut z, |i, j| names[i] == names[j]);
            prop_assert_eq!(highest(&z, least), plain_highest(&names, least));
        }
    }

    fn counter() -> &'static AtomicU64 {
        Box::leak(Box::new(AtomicU64::new(0)))
    }

    /// Checks each folder of `entries` in turn, as the tail does at each
    /// create.
    fn check_all(events: &mut RepeatEvents, index: &VolumeIndex, entries: &[EntryIdx]) {
        for &folder in entries {
            events.check(index, folder, 0);
        }
    }

    /// A copy into itself must warn once and then update, not make an event
    /// for each create, and an update must take a higher number, or a held
    /// request never learns of it.
    #[test]
    fn a_fifth_copy_updates_the_event_of_the_fourth() {
        let (index, entries) = tree(&times("a", 5));
        let mut events = RepeatEvents::allocate();
        events.number_from(counter());
        check_all(&mut events, &index, &entries[..4]);
        let made: Vec<_> = events.since(0).copied().collect();
        assert_eq!(made.len(), 1);
        assert_eq!((made[0].copies, made[0].number), (4, 1));
        assert!(events.check(&index, entries[4], 0));
        let updated: Vec<_> = events.since(0).copied().collect();
        assert_eq!(updated.len(), 1);
        assert_eq!((updated[0].copies, updated[0].number), (5, 2));
        assert_eq!(updated[0].top, made[0].top);
        assert_eq!(updated[0].deepest, index.entry(entries[4]).frn());
    }

    #[test]
    fn nine_creates_of_two_names_make_one_event_from_the_top() {
        let (index, entries) = tree(&format!(r"{}\a", times(r"a\b", 4)));
        let mut events = RepeatEvents::allocate();
        events.number_from(counter());
        check_all(&mut events, &index, &entries);
        let made: Vec<_> = events.since(0).copied().collect();
        assert_eq!(made.len(), 1);
        assert_eq!(made[0].period, 2);
        assert_eq!(made[0].top, index.entry(entries[0]).frn());
    }

    /// A tree that passed 4 copies while no event could be made, such as
    /// before the service started, warns at its next create.
    #[test]
    fn a_tree_past_four_copies_with_no_event_gets_one_at_its_next_create() {
        let (index, entries) = tree(&times("a", 7));
        let mut events = RepeatEvents::allocate();
        events.number_from(counter());
        assert!(events.check(&index, entries[6], 0));
        let made: Vec<_> = events.since(0).copied().collect();
        assert_eq!((made.len(), made[0].copies), (1, 7));
    }

    /// An index that a build is making, and every test or bench index, has
    /// no counter, so it must make no event without a number.
    #[test]
    fn a_list_without_a_counter_records_nothing() {
        let (index, entries) = tree(&times("a", 4));
        let mut events = RepeatEvents::allocate();
        assert!(!events.check(&index, entries[3], 0));
        assert_eq!(events.since(0).count(), 0);
    }

    /// A copy tool can add bits such as BASIC_INFO_CHANGE to the CLOSE
    /// record of the open that created a folder, and that record must still
    /// count, while the open's first record, without CLOSE, must not, or one
    /// create changes the event twice. A batch that replays the changes made
    /// during a build must make none, or a copy made before the service
    /// started warns.
    #[test]
    fn a_folder_create_is_checked_once_and_not_while_a_build_replays() {
        let (mut index, entries) = tree(&times("a", 3));
        index.repeats_mut().number_from(counter());
        let created = |reason| ParsedUsnRecord {
            frn: Frn(200),
            parent_frn: index.entry(entries[2]).frn(),
            reason,
            attributes: FILE_ATTRIBUTE_DIRECTORY,
            name: b"a".to_vec(),
            time: 0,
        };
        let closed = USN_REASON_FILE_CREATE | USN_REASON_BASIC_INFO_CHANGE | USN_REASON_CLOSE;
        let records = [created(USN_REASON_FILE_CREATE), created(closed)];
        let replay = Batch {
            detects_repeats: false,
            ..Batch::of(&records)
        };
        index.apply_batch(&replay);
        assert_eq!(index.repeats().since(0).count(), 0);
        index.apply_batch(&Batch::of(&records));
        let made: Vec<_> = index.repeats().since(0).copied().collect();
        assert_eq!(made.len(), 1);
        assert_eq!((made[0].copies, made[0].number), (4, 1));
    }

    /// A filtered user must not learn the folder names of a tree in a
    /// folder they may not list.
    #[test]
    fn a_filtered_user_is_told_only_of_trees_in_folders_they_may_list() {
        let mut index = VolumeIndex::new(NTFS_ROOT_FRN);
        let open = add_path(&mut index, 100, &format!(r"open\{}", times("a", 4)));
        let shut = add_path(&mut index, 200, &format!(r"shut\{}", times("b", 4)));
        index.set_folder_descriptor(Frn(100), b"open");
        index.set_folder_descriptor(Frn(200), b"shut");
        let mut events = RepeatEvents::allocate();
        events.number_from(counter());
        assert!(events.check(&index, open[4], 0));
        assert!(events.check(&index, shut[4], 0));
        let may_list = |descriptor: &[u8]| descriptor == b"open";
        let seen = events.reported(&index, r"C:\", |_| true, Some(&may_list));
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].top, r"C:\open\a");
        let everyone = None::<&fn(&[u8]) -> bool>;
        assert_eq!(events.reported(&index, r"C:\", |_| true, everyone).len(), 2);
    }

    /// The list must never grow under the write lock, and a dropped event
    /// must be counted, so a reply can say that events are missing.
    #[test]
    fn a_full_list_drops_the_event_changed_least_recently() {
        let mut index = VolumeIndex::new(NTFS_ROOT_FRN);
        let mut events = RepeatEvents::allocate();
        events.number_from(counter());
        let mut tops = Vec::new();
        for tree in 0..=super::EVENTS as u128 {
            let name = format!("t{tree}");
            let entries = add_path(
                &mut index,
                1000 + 10 * tree,
                &times(&name, EVENT_COPIES as usize),
            );
            assert!(events.check(&index, entries[3], 0));
            tops.push(index.entry(entries[0]).frn());
        }
        assert_eq!(events.since(0).count(), super::EVENTS);
        assert_eq!(events.dropped(), 1);
        assert!(events.since(0).all(|event| event.top != tops[0]));
        assert_eq!(events.since(64).count(), 1);
    }
}
