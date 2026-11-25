//! The compare window's native parts (DESIGN.md: Compare by content): the
//! RichEdit controls of its sides, their line numbers and its two-line list,
//! the colours and font of their text, the files dropped on the window, and
//! the files its worker reads and saves.

use super::Hwnd;
use super::controls::{child, send};
use super::gdi::{high_contrast, system_color};
use super::notify::{ControlEvent, Notification};
use super::work::OffThread;
use crate::core::compare::EditedRows;
use crate::core::compare::rtf::{Font as RtfFont, Palette, Rgb};
use std::cell::{Cell, RefCell};
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
    EM_EXSETSEL, EM_GETEVENTMASK, EM_GETOPTIONS, EM_GETSCROLLPOS, EM_GETTEXTLENGTHEX,
    EM_GETTEXTRANGE, EM_PASTESPECIAL, EM_REDO, EM_SETEVENTMASK, EM_SETSCROLLPOS,
    EM_SETTARGETDEVICE, EM_SETTEXTEX, EM_SETUNDOLIMIT, EM_SETZOOM, EM_STREAMIN, EN_SELCHANGE,
    ENM_CHANGE, ENM_SELCHANGE, ES_DISABLENOSCROLL, ES_NOOLEDRAGDROP, GETTEXTLENGTHEX, GTL_NUMCHARS,
    GTL_PRECISE, SF_RTF, SFF_SELECTION, TEXTRANGEW,
};
use windows::Win32::UI::Controls::{
    EM_GETFIRSTVISIBLELINE, EM_GETLINECOUNT, EM_LINEINDEX, EM_LINELENGTH, EM_LINESCROLL,
    EM_REPLACESEL, EM_SCROLLCARET, EM_SETREADONLY, EM_UNDO,
};
use windows::Win32::UI::HiDpi::SystemParametersInfoForDpi;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    EN_CHANGE, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, GetParent,
    NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SendMessageW, WINDOW_EX_STYLE, WM_CHAR, WM_CLEAR,
    WM_CUT, WM_HSCROLL, WM_IME_CHAR, WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION,
    WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCDESTROY, WM_PASTE,
    WM_RBUTTONDOWN, WM_SETTEXT, WM_SIZE, WM_SYSCHAR, WM_SYSKEYDOWN, WM_TIMER, WM_UNDO, WM_UNICHAR,
    WM_VSCROLL, WS_EX_CLIENTEDGE, WS_HSCROLL, WS_TABSTOP, WS_VSCROLL,
};
use windows::core::{PCWSTR, PWSTR, w};

/// The private message with which a side tells its window that it may
/// have scrolled, by any means, so the other side and the line numbers
/// follow it.
pub(crate) const SIDE_SCROLLED: u32 = super::wake::WAKE + 14;

/// Where an edit of a side can change its text: not in the `before` units
/// before the selection that the edit replaces, nor in the `after` units
/// after it. An edit replaces the selection, so the window reads only the
/// rows between them, and not the whole side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EditBounds {
    before: usize,
    after: usize,
}

impl EditBounds {
    /// The bounds of both edits.
    fn and(self, other: EditBounds) -> EditBounds {
        EditBounds {
            before: self.before.min(other.before),
            after: self.after.min(other.after),
        }
    }
}

/// The edits that run in a side, which its subclass records.
struct Editing {
    side: Hwnd,
    /// The message that can change the text and runs now.
    message: Option<EditBounds>,
    /// An IME composition, which changes the text over several messages
    /// from where it started; `Some(None)` once the window replaced the
    /// text under it, which leaves its start unknown.
    composition: Option<Option<EditBounds>>,
}

thread_local! {
    static EDITING: RefCell<Vec<Editing>> = const { RefCell::new(Vec::new()) };
}

