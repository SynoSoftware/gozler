//! Bounds accessibility tree walks to the native list's visible items.
//! Windows' default proxy enumerates offscreen cells too, monopolizing the
//! window thread in UI Automation BulkFetch (issue #158). Its second list
//! proxy supplies visible-item navigation and the same native control patterns.

use super::ListView;
use crate::win::Hwnd;
use std::cell::{Cell, OnceCell};
use std::rc::Rc;
use windows::Win32::Foundation::{E_NOINTERFACE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, SAFEARRAY};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IRawElementProviderFragment, IRawElementProviderFragment_Impl,
    IRawElementProviderFragmentRoot, IRawElementProviderFragmentRoot_Impl,
    IRawElementProviderSimple, IRawElementProviderSimple_Impl, IUIAutomation,
    IUIAutomationProxyFactory, NavigateDirection, ProviderOptions,
    ProviderOptions_ServerSideProvider, UIA_E_ELEMENTNOTAVAILABLE, UIA_PATTERN_ID, UIA_PROPERTY_ID,
    UiaHostProviderFromHwnd, UiaRect, UiaReturnRawElementProvider, UiaRootObjectId,
};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{OBJID_CLIENT, WM_GETOBJECT, WM_NCDESTROY};
use windows::core::{Error, HRESULT, IUnknown, Interface, Result, implement};

thread_local! {
    static FACTORY: OnceCell<IUIAutomationProxyFactory> = const { OnceCell::new() };
}

fn factory() -> Result<IUIAutomationProxyFactory> {
    FACTORY.with(|cached| {
        if let Some(factory) = cached.get() {
            return Ok(factory.clone());
        }
        // SAFETY: the window thread initializes COM before making its controls.
        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
        // SAFETY: these methods return owned COM interfaces and strings.
        let factory = unsafe {
            let mapping = automation.ProxyFactoryMapping()?;
            let mut first = false;
            let mut found = None;
            for index in 0..mapping.Count()? {
                let entry = mapping.GetEntry(index)?;
                if entry.ClassName()? == "SysListView32" {
                    if first {
                        found = Some(entry.ProxyFactory()?);
                        break;
                    }
                    first = true;
                }
            }
            found.ok_or_else(|| Error::from_hresult(E_NOINTERFACE))?
        };
        let _ = cached.set(factory.clone());
        Ok(factory)
    })
}

struct State {
    hwnd: Hwnd,
    proxy: IRawElementProviderSimple,
    alive: Cell<bool>,
}

#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IRawElementProviderFragmentRoot,
    Agile = false
)]
struct Provider {
    state: Rc<State>,
    item: Option<IRawElementProviderSimple>,
}

impl Provider {
    fn root(state: &Rc<State>) -> IRawElementProviderSimple {
        Self {
            state: Rc::clone(state),
            item: None,
        }
        .into()
    }
}

impl Provider_Impl {
    fn proxy(&self) -> Result<&IRawElementProviderSimple> {
        if !self.state.alive.get() {
            return Err(Error::from_hresult(HRESULT(
                UIA_E_ELEMENTNOTAVAILABLE as i32,
            )));
        }
        Ok(self.item.as_ref().unwrap_or(&self.state.proxy))
    }

    fn wrap(&self, fragment: IRawElementProviderFragment) -> Result<IRawElementProviderFragment> {
        let proxy: IRawElementProviderSimple = fragment.cast()?;
        let item = (proxy != self.state.proxy).then_some(proxy);
        let provider: IRawElementProviderSimple = Provider {
            state: Rc::clone(&self.state),
            item,
        }
        .into();
        provider.cast()
    }
}

impl IRawElementProviderSimple_Impl for Provider_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    fn GetPatternProvider(&self, pattern: UIA_PATTERN_ID) -> Result<IUnknown> {
        // SAFETY: the STA provider and its native proxy run on the control's thread.
        unsafe { self.proxy()?.GetPatternProvider(pattern) }
    }

    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> Result<VARIANT> {
        // SAFETY: the live proxy owns the returned value.
        unsafe { self.proxy()?.GetPropertyValue(property) }
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        self.proxy()?;
        if self.item.is_some() {
            return Err(Error::empty());
        }
        // SAFETY: only the fragment root has a host window.
        unsafe { UiaHostProviderFromHwnd(self.state.hwnd.0) }
    }
}

