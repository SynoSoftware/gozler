//! One window per user session: a named mutex decides which process is
//! first, and a second start hands over to the first window or asks it to
//! exit (DESIGN.md: Window lifetime).

use super::Hwnd;
use crate::core::startup::{REQUEST_TAG, Request};
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, FILETIME, GetLastError, HANDLE, LPARAM, WPARAM,
};
use windows::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_ELEVATION_TYPE, TOKEN_INFORMATION_CLASS,
    TOKEN_QUERY, TokenElevation, TokenElevationType, TokenElevationTypeLimited,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, GetProcessTimes, INFINITE, OpenProcess, OpenProcessToken,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    QueryFullProcessImageNameW, WaitForSingleObject,
};
use windows::Win32::UI::Controls::{LIM_SMALL, LoadIconMetric};
use windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, ChangeWindowMessageFilterEx, DestroyIcon, FindWindowW,
    GetWindowThreadProcessId, IDI_SHIELD, MSGFLT_ALLOW, RegisterWindowMessageW, SMTO_ABORTIFHUNG,
    SMTO_NOTIMEOUTIFNOTHUNG, SendMessageTimeoutW, WM_COPYDATA,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

/// The mutex that the first process holds until it exits.
pub(crate) struct FirstInstance(HANDLE);

impl Drop for FirstInstance {
    fn drop(&mut self) {
        // SAFETY: the handle is this value's own.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// The mutex `name` if this process is the first to create it; `None` when
/// another process holds it.
pub(crate) fn claim(name: &str) -> windows::core::Result<Option<FirstInstance>> {
    // SAFETY: a named mutex with default security, not owned at creation.
    let handle = unsafe { CreateMutexW(None, false, &HSTRING::from(name)) }?;
    // SAFETY: no parameters; it reads the result of the call above.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // SAFETY: the handle is this function's own.
        let _ = unsafe { CloseHandle(handle) };
        return Ok(None);
    }
    Ok(Some(FirstInstance(handle)))
}

/// The message ID that `name` stands for in every process of the session.
pub(crate) fn register_message(name: &str) -> u32 {
    // SAFETY: the name outlives the call.
    unsafe { RegisterWindowMessageW(&HSTRING::from(name)) }
}

/// The first window of class `class`; `None` while the first process has
/// no window, as while it starts or ends.
pub(crate) fn find(class: &str) -> Option<Hwnd> {
    // SAFETY: the class name outlives the call.
    unsafe { FindWindowW(&HSTRING::from(class), PCWSTR::null()) }
        .ok()
        .map(Hwnd)
}

/// The program of a running window: its file, and when its process started.
pub(crate) struct Program {
    pub(crate) path: PathBuf,
    pub(crate) started: SystemTime,
}

/// The process of the first window, held open so this start can wait for it
/// to end after it agreed to exit.
pub(crate) struct Process(HANDLE);

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: the handle is this value's own.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

impl Process {
    /// `window`'s process; `None` when Windows refuses it, as for a process
    /// this one may not query.
    pub(crate) fn of(window: Hwnd) -> Option<Process> {
        // SAFETY: plain values; the rights read only the name and the times,
        // and wait for the end.
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                process_id(window),
            )
        };
        handle.ok().map(Process)
    }

    /// The process's program; `None` when Windows does not say.
    pub(crate) fn program(&self) -> Option<Program> {
        let mut name = vec![0u16; 32_768];
        let mut length = name.len() as u32;
        // SAFETY: `name` holds `length` units, which the call lowers to the
        // name's length.
        unsafe {
            QueryFullProcessImageNameW(
                self.0,
                PROCESS_NAME_WIN32,
                PWSTR(name.as_mut_ptr()),
                &mut length,
            )
        }
        .ok()?;
        let [mut created, mut exited, mut kernel, mut user] = [FILETIME::default(); 4];
        // SAFETY: the four times are locals the call fills in.
        unsafe { GetProcessTimes(self.0, &mut created, &mut exited, &mut kernel, &mut user) }
            .ok()?;
        Some(Program {
            path: PathBuf::from(std::ffi::OsString::from_wide(&name[..length as usize])),
            started: system_time(created)?,
        })
    }

    /// Waits, without a limit, until the process has ended.
    pub(crate) fn wait_for_end(&self) {
        // SAFETY: the handle is this value's own and has the right to wait.
        let _ = unsafe { WaitForSingleObject(self.0, INFINITE) };
    }

    /// Whether the process runs as administrator; `false` when Windows
    /// does not say.
    pub(crate) fn is_elevated(&self) -> bool {
        // SAFETY: the handle is this value's own, and Windows answers
        // `TokenElevation` with a `TOKEN_ELEVATION`.
        unsafe { token_information::<TOKEN_ELEVATION>(self.0, TokenElevation) }
            .is_some_and(|elevation| elevation.TokenIsElevated != 0)
    }
}

