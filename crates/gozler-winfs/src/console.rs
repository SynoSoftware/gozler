//! Console color and width. Windows draws ANSI color codes only after
//! a program turns on virtual terminal processing for it.

use windows_sys::Win32::System::Console::{
    CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode,
    GetConsoleScreenBufferInfo, GetStdHandle, STD_OUTPUT_HANDLE, SetConsoleMode,
};

/// Turns on ANSI color codes for the console that stdout writes to, and says
/// whether the console now draws them. False when stdout is not a console,
/// such as a file or a pipe, and when the console cannot draw them, such as
/// the legacy console of Windows 10.
pub fn enable_console_color() -> bool {
    // SAFETY: GetStdHandle takes no pointers; a missing handle comes back
    // null or invalid, and GetConsoleMode then fails.
    let out = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    let mut mode: CONSOLE_MODE = 0;
    // SAFETY: `mode` is a valid place for the call to write the mode.
    if unsafe { GetConsoleMode(out, &mut mode) } == 0 {
        return false;
    }
    if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0 {
        return true;
    }
    // SAFETY: `out` is a console handle, as GetConsoleMode just confirmed.
    unsafe { SetConsoleMode(out, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0 }
}

/// The visible console's column count, or `None` for redirected stdout.
pub fn console_width() -> Option<usize> {
    // SAFETY: GetStdHandle takes no pointers; a missing or non-console handle
    // makes GetConsoleScreenBufferInfo fail. `info` is a valid output buffer.
    let info = unsafe {
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut info = std::mem::zeroed::<CONSOLE_SCREEN_BUFFER_INFO>();
        if GetConsoleScreenBufferInfo(out, &mut info) == 0 {
            return None;
        }
        info
    };
    let width = i32::from(info.srWindow.Right) - i32::from(info.srWindow.Left) + 1;
    usize::try_from(width).ok().filter(|&width| width > 0)
}
