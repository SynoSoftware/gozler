//! This computer's time zone, as the clock in the taskbar keeps it.

use jiff::tz::{Offset, TimeZone};
use windows_sys::Win32::System::Time::{
    DYNAMIC_TIME_ZONE_INFORMATION, GetDynamicTimeZoneInformation, TIME_ZONE_ID_INVALID,
};

/// This computer's time zone. With "Adjust for daylight saving time
/// automatically" off, the clock keeps the zone's standard offset all year,
/// but `TimeZone::system` maps the zone's name to its rules and applies
/// daylight saving, so every local date would be an hour off in summer.
pub fn local_time_zone() -> TimeZone {
    let mut info = DYNAMIC_TIME_ZONE_INFORMATION::default();
    // SAFETY: plain FFI writing one DYNAMIC_TIME_ZONE_INFORMATION through a
    // valid pointer.
    let answer = unsafe { GetDynamicTimeZoneInformation(&mut info) };
    if answer != TIME_ZONE_ID_INVALID && info.DynamicDaylightTimeDisabled {
        // UTC = local time + bias, in minutes.
        let bias = info.Bias.saturating_add(info.StandardBias);
        if let Ok(offset) = Offset::from_seconds(bias.saturating_mul(-60)) {
            return TimeZone::fixed(offset);
        }
    }
    TimeZone::system()
}
