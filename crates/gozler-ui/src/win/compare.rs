//! The compare window's native parts (DESIGN.md: Compare by content): the
//! RichEdit controls of its sides, their line numbers and its two-line list,
//! the colours and font of their text, the files dropped on the window, and
//! the files its worker reads and saves.

use super::Hwnd;
use super::controls::{child, send};
use super::gdi::{high_contrast, system_color};
use super::notify::{ControlEvent, Notification};
use super::work::OffThread;
use crate::core::compare::rtf::{Font as RtfFont, Palette, Rgb};
use std::cell::Cell;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::ops::Range;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use windows::Win32::Foundation::{
    COLORREF, ERROR_FILE_NOT_FOUND, ERROR_UNABLE_TO_MOVE_REPLACEMENT,
    ERROR_UNABLE_TO_MOVE_REPLACEMENT_2, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    COLOR_BTNFACE, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW,
    COLOR_WINDOWTEXT,
};
use windows::Win32::Storage::FileSystem::{
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, MOVEFILE_REPLACE_EXISTING,
    MOVEFILE_WRITE_THROUGH, MoveFileExW, REPLACEFILE_IGNORE_ACL_ERRORS,
    REPLACEFILE_IGNORE_MERGE_ERRORS, ReplaceFileW,
};
use windows::Win32::System::LibraryLoader::LoadLibraryW;
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::SystemServices::MK_CONTROL;
use windows::Win32::UI::Controls::RichEdit::{
    CHARRANGE, ECO_READONLY, EDITSTREAM, EM_EXGETSEL, EM_EXLIMITTEXT, EM_EXLINEFROMCHAR,
    EM_EXSETSEL, EM_FINDTEXTEXW, EM_GETEVENTMASK, EM_GETOPTIONS, EM_GETSCROLLPOS, EM_GETTEXTEX,
    EM_GETTEXTLENGTHEX, EM_GETTEXTRANGE, EM_PASTESPECIAL, EM_SETEVENTMASK, EM_SETSCROLLPOS,
    EM_SETTARGETDEVICE, EM_SETUNDOLIMIT, EM_SETZOOM, EM_STREAMIN, EN_SELCHANGE, ENM_CHANGE,
    ENM_SELCHANGE, ES_DISABLENOSCROLL, ES_NOOLEDRAGDROP, FINDTEXTEXW, GETTEXTEX, GETTEXTLENGTHEX,
    GT_DEFAULT, GTL_NUMCHARS, GTL_PRECISE, SF_RTF, TEXTRANGEW,
};
use windows::Win32::UI::Controls::{
    EM_GETFIRSTVISIBLELINE, EM_LINEINDEX, EM_LINELENGTH, EM_LINESCROLL, EM_SCROLLCARET,
    EM_SETREADONLY,
};
use windows::Win32::UI::HiDpi::SystemParametersInfoForDpi;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    EN_CHANGE, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, GetParent,
    NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SendMessageW, WINDOW_EX_STYLE, WM_CHAR, WM_COPY,
    WM_HSCROLL, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN,
    WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCDESTROY, WM_RBUTTONDOWN, WM_SIZE,
    WM_SYSKEYDOWN, WM_TIMER, WM_VSCROLL, WS_EX_CLIENTEDGE, WS_HSCROLL, WS_TABSTOP, WS_VSCROLL,
};
use windows::core::{PCWSTR, PWSTR, w};

/// The private message with which a side tells its window that it may
/// have scrolled, by any means, so the other side and the line numbers
/// follow it.
pub(crate) const SIDE_SCROLLED: u32 = super::wake::WAKE + 14;

/// The RichEdit class of `Msftedit.dll`, which [`RichText::create`] loads.
const RICH_EDIT_CLASS: PCWSTR = w!("RICHEDIT50W");

