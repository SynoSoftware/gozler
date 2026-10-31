//! The user's regional calendar settings.

use windows_sys::Win32::Globalization::{
    GetLocaleInfoEx, LOCALE_IFIRSTDAYOFWEEK, LOCALE_RETURN_NUMBER,
};

/// The first day of the user's week, from Monday (0) to Sunday (6).
/// Falls back to Monday if Windows cannot provide the setting.
pub fn first_day_of_week() -> u8 {
    let mut day = 0u32;
    // SAFETY: the user's default locale; LOCALE_RETURN_NUMBER writes a
    // DWORD through the aligned pointer, whose size is two UTF-16 units.
    let written = unsafe {
        GetLocaleInfoEx(
            std::ptr::null(),
            LOCALE_IFIRSTDAYOFWEEK | LOCALE_RETURN_NUMBER,
            (&raw mut day).cast(),
            2,
        )
    };
    if written > 0 && day <= 6 {
        day as u8
    } else {
        0
    }
}
