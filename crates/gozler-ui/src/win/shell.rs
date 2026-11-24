//! COM and OLE, the known folders, the save dialog, the folder and program
//! pickers, the shell's verbs: open, show in File Explorer, Properties and
//! run as administrator, dragging entries out, the person's registry settings,
//! and the Date taken that File Explorer shows.

use super::Hwnd;
use crate::core::clipboard::folder_of;
use crate::core::selection::DragEntry;
use std::cell::Cell;
use std::ffi::OsStr;
use std::io;
use std::marker::PhantomData;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use windows::Win32::Foundation::{
    CloseHandle, E_POINTER, ERROR_CANCELLED, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, FILETIME, HANDLE,
    HWND, POINT, PROPERTYKEY, WAIT_OBJECT_0, WAIT_TIMEOUT, WIN32_ERROR,
};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, TOKEN_ALL_ACCESS, TOKEN_ASSIGN_PRIMARY,
    TOKEN_DUPLICATE, TOKEN_QUERY, TokenPrimary,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, GetVolumeInformationW, WIN32_FIND_DATAW,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
    CreateBindCtx, IBindCtx, IDataObject, IDispatch, IServiceProvider,
};
use windows::Win32::System::Ole::{
    DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, IDropSource, OleInitialize, OleUninitialize,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_ROUTINE_FLAGS, REG_SZ, RRF_RT_REG_BINARY,
    RRF_RT_REG_SZ, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegSetKeyValueW,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessWithTokenW, GetExitCodeProcess, INFINITE, LOGON_WITH_PROFILE,
    OpenProcess, OpenProcessToken, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    STARTUPINFOW, WaitForSingleObject,
};
use windows::Win32::System::Variant::{VARIANT, VT_BSTR, VariantClear};
use windows::Win32::UI::Shell::Common::{COMDLG_FILTERSPEC, ITEMIDLIST};
use windows::Win32::UI::Shell::{
    DragAcceptFiles, DragFinish, DragQueryFileW, DragQueryPoint, FOLDERID_Documents,
    FOLDERID_Downloads, FOLDERID_LocalAppData, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
    FOS_NOTESTFILECREATE, FOS_OVERWRITEPROMPT, FOS_PICKFOLDERS, FileOpenDialog, FileSaveDialog,
    HDROP, IFileOpenDialog, IFileSaveDialog, IFileSystemBindData, IFileSystemBindData_Impl,
    ILClone, ILCreateFromPathW, ILFindLastID, ILFree, ILIsParent, ILRemoveLastID, IShellBrowser,
    IShellDispatch2, IShellFolderViewDual, IShellItem, IShellItem2, IShellWindows, KF_FLAG_DEFAULT,
    KF_FLAG_DONT_VERIFY, KNOWN_FOLDER_FLAG, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC,
    SEE_MASK_NOCLOSEPROCESS, SHCreateDataObject, SHCreateItemFromParsingName, SHDoDragDrop,
    SHELLEXECUTEINFOW, SHGetKnownFolderPath, SHOP_FILEPATH, SHObjectProperties,
    SHOpenFolderAndSelectItems, SHParseDisplayName, SID_STopLevelBrowser, SIGDN_FILESYSPATH,
    STR_FILE_SYS_BIND_DATA, SVGIO_BACKGROUND, SWC_DESKTOP, SWFO_NEEDDISPATCH, ShellExecuteExW,
    ShellWindows,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetShellWindow, GetWindowThreadProcessId, SHOW_WINDOW_CMD, SW_HIDE, SW_SHOWNORMAL,
};
use windows::core::{BSTR, GUID, HSTRING, IUnknown, Interface, PCWSTR, PWSTR, w};

mod windows_shortcut;

pub(crate) fn set_windows_shortcut_disabled(
    keys: &[u8],
    worker: &super::work::OffThread,
) -> io::Result<()> {
    windows_shortcut::set_disabled(keys, worker)
}

/// Starts COM on this thread in the apartment model, which the shell's
/// dialogs need.
pub(crate) fn initialize_com() {
    // SAFETY: called once on each thread that makes COM or shell calls,
    // before the first.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
}

/// Starts OLE on the window's thread, which a drag needs; the thread's COM
/// apartment stays as [`initialize_com`] made it.
pub(crate) fn initialize_ole() {
    // SAFETY: called once on the window's thread, after `initialize_com`.
    let _ = unsafe { OleInitialize(None) };
}

/// One entry as the index holds it, which the shell reads through
/// `STR_FILE_SYS_BIND_DATA` instead of reading the entry from the disk.
#[windows::core::implement(IFileSystemBindData)]
struct FindData(Cell<WIN32_FIND_DATAW>);

impl IFileSystemBindData_Impl for FindData_Impl {
    fn SetFindData(&self, data: *const WIN32_FIND_DATAW) -> windows::core::Result<()> {
        if data.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: the caller passes one find data, checked not to be null.
        self.0.set(unsafe { *data });
        Ok(())
    }

    fn GetFindData(&self, data: *mut WIN32_FIND_DATAW) -> windows::core::Result<()> {
        if data.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: the caller passes a buffer for one find data, checked not
        // to be null.
        unsafe { *data = self.0.get() };
        Ok(())
    }
}