/// What a RichEdit control of the compare window holds, as its subclass
/// tells them apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RichKind {
    /// A side, which the person reads and edits, and which scrolls.
    Side = 1,
    /// A side's line numbers, which follow it and take neither a click
    /// nor the wheel, which goes to the side.
    Numbers = 2,
    /// The two-line list.
    List = 3,
}

/// A RichEdit control of the compare window, whose text is RTF that the
/// window's worker built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RichText(Hwnd);

impl RichText {
    pub(crate) fn hwnd(self) -> Hwnd {
        self.0
    }

    /// A side, which tells its window of each change, selection and
    /// scroll.
    pub(crate) fn create_side(parent: Hwnd, id: u16) -> windows::core::Result<RichText> {
        // Both scroll bars always show, so both sides' text has the same
        // height and the two scroll as far.
        let style =
            ES_AUTOVSCROLL as u32 | ES_DISABLENOSCROLL | WS_TABSTOP.0 | WS_VSCROLL.0 | WS_HSCROLL.0;
        let side = Self::create(parent, id, style, WS_EX_CLIENTEDGE, RichKind::Side, 0)?;
        send(
            side.0,
            EM_SETEVENTMASK,
            0,
            (ENM_CHANGE | ENM_SELCHANGE) as isize,
        );
        Ok(side)
    }

    /// The line numbers of `side`, to which the wheel over them goes.
    pub(crate) fn create_numbers(
        parent: Hwnd,
        id: u16,
        side: RichText,
    ) -> windows::core::Result<RichText> {
        let data = side.0.0.0 as usize;
        let style = ES_AUTOVSCROLL as u32;
        Self::create(
            parent,
            id,
            style,
            WINDOW_EX_STYLE(0),
            RichKind::Numbers,
            data,
        )
    }

    pub(crate) fn create_list(parent: Hwnd, id: u16) -> windows::core::Result<RichText> {
        let style = WS_TABSTOP.0 | WS_HSCROLL.0;
        Self::create(parent, id, style, WS_EX_CLIENTEDGE, RichKind::List, 0)
    }

    /// Creates the control in `parent`, read-only, without wrapping or
    /// undo, and with no limit on its text; its subclass keeps `data`.
    fn create(
        parent: Hwnd,
        id: u16,
        style: u32,
        ex_style: WINDOW_EX_STYLE,
        kind: RichKind,
        data: usize,
    ) -> windows::core::Result<RichText> {
        load_rich_edit()?;
        let base = (ES_MULTILINE | ES_READONLY | ES_AUTOHSCROLL) as u32 | ES_NOOLEDRAGDROP;
        let hwnd = child(parent, RICH_EDIT_CLASS, base | style, ex_style, id)?;
        send(hwnd, EM_SETUNDOLIMIT, 0, 0);
        send(hwnd, EM_EXLIMITTEXT, 0, 0x7FFF_FFFE);
        // A target device of none with a line width of 1 turns wrapping off.
        send(hwnd, EM_SETTARGETDEVICE, 0, 1);
        // SAFETY: the procedure keeps `data`, a plain value, and removes
        // itself on WM_NCDESTROY, the control's last message.
        if !unsafe { SetWindowSubclass(hwnd.0, Some(procedure), kind as usize, data) }.as_bool() {
            return Err(windows::core::Error::from_thread());
        }
        Ok(RichText(hwnd))
    }