impl IRawElementProviderFragment_Impl for Provider_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        let fragment: IRawElementProviderFragment = self.proxy()?.cast()?;
        // SAFETY: the native visible-item proxy defines bounded tree navigation.
        self.wrap(unsafe { fragment.Navigate(direction)? })
    }

    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        let fragment: IRawElementProviderFragment = self.proxy()?.cast()?;
        // SAFETY: ownership of the native runtime-ID array passes to UI Automation.
        unsafe { fragment.GetRuntimeId() }
    }

    fn BoundingRectangle(&self) -> Result<UiaRect> {
        let fragment: IRawElementProviderFragment = self.proxy()?.cast()?;
        // SAFETY: the live proxy reads the control's current bounds.
        unsafe { fragment.BoundingRectangle() }
    }

    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        self.proxy()?;
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> Result<()> {
        let fragment: IRawElementProviderFragment = self.proxy()?.cast()?;
        self.state.hwnd.focus();
        crate::win::present::mark_after(self.state.hwnd, || {
            // SAFETY: the native proxy keeps the control's item-focus behavior.
            unsafe { fragment.SetFocus() }
        })
    }

    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        self.proxy()?;
        Provider::root(&self.state).cast()
    }
}

impl IRawElementProviderFragmentRoot_Impl for Provider_Impl {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        self.proxy()?;
        let root: IRawElementProviderFragmentRoot = self.state.proxy.cast()?;
        // SAFETY: the native root hit-tests the live list in screen coordinates.
        self.wrap(unsafe { root.ElementProviderFromPoint(x, y)? })
    }

    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        self.proxy()?;
        let root: IRawElementProviderFragmentRoot = self.state.proxy.cast()?;
        // SAFETY: the native root identifies the control's focused item.
        self.wrap(unsafe { root.GetFocus()? })
    }
}