/// What `FindFirstFileW` would report for `file`, from what its row holds;
/// a size or a date the row lacks is 0.
fn find_data(file: &DragEntry) -> WIN32_FIND_DATAW {
    let mut attributes = file.attributes.unwrap_or(0);
    if file.is_folder {
        attributes |= FILE_ATTRIBUTE_DIRECTORY.0;
    }
    if attributes == 0 {
        attributes = FILE_ATTRIBUTE_NORMAL.0;
    }
    // A folder's size in the row is the sum of its files; Windows gives 0.
    let size = if file.is_folder {
        0
    } else {
        file.size.unwrap_or(0)
    };
    let modified = file.modified.unwrap_or(0) as u64;
    let mut data = WIN32_FIND_DATAW {
        dwFileAttributes: attributes,
        ftLastWriteTime: FILETIME {
            dwLowDateTime: modified as u32,
            dwHighDateTime: (modified >> 32) as u32,
        },
        nFileSizeHigh: (size >> 32) as u32,
        nFileSizeLow: size as u32,
        ..Default::default()
    };
    let last = file.path.iter().rposition(|&u| u == u16::from(b'\\'));
    let name = &file.path[last.map_or(0, |i| i + 1)..];
    // The last unit stays the NUL; an NTFS or ReFS name always fits.
    let length = name.len().min(data.cFileName.len() - 1);
    data.cFileName[..length].copy_from_slice(&name[..length]);
    data
}

/// Drags `files` as File Explorer drags its selection: the shell's own data
/// object, so a drop target gets the files and the shell's item list, with
/// Explorer's drag image, and the target copies, moves or links them.
/// Returns when the drop ends, with the first path that the shell could not
/// name, such as one on a drive that is gone; the others are dragged.
///
/// Each path is named from what its row holds, without reading the disk, so
/// an entry deleted since the search is dragged too, and the drop target
/// reports it, as it does for any stale item.
pub(crate) fn drag(files: &[DragEntry]) -> Option<Vec<u16>> {
    let found: IFileSystemBindData = FindData(Cell::new(WIN32_FIND_DATAW::default())).into();
    // SAFETY: plain COM calls on the window's thread, whose apartment
    // `initialize_com` started; the context holds its own reference.
    let context = unsafe {
        CreateBindCtx(0).and_then(|context| {
            context.RegisterObjectParam(STR_FILE_SYS_BIND_DATA, &found.cast::<IUnknown>()?)?;
            Ok(context)
        })
    };
    let Ok(context) = context else {
        return None;
    };
    let mut missing = None;
    let mut lists = Vec::with_capacity(files.len());
    let mut paths = Vec::with_capacity(files.len());
    for file in files {
        let path = &file.path;
        let mut list = std::ptr::null_mut();
        // SAFETY: the find data and the path outlive the calls; the second
        // writes `list`.
        let named = unsafe {
            found.SetFindData(&find_data(file)).and_then(|()| {
                SHParseDisplayName(&HSTRING::from_wide(path), &context, &mut list, 0, None)
            })
        };
        if named.is_ok() && !list.is_null() {
            lists.push(list);
            paths.push(PathBuf::from(std::ffi::OsString::from_wide(path)));
        } else {
            missing.get_or_insert_with(|| path.clone());
        }
    }
    let Some(&first) = lists.first() else {
        return missing;
    };
    // SAFETY: every list is this function's own until it is freed below, and
    // the last item of a list names its entry inside its folder.
    unsafe {
        let parent = ILClone(first);
        let _ = ILRemoveLastID(Some(parent));
        let shared = lists
            .iter()
            .all(|list| ILIsParent(parent, *list, true).as_bool());
        let data: windows::core::Result<IDataObject> = if shared {
            let children: Vec<*const ITEMIDLIST> = lists
                .iter()
                .map(|list| ILFindLastID(*list).cast_const())
                .collect();
            SHCreateDataObject(
                Some(parent),
                Some(children.as_slice()),
                None::<&IDataObject>,
            )
        } else {
            // Each list names its entry from the desktop.
            let absolute: Vec<*const ITEMIDLIST> =
                lists.iter().map(|list| list.cast_const()).collect();
            SHCreateDataObject(None, Some(absolute.as_slice()), None::<&IDataObject>)
        };
        if let Ok(data) = data {
            let _own = super::drop_target::own_drag(&data, paths.into());
            let _modal = super::present::modal_loop();
            // A move needs nothing here: the index follows the journal.
            let _ = SHDoDragDrop(
                None,
                &data,
                None::<&IDropSource>,
                DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK,
            );
        }
        ILFree(Some(parent));
        for list in lists {
            ILFree(Some(list));
        }
    }
    missing
}

/// The user's local application data folder, such as
/// `C:\Users\ana\AppData\Local`.
pub(crate) fn local_app_data() -> windows::core::Result<PathBuf> {
    known_folder(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT).map(PathBuf::from)
}

/// The user's Documents folder, which the left panel shows the first time
/// (DESIGN.md: A panel). It is not checked, because a folder redirected to a
/// server that does not answer would hold the window; the panel's reading finds
/// out.
pub(crate) fn documents() -> windows::core::Result<String> {
    known_folder(&FOLDERID_Documents, KF_FLAG_DONT_VERIFY)
}

/// The user's Downloads folder, which the right panel shows the first time,
/// not checked, as [`documents`].
pub(crate) fn downloads() -> windows::core::Result<String> {
    known_folder(&FOLDERID_Downloads, KF_FLAG_DONT_VERIFY)
}

