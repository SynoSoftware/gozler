//! The Windows regional format and display languages. Windows formats the
//! numbers, so its regional rules have one owner (DESIGN.md:
//! Language).

use super::wide;
use crate::core::text::Numbers;
use crate::core::viewer::CodePage;
use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::Globalization::{
    CP_ACP, CP_OEMCP, CP_UTF8, CPINFO, DATE_SHORTDATE, GetACP, GetCPInfo, GetDateFormatEx,
    GetLocaleInfoEx, GetNumberFormatEx, GetOEMCP, GetTimeFormatEx, GetUserPreferredUILanguages,
    LOCALE_ILZERO, LOCALE_INEGNUMBER, LOCALE_RETURN_NUMBER, LOCALE_SDECIMAL, LOCALE_SGROUPING,
    LOCALE_STHOUSAND, MUI_LANGUAGE_NAME, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar,
    NUMBERFMTW, TIME_FORMAT_FLAGS, TIME_NOSECONDS, WC_NO_BEST_FIT_CHARS, WideCharToMultiByte,
};
use windows::core::{BOOL, PCSTR, PCWSTR, PWSTR};

/// `bytes` in the console's code page, in which a program such as `net`
/// writes to a file, as text.
pub(crate) fn console_text(bytes: &[u8]) -> String {
    decode(CP_OEMCP, bytes)
}

/// `bytes` in the code page `page`, as the Viewer shows a text file in it.
pub(crate) fn code_page_text(page: CodePage, bytes: &[u8]) -> String {
    decode(code_page_number(page), bytes)
}

/// The bytes that spell `c` in the code page `page`, as Find looks for it;
/// `None` when the code page has no such character, where Windows would
/// write its default character or a look-alike instead.
pub(crate) fn code_page_bytes(page: CodePage, c: char) -> Option<Vec<u8>> {
    let Some(page) = non_utf8_number(page) else {
        return Some(c.to_string().into_bytes());
    };
    let mut units = [0; 2];
    let units = c.encode_utf16(&mut units);
    let mut bytes = [0u8; 8];
    let mut defaulted = BOOL(0);
    // SAFETY: both buffers are locals that the call writes at most their
    // lengths of; `page` is not UTF-8, and no code page of the Viewer is
    // UTF-7, which refuse these flags.
    let written = unsafe {
        WideCharToMultiByte(
            page,
            WC_NO_BEST_FIT_CHARS,
            units,
            Some(&mut bytes),
            PCSTR::null(),
            Some(&raw mut defaulted),
        )
    };
    let written = usize::try_from(written).ok().filter(|n| *n > 0)?;
    (!defaulted.as_bool()).then(|| bytes[..written].to_vec())
}

/// The bytes that start a character of two bytes in the code page `page`,
/// as Find skips the second bytes; none when Windows does not know `page`.
pub(crate) fn code_page_leads(page: CodePage) -> [bool; 256] {
    let mut leads = [false; 256];
    let mut info = CPINFO::default();
    // SAFETY: `info` is a local that the call fills.
    if unsafe { GetCPInfo(code_page_number(page), &raw mut info) }.is_err() {
        return leads;
    }
    // Pairs of first and last lead bytes, ended by a pair of zeros.
    for &[first, last] in info.LeadByte.as_chunks::<2>().0 {
        if first == 0 {
            break;
        }
        for byte in first..=last {
            leads[usize::from(byte)] = true;
        }
    }
    leads
}

/// `text` in the code page `page`, as the compare window saves a file in
/// it; `None` when the code page cannot hold one of its characters.
pub(crate) fn code_page_encoded(page: CodePage, text: &str) -> Option<Vec<u8>> {
    let Some(page) = non_utf8_number(page) else {
        return Some(text.as_bytes().to_vec());
    };
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.is_empty() {
        return Some(Vec::new());
    }
    let mut defaulted = BOOL(0);
    // SAFETY: the first call measures and the second writes at most the
    // buffer's length; as in `code_page_bytes`, `page` is not UTF-8 and no
    // code page here is UTF-7, which refuse the flags.
    unsafe {
        let length = WideCharToMultiByte(
            page,
            WC_NO_BEST_FIT_CHARS,
            &units,
            None,
            PCSTR::null(),
            None,
        );
        let mut bytes = vec![0u8; usize::try_from(length).ok().filter(|n| *n > 0)?];
        let written = WideCharToMultiByte(
            page,
            WC_NO_BEST_FIT_CHARS,
            &units,
            Some(&mut bytes),
            PCSTR::null(),
            Some(&raw mut defaulted),
        );
        bytes.truncate(usize::try_from(written).unwrap_or(0));
        (!defaulted.as_bool()).then_some(bytes)
    }
}