    /// Replaces the text with `rtf`. The control tells nothing of the
    /// change, which is no edit of the person's.
    pub(crate) fn set_rtf(self, rtf: &str) {
        struct Source<'a> {
            bytes: &'a [u8],
            at: usize,
        }
        unsafe extern "system" fn read(
            cookie: usize,
            buffer: *mut u8,
            wanted: i32,
            given: *mut i32,
        ) -> u32 {
            // SAFETY: the cookie is the source that `set_rtf` passed, alive
            // while EM_STREAMIN runs; the control's buffer holds `wanted`
            // bytes.
            unsafe {
                let source = &mut *(cookie as *mut Source<'_>);
                let count = usize::try_from(wanted)
                    .unwrap_or(0)
                    .min(source.bytes.len() - source.at);
                std::ptr::copy_nonoverlapping(source.bytes.as_ptr().add(source.at), buffer, count);
                source.at += count;
                *given = count as i32;
            }
            0
        }
        let mut source = Source {
            bytes: rtf.as_bytes(),
            at: 0,
        };
        let mut stream = EDITSTREAM {
            dwCookie: &raw mut source as usize,
            dwError: 0,
            pfnCallback: Some(read),
        };
        let mask = send(self.0, EM_GETEVENTMASK, 0, 0);
        send(self.0, EM_SETEVENTMASK, 0, 0);
        super::present::mark_after(self.0, || {
            // SAFETY: the stream and its source outlive the call.
            unsafe {
                SendMessageW(
                    self.0.0,
                    EM_STREAMIN,
                    Some(WPARAM(SF_RTF as usize)),
                    Some(LPARAM(&raw mut stream as isize)),
                )
            };
        });
        send(self.0, EM_SETEVENTMASK, 0, mask);
    }

    /// The length of the text in characters, its row breaks included.
    pub(crate) fn length(self) -> usize {
        let length = GETTEXTLENGTHEX {
            flags: GTL_NUMCHARS | GTL_PRECISE,
            codepage: 1200,
        };
        // SAFETY: the structure outlives the call.
        let units = unsafe {
            SendMessageW(
                self.0.0,
                EM_GETTEXTLENGTHEX,
                Some(WPARAM(&raw const length as usize)),
                None,
            )
        };
        usize::try_from(units.0).unwrap_or(0)
    }

    /// The text as the control holds it, its rows separated by CR.
    pub(crate) fn text(self) -> Vec<u16> {
        let mut buffer = vec![0u16; self.length() + 1];
        let get = GETTEXTEX {
            cb: u32::try_from(buffer.len() * 2).unwrap_or(u32::MAX),
            flags: GT_DEFAULT,
            codepage: 1200,
            ..Default::default()
        };
        // SAFETY: the buffer holds `cb` bytes, and both outlive the call.
        let copied = unsafe {
            SendMessageW(
                self.0.0,
                EM_GETTEXTEX,
                Some(WPARAM(&raw const get as usize)),
                Some(LPARAM(buffer.as_mut_ptr() as isize)),
            )
        };
        buffer.truncate(usize::try_from(copied.0).unwrap_or(0).min(buffer.len()));
        buffer
    }

    /// How many characters `row` holds, without its line break.
    pub(crate) fn row_length(self, row: usize) -> usize {
        self.row_start(row).map_or(0, |start| {
            usize::try_from(send(self.0, EM_LINELENGTH, start, 0)).unwrap_or(0)
        })
    }

    /// The text of `row`, without its line break, up to its first
    /// [`ROW_TEXT_LIMIT`] characters.
    pub(crate) fn row_text(self, row: usize) -> String {
        let Some(start) = self.row_start(row) else {
            return String::new();
        };
        let length = self.row_length(row).min(ROW_TEXT_LIMIT);
        let mut buffer = vec![0u16; length + 1];
        let position = |at: usize| i32::try_from(at).unwrap_or(i32::MAX);
        let mut range = TEXTRANGEW {
            chrg: CHARRANGE {
                cpMin: position(start),
                cpMax: position(start + length),
            },
            lpstrText: PWSTR(buffer.as_mut_ptr()),
        };
        // SAFETY: the buffer holds the range's characters and a NUL, and
        // both outlive the call.
        let copied = unsafe {
            SendMessageW(
                self.0.0,
                EM_GETTEXTRANGE,
                None,
                Some(LPARAM(&raw mut range as isize)),
            )
        };
        buffer.truncate(usize::try_from(copied.0).unwrap_or(0).min(length));
        String::from_utf16_lossy(&buffer)
    }