fn known_folder(
    id: &windows::core::GUID,
    flags: KNOWN_FOLDER_FLAG,
) -> windows::core::Result<String> {
    // SAFETY: the returned string is copied, then freed as the call requires.
    unsafe {
        let path = SHGetKnownFolderPath(id, flags, None)?;
        let text = path.to_string();
        CoTaskMemFree(Some(path.0.cast_const().cast()));
        text.map_err(|_| windows::core::Error::from_thread())
    }
}

/// The shell slot keeps this apartment and its pump alive between invocations;
/// NOASYNC alone does not protect URI handlers after ShellExecuteEx returns.
pub(crate) struct Session {
    initialized: windows::core::Result<()>,
    ole: Cell<bool>,
    _thread: PhantomData<Rc<()>>,
}

impl Session {
    pub(crate) fn new(_worker: &super::work::OffThread) -> Self {
        // SAFETY: a fresh worker thread initializes its own STA once.
        let initialized =
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok() };
        Self {
            initialized,
            ole: Cell::new(false),
            _thread: PhantomData,
        }
    }

    pub(crate) fn ole(&self, _worker: &super::work::OffThread) -> bool {
        if self.ole.get() {
            return true;
        }
        if self.initialized.is_err() {
            return false;
        }
        // SAFETY: this session's STA stays on its creating worker thread.
        let initialized = unsafe { OleInitialize(None) }.is_ok();
        self.ole.set(initialized);
        initialized
    }

    pub(crate) fn open(&self, target: &str) -> io::Result<()> {
        self.ready()?;
        execute(
            HWND::default(),
            w!("open"),
            &HSTRING::from(target),
            &HSTRING::new(),
            None,
            SEE_MASK_NOASYNC,
            SW_SHOWNORMAL,
        )
        .map(|_| ())
    }

    pub(crate) fn start_program(&self, program: &Path) -> io::Result<()> {
        self.ready()?;
        let program: Vec<_> = program.as_os_str().encode_wide().collect();
        execute(
            HWND::default(),
            w!("open"),
            &HSTRING::from_wide(&program),
            &HSTRING::new(),
            None,
            SEE_MASK_NOASYNC,
            SW_SHOWNORMAL,
        )
        .map(|_| ())
    }

    pub(crate) fn open_entry(&self, path: &[u16]) -> io::Result<()> {
        self.ready()?;
        // A program starts in its own folder, as File Explorer and Total
        // Commander start it, so it finds the files beside it.
        let folder = HSTRING::from_wide(folder_of(path));
        execute(
            HWND::default(),
            PCWSTR::null(),
            &HSTRING::from_wide(path),
            &HSTRING::new(),
            Some(&folder),
            SEE_MASK_NOASYNC,
            SW_SHOWNORMAL,
        )
        .map(|_| ())
    }

    pub(crate) fn explore(&self, folder: &[u16]) -> io::Result<()> {
        self.ready()?;
        let quote = u16::from(b'"');
        let mut parameters = vec![quote];
        parameters.extend_from_slice(folder);
        // A root's backslash must not escape the closing quote.
        if folder.last() == Some(&u16::from(b'\\')) {
            parameters.push(u16::from(b'\\'));
        }
        parameters.push(quote);
        execute(
            HWND::default(),
            PCWSTR::null(),
            &HSTRING::from("explorer.exe"),
            &HSTRING::from_wide(&parameters),
            None,
            SEE_MASK_NOASYNC,
            SW_SHOWNORMAL,
        )
        .map(|_| ())
    }

    pub(crate) fn show_in_folder(&self, folder: &[u16], entries: &[Vec<u16>]) -> io::Result<()> {
        self.ready()?;
        show_in_folder(folder, entries)
    }

    pub(crate) fn properties(&self, path: &[u16]) -> io::Result<()> {
        self.ready()?;
        // SAFETY: SHObjectProperties allows no owner. Path outlives invocation,
        // and the worker's apartment keeps pumping afterward.
        let opened = unsafe {
            SHObjectProperties(
                None,
                SHOP_FILEPATH,
                &HSTRING::from_wide(path),
                PCWSTR::null(),
            )
        };
        if opened.as_bool() {
            Ok(())
        } else {
            Err(io_error(windows::Win32::Foundation::E_FAIL.into()))
        }
    }

    pub(crate) fn run_command(&self, program: &str, parameters: &str) -> io::Result<()> {
        self.ready()?;
        execute(
            HWND::default(),
            PCWSTR::null(),
            &HSTRING::from(program),
            &HSTRING::from(parameters),
            None,
            SEE_MASK_NOASYNC,
            SW_SHOWNORMAL,
        )
        .map(|_| ())
    }

    fn ready(&self) -> io::Result<()> {
        self.initialized
            .as_ref()
            .map_err(|error| io_error(error.clone()))
            .copied()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.ole.get() {
            // SAFETY: all source leases finish before the session leaves its
            // worker scope; this balances that thread's OleInitialize.
            unsafe { OleUninitialize() };
        }
        if self.initialized.is_ok() {
            // SAFETY: match this thread's successful CoInitializeEx, after the
            // slot closes; no apartment interface crosses to another thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// Opens the folder `folder` in File Explorer with `entries`, which are in
/// it, selected. Each path is UTF-16 without a NUL.
fn show_in_folder(folder: &[u16], entries: &[Vec<u16>]) -> io::Result<()> {
    let item_list = |path: &[u16]| {
        // SAFETY: the path outlives the call; null is a path the shell
        // cannot name.
        let list = unsafe { ILCreateFromPathW(&HSTRING::from_wide(path)) };
        (!list.is_null()).then_some(list)
    };
    let Some(folder) = item_list(folder) else {
        return Err(io::ErrorKind::NotFound.into());
    };
    let entries: Vec<*mut ITEMIDLIST> = entries.iter().filter_map(|e| item_list(e)).collect();
    // SAFETY: each list is this function's own until it is freed below, and
    // the last item of an entry's list names it inside its folder.
    let result = unsafe {
        let children: Vec<*const ITEMIDLIST> = entries
            .iter()
            .map(|list| ILFindLastID(*list).cast_const())
            .collect();
        let result = SHOpenFolderAndSelectItems(folder, Some(children.as_slice()), 0);
        for list in entries {
            ILFree(Some(list));
        }
        ILFree(Some(folder));
        result
    };
    result.map_err(io_error)
}

/// One file type of the save dialog: its name and its extension.
pub(crate) struct FileType<'a> {
    pub(crate) name: &'a str,
    pub(crate) extension: &'a str,
}

/// Shows the Windows save dialog, open at `start` when that folder exists,
/// which asks before it replaces a file, and returns the chosen file and the
/// index of the type it showed, or `None` when the person cancels. It runs
/// its own message loop, so the caller holds no borrow across it
/// (DESIGN.md: window rule 1).
pub(crate) fn save_dialog(
    owner: Hwnd,
    title: &str,
    start: Option<&str>,
    file_name: &str,
    types: &[FileType],
) -> Option<(PathBuf, usize)> {
    let names: Vec<HSTRING> = types.iter().map(|t| HSTRING::from(t.name)).collect();
    let specs: Vec<HSTRING> = types
        .iter()
        .map(|t| HSTRING::from(format!("*.{}", t.extension)))
        .collect();
    let filters: Vec<COMDLG_FILTERSPEC> = names
        .iter()
        .zip(&specs)
        .map(|(name, spec)| COMDLG_FILTERSPEC {
            pszName: PCWSTR(name.as_ptr()),
            pszSpec: PCWSTR(spec.as_ptr()),
        })
        .collect();
    // SAFETY: every string outlives the dialog, which copies them; the
    // chosen path is copied, then freed as the call requires.
    unsafe {
        let dialog: IFileSaveDialog =
            CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        // The export creates the file itself, and only a file on disk can
        // be written.
        dialog
            .SetOptions(options | FOS_OVERWRITEPROMPT | FOS_NOTESTFILECREATE | FOS_FORCEFILESYSTEM)
            .ok()?;
        dialog.SetFileTypes(&filters).ok()?;
        dialog.SetFileTypeIndex(1).ok()?;
        // A type of any extension, `*`, adds none to a name typed without.
        if let Some(first) = types.first().filter(|first| first.extension != "*") {
            dialog
                .SetDefaultExtension(&HSTRING::from(first.extension))
                .ok()?;
        }
        dialog.SetTitle(&HSTRING::from(title)).ok()?;
        if let Some(start) = start
            && let Ok(folder) = SHCreateItemFromParsingName::<_, _, IShellItem>(
                &HSTRING::from(start),
                None::<&IBindCtx>,
            )
        {
            let _ = dialog.SetFolder(&folder);
        }
        dialog.SetFileName(&HSTRING::from(file_name)).ok()?;
        let _modal = super::present::modal_loop();
        dialog.Show(Some(owner.0)).ok()?;
        let index = dialog.GetFileTypeIndex().ok()?;
        let path = dialog
            .GetResult()
            .ok()?
            .GetDisplayName(SIGDN_FILESYSPATH)
            .ok()?;
        let text = path.to_string();
        CoTaskMemFree(Some(path.0.cast_const().cast()));
        let index = usize::try_from(index).ok()?.checked_sub(1)?;
        Some((PathBuf::from(text.ok()?), index))
    }
}

/// Shows the Windows folder picker, open at `start` when that folder
/// exists, and returns the chosen folder, or `None` when the person
/// cancels. It runs its own message loop, so the caller holds no borrow
/// across it (DESIGN.md: window rule 1).
pub(crate) fn pick_folder(owner: Hwnd, start: Option<&str>) -> Option<String> {
    // SAFETY: every string outlives the call that reads it; the chosen
    // path is copied, then freed as the call requires.
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        // Only a folder on disk can be searched.
        dialog
            .SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
            .ok()?;
        if let Some(start) = start
            && let Ok(folder) = SHCreateItemFromParsingName::<_, _, IShellItem>(
                &HSTRING::from(start),
                None::<&IBindCtx>,
            )
        {
            let _ = dialog.SetFolder(&folder);
        }
        let _modal = super::present::modal_loop();
        dialog.Show(Some(owner.0)).ok()?;
        let path = dialog
            .GetResult()
            .ok()?
            .GetDisplayName(SIGDN_FILESYSPATH)
            .ok()?;
        let text = path.to_string();
        CoTaskMemFree(Some(path.0.cast_const().cast()));
        text.ok()
    }
}

