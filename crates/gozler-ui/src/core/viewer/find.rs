//! Find in the Viewer (DESIGN.md: The Viewer): the bytes that spell what the
//! person looks for in the file's encoding, and where they occur in the
//! bytes that the reading thread reads, one bounded part of the file at a
//! time. Matching bytes needs no decoding of the file, so a match's offsets
//! are exact in every encoding, and a file of any size is searched in the
//! memory of one part.

use super::{CodePage, CodePageText, Encoding, decode_piece, is_word};
use std::ops::Range;

/// The bytes before a match, and after it, that hold the character next to
/// it in any encoding: a UTF-8 character or a UTF-16 surrogate pair.
pub(crate) const CONTEXT: usize = 4;

/// The bytes that Find reads before a part in a code page of two-byte
/// characters. Text holds a byte that cannot lead a character, such as a
/// space, an ASCII letter or a line end, far more often than this.
const LEAD_CONTEXT: u64 = 256;

/// What Find looks for, as the person typed it in the Find dialog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Query {
    pub(crate) text: String,
    pub(crate) match_case: bool,
    pub(crate) whole_words: bool,
    /// `text` is bytes in hex, such as "4D 5A"; then case and whole words
    /// do not apply.
    pub(crate) hex: bool,
}

impl Query {
    /// Whether Find can look for it: any text, or in hex, whole bytes.
    pub(crate) fn is_valid(&self) -> bool {
        if self.hex {
            hex_bytes(&self.text).is_some()
        } else {
            !self.text.is_empty()
        }
    }
}

/// The bytes that `text` spells in hex: pairs of hex digits, with or
/// without spaces, such as "4D 5A" or "4d5a"; `None` for no digits, an odd
/// number of them or another character.
pub(crate) fn hex_bytes(text: &str) -> Option<Vec<u8>> {
    let digits: Vec<u8> = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_digit(16).and_then(|d| u8::try_from(d).ok()))
        .collect::<Option<_>>()?;
    if digits.is_empty() || !digits.len().is_multiple_of(2) {
        return None;
    }
    Some(
        digits
            .chunks(2)
            .map(|pair| pair[0] << 4 | pair[1])
            .collect(),
    )
}

/// The bytes that spell a character in a code page, as Windows encodes it;
/// `None` when the code page has no such character.
pub(crate) type CodePageBytes = dyn Fn(CodePage, char) -> Option<Vec<u8>>;

/// The bytes that start a character of two bytes in a code page, such as
/// Shift_JIS; none in a code page of single bytes.
pub(crate) type CodePageLeads = dyn Fn(CodePage) -> [bool; 256];

/// What Find looks for, as bytes in the file's encoding.
pub(crate) struct Pattern {
    /// For each character, the byte sequences that spell it, its other
    /// cases too unless the case must match; one byte each in hex.
    chars: Vec<Vec<Vec<u8>>>,
    /// The bytes that start a match.
    leading: [bool; 256],
    /// The most bytes a match takes.
    longest: usize,
    whole_words: bool,
    encoding: Encoding,
    /// The first byte where a match may start, after which text matches
    /// start at whole code units.
    data_start: u64,
    /// The bytes of a code unit for text, 1 for hex bytes.
    unit: u64,
    /// The bytes that start a character of two bytes, after which a match
    /// cannot start, because the next byte is that character's second.
    leads: [bool; 256],
}

impl Pattern {
    /// `query` in a file of `encoding` whose text starts at `data_start`;
    /// `None` when the text cannot occur in it: nothing to look for, or a
    /// character that the encoding cannot spell. Hex bytes match at any
    /// byte.
    pub(crate) fn new(
        query: &Query,
        encoding: Encoding,
        data_start: u64,
        code_page: &CodePageBytes,
        leads: &CodePageLeads,
    ) -> Option<Pattern> {
        let chars: Vec<Vec<Vec<u8>>> = if query.hex {
            hex_bytes(&query.text)?
                .into_iter()
                .map(|byte| vec![vec![byte]])
                .collect()
        } else {
            query
                .text
                .chars()
                .map(|c| {
                    let mut spellings: Vec<Vec<u8>> = Vec::new();
                    for case in cases(c, query.match_case) {
                        if let Some(bytes) = spell(encoding, case, code_page)
                            && !bytes.is_empty()
                            && !spellings.contains(&bytes)
                        {
                            spellings.push(bytes);
                        }
                    }
                    (!spellings.is_empty()).then_some(spellings)
                })
                .collect::<Option<_>>()?
        };
        let mut leading = [false; 256];
        for spelling in chars.first()? {
            leading[usize::from(spelling[0])] = true;
        }
        let longest = chars
            .iter()
            .map(|spellings| spellings.iter().map(Vec::len).max().unwrap_or(0))
            .sum();
        Some(Pattern {
            chars,
            leading,
            longest,
            whole_words: query.whole_words && !query.hex,
            encoding,
            data_start,
            unit: if query.hex {
                1
            } else {
                encoding.width() as u64
            },
            leads: match encoding {
                Encoding::CodePage(page) if !query.hex => leads(page),
                _ => [false; 256],
            },
        })
    }