/// The number of the code page that `page` is now, or `None` when it is
/// UTF-8, which holds every character: Windows' ANSI and OEM code pages are
/// UTF-8 when the person turns on "Use Unicode UTF-8 for worldwide language
/// support", and `WideCharToMultiByte` then refuses the flags that report a
/// character the code page cannot hold.
fn non_utf8_number(page: CodePage) -> Option<u32> {
    let number = match page {
        // SAFETY: GetACP takes no argument.
        CodePage::Ansi => unsafe { GetACP() },
        // SAFETY: GetOEMCP takes no argument.
        CodePage::Oem => unsafe { GetOEMCP() },
        CodePage::Number(number) => u32::from(number),
    };
    (number != CP_UTF8).then_some(number)
}

fn code_page_number(page: CodePage) -> u32 {
    match page {
        CodePage::Ansi => CP_ACP,
        CodePage::Oem => CP_OEMCP,
        CodePage::Number(number) => u32::from(number),
    }
}

fn decode(code_page: u32, bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let flags = MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0);
    // SAFETY: the first call measures; the second writes at most the
    // buffer's length.
    unsafe {
        let length = MultiByteToWideChar(code_page, flags, bytes, None);
        let mut text = vec![0u16; usize::try_from(length).unwrap_or(0)];
        let written = MultiByteToWideChar(code_page, flags, bytes, Some(&mut text));
        String::from_utf16_lossy(&text[..usize::try_from(written).unwrap_or(0)])
    }
}

/// The user's regional number format, read when created; a change to the
/// regional format needs a new one.
#[derive(Clone)]
pub(crate) struct WindowsNumbers {
    leading_zero: u32,
    grouping: u32,
    negative_order: u32,
    decimal: Vec<u16>,
    decimal_text: String,
    thousand: Vec<u16>,
}

impl WindowsNumbers {
    pub(crate) fn new() -> WindowsNumbers {
        let decimal = locale_text(LOCALE_SDECIMAL).unwrap_or_else(|| ".".to_owned());
        WindowsNumbers {
            leading_zero: locale_number(LOCALE_ILZERO).unwrap_or(1),
            grouping: grouping(&locale_text(LOCALE_SGROUPING).unwrap_or_else(|| "3;0".to_owned())),
            negative_order: locale_number(LOCALE_INEGNUMBER).unwrap_or(1),
            decimal: wide(&decimal),
            decimal_text: decimal,
            thousand: wide(&locale_text(LOCALE_STHOUSAND).unwrap_or_else(|| ",".to_owned())),
        }
    }

    /// `value`, written with `.` and no grouping, in the regional format
    /// with `digits` decimals.
    fn format(&self, value: &str, digits: u8) -> String {
        let mut decimal = self.decimal.clone();
        let mut thousand = self.thousand.clone();
        let format = NUMBERFMTW {
            NumDigits: u32::from(digits),
            LeadingZero: self.leading_zero,
            Grouping: self.grouping,
            lpDecimalSep: PWSTR(decimal.as_mut_ptr()),
            lpThousandSep: PWSTR(thousand.as_mut_ptr()),
            NegativeOrder: self.negative_order,
        };
        let mut buffer = [0u16; 64];
        let value = wide(value);
        // SAFETY: `format`, its separators and `value` outlive the call,
        // which writes at most the buffer's length.
        let written = unsafe {
            GetNumberFormatEx(
                PCWSTR::null(),
                0,
                PCWSTR(value.as_ptr()),
                Some(&format),
                Some(&mut buffer),
            )
        };
        match usize::try_from(written) {
            Ok(n) if n > 0 => String::from_utf16_lossy(&buffer[..n - 1]),
            _ => String::from_utf16_lossy(&value[..value.len() - 1]),
        }
    }
}

impl Numbers for WindowsNumbers {
    fn integer(&self, n: u64) -> String {
        self.format(&n.to_string(), 0)
    }

    fn decimal(&self, n: f64, digits: u8) -> String {
        self.format(&format!("{n:.*}", usize::from(digits)), digits)
    }

    fn date_time(&self, filetime: i64) -> String {
        date_time(filetime, TIME_NOSECONDS)
    }

    fn date_time_seconds(&self, filetime: i64) -> String {
        date_time(filetime, TIME_FORMAT_FLAGS(0))
    }

    fn date(&self, date: jiff::civil::Date) -> String {
        short_date(&system_time(
            date.to_datetime(jiff::civil::Time::midnight()),
        ))
    }

