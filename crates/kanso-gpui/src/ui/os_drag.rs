//! 向资源管理器、桌面等其他应用发起系统文件拖放（OLE `DoDragDrop`）。
//!
//! GPUI 0.2.2 只实现了"接收"一侧（窗口注册了 `IDropTarget`），没有发起接口，所以这里自己实现
//! `IDataObject`（只提供 `CF_HDROP`）与 `IDropSource`，调用 `DoDragDrop`。GPUI 平台层在创建时已对主线程
//! 调过 `OleInitialize`，`DoDragDrop` 必须在那个线程上调用——本模块的函数都只在 UI 线程使用。
//!
//! - 只允许复制（`DROPEFFECT_COPY`）：接收方不会移动或删除源文件，本地文件与远端暂存副本都一样。
//! - 触发时机与"何时切换"的判断在 `kanso_gpui::app::os_drag`（有单元测试），这里只做系统调用。
//! - 落回本窗口时，GPUI 自己的 `IDropTarget` 会读到同一份 `CF_HDROP`，按外部文件拖入处理（终端插入路径）。
//!   Filer 的上传入口用 `app::os_drag::self_dragging()` 忽略自己拖出的文件。

// 实现 COM 接口、调用 OLE / 全局内存 API 都必须 unsafe；每处写明前提。
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::mem::ManuallyDrop;

use gpui::Window;
use kanso_gpui::app::os_drag::{
    ContinueDrag, OsOutcome, Pointer, SelfDragGuard, build_hdrop, continue_drag, is_outside,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{
    DATA_S_SAMEFORMATETC, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS,
    DV_E_FORMATETC, DV_E_TYMED, E_NOTIMPL, E_OUTOFMEMORY, GlobalFree, HGLOBAL, HWND, LPARAM,
    OLE_E_ADVISENOTSUPPORTED, POINT, RECT, S_OK, WPARAM,
};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::*;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::*;
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::Shell::SHCreateStdEnumFmtEtc;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetCursorPos, PostMessageW, WM_LBUTTONUP,
};
use windows::core::{BOOL, Error, HRESULT, implement};

/// 唯一提供的格式：`CF_HDROP`，全局内存，内容见 `build_hdrop`。
fn hdrop_format() -> FORMATETC {
    FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

/// 请求的格式是否是我们提供的那一种。
fn check_format(f: &FORMATETC) -> HRESULT {
    if f.cfFormat != CF_HDROP.0 || f.dwAspect != DVASPECT_CONTENT.0 || f.lindex != -1 {
        DV_E_FORMATETC
    } else if f.tymed & (TYMED_HGLOBAL.0 as u32) == 0 {
        DV_E_TYMED
    } else {
        S_OK
    }
}

/// 把字节复制进新分配的可移动全局内存（所有权随 `STGMEDIUM` 交给接收方，由它 `ReleaseStgMedium`）。
fn alloc_hglobal(bytes: &[u8]) -> windows::core::Result<HGLOBAL> {
    // SAFETY: `GlobalAlloc` 返回长度为 `bytes.len()` 的可移动内存；加锁成功后指针有效且不与 `bytes` 重叠，
    // 复制恰好 `bytes.len()` 字节；失败路径释放刚分配的句柄，不泄漏。
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
        let p = GlobalLock(h);
        if p.is_null() {
            let _ = GlobalFree(Some(h));
            return Err(Error::from(E_OUTOFMEMORY));
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), bytes.len());
        // 解锁在锁计数归零时按 Win32 约定可能返回"错误码 0"，不是失败。
        let _ = GlobalUnlock(h);
        Ok(h)
    }
}

/// 携带一组本机路径的数据对象。
#[implement(IDataObject)]
struct FileDataObject {
    /// 预先排布好的 `CF_HDROP` 内容。
    hdrop: Vec<u8>,
}

#[allow(non_snake_case)]
impl IDataObject_Impl for FileDataObject_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        // SAFETY: COM 约定 `pformatetcin` 指向调用方持有的有效 `FORMATETC`；为空时按无效参数处理。
        let Some(format) = (unsafe { pformatetcin.as_ref() }) else {
            return Err(Error::from(DV_E_FORMATETC));
        };
        let status = check_format(format);
        if status != S_OK {
            return Err(Error::from(status));
        }
        let hglobal = alloc_hglobal(&self.hdrop)?;
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: hglobal },
            pUnkForRelease: ManuallyDrop::new(None),
        })
    }

    fn GetDataHere(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *mut STGMEDIUM,
    ) -> windows::core::Result<()> {
        Err(Error::from(E_NOTIMPL))
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        // SAFETY: 同 `GetData`。
        match unsafe { pformatetc.as_ref() } {
            Some(format) => check_format(format),
            None => DV_E_FORMATETC,
        }
    }

    fn GetCanonicalFormatEtc(
        &self,
        _pformatectin: *const FORMATETC,
        pformatetcout: *mut FORMATETC,
    ) -> HRESULT {
        // SAFETY: 输出参数由调用方提供；非空时把 `ptd` 置空，表示与输入同一格式。
        if let Some(out) = unsafe { pformatetcout.as_mut() } {
            out.ptd = std::ptr::null_mut();
        }
        DATA_S_SAMEFORMATETC
    }

    fn SetData(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *const STGMEDIUM,
        _frelease: BOOL,
    ) -> windows::core::Result<()> {
        // 资源管理器会回写 "Performed DropEffect" 之类的信息；我们不关心，拒绝即可。
        Err(Error::from(E_NOTIMPL))
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> windows::core::Result<IEnumFORMATETC> {
        if dwdirection != DATADIR_GET.0 as u32 {
            return Err(Error::from(E_NOTIMPL));
        }
        let formats = [hdrop_format()];
        // SAFETY: 系统按数量复制 `formats` 里的结构；调用返回后不再引用该切片。
        unsafe { SHCreateStdEnumFmtEtc(&formats) }
    }

    fn DAdvise(
        &self,
        _pformatetc: *const FORMATETC,
        _advf: u32,
        _padvsink: windows::core::Ref<'_, IAdviseSink>,
    ) -> windows::core::Result<u32> {
        Err(Error::from(OLE_E_ADVISENOTSUPPORTED))
    }

    fn DUnadvise(&self, _dwconnection: u32) -> windows::core::Result<()> {
        Err(Error::from(OLE_E_ADVISENOTSUPPORTED))
    }

    fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
        Err(Error::from(OLE_E_ADVISENOTSUPPORTED))
    }
}