    /// How many bytes past the last place where a match may start the
    /// caller reads, so that the match and the character after it fit.
    pub(crate) fn reach(&self) -> u64 {
        (self.longest + CONTEXT) as u64
    }

    /// How many bytes before the first place where a match may start the
    /// caller reads: [`CONTEXT`], or in a code page of two-byte characters
    /// enough to hold a byte that ends a character, where the walk over the
    /// characters starts.
    pub(crate) fn back(&self) -> u64 {
        if self.leads.contains(&true) {
            LEAD_CONTEXT
        } else {
            CONTEXT as u64
        }
    }

    /// The first match whose first byte lies within `starts`, as the
    /// file's offsets it covers. `bytes` are the file's bytes from offset
    /// `base`: from [`Pattern::back`] bytes before `starts`, or the text's start,
    /// to [`Pattern::reach`] bytes past it, or the file's end.
    /// `code_page` decodes the characters next to a match for whole words.
    pub(crate) fn first(
        &self,
        bytes: &[u8],
        base: u64,
        starts: Range<u64>,
        code_page: &CodePageText,
    ) -> Option<Range<u64>> {
        self.starts(bytes, base, starts)
            .find_map(|at| self.match_at(bytes, base, at, code_page))
    }

    /// The last match whose first byte lies within `starts`, as for
    /// [`Pattern::first`].
    pub(crate) fn last(
        &self,
        bytes: &[u8],
        base: u64,
        starts: Range<u64>,
        code_page: &CodePageText,
    ) -> Option<Range<u64>> {
        self.starts(bytes, base, starts)
            .filter_map(|at| self.match_at(bytes, base, at, code_page))
            .last()
    }

    /// The indexes of `bytes` within `starts` where a match can start: a
    /// first byte, at a code unit of the text, at the start of a character.
    /// Characters are found by one forward walk, which costs no more than
    /// the part, from a character start before `starts`.
    fn starts<'a>(
        &'a self,
        bytes: &'a [u8],
        base: u64,
        starts: Range<u64>,
    ) -> impl Iterator<Item = usize> + 'a {
        let index = |offset: u64| {
            usize::try_from(offset.saturating_sub(base))
                .unwrap_or(usize::MAX)
                .min(bytes.len())
        };
        let from = index(starts.start.max(self.data_start));
        let text = index(self.data_start);
        let mut character = self.character_start(bytes, text, from);
        (character..index(starts.end)).filter(move |&i| {
            let starts_character = i == character;
            if starts_character {
                character = i + if self.leads[usize::from(bytes[i])] {
                    2
                } else {
                    1
                };
            }
            let offset = base + i as u64;
            i >= from
                && starts_character
                && self.leading[usize::from(bytes[i])]
                && (offset - self.data_start).is_multiple_of(self.unit)
        })
    }

    /// The match that starts at `bytes[at]`, as the file's offsets.
    fn match_at(
        &self,
        bytes: &[u8],
        base: u64,
        at: usize,
        code_page: &CodePageText,
    ) -> Option<Range<u64>> {
        let mut end = at;
        for spellings in &self.chars {
            let spelling = spellings
                .iter()
                .find(|spelling| bytes.get(end..end + spelling.len()) == Some(spelling))?;
            end += spelling.len();
        }
        if self.whole_words
            && (self.word_before(bytes, base, at, code_page)
                || self.word_after(bytes, end, code_page))
        {
            return None;
        }
        Some(base + at as u64..base + end as u64)
    }

    /// Whether the character that ends at `bytes[at]` belongs to a word.
    fn word_before(&self, bytes: &[u8], base: u64, at: usize, code_page: &CodePageText) -> bool {
        let text_start = usize::try_from(self.data_start.saturating_sub(base))
            .unwrap_or(at)
            .min(at);
        let from = if self.leads.contains(&true) {
            // The character before `at` ends at `at - 1`, so the decode
            // starts at a character start before that byte, looked for as
            // far back as a part's start is, so each match costs as much.
            let floor = at.saturating_sub(LEAD_CONTEXT as usize).max(text_start);
            self.character_start(bytes, floor, at.saturating_sub(1).max(floor))
        } else {
            // An even count keeps UTF-16 code units whole.
            let from = at.saturating_sub(CONTEXT).max(text_start);
            from + (at - from) % self.encoding.width()
        };
        decode_piece(self.encoding, &bytes[from..at], code_page)
            .chars()
            .next_back()
            .is_some_and(is_word)
    }

    /// A character start at or before `before`: a byte that cannot lead a
    /// character ends one, so the byte after the last such byte in
    /// `bytes[floor..before]`, or `floor`. Without lead bytes, `before`.
    fn character_start(&self, bytes: &[u8], floor: usize, before: usize) -> usize {
        bytes[floor..before]
            .iter()
            .rposition(|byte| !self.leads[usize::from(*byte)])
            .map_or(floor, |last| floor + last + 1)
    }

    /// Whether the character that starts at `bytes[at]` belongs to a word.
    fn word_after(&self, bytes: &[u8], at: usize, code_page: &CodePageText) -> bool {
        let to = (at + CONTEXT).min(bytes.len());
        decode_piece(self.encoding, &bytes[at.min(to)..to], code_page)
            .chars()
            .next()
            .is_some_and(is_word)
    }
}