    fn decimal_separator(&self) -> &str {
        &self.decimal_text
    }
}

/// A FILETIME in the short date and the time format with `time_flags`;
/// empty for a time that `jiff` cannot hold.
fn date_time(filetime: i64, time_flags: TIME_FORMAT_FLAGS) -> String {
    let Ok(at) =
        jiff::Timestamp::from_millisecond(gozler_core::output::filetime_to_unix_ms(filetime))
    else {
        return String::new();
    };
    let local = at.to_zoned(gozler_winfs::local_time_zone()).datetime();
    let time = system_time(local);
    let mut clock = [0u16; 64];
    // SAFETY: `time` is a complete SYSTEMTIME; the call writes at most its
    // buffer's length.
    let clock_len = unsafe {
        GetTimeFormatEx(
            PCWSTR::null(),
            time_flags,
            Some(&time),
            PCWSTR::null(),
            Some(&mut clock),
        )
    };
    format!("{} {}", short_date(&time), text(&clock, clock_len))
}

fn system_time(local: jiff::civil::DateTime) -> SYSTEMTIME {
    SYSTEMTIME {
        wYear: u16::try_from(local.year()).unwrap_or(0),
        wMonth: local.month() as u16,
        wDayOfWeek: local.weekday().to_sunday_zero_offset() as u16,
        wDay: local.day() as u16,
        wHour: local.hour() as u16,
        wMinute: local.minute() as u16,
        wSecond: local.second() as u16,
        wMilliseconds: 0,
    }
}

/// `time`'s day in the user's short date format.
fn short_date(time: &SYSTEMTIME) -> String {
    let mut date = [0u16; 64];
    // SAFETY: `time` is a complete SYSTEMTIME; the call writes at most the
    // buffer's length.
    let written = unsafe {
        GetDateFormatEx(
            PCWSTR::null(),
            DATE_SHORTDATE,
            Some(time),
            PCWSTR::null(),
            Some(&mut date),
            PCWSTR::null(),
        )
    };
    text(&date, written)
}

/// The text a formatting call wrote into `buffer`: `written` units with the
/// NUL, or nothing when it failed.
fn text(buffer: &[u16], written: i32) -> String {
    match usize::try_from(written) {
        Ok(n) if n > 0 => String::from_utf16_lossy(&buffer[..n - 1]),
        _ => String::new(),
    }
}

/// `LOCALE_SGROUPING` as `NUMBERFMTW` takes it: "3;0" is 3, "3;2;0" is 32,
/// and "3" without the repeating 0 is 30.
fn grouping(text: &str) -> u32 {
    let mut digits: String = text.split(';').collect();
    match digits.strip_suffix('0') {
        Some(repeating) if text.ends_with(";0") => digits = repeating.to_owned(),
        _ => digits.push('0'),
    }
    digits.parse().unwrap_or(3)
}

fn locale_text(kind: u32) -> Option<String> {
    let mut buffer = [0u16; 16];
    // SAFETY: the user's default locale; the call writes at most the
    // buffer's length.
    let written = unsafe { GetLocaleInfoEx(PCWSTR::null(), kind, Some(&mut buffer)) };
    let n = usize::try_from(written).ok().filter(|n| *n > 0)?;
    Some(String::from_utf16_lossy(&buffer[..n - 1]))
}

fn locale_number(kind: u32) -> Option<u32> {
    let mut buffer = [0u16; 2];
    // SAFETY: with LOCALE_RETURN_NUMBER the call writes one u32 as two u16s.
    let written = unsafe {
        GetLocaleInfoEx(
            PCWSTR::null(),
            kind | LOCALE_RETURN_NUMBER,
            Some(&mut buffer),
        )
    };
    (written > 0).then(|| u32::from(buffer[0]) | (u32::from(buffer[1]) << 16))
}

/// The Windows display languages of the user, in their order of preference,
/// such as `de-DE`.
pub(crate) fn display_languages() -> Vec<String> {
    let mut count = 0u32;
    let mut length = 0u32;
    // SAFETY: with no buffer the call reports the length it needs.
    if unsafe { GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut length) }
        .is_err()
    {
        return Vec::new();
    }
    let mut buffer = vec![0u16; length as usize];
    // SAFETY: `buffer` has the length the first call asked for.
    if unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            Some(PWSTR(buffer.as_mut_ptr())),
            &mut length,
        )
    }
    .is_err()
    {
        return Vec::new();
    }
    buffer
        .split(|c| *c == 0)
        .filter(|tag| !tag.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}