/// Shows the Windows file dialog for programs, titled `title`, whose file
/// type is named `programs`, and returns the chosen program, or `None` when
/// the person cancels. It runs its own message loop, so the caller holds no
/// borrow across it (DESIGN.md: window rule 1).
pub(crate) fn pick_program(owner: Hwnd, title: &str, programs: &str) -> Option<String> {
    pick_file(owner, title, &format!("{programs} (*.exe)"), "*.exe")
}

/// Lets `window` take files that File Explorer drops on it.
pub(crate) fn accept_files(window: Hwnd) {
    // SAFETY: plain values.
    unsafe { DragAcceptFiles(window.0, true) };
}

/// The files of the drop `drop` and its point in the window's client
/// coordinates; ends the drop.
///
/// # Safety
/// `drop` is the handle of a `WM_DROPFILES` that the window received.
pub(super) unsafe fn dropped_files(drop: HDROP) -> (Vec<Vec<u16>>, (i32, i32)) {
    // SAFETY: the caller's contract; DragFinish ends the handle's life last.
    unsafe {
        let files = drop_paths(drop);
        let mut point = POINT::default();
        let _ = DragQueryPoint(drop, &raw mut point);
        DragFinish(drop);
        (files, (point.x, point.y))
    }
}

/// The paths that the drop block `drop` holds.
///
/// # Safety
/// `drop` is a `DROPFILES` block: a drop's handle or a `CF_HDROP` medium.
pub(super) unsafe fn drop_paths(drop: HDROP) -> Vec<Vec<u16>> {
    // SAFETY: the caller's contract; each buffer holds the length the call
    // reports.
    unsafe {
        let count = DragQueryFileW(drop, u32::MAX, None);
        (0..count)
            .map(|index| {
                let length = DragQueryFileW(drop, index, None) as usize;
                let mut path = vec![0u16; length + 1];
                let copied = DragQueryFileW(drop, index, Some(&mut path)) as usize;
                path.truncate(copied);
                path
            })
            .filter(|path| !path.is_empty())
            .collect()
    }
}