/// `c` and, unless the case must match, its lowercase and uppercase forms
/// that are one character each.
fn cases(c: char, match_case: bool) -> Vec<char> {
    let mut cases = vec![c];
    if !match_case {
        let mut lower = c.to_lowercase();
        if let (Some(form), None) = (lower.next(), lower.next()) {
            cases.push(form);
        }
        let mut upper = c.to_uppercase();
        if let (Some(form), None) = (upper.next(), upper.next()) {
            cases.push(form);
        }
    }
    cases
}

/// The bytes that spell `c` in `encoding`.
fn spell(encoding: Encoding, c: char, code_page: &CodePageBytes) -> Option<Vec<u8>> {
    let mut units = [0; 2];
    let units = c.encode_utf16(&mut units);
    match encoding {
        Encoding::Utf8 => Some(c.to_string().into_bytes()),
        Encoding::Utf16Le => Some(units.iter().flat_map(|u| u.to_le_bytes()).collect()),
        Encoding::Utf16Be => Some(units.iter().flat_map(|u| u.to_be_bytes()).collect()),
        Encoding::CodePage(page) => code_page(page, c),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::latin1;
    use super::*;

    fn latin1_bytes(_: CodePage, c: char) -> Option<Vec<u8>> {
        u8::try_from(u32::from(c)).ok().map(|b| vec![b])
    }

    fn pattern(text: &str, encoding: Encoding, change: impl FnOnce(&mut Query)) -> Pattern {
        let mut query = Query {
            text: text.to_owned(),
            ..Query::default()
        };
        change(&mut query);
        Pattern::new(&query, encoding, 0, &latin1_bytes, &|_| [false; 256])
            .expect("the text can occur")
    }

    fn all(bytes: &[u8]) -> Range<u64> {
        0..bytes.len() as u64
    }

    /// Case and whole words decide as a person reads the text, in each
    /// encoding's bytes.
    #[test]
    fn find_matches_any_case_and_only_whole_words_when_asked() {
        let text = b"An error, errors and ERROR.";
        let find = |p: &Pattern| p.first(text, 0, all(text), &latin1);
        assert_eq!(find(&pattern("error", Encoding::Utf8, |_| {})), Some(3..8));
        let case = pattern("ERROR", Encoding::Utf8, |q| q.match_case = true);
        assert_eq!(find(&case), Some(21..26));
        let words = pattern("errors", Encoding::Utf8, |q| q.whole_words = true);
        assert_eq!(find(&words), Some(10..16));
        let words = pattern("error", Encoding::Utf8, |q| q.whole_words = true);
        let last = words.last(text, 0, all(text), &latin1);
        assert_eq!(last, Some(21..26));
        // UTF-16 matches only at whole code units.
        let wide: Vec<u8> = "xAx".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let a = pattern("a", Encoding::Utf16Le, |_| {});
        assert_eq!(a.first(&wide, 0, all(&wide), &latin1), Some(2..4));
        let shifted = [&[0x41][..], &wide].concat();
        assert_eq!(a.first(&shifted, 0, 1..2, &latin1), None);
    }

    /// In a code page of two-byte characters, the second byte can equal a
    /// letter, which is not in the text.
    #[test]
    fn find_skips_the_second_byte_of_a_character() {
        // Shift_JIS: 0x83 0x41 is one katakana character, then "A".
        let text = [0x83, 0x41, 0x41];
        let mut leads = [false; 256];
        leads[0x83] = true;
        let query = Query {
            text: "A".to_owned(),
            match_case: true,
            ..Query::default()
        };
        let encoding = Encoding::CodePage(CodePage::Number(932));
        let a = Pattern::new(&query, encoding, 0, &latin1_bytes, &move |_| leads)
            .expect("the text can occur");
        assert_eq!(a.first(&text, 0, all(&text), &latin1), Some(2..3));
        assert_eq!(a.last(&text, 0, all(&text), &latin1), Some(2..3));
    }

    #[test]
    fn hex_bytes_take_pairs_with_or_without_spaces() {
        assert_eq!(hex_bytes("4D 5a"), Some(vec![0x4D, 0x5A]));
        assert_eq!(hex_bytes("4d5A00"), Some(vec![0x4D, 0x5A, 0]));
        assert_eq!(hex_bytes("4D5"), None);
        assert_eq!(hex_bytes("4G"), None);
        assert_eq!(hex_bytes(" "), None);
    }

    proptest::proptest! {
        /// Bytes from the disk can be anything; Find never panics on them.
        #[test]
        fn find_over_any_bytes_never_panics(
            bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..64),
            base in 0u64..8,
            from in 0u64..80,
            to in 0u64..80,
        ) {
            for encoding in [Encoding::Utf8, Encoding::Utf16Be] {
                let words = pattern("a\u{e9}", encoding, |q| q.whole_words = true);
                let _ = words.first(&bytes, base, from..to, &latin1);
                let _ = words.last(&bytes, base, from..to, &latin1);
            }
        }
    }
}