/// 拖动源：Esc 取消、松开左键落下，光标用系统默认样式。
#[implement(IDropSource)]
struct FileDropSource(());

#[allow(non_snake_case)]
impl IDropSource_Impl for FileDropSource_Impl {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        match continue_drag(fescapepressed.as_bool(), grfkeystate.0 & MK_LBUTTON.0 != 0) {
            ContinueDrag::Cancel => DRAGDROP_S_CANCEL,
            ContinueDrag::Drop => DRAGDROP_S_DROP,
            ContinueDrag::Continue => S_OK,
        }
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// 发起系统拖放并阻塞到结束（`DoDragDrop` 内部是模态消息循环，期间 GPUI 的窗口消息、
/// 重绘、定时器照常被派发）。调用时不能持有任何 GPUI 实体 / `App` 的借用。
pub(crate) fn run(paths: &[String]) -> OsOutcome {
    let _guard = SelfDragGuard::new();
    let data: IDataObject = FileDataObject {
        hdrop: build_hdrop(paths),
    }
    .into();
    let source: IDropSource = FileDropSource(()).into();
    let mut effect = DROPEFFECT_NONE;
    // SAFETY: 两个接口对象在调用期间由本函数持有；`effect` 是有效的输出位置；当前线程是已 `OleInitialize` 的 UI 线程。
    let hr = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
    if hr == DRAGDROP_S_DROP {
        if effect == DROPEFFECT_NONE {
            OsOutcome::Cancelled
        } else {
            OsOutcome::Dropped
        }
    } else if hr == DRAGDROP_S_CANCEL {
        OsOutcome::Cancelled
    } else {
        OsOutcome::Failed(Error::from(hr).message())
    }
}

/// 当前窗口的原生句柄（数值）。
pub(crate) fn window_hwnd(window: &Window) -> Option<isize> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

fn to_hwnd(hwnd: isize) -> HWND {
    HWND(hwnd as *mut c_void)
}

/// 采样鼠标：是否在窗口客户区外、左键是否按着。任一系统调用失败返回 `None`（调用方按"不切换"处理）。
pub(crate) fn pointer(window: &Window) -> Option<Pointer> {
    let hwnd = to_hwnd(window_hwnd(window)?);
    let mut pt = POINT::default();
    let mut rect = RECT::default();
    // SAFETY: `pt` / `rect` 是有效的输出位置；`hwnd` 来自当前窗口，调用期间窗口存活（本函数在 UI 线程、窗口更新中调用）。
    unsafe {
        GetCursorPos(&mut pt).ok()?;
        GetClientRect(hwnd, &mut rect).ok()?;
        if !ScreenToClient(hwnd, &mut pt).as_bool() {
            return None;
        }
        // 高位表示"此刻按着"。
        let left_down = (GetAsyncKeyState(i32::from(VK_LBUTTON.0)) as u16) & 0x8000 != 0;
        Some(Pointer {
            outside: is_outside(pt.x, pt.y, rect.right - rect.left, rect.bottom - rect.top),
            left_down,
        })
    }
}

/// 系统拖放结束后，给窗口补一个左键松开消息。GPUI 的内部拖动在启动系统拖放前已被终止，
/// 而真正的松开事件被 OLE 的模态循环吃掉了，窗口收不到；补发它能让按下态、鼠标捕获等恢复。
pub(crate) fn release_ui(hwnd: isize) {
    let hwnd = to_hwnd(hwnd);
    let mut pt = POINT::default();
    // SAFETY: 输出位置有效；句柄只用于投递消息，窗口已销毁时投递失败，忽略即可。
    unsafe {
        if GetCursorPos(&mut pt).is_err() || !ScreenToClient(hwnd, &mut pt).as_bool() {
            pt = POINT::default();
        }
        // 客户区坐标放进 lparam 的低 / 高 16 位（有符号值按 16 位截断，与 GPUI 的 signed_loword 对应）。
        let packed = (u32::from(pt.y as i16 as u16) << 16) | u32::from(pt.x as i16 as u16);
        let _ = PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), LPARAM(packed as isize));
    }
}