/// Shows the Windows file dialog, titled `title`, for the files that
/// `spec` matches, such as `*.*`, whose type is named `name`, and returns
/// the chosen file, or `None` when the person cancels. It runs its own
/// message loop, so the caller holds no borrow across it (DESIGN.md:
/// window rule 1).
pub(crate) fn pick_file(owner: Hwnd, title: &str, name: &str, spec: &str) -> Option<String> {
    let (name, spec) = (HSTRING::from(name), HSTRING::from(spec));
    let filter = [COMDLG_FILTERSPEC {
        pszName: PCWSTR(name.as_ptr()),
        pszSpec: PCWSTR(spec.as_ptr()),
    }];
    // SAFETY: every string outlives the dialog, which copies them; the
    // chosen path is copied, then freed as the call requires.
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(options | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM)
            .ok()?;
        dialog.SetFileTypes(&filter).ok()?;
        dialog.SetTitle(&HSTRING::from(title)).ok()?;
        let _modal = super::present::modal_loop();
        dialog.Show(Some(owner.0)).ok()?;
        let path = dialog
            .GetResult()
            .ok()?
            .GetDisplayName(SIGDN_FILESYSPATH)
            .ok()?;
        let text = path.to_string();
        CoTaskMemFree(Some(path.0.cast_const().cast()));
        text.ok()
    }
}

/// The label of the volume whose root is `root`, such as "Windows" for
/// `C:\`, or `None` when Windows cannot read it, as for an offline drive.
pub(crate) fn volume_label(root: &str, _worker: &super::work::OffThread) -> Option<String> {
    let mut label = [0u16; 261];
    // SAFETY: the root outlives the call, which writes at most the
    // buffer's length.
    unsafe {
        GetVolumeInformationW(
            &HSTRING::from(root),
            Some(&mut label),
            None,
            None,
            None,
            None,
        )
    }
    .ok()?;
    let length = label.iter().position(|&u| u == 0).unwrap_or(label.len());
    Some(String::from_utf16_lossy(&label[..length]))
}

/// An operating-system error as Rust's, so [`super::error_text`] gives
/// Windows' words for it.
pub(super) fn io_error(error: windows::core::Error) -> io::Error {
    let code = error.code().0 as u32;
    // An HRESULT made from a Win32 error code holds the code in its low word.
    if code & 0xffff_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((code & 0xffff) as i32)
    } else {
        io::Error::other(error.message())
    }
}

/// A registry call's result, where a missing key or value is no error,
/// because deleting one that is gone leaves what was asked.
fn registry(result: WIN32_ERROR) -> io::Result<()> {
    match result {
        ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
        error => Err(io::Error::from_raw_os_error(error.0 as i32)),
    }
}

/// The current user's key that starts programs at sign-in.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Where Task Manager and Windows Settings record that they turned a `Run`
/// entry off.
const STARTUP_APPROVED_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
/// The name of Gozler's values.
const GOZLER: &str = "gozler";
/// The keys of File Explorer's "Search with Gozler" on a folder and on a
/// folder's background, which earlier versions added.
const VERB_KEYS: [&str; 2] = [
    r"Software\Classes\Directory\shell\gozler",
    r"Software\Classes\Directory\Background\shell\gozler",
];

/// A value's name, where `None` is the key's default value.
fn value_name(name: Option<&str>) -> HSTRING {
    HSTRING::from(name.unwrap_or_default())
}

/// Writes the string `value` as the value `name` of the current user's key
/// `key`, which it creates when it is missing.
fn set_string(key: &str, name: Option<&str>, value: &str) -> io::Result<()> {
    set_string_in(HKEY_CURRENT_USER, key, name, value)
}