/// Whether `msg` can change a side's text, as typing, pasting, cutting,
/// deleting or an IME does.
fn changes_text(msg: u32) -> bool {
    matches!(
        msg,
        WM_CHAR
            | WM_SYSCHAR
            | WM_UNICHAR
            | WM_IME_CHAR
            | WM_IME_COMPOSITION
            | WM_KEYDOWN
            | WM_SYSKEYDOWN
            | WM_PASTE
            | WM_CUT
            | WM_CLEAR
            | WM_UNDO
            | WM_SETTEXT
            | EM_UNDO
            | EM_REDO
            | EM_PASTESPECIAL
            | EM_REPLACESEL
            | EM_SETTEXTEX
    )
}

/// Runs `change` on the record of `side`'s edits, when it has one.
fn with_editing<T>(side: Hwnd, change: impl FnOnce(&mut Editing) -> T) -> Option<T> {
    EDITING.with_borrow_mut(|editing| {
        (editing.iter_mut())
            .find(|each| each.side == side)
            .map(change)
    })
}

/// Records that a message that can change `side`'s text starts, or with
/// `composition` that an IME composition starts, within `bounds`; `false`
/// when one runs already, whose bounds hold this one's.
fn edit_starts(side: Hwnd, bounds: EditBounds, composition: bool) -> bool {
    EDITING.with_borrow_mut(|editing| {
        let index = match editing.iter().position(|each| each.side == side) {
            Some(index) => index,
            None => {
                editing.push(Editing {
                    side,
                    message: None,
                    composition: None,
                });
                editing.len() - 1
            }
        };
        let record = &mut editing[index];
        if composition {
            if record.composition.is_some() {
                return false;
            }
            record.composition = Some(Some(bounds));
        } else {
            if record.message.is_some() {
                return false;
            }
            record.message = Some(bounds);
        }
        true
    })
}