/// Whether this process runs as administrator.
pub(crate) fn is_elevated() -> bool {
    // SAFETY: the pseudo-handle of this process needs no closing, and
    // Windows answers `TokenElevation` with a `TOKEN_ELEVATION`.
    unsafe { token_information::<TOKEN_ELEVATION>(GetCurrentProcess(), TokenElevation) }
        .is_some_and(|elevation| elevation.TokenIsElevated != 0)
}

/// Whether Windows can start this program again as administrator for the
/// same person: this process runs with an administrator's filtered token.
/// A standard user's prompt asks for another account, whose Gozler has
/// other settings, and a process that already runs as administrator gains
/// nothing.
pub(crate) fn may_run_as_administrator() -> bool {
    // SAFETY: the pseudo-handle of this process needs no closing, and
    // Windows answers `TokenElevationType` with a `TOKEN_ELEVATION_TYPE`.
    let elevation_type = unsafe {
        token_information::<TOKEN_ELEVATION_TYPE>(GetCurrentProcess(), TokenElevationType)
    };
    elevation_type == Some(TokenElevationTypeLimited)
}

/// The answer to `class` about the token of `process`; `None` when Windows
/// refuses the token or the question.
///
/// # Safety
///
/// `process` is open with `PROCESS_QUERY_LIMITED_INFORMATION`, and `T` is
/// the structure that Windows answers `class` with.
unsafe fn token_information<T: Default>(
    process: HANDLE,
    class: TOKEN_INFORMATION_CLASS,
) -> Option<T> {
    let mut token = HANDLE::default();
    // SAFETY: the caller gives an open process; `token` is a local the call
    // fills in.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.ok()?;
    let mut answer = T::default();
    let mut length = 0;
    // SAFETY: `answer` holds the `T` that the caller says `class` answers
    // with, and the call writes at most its size.
    let read = unsafe {
        GetTokenInformation(
            token,
            class,
            Some((&raw mut answer).cast()),
            size_of::<T>() as u32,
            &mut length,
        )
    };
    // SAFETY: the token is this function's own.
    let _ = unsafe { CloseHandle(token) };
    read.ok().map(|()| answer)
}

/// Puts the administrator shield on `window`'s taskbar button, because a
/// taskbar that shows no labels hides the title; `description` is what a
/// screen reader says for it. Explorer drops it when it makes the button
/// again, which `TaskbarButtonCreated` tells.
pub(crate) fn mark_taskbar_button(window: Hwnd, description: &str) {
    // SAFETY: COM is initialized on this thread at start; the instance is
    // released when it drops.
    let taskbar: windows::core::Result<ITaskbarList3> =
        unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER) };
    // SAFETY: Windows' own shield icon, loaded at the small icon size.
    let shield = unsafe { LoadIconMetric(None, IDI_SHIELD, LIM_SMALL) };
    let (Ok(taskbar), Ok(shield)) = (taskbar, shield) else {
        return;
    };
    // SAFETY: HrInit comes before any other call; the taskbar keeps its own
    // copy of the icon, so this one is destroyed after the call.
    unsafe {
        if taskbar.HrInit().is_ok() {
            let _ = taskbar.SetOverlayIcon(window.0, shield, &HSTRING::from(description));
        }
        let _ = DestroyIcon(shield);
    }
}