/// Writes the string `value` as the value `name` of the machine's key `key`,
/// which it creates when it is missing. Only an administrator can.
pub(crate) fn set_machine_string(key: &str, name: &str, value: &str) -> io::Result<()> {
    set_string_in(HKEY_LOCAL_MACHINE, key, Some(name), value)
}

fn set_string_in(root: HKEY, key: &str, name: Option<&str>, value: &str) -> io::Result<()> {
    let data: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: the names and the data outlive the call, and the length is
    // the data's, with its NUL, in bytes.
    registry(unsafe {
        RegSetKeyValueW(
            root,
            &HSTRING::from(key),
            &value_name(name),
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        )
    })
}

/// Reads the value `name` of the current user's key `key` into `data`, of
/// the type `kind`, and returns its length in bytes; with no `data`, it
/// only asks whether the value exists.
fn get_value(
    key: &str,
    name: Option<&str>,
    kind: REG_ROUTINE_FLAGS,
    data: Option<&mut [u8]>,
) -> Option<u32> {
    let mut length = data.as_ref().map_or(0, |d| d.len() as u32);
    let buffer = data.map(|d| d.as_mut_ptr().cast());
    // SAFETY: the names outlive the call, which writes at most `length`
    // bytes into the buffer.
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            &value_name(name),
            kind,
            None,
            buffer,
            buffer.map(|_| &raw mut length),
        )
    };
    (result == ERROR_SUCCESS).then_some(length)
}

fn delete_value(key: &str, name: &str) -> io::Result<()> {
    // SAFETY: the names outlive the call.
    registry(unsafe {
        RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(name))
    })
}

fn delete_key(key: &str) -> io::Result<()> {
    // SAFETY: the name outlives the call.
    registry(unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key)) })
}

pub(crate) fn remove_settings(worker: &super::work::OffThread) -> io::Result<()> {
    windows_shortcut::set_disabled(&[], worker)?;
    delete_value(RUN_KEY, GOZLER)?;
    delete_value(STARTUP_APPROVED_KEY, GOZLER)?;
    for key in VERB_KEYS {
        delete_key(key)?;
    }
    delete_key(r"Software\Gozler")
}

/// Whether Windows starts Gozler at sign-in: the `Run` value exists, and
/// Task Manager did not turn it off. An odd first byte of the
/// `StartupApproved` value is off (DESIGN.md: The General page).
pub(crate) fn starts_at_sign_in(_worker: &super::work::OffThread) -> bool {
    if get_value(RUN_KEY, Some(GOZLER), RRF_RT_REG_SZ, None).is_none() {
        return false;
    }
    let mut approved = [0u8; 12];
    match get_value(
        STARTUP_APPROVED_KEY,
        Some(GOZLER),
        RRF_RT_REG_BINARY,
        Some(&mut approved),
    ) {
        Some(length) if length > 0 => approved[0] % 2 == 0,
        Some(_) | None => true,
    }
}

/// Starts `program` at sign-in with `--sign-in`, or no longer does. Turning
/// it on also removes Task Manager's off mark, so the check box and Task
/// Manager agree.
pub(crate) fn set_start_at_sign_in(
    on: bool,
    program: &Path,
    _worker: &super::work::OffThread,
) -> io::Result<()> {
    if on {
        let command = format!("\"{}\" --sign-in", program.display());
        set_string(RUN_KEY, Some(GOZLER), &command)?;
    } else {
        delete_value(RUN_KEY, GOZLER)?;
    }
    delete_value(STARTUP_APPROVED_KEY, GOZLER)
}

/// Removes "Search with Gozler" from File Explorer's menus, which an
/// earlier version added, so no item that Gozler no longer serves stays
/// there (DESIGN.md: The Context menu page). Keys that are gone stay gone.
pub(crate) fn remove_search_with() {
    for key in VERB_KEYS {
        let _ = delete_key(key);
    }
}

/// A child program. Dropping it closes the handle,
/// not the program.
pub(crate) struct Process(HANDLE);

// SAFETY: any thread may wait on and close a process handle.
unsafe impl Send for Process {}