    /// The selection, as character positions; empty at the caret.
    pub(crate) fn selection(self) -> Range<usize> {
        let mut range = CHARRANGE::default();
        // SAFETY: `range` is a local the call writes.
        unsafe {
            SendMessageW(
                self.0.0,
                EM_EXGETSEL,
                None,
                Some(LPARAM(&raw mut range as isize)),
            )
        };
        let at = |position: i32| usize::try_from(position).unwrap_or(0);
        at(range.cpMin)..at(range.cpMax).max(at(range.cpMin))
    }

    /// Selects `range`, or places the caret at an empty one, and scrolls
    /// it into view.
    pub(crate) fn select(self, range: Range<usize>) {
        let position = |at: usize| i32::try_from(at).unwrap_or(i32::MAX);
        let range = CHARRANGE {
            cpMin: position(range.start),
            cpMax: position(range.end),
        };
        super::present::mark_after(self.0, || {
            // SAFETY: `range` outlives the call.
            unsafe {
                SendMessageW(
                    self.0.0,
                    EM_EXSETSEL,
                    None,
                    Some(LPARAM(&raw const range as isize)),
                )
            };
            send(self.0, EM_SCROLLCARET, 0, 0);
        });
    }

    /// Selects the whole text, without scrolling.
    pub(crate) fn select_all(self) {
        let all = CHARRANGE {
            cpMin: 0,
            cpMax: -1,
        };
        super::present::mark_after(self.0, || {
            // SAFETY: `all` outlives the call.
            unsafe {
                SendMessageW(
                    self.0.0,
                    EM_EXSETSEL,
                    None,
                    Some(LPARAM(&raw const all as isize)),
                )
            };
        });
    }

    /// Scrolls `rows` rows down, or up for a negative number.
    pub(crate) fn scroll_rows(self, rows: isize) {
        super::present::mark_after(self.0, || send(self.0, EM_LINESCROLL, 0, rows));
    }

    /// The row that holds the character at `position`.
    pub(crate) fn row_of(self, position: usize) -> usize {
        let at = isize::try_from(position).unwrap_or(isize::MAX);
        usize::try_from(send(self.0, EM_EXLINEFROMCHAR, 0, at)).unwrap_or(0)
    }

    /// The position of the first character of `row`; `None` past the
    /// last row.
    pub(crate) fn row_start(self, row: usize) -> Option<usize> {
        usize::try_from(send(self.0, EM_LINEINDEX, row, 0)).ok()
    }

    /// The row at the top of the control.
    pub(crate) fn first_visible_row(self) -> usize {
        usize::try_from(send(self.0, EM_GETFIRSTVISIBLELINE, 0, 0)).unwrap_or(0)
    }

    /// The point of the text at the control's top left corner.
    pub(crate) fn scroll_position(self) -> (i32, i32) {
        let mut point = POINT::default();
        // SAFETY: `point` is a local the call writes.
        unsafe {
            SendMessageW(
                self.0.0,
                EM_GETSCROLLPOS,
                None,
                Some(LPARAM(&raw mut point as isize)),
            )
        };
        (point.x, point.y)
    }

    pub(crate) fn set_scroll_position(self, (x, y): (i32, i32)) {
        let point = POINT { x, y };
        super::present::mark_after(self.0, || {
            // SAFETY: `point` outlives the call.
            unsafe {
                SendMessageW(
                    self.0.0,
                    EM_SETSCROLLPOS,
                    None,
                    Some(LPARAM(&raw const point as isize)),
                )
            };
        });
    }

    /// Makes the text read-only or not; one that already is stays unpainted,
    /// because the window asks after every keystroke.
    pub(crate) fn set_read_only(self, read_only: bool) {
        // EM_SETREADONLY sets the option that EM_GETOPTIONS reads back.
        let options = send(self.0, EM_GETOPTIONS, 0, 0);
        if ((options & ECO_READONLY as isize) != 0) == read_only {
            return;
        }
        super::present::mark_after(self.0, || {
            send(self.0, EM_SETREADONLY, usize::from(read_only), 0)
        });
    }