/// Records that the message, or with `composition` the IME composition,
/// that changed `side`'s text ended.
fn edit_ends(side: Hwnd, composition: bool) {
    with_editing(side, |editing| {
        if composition {
            editing.composition = None;
        } else {
            editing.message = None;
        }
    });
}

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

    /// Replaces the text with `rtf`, whose rows the control then holds. The
    /// control tells nothing of the change, which is no edit of the
    /// person's.
    pub(crate) fn set_rtf(self, rtf: &str) {
        // A whole stream takes the document's last paragraph mark for the
        // one that ends every RichEdit text, so a last row that is empty
        // would be lost. A mark after the last row keeps every row.
        let body = (rtf.strip_suffix('}')).expect("an RTF document ends with its closing brace");
        self.quietly(|| self.stream_in(&[body.as_bytes(), br"\par}"], SF_RTF));
    }

    /// Replaces `rows`, which are not empty, with the rows of `rtf`, which
    /// holds one row or more and ends without a paragraph mark, as
    /// a slice of a [`crate::core::compare::rtf::Patch`] does. The control tells
    /// nothing of the change, and the selection ends after the new rows.
    pub(crate) fn replace_rows(self, rows: Range<usize>, rtf: &str) {
        let last = rows.end.saturating_sub(1).max(rows.start);
        let start = self.row_start(rows.start).unwrap_or_else(|| self.length());
        let end = self
            .row_start(last)
            .map_or(start, |at| at + self.row_length(last))
            .max(start);
        let range = CHARRANGE {
            cpMin: i32::try_from(start).unwrap_or(i32::MAX),
            cpMax: i32::try_from(end).unwrap_or(i32::MAX),
        };
        self.quietly(|| {
            // SAFETY: `range` outlives the call.
            unsafe {
                SendMessageW(
                    self.0.0,
                    EM_EXSETSEL,
                    None,
                    Some(LPARAM(&raw const range as isize)),
                )
            };
            self.stream_in(&[rtf.as_bytes()], SF_RTF | SFF_SELECTION);
        });
    }

    /// Replaces the last row with the rows of `rtf`, which start with it
    /// again, as a slice of a [`crate::core::compare::rtf::Patch`] after the
    /// first does.
    /// The selection and the view stay, so rows that stream in below do not
    /// move what the person sees.
    pub(crate) fn extend(self, rtf: &str) {
        let selection = self.selection();
        let position = self.scroll_position();
        let last = self.row_count().saturating_sub(1);
        self.replace_rows(last..last + 1, rtf);
        let position = POINT {
            x: position.0,
            y: position.1,
        };
        let at = |at: usize| i32::try_from(at).unwrap_or(i32::MAX);
        let selection = CHARRANGE {
            cpMin: at(selection.start),
            cpMax: at(selection.end),
        };
        self.quietly(|| {
            // SAFETY: `selection` and `position` outlive the calls.
            unsafe {
                SendMessageW(
                    self.0.0,
                    EM_EXSETSEL,
                    None,
                    Some(LPARAM(&raw const selection as isize)),
                );
                SendMessageW(
                    self.0.0,
                    EM_SETSCROLLPOS,
                    None,
                    Some(LPARAM(&raw const position as isize)),
                );
            }
        });
    }

    /// Runs `change`, which replaces text, without telling the window of
    /// the change or of the selection it moves, because neither is the
    /// person's.
    fn quietly(self, change: impl FnOnce()) {
        let mask = send(self.0, EM_GETEVENTMASK, 0, 0);
        send(self.0, EM_SETEVENTMASK, 0, 0);
        super::present::mark_after(self.0, change);
        send(self.0, EM_SETEVENTMASK, 0, mask);
        // A composition no longer knows where its text starts.
        with_editing(self.0, |editing| {
            if let Some(composition) = &mut editing.composition {
                *composition = None;
            }
        });
    }

    /// Streams the RTF in `parts`, one after the other, in as `format` says.
    fn stream_in(self, parts: &[&[u8]], format: u32) {
        struct Source<'a> {
            parts: std::slice::Iter<'a, &'a [u8]>,
            rest: &'a [u8],
        }
        unsafe extern "system" fn read(
            cookie: usize,
            buffer: *mut u8,
            wanted: i32,
            given: *mut i32,
        ) -> u32 {
            // SAFETY: the cookie is the source that `stream_in` passed, alive
            // while EM_STREAMIN runs; the control's buffer holds `wanted`
            // bytes.
            unsafe {
                let source = &mut *(cookie as *mut Source<'_>);
                while source.rest.is_empty()
                    && let Some(part) = source.parts.next()
                {
                    source.rest = *part;
                }
                let count = usize::try_from(wanted).unwrap_or(0).min(source.rest.len());
                std::ptr::copy_nonoverlapping(source.rest.as_ptr(), buffer, count);
                source.rest = &source.rest[count..];
                *given = count as i32;
            }
            0
        }
        let mut source = Source {
            parts: parts.iter(),
            rest: &[],
        };
        let mut stream = EDITSTREAM {
            dwCookie: &raw mut source as usize,
            dwError: 0,
            pfnCallback: Some(read),
        };
        // SAFETY: the stream and its source outlive the call.
        unsafe {
            SendMessageW(
                self.0.0,
                EM_STREAMIN,
                Some(WPARAM(format as usize)),
                Some(LPARAM(&raw mut stream as isize)),
            )
        };
    }

    /// The rows that the edit that runs now changed, between the selection
    /// that it replaced and the caret after it; `None` when no message that
    /// edits runs, or where the edit started is not known.
    pub(crate) fn edited_rows(self) -> Option<EditedRows> {
        let bounds = self.edit_bounds()?;
        let length = self.length();
        let caret = self.selection().start.min(length);
        let start = bounds.before.min(caret);
        let end = length - bounds.after.min(length - caret);
        let first = self.row_of(start);
        let last = self.row_of(end).max(first);
        let from = self.row_start(first).unwrap_or(0).min(start);
        let to = self
            .row_start(last)
            .map_or(length, |at| at + self.row_length(last))
            .max(end);
        Some(EditedRows {
            first,
            last,
            count: self.row_count(),
            text: self.text_of(from..to),
            caret: caret - from,
        })
    }

    /// Where the edit that runs now can change the text, or `None` when no
    /// message that edits runs, or an edit's start is not known.
    fn edit_bounds(self) -> Option<EditBounds> {
        with_editing(self.0, |editing| match editing.composition {
            Some(None) => None,
            Some(Some(composition)) => {
                Some((editing.message).map_or(composition, |message| message.and(composition)))
            }
            None => editing.message,
        })
        .flatten()
    }

    /// The edit's bounds at the selection of now.
    fn bounds_now(self) -> EditBounds {
        let selection = self.selection();
        EditBounds {
            before: selection.start,
            after: self.length().saturating_sub(selection.end),
        }
    }

    /// The rows the control holds; an empty control holds one.
    pub(crate) fn row_count(self) -> usize {
        usize::try_from(send(self.0, EM_GETLINECOUNT, 0, 0))
            .unwrap_or(1)
            .max(1)
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

    /// The text of `range`, with the rows separated by CR.
    pub(crate) fn text_of(self, range: Range<usize>) -> Vec<u16> {
        let length = range.len();
        let mut buffer = vec![0u16; length + 1];
        let position = |at: usize| i32::try_from(at).unwrap_or(i32::MAX);
        let mut range = TEXTRANGEW {
            chrg: CHARRANGE {
                cpMin: position(range.start),
                cpMax: position(range.end),
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
        String::from_utf16_lossy(&self.text_of(start..start + length))
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

    /// The selected text with its rows separated by CR LF, as a text box
    /// copies it, or `None` while nothing is selected. The caller publishes
    /// it through the clipboard worker, because `WM_COPY` makes the
    /// window's thread wait for the program whose clipboard it replaces.
    pub(crate) fn selected_text(self) -> Option<String> {
        let selection = self.selection();
        if selection.is_empty() {
            return None;
        }
        Some(String::from_utf16_lossy(&self.text_of(selection)).replace('\r', "\r\n"))
    }

    /// Pastes the clipboard's text without its formatting.
    pub(crate) fn paste_text(self) {
        super::present::mark_after(self.0, || {
            send(self.0, EM_PASTESPECIAL, usize::from(CF_UNICODETEXT.0), 0)
        });
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
            EDITING.with_borrow_mut(|editing| editing.retain(|each| each.side.0 != hwnd));
            // SAFETY: the subclass that `create` set.
            let _ = unsafe { RemoveWindowSubclass(hwnd, Some(procedure), kind) };
        }
        _ => {}
    }
    // The window reads an edit's rows between the selection that the edit
    // replaces and the caret after it, so a side records the selection
    // before a message that can change its text.
    let side = (kind == RichKind::Side as usize).then_some(RichText(Hwnd(hwnd)));
    let composition = msg == WM_IME_STARTCOMPOSITION;
    let started = match side {
        Some(side) if composition || changes_text(msg) => {
            edit_starts(side.0, side.bounds_now(), composition) && !composition
        }
        _ => false,
    };
    // SAFETY: the parameters are the ones Windows passed.
    let result = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
    if let Some(side) = side {
        if started {
            edit_ends(side.0, false);
        }
        if msg == WM_IME_ENDCOMPOSITION {
            edit_ends(side.0, true);
        }
    }
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
    // Fluent's red for errors and removals. As in Total Commander, the
    // words that differ show in red on the text's own background, and a row
    // that differs, or has no line, is grey in the line-number column.
    let red = Rgb(0xC4, 0x2B, 0x1C);
    let grey = window.mixed(text, 8);
    Palette {
        text,
        muted,
        mark_text: red,
        mark_back: window,
        differs_text: red,
        differs_back: grey,
        gap_back: grey,
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