impl Process {
    /// Waits up to `timeout_ms` for the program to end, and gives its exit
    /// code; `None` while it still runs.
    pub(crate) fn wait(&self, timeout_ms: u32) -> io::Result<Option<u32>> {
        // SAFETY: the handle is open until `drop`.
        unsafe {
            match WaitForSingleObject(self.0, timeout_ms) {
                WAIT_OBJECT_0 => {
                    let mut code = 0;
                    GetExitCodeProcess(self.0, &mut code).map_err(io_error)?;
                    Ok(Some(code))
                }
                WAIT_TIMEOUT => Ok(None),
                _ => Err(io::Error::last_os_error()),
            }
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: this process owns the handle and closes it once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

impl Session {
    /// Waits for Windows' permission prompt on this worker's apartment. No
    /// owner HWND may attach its input or lifetime to the window's thread.
    pub(crate) fn start_as_administrator(
        &self,
        program: &str,
        parameters: &str,
    ) -> io::Result<Process> {
        self.ready()?;
        let process = execute(
            HWND::default(),
            w!("runas"),
            &HSTRING::from(program),
            &HSTRING::from(parameters),
            None,
            SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
            SW_HIDE,
        )?;
        if process.is_invalid() {
            return Err(io_error(windows::Win32::Foundation::E_FAIL.into()));
        }
        Ok(Process(process))
    }

    /// Starts `program` with `parameters` as administrator, with its window
    /// shown, once the person allows it in Windows' permission prompt.
    pub(crate) fn start_program_as_administrator(
        &self,
        program: &Path,
        parameters: &OsStr,
    ) -> io::Result<()> {
        self.ready()?;
        let program: Vec<_> = program.as_os_str().encode_wide().collect();
        let parameters: Vec<_> = parameters.encode_wide().collect();
        execute(
            HWND::default(),
            w!("runas"),
            &HSTRING::from_wide(&program),
            &HSTRING::from_wide(&parameters),
            None,
            SEE_MASK_NOASYNC,
            SW_SHOWNORMAL,
        )
        .map(|_| ())
    }

    /// A settings input file must remain until the accepted child exits,
    /// even if that takes longer than the window's lifetime.
    pub(crate) fn run_as_administrator_and_wait(
        &self,
        program: &str,
        parameters: &str,
    ) -> io::Result<u32> {
        self.start_as_administrator(program, parameters)?
            .wait(u32::MAX)?
            .ok_or_else(|| io_error(windows::Win32::Foundation::E_FAIL.into()))
    }
}

/// Whether `error` says that the person said no to Windows' permission
/// prompt.
pub(crate) fn refused(error: &io::Error) -> bool {
    error.raw_os_error() == Some(ERROR_CANCELLED.0 as i32)
}

/// Runs as the desktop user and waits for exit, so Setup can remove the
/// executable afterward; requires administrator rights and a running shell.
pub(crate) fn run_as_user(
    program: &Path,
    parameters: &OsStr,
    _worker: &super::work::OffThread,
) -> io::Result<u32> {
    gozler_winfs::enable_desktop_privileges()
        .map_err(|error| io::Error::from_raw_os_error(error.code as i32))?;
    let mut program: Vec<u16> = program.as_os_str().encode_wide().collect();
    let mut command: Vec<u16> = [u16::from(b'"')]
        .into_iter()
        .chain(program.iter().copied())
        .chain([u16::from(b'"'), u16::from(b' ')])
        .chain(parameters.encode_wide())
        .chain([0])
        .collect();
    program.push(0);
    // SAFETY: the shell HWND belongs to this session; each opened handle has
    // one owner, and the mutable command and startup data outlive creation.
    unsafe {
        let mut id = 0;
        GetWindowThreadProcessId(GetShellWindow(), Some(&mut id));
        let shell = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, id).map_err(io_error)?;
        let shell = OwnedHandle::from_raw_handle(shell.0);
        let mut token = HANDLE::default();
        OpenProcessToken(
            HANDLE(shell.as_raw_handle()),
            TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY,
            &mut token,
        )
        .map_err(io_error)?;
        let token = OwnedHandle::from_raw_handle(token.0);
        // Windows can refuse process creation with the shell's original token.
        let mut primary = HANDLE::default();
        DuplicateTokenEx(
            HANDLE(token.as_raw_handle()),
            TOKEN_ALL_ACCESS,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        )
        .map_err(io_error)?;
        let primary = OwnedHandle::from_raw_handle(primary.0);
        let startup = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut info = PROCESS_INFORMATION::default();
        CreateProcessWithTokenW(
            HANDLE(primary.as_raw_handle()),
            LOGON_WITH_PROFILE,
            PCWSTR(program.as_ptr()),
            Some(PWSTR(command.as_mut_ptr())),
            CREATE_NO_WINDOW,
            None,
            PCWSTR::null(),
            &startup,
            &mut info,
        )
        .map_err(io_error)?;
        let _ = CloseHandle(info.hThread);
        let process = Process(info.hProcess);
        Ok(process
            .wait(INFINITE)?
            .expect("an infinite wait cannot time out"))
    }
}

/// Starts `program` with `parameters` as the desktop's File Explorer does,
/// which for a program that runs as administrator means without
/// administrator rights: Windows gives a program the rights of the program
/// that starts it, and File Explorer runs with the person's filtered token.
/// File Explorer's desktop view starts it, through
/// `IShellDispatch2::ShellExecute`. Fails when File Explorer does not run.
pub(crate) fn start_as_desktop_user(
    program: &Path,
    parameters: &OsStr,
    _worker: &super::work::OffThread,
) -> io::Result<()> {
    // SAFETY: this worker thread initializes its own STA once.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
        .ok()
        .map_err(io_error)?;
    // SAFETY: COM runs on this thread, and the call releases every
    // interface before it returns.
    let started = unsafe { execute_from_desktop(program, parameters) };
    // SAFETY: matches the successful CoInitializeEx above.
    unsafe { CoUninitialize() };
    started.map_err(io_error)
}

/// [`start_as_desktop_user`]'s calls to File Explorer.
///
/// # Safety
///
/// COM is initialized as an STA on this thread.
unsafe fn execute_from_desktop(program: &Path, parameters: &OsStr) -> windows::core::Result<()> {
    // SAFETY: plain COM calls on this thread's apartment; each VARIANT is
    // empty or holds a BSTR that it owns until VariantClear frees it.
    unsafe {
        let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER)?;
        let empty = VARIANT::default();
        let mut hwnd = 0;
        let desktop =
            windows.FindWindowSW(&empty, &empty, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)?;
        let browser: IShellBrowser = desktop
            .cast::<IServiceProvider>()?
            .QueryService(&SID_STopLevelBrowser)?;
        let view: IDispatch = browser
            .QueryActiveShellView()?
            .GetItemObject(SVGIO_BACKGROUND)?;
        let shell: IShellDispatch2 = view.cast::<IShellFolderViewDual>()?.Application()?.cast()?;
        let wide = |text: &OsStr| BSTR::from_wide(&text.encode_wide().collect::<Vec<_>>());
        let mut arguments = VARIANT::default();
        let inner = &mut *arguments.Anonymous.Anonymous;
        inner.vt = VT_BSTR;
        inner.Anonymous.bstrVal = std::mem::ManuallyDrop::new(wide(parameters));
        let started = shell.ShellExecute(
            &wide(program.as_os_str()),
            &arguments,
            &empty,
            &empty,
            &empty,
        );
        let _ = VariantClear(&mut arguments);
        started
    }
}

/// `ShellExecuteExW` with `verb`, without Windows' own error messages,
/// because the caller says what failed. Gives the process handle that
/// `SEE_MASK_NOCLOSEPROCESS` in `mask` asks for.
fn execute(
    owner: HWND,
    verb: PCWSTR,
    program: &HSTRING,
    parameters: &HSTRING,
    directory: Option<&HSTRING>,
    mask: u32,
    show: SHOW_WINDOW_CMD,
) -> io::Result<HANDLE> {
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: mask | SEE_MASK_FLAG_NO_UI,
        hwnd: owner,
        lpVerb: verb,
        lpFile: PCWSTR(program.as_ptr()),
        lpParameters: if parameters.is_empty() {
            PCWSTR::null()
        } else {
            PCWSTR(parameters.as_ptr())
        },
        lpDirectory: directory.map_or(PCWSTR::null(), |folder| PCWSTR(folder.as_ptr())),
        nShow: show.0,
        ..Default::default()
    };
    // SAFETY: `info` is complete, with its size set, and its strings outlive
    // the call.
    unsafe { ShellExecuteExW(&mut info) }.map_err(io_error)?;
    Ok(info.hProcess)
}