    /// Shows the text at `percent` of its size.
    pub(crate) fn set_zoom(self, percent: u16) {
        super::present::mark_after(self.0, || {
            send(self.0, EM_SETZOOM, usize::from(percent), 100)
        });
    }

    /// Puts the selection on the clipboard as the control does.
    pub(crate) fn copy(self) {
        send(self.0, WM_COPY, 0, 0);
    }

    /// Pastes the clipboard's text without its formatting.
    pub(crate) fn paste_text(self) {
        super::present::mark_after(self.0, || {
            send(self.0, EM_PASTESPECIAL, usize::from(CF_UNICODETEXT.0), 0)
        });
    }

    /// The next match of `text` from `from`, or with `backward` the one
    /// before it.
    pub(crate) fn find(
        self,
        text: &str,
        from: usize,
        backward: bool,
        match_case: bool,
        whole_words: bool,
    ) -> Option<Range<usize>> {
        // The flags of EM_FINDTEXTEXW, from commdlg.h.
        const FR_DOWN: usize = 0x1;
        const FR_WHOLEWORD: usize = 0x2;
        const FR_MATCHCASE: usize = 0x4;
        let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let from = i32::try_from(from).unwrap_or(i32::MAX);
        let mut find = FINDTEXTEXW {
            chrg: CHARRANGE {
                cpMin: from,
                cpMax: if backward { 0 } else { -1 },
            },
            lpstrText: PCWSTR(wide.as_ptr()),
            chrgText: CHARRANGE::default(),
        };
        let flags = if backward { 0 } else { FR_DOWN }
            | if match_case { FR_MATCHCASE } else { 0 }
            | if whole_words { FR_WHOLEWORD } else { 0 };
        // SAFETY: the text and the structure outlive the call.
        let found = unsafe {
            SendMessageW(
                self.0.0,
                EM_FINDTEXTEXW,
                Some(WPARAM(flags)),
                Some(LPARAM(&raw mut find as isize)),
            )
        };
        let range = find.chrgText;
        (found.0 >= 0).then(|| {
            let at = |position: i32| usize::try_from(position).unwrap_or(0);
            at(range.cpMin)..at(range.cpMax)
        })
    }

    /// Whether `event` says that the person changed the text.
    pub(crate) fn changed(self, event: ControlEvent) -> bool {
        event.code == EN_CHANGE && event.from == self.0
    }

    /// Whether `notification` says that the selection or the caret moved.
    pub(crate) fn selection_changed(self, notification: &Notification<'_>) -> bool {
        notification.sender() == self.0 && notification.header().code == EN_SELCHANGE
    }
}

/// Loads `Msftedit.dll`, which registers the RichEdit class, once; the
/// process keeps it.
fn load_rich_edit() -> windows::core::Result<()> {
    thread_local! {
        static LOADED: Cell<bool> = const { Cell::new(false) };
    }
    if LOADED.get() {
        return Ok(());
    }
    // SAFETY: a system library, which Windows finds among its known DLLs.
    unsafe { LoadLibraryW(w!("Msftedit.dll")) }?;
    LOADED.set(true);
    Ok(())
}