/// Lets programs that run without administrator rights send `messages` to
/// `window`, which runs as administrator, such as a later start that hands
/// over to it and Explorer. Windows otherwise drops a message from a lower
/// integrity level.
pub(crate) fn accept_from_lower(window: Hwnd, messages: &[u32]) {
    for &message in messages {
        // SAFETY: plain values; the filter belongs to `window`.
        let _ = unsafe { ChangeWindowMessageFilterEx(window.0, message, MSGFLT_ALLOW, None) };
    }
}

/// A `FILETIME`, in 100 ns units since 1601, as a [`SystemTime`].
fn system_time(time: FILETIME) -> Option<SystemTime> {
    const UNIX_EPOCH_IN_FILETIME: u64 = 116_444_736_000_000_000;
    let units = (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    let since_unix = units.checked_sub(UNIX_EPOCH_IN_FILETIME)?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_nanos(since_unix.checked_mul(100)?))
}

fn process_id(window: Hwnd) -> u32 {
    let mut process = 0;
    // SAFETY: `process` is a local the call fills in.
    unsafe { GetWindowThreadProcessId(window.0, Some(&mut process)) };
    process
}

/// Lets `window`'s process come to the front. Windows refuses it when this
/// process may not give the foreground away, and that window then flashes
/// on the taskbar instead.
fn allow_foreground(window: Hwnd) {
    // SAFETY: plain values.
    let _ = unsafe { AllowSetForegroundWindow(process_id(window)) };
}

/// How long a second start waits for the first window to take a folder.
const HAND_OVER_TIMEOUT_MS: u32 = 5_000;

/// Hands a request to the first window, preserving the legacy Location tag.
pub(crate) fn hand_over(window: Hwnd, message: u32, request: Option<&Request>) -> bool {
    allow_foreground(window);
    let Some(request) = request else {
        window.post(message);
        return true;
    };
    let (tag, text) = match request {
        Request::Location(folder) => (message as usize, folder.clone()),
        _ => (REQUEST_TAG, request.encode()),
    };
    let text: Vec<u16> = text.encode_utf16().collect();
    let data = COPYDATASTRUCT {
        dwData: tag,
        cbData: (text.len() * 2) as u32,
        lpData: text.as_ptr().cast_mut().cast(),
    };
    let mut answer = 0usize;
    // SAFETY: `data` and its text outlive the call, which Windows copies
    // into the first process; a window that does not answer in time is
    // left alone.
    let sent = unsafe {
        SendMessageTimeoutW(
            window.0,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM((&raw const data) as isize),
            SMTO_ABORTIFHUNG,
            HAND_OVER_TIMEOUT_MS,
            Some(&raw mut answer),
        )
    };
    matches!(request, Request::Location(_)) || sent.0 != 0 && answer == 1
}

/// Lets the first window come to the front, sends it `message` and returns
/// its answer, waiting as long as the window answers other messages, so
/// the person can take their time over a question it asks first. `None`
/// when the window does not respond.
pub(crate) fn ask(window: Hwnd, message: u32) -> Option<isize> {
    allow_foreground(window);
    let mut answer: usize = 0;
    // SAFETY: plain values; `answer` is a local the call fills in.
    let sent = unsafe {
        SendMessageTimeoutW(
            window.0,
            message,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG | SMTO_NOTIMEOUTIFNOTHUNG,
            HAND_OVER_TIMEOUT_MS,
            Some(&raw mut answer),
        )
    };
    (sent.0 != 0).then_some(answer as isize)
}