/// `System.Photo.DateTaken`, File Explorer's Date taken.
const DATE_TAKEN: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0x14b8_1da1_0135_4d31_96d9_6cbf_c967_1a99),
    pid: 36867,
};

/// `System.Media.DateEncoded`, File Explorer's Media created.
const MEDIA_CREATED: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0x2e4b_640d_5019_46d8_8881_5541_4cc5_caa0),
    pid: 100,
};

/// The Date taken of each of `paths`, or its Media created for a video, as
/// File Explorer shows them, as a `FILETIME`; `None` for an entry that has
/// neither. Runs on a work-owner worker, in an apartment of its own, and
/// stops with what it read once `stopped` says so.
pub(crate) fn dates_taken(paths: &[Vec<u16>], stopped: impl Fn() -> bool) -> Vec<Option<i64>> {
    // SAFETY: balanced below when it succeeds; a thread already in another
    // apartment keeps it and can still make the calls.
    let entered = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let mut dates = Vec::with_capacity(paths.len());
    for path in paths {
        if stopped() {
            break;
        }
        let path: Vec<u16> = path.iter().copied().chain([0]).collect();
        // SAFETY: `path` is NUL-terminated and outlives the call; the item
        // lives in this apartment.
        let item: windows::core::Result<IShellItem2> =
            unsafe { SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None) };
        let time = item.and_then(|item| {
            // SAFETY: plain property reads of a live item.
            unsafe { item.GetFileTime(&DATE_TAKEN) }
                .or_else(|_| unsafe { item.GetFileTime(&MEDIA_CREATED) })
        });
        dates
            .push(time.ok().map(|time| {
                (i64::from(time.dwHighDateTime) << 32) | i64::from(time.dwLowDateTime)
            }));
    }
    if entered {
        // SAFETY: matches the successful CoInitializeEx above, after every
        // item was released.
        unsafe { CoUninitialize() };
    }
    dates
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The drop target copies what the find data says: a folder without
    /// the folder attribute would be copied as a file, and a folder's
    /// summed size would be taken for its own.
    #[test]
    fn find_data_names_a_folder_as_a_folder_and_a_file_with_its_row() {
        let row = |path: &str, is_folder| DragEntry {
            path: path.encode_utf16().collect(),
            is_folder,
            attributes: None,
            size: Some(0x1_0000_0002),
            modified: Some(0x0123_4567_89AB_CDEF),
        };
        let folder = find_data(&row(r"C:\docs", true));
        assert_eq!(folder.dwFileAttributes, FILE_ATTRIBUTE_DIRECTORY.0);
        assert_eq!((folder.nFileSizeHigh, folder.nFileSizeLow), (0, 0));
        let file = find_data(&row(r"C:\docs\Ünï.txt", false));
        assert_eq!(file.dwFileAttributes, FILE_ATTRIBUTE_NORMAL.0);
        assert_eq!((file.nFileSizeHigh, file.nFileSizeLow), (1, 2));
        assert_eq!(
            (
                file.ftLastWriteTime.dwHighDateTime,
                file.ftLastWriteTime.dwLowDateTime
            ),
            (0x0123_4567, 0x89AB_CDEF)
        );
        let name: Vec<u16> = "Ünï.txt\0".encode_utf16().collect();
        assert_eq!(file.cFileName[..name.len()], name[..]);
    }
}