/// Passes `Ctrl` and the wheel to the window, which zooms all its
/// controls; passes the wheel over the line numbers to their side and
/// keeps the mouse's buttons from them; and tells the window after each
/// message that can scroll a side: the scroll bar, the wheel, a key, a
/// drag past its edge and a new size. It receives scroll messages and
/// sends none.
unsafe extern "system" fn procedure(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    kind: usize,
    data: usize,
) -> LRESULT {
    // SAFETY: plain values; a control without a parent gives an error.
    let parent = || unsafe { GetParent(hwnd) }.ok();
    let numbers = kind == RichKind::Numbers as usize;
    match msg {
        WM_MOUSEWHEEL if wparam.0 & MK_CONTROL.0 as usize != 0 => {
            if let Some(parent) = parent() {
                // SAFETY: the parameters are the ones Windows passed.
                unsafe { SendMessageW(parent, msg, Some(wparam), Some(lparam)) };
            }
            return LRESULT(0);
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL if numbers => {
            // SAFETY: `data` is the side's handle, which `create_numbers`
            // passed; a side gone first ignores the message.
            unsafe { SendMessageW(HWND(data as *mut _), msg, Some(wparam), Some(lparam)) };
            return LRESULT(0);
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_MOUSEMOVE
            if numbers =>
        {
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            // SAFETY: the subclass that `create` set.
            let _ = unsafe { RemoveWindowSubclass(hwnd, Some(procedure), kind) };
        }
        _ => {}
    }
    // SAFETY: the parameters are the ones Windows passed.
    let result = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
    let scrolls = matches!(
        msg,
        WM_VSCROLL
            | WM_HSCROLL
            | WM_MOUSEWHEEL
            | WM_MOUSEHWHEEL
            | WM_KEYDOWN
            | WM_SYSKEYDOWN
            | WM_CHAR
            | WM_TIMER
            | WM_LBUTTONDOWN
            | WM_LBUTTONUP
            | WM_SIZE
    ) || (msg == WM_MOUSEMOVE && wparam.0 != 0);
    if kind == RichKind::Side as usize
        && scrolls
        && let Some(parent) = parent()
    {
        // SAFETY: plain values.
        unsafe { SendMessageW(parent, SIDE_SCROLLED, None, None) };
    }
    result
}

/// The colours of the marks in the system's colours of now: red marks on a
/// light red tint, or in high contrast the selection colours, because its
/// scheme has no other colour that stands out.
pub(crate) fn palette() -> Palette {
    let rgb = |color: COLORREF| {
        let [red, green, blue, _] = color.0.to_le_bytes();
        Rgb(red, green, blue)
    };
    let window = rgb(system_color(COLOR_WINDOW));
    let text = rgb(system_color(COLOR_WINDOWTEXT));
    let muted = rgb(system_color(COLOR_GRAYTEXT));
    if high_contrast() {
        let highlight = rgb(system_color(COLOR_HIGHLIGHT));
        let highlight_text = rgb(system_color(COLOR_HIGHLIGHTTEXT));
        return Palette {
            text,
            muted,
            mark_text: highlight_text,
            mark_back: highlight,
            differs_text: highlight_text,
            differs_back: highlight,
            gap_back: rgb(system_color(COLOR_BTNFACE)),
        };
    }
    // Fluent's red for errors and removals.
    let red = Rgb(0xC4, 0x2B, 0x1C);
    let tint = window.mixed(red, 18);
    Palette {
        text,
        muted,
        mark_text: red,
        mark_back: tint,
        differs_text: red,
        differs_back: tint,
        gap_back: window.mixed(text, 8),
    }
}

/// Consolas at the size of Windows' message font, as the Viewer's text.
/// A RichEdit scales points to its own DPI, so the size is the font's at
/// 96 DPI.
pub(crate) fn rtf_font() -> RtfFont {
    let mut metrics = NONCLIENTMETRICSW {
        cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    // SAFETY: `metrics` is a local with its size set, as the call needs.
    let read = unsafe {
        SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            metrics.cbSize,
            Some((&raw mut metrics).cast()),
            0,
            96,
        )
    };
    // 12 pixels at 96 DPI, 9 points, when Windows does not say.
    let pixels = if read.is_ok() {
        metrics.lfMessageFont.lfHeight.unsigned_abs().max(6)
    } else {
        12
    };
    // A pixel is 3/4 of a point at 96 DPI, so 3/2 of a half point.
    let half_points = pixels * 3 / 2;
    // Consolas' characters are 1126/2048 of its height wide, and a half
    // point is 10 twips; a tab stops every 8 characters.
    let tab_twips = half_points * 10 * 1126 * 8 / 2048;
    RtfFont {
        half_points,
        tab_twips,
    }
}

/// The most characters [`RichText::row_text`] reads of a row. The window
/// reads the caret's row at every move for the two-line list, and a row can
/// hold a whole file of 32 MB on one line.
const ROW_TEXT_LIMIT: usize = 64 * 1024;

/// Opens `path` to read, sharing it so that other programs can still
/// write, rename and delete it while the comparison reads it.
pub(crate) fn open_shared(path: &[u16], _worker: &OffThread) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
        .open(PathBuf::from(OsString::from_wide(path)))
}

/// Whether `path` names a file or folder that is there.
pub(crate) fn exists(path: &[u16], _worker: &OffThread) -> bool {
    std::fs::metadata(PathBuf::from(OsString::from_wide(path))).is_ok()
}

/// Why Save failed.
pub(crate) struct SaveFailure {
    pub(crate) error: io::Error,
    /// The new file that holds the saved text, when Windows had already
    /// moved the file it replaces away; `None` when the file is as it was.
    pub(crate) kept: Option<PathBuf>,
}

/// Writes `bytes` to `path` as Save does (DESIGN.md: Compare by content):
/// into a new file in the same folder, which then replaces the file there,
/// keeping its attributes, or takes its name when there is none.
pub(crate) fn save_file(
    path: &[u16],
    bytes: &[u8],
    _worker: &OffThread,
) -> Result<(), SaveFailure> {
    static SAVES: AtomicU32 = AtomicU32::new(0);
    let path = PathBuf::from(OsString::from_wide(path));
    let name = path.file_name().ok_or_else(|| SaveFailure {
        error: io::Error::from(io::ErrorKind::InvalidInput),
        kept: None,
    })?;
    let temporary = path.with_file_name(format!(
        "{}.{}-{}.gozler-save",
        name.to_string_lossy(),
        std::process::id(),
        SAVES.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    let moved = written
        .map_err(|error| SaveFailure { error, kept: None })
        .and_then(|()| replace(&path, &temporary));
    if let Err(SaveFailure { kept: None, .. }) = moved {
        let _ = std::fs::remove_file(&temporary);
    }
    moved
}

/// Puts `temporary` in the place of `path`.
fn replace(path: &Path, temporary: &Path) -> Result<(), SaveFailure> {
    let wide = |path: &Path| -> Vec<u16> { path.as_os_str().encode_wide().chain([0]).collect() };
    let (target, source) = (wide(path), wide(temporary));
    let failure = |error, kept: bool| SaveFailure {
        error: super::shell::io_error(error),
        kept: kept.then(|| temporary.to_path_buf()),
    };
    // SAFETY: both paths end with a NUL and outlive the calls.
    let replaced = unsafe {
        ReplaceFileW(
            PCWSTR(target.as_ptr()),
            PCWSTR(source.as_ptr()),
            PCWSTR::null(),
            REPLACEFILE_IGNORE_MERGE_ERRORS | REPLACEFILE_IGNORE_ACL_ERRORS,
            None,
            None,
        )
    };
    let Err(error) = replaced else {
        return Ok(());
    };
    let code = error.code();
    // Windows has already removed or renamed the file, so only `temporary`
    // holds its text and must stay when the move fails too.
    let moved_away = code == ERROR_UNABLE_TO_MOVE_REPLACEMENT.to_hresult()
        || code == ERROR_UNABLE_TO_MOVE_REPLACEMENT_2.to_hresult();
    if !moved_away && code != ERROR_FILE_NOT_FOUND.to_hresult() {
        return Err(failure(error, false));
    }
    // SAFETY: as for ReplaceFileW.
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|error| failure(error, moved_away))
}