pub(super) fn attach(list: ListView) -> Result<()> {
    // SAFETY: this thread owns the list and supplies standard MSAA object IDs.
    let proxy = unsafe { factory()?.CreateProvider(list.hwnd().0, OBJID_CLIENT.0, 0)? };
    let state = Box::new(Rc::new(State {
        hwnd: list.hwnd(),
        proxy,
        alive: Cell::new(true),
    }));
    let data = Box::into_raw(state);
    // SAFETY: the subclass owns data until WM_NCDESTROY, on this same thread.
    if !unsafe { SetWindowSubclass(list.hwnd().0, Some(subclass), 1, data as usize) }.as_bool() {
        // SAFETY: installation failed, so no callback owns this allocation.
        drop(unsafe { Box::from_raw(data) });
        return Err(Error::from_thread());
    }
    Ok(())
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    // SAFETY: attach installed this pointer, which WM_NCDESTROY releases once.
    let state = unsafe { &*(data as *const Rc<State>) };
    if message == WM_GETOBJECT && lparam.0 as i32 == UiaRootObjectId {
        let provider = Provider::root(state);
        // SAFETY: the owned provider outlives the synchronous return; UIA retains it.
        return unsafe { UiaReturnRawElementProvider(hwnd, wparam, lparam, &provider) };
    }
    if message == WM_NCDESTROY {
        state.alive.set(false);
        // SAFETY: this is the subclass's final message; no control access follows.
        unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(subclass), id);
            drop(Box::from_raw(data as *mut Rc<State>));
            UiaReturnRawElementProvider(hwnd, WPARAM(0), LPARAM(0), None);
        }
    }
    // SAFETY: unhandled messages retain the native control's behavior.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::layout::{Px, Rect};
    use crate::win::controls::ListColumn;
    use crate::win::notify::ListNotify;
    use crate::win::window::{self, Background, Message, WindowProc};
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Child, Command, Stdio};
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
    use windows::Win32::UI::Accessibility::{
        CUIAutomation8, IUIAutomation2, IUIAutomationSelectionPattern, TreeScope_Subtree,
        UIA_ControlTypePropertyId, UIA_IsOffscreenPropertyId, UIA_NamePropertyId,
        UIA_SelectionPatternId,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MSG, SW_SHOWNOACTIVATE, ShowWindow, TranslateMessage,
    };

    struct Fixture {
        list: Cell<Option<ListView>>,
    }

    impl WindowProc for Fixture {
        fn message(&self, _: Hwnd, message: Message<'_>) -> Option<LRESULT> {
            if let Message::Notify(notification) = message
                && let Some(ListNotify::GetDispInfo(mut info)) =
                    self.list.get()?.notification(notification)
            {
                info.set_text(&format!("row {}", info.row()));
            }
            None
        }
    }

    struct FixtureProcess(Child);

    impl Drop for FixtureProcess {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn fixture() {
        crate::win::shell::initialize_com();
        crate::win::controls::initialize();
        let class = "gozler.test.accessibility";
        window::register_class::<Fixture>(class, 0, Background::Window).unwrap();
        let fixture = Rc::new(Fixture {
            list: Cell::new(None),
        });
        let hwnd = window::create_window(class, Rc::clone(&fixture), None).unwrap();
        let list = ListView::create(hwnd, 100).unwrap();
        fixture.list.set(Some(list));
        list.hwnd().set_rect(Rect {
            width: Px(500),
            height: Px(300),
            ..Rect::default()
        });
        for (index, header) in ["Name", "Path"].into_iter().enumerate() {
            list.insert_column(
                index,
                &ListColumn {
                    header,
                    width: Px(200),
                    right_aligned: false,
                },
            );
        }
        list.set_count(3_000_000);
        list.select_only(Some(2_999_999));
        assert_eq!(list.visible_rows().start, 0);
        // SAFETY: only this test's window is shown, without taking keyboard focus.
        let _ = unsafe { ShowWindow(hwnd.0, SW_SHOWNOACTIVATE) };
        println!("fixture:{}", list.hwnd().0.0 as usize);
        std::io::stdout().flush().unwrap();
        let mut message = MSG::default();
        // SAFETY: the fixture owns and dispatches its thread's native messages.
        unsafe {
            while GetMessageW(&mut message, None, 0, 0).0 > 0 {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    #[test]
    #[ignore = "shows a window; run with build.ps1 -Timing bulk_tree_walk"]
    fn bulk_tree_walk_does_not_enumerate_millions_of_offscreen_rows() {
        if std::env::var_os("GOZLER_ACCESSIBILITY_FIXTURE").is_some() {
            fixture();
            return;
        }
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "win::controls::list_view::accessibility::tests::bulk_tree_walk_does_not_enumerate_millions_of_offscreen_rows", "--ignored", "--nocapture"])
            .env("GOZLER_ACCESSIBILITY_FIXTURE", "1")
            .stdout(Stdio::piped())
            .spawn().unwrap();
        let mut fixture = FixtureProcess(child);
        let output = fixture.0.stdout.take().unwrap();
        let handle = BufReader::new(output)
            .lines()
            .map(|line| line.unwrap())
            .find_map(|line| {
                line.split_once("fixture:")
                    .map(|(_, handle)| handle.parse::<usize>().unwrap())
            })
            .expect("the fixture prints its list handle before running messages");
        // SAFETY: this client thread owns no windows; UIA calls run in its MTA.
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap();
            let automation: IUIAutomation2 =
                CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER).unwrap();
            automation.SetConnectionTimeout(5_000).unwrap();
            automation.SetTransactionTimeout(5_000).unwrap();
            let cache = automation.CreateCacheRequest().unwrap();
            cache.SetTreeScope(TreeScope_Subtree).unwrap();
            for property in [
                UIA_NamePropertyId,
                UIA_ControlTypePropertyId,
                UIA_IsOffscreenPropertyId,
            ] {
                cache.AddProperty(property).unwrap();
            }
            let list = automation
                .ElementFromHandleBuildCache(HWND(handle as *mut _), &cache)
                .unwrap();
            let children = list.GetCachedChildren().unwrap().Length().unwrap();
            assert!(
                (1..100).contains(&children),
                "tree exposed {children} children"
            );
            let selection: IUIAutomationSelectionPattern =
                list.GetCurrentPatternAs(UIA_SelectionPatternId).unwrap();
            let selected = selection.GetCurrentSelection().unwrap();
            assert_eq!(selected.Length().unwrap(), 1);
            let item = selected.GetElement(0).unwrap();
            assert!(item.CurrentIsOffscreen().unwrap().as_bool());
            assert_eq!(item.CurrentName().unwrap(), "row 2999999");
        }
    }
}
