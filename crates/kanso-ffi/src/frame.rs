//! 帧快照的 C ABI 导出。
//!
//! 布局由 `kanso-frame` 定义，C 侧见 `include/kanso_frame.h`。本模块目前只导出 mock 帧的
//! 创建 / 读取 / 销毁；按会话取帧的 acquire / release 接口要等 core API 定下后再加。
//!
//! 所有导出函数都用 `catch_unwind` 包住：panic 穿过 `extern "C"` 边界是未定义行为，
//! 所以 panic 时返回 NULL 或什么也不做。

use kanso_frame::{Frame, FrameHeader, mock};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// 对 C 不透明的帧句柄，内部就是一个 `kanso_frame::Frame`。
pub struct KansoFrame(Frame);

/// 创建确定性的 mock 帧，用 `kanso_frame_destroy` 释放。失败（panic）返回 NULL。
#[unsafe(no_mangle)]
pub extern "C" fn kanso_mock_frame_create(cols: u16, rows: u16, seq: u64) -> *mut KansoFrame {
    catch_unwind(|| Box::into_raw(Box::new(KansoFrame(mock::mock_frame(cols, rows, seq)))))
        .unwrap_or(core::ptr::null_mut())
}

/// 创建"满屏彩色文字"的 mock 帧（渲染器性能测试用），用 `kanso_frame_destroy` 释放。
/// 失败（panic）返回 NULL。
#[unsafe(no_mangle)]
pub extern "C" fn kanso_mock_frame_create_dense(cols: u16, rows: u16, seq: u64) -> *mut KansoFrame {
    catch_unwind(|| {
        Box::into_raw(Box::new(KansoFrame(mock::mock_frame_dense(
            cols, rows, seq,
        ))))
    })
    .unwrap_or(core::ptr::null_mut())
}

/// 返回帧缓冲区起点（即帧头）。指针在 `kanso_frame_destroy` 前有效，期间内容不会被修改。
/// `frame` 为 NULL 返回 NULL。
///
/// # Safety
///
/// `frame` 必须是 NULL，或由本库创建且尚未 destroy 的有效句柄。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kanso_frame_header(frame: *const KansoFrame) -> *const FrameHeader {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: 调用方保证 `frame` 为 NULL 或有效句柄；`as_ref` 先处理了 NULL。
        match unsafe { frame.as_ref() } {
            // 缓冲区起点 8 字节对齐且以 FrameHeader 开头，指针转换成立。
            Some(f) => f.0.as_ptr().cast::<FrameHeader>(),
            None => core::ptr::null(),
        }
    }))
    .unwrap_or(core::ptr::null())
}

/// 销毁帧。传 NULL 无操作；同一个句柄不能销毁两次。
///
/// # Safety
///
/// `frame` 必须是 NULL，或由 `kanso_mock_frame_create` 返回且尚未 destroy 的句柄；
/// destroy 之后不得再通过 `kanso_frame_header` 得到的指针访问。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kanso_frame_destroy(frame: *mut KansoFrame) {
    if frame.is_null() {
        return;
    }
    // 即使 drop 过程 panic 也不能越过边界，结果无需处理。
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: 非 NULL 且由 `Box::into_raw` 创建，调用方保证只销毁一次。
        drop(unsafe { Box::from_raw(frame) });
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dense_mock_has_no_blank_cells() {
        let f = kanso_mock_frame_create_dense(200, 60, 3);
        assert!(!f.is_null());
        // SAFETY: `f` 刚创建且未销毁。
        let h = unsafe { kanso_frame_header(f) };
        assert!(!h.is_null());
        // SAFETY: `h` 指向存活帧的帧头。
        unsafe {
            assert_eq!(((*h).cols, (*h).rows), (200, 60));
            kanso_frame_destroy(f);
        }
    }

    #[test]
    fn create_header_destroy() {
        let f = kanso_mock_frame_create(80, 24, 1);
        assert!(!f.is_null());
        // SAFETY: `f` 刚创建且未销毁。
        let h = unsafe { kanso_frame_header(f) };
        assert!(!h.is_null());
        // SAFETY: `h` 指向存活帧的帧头。
        assert_eq!(unsafe { (*h).magic }, kanso_frame::FRAME_MAGIC);
        // SAFETY: 句柄有效且只销毁一次；NULL 为空操作。
        unsafe {
            kanso_frame_destroy(f);
            kanso_frame_destroy(core::ptr::null_mut());
        }
        // SAFETY: NULL 合法。
        assert!(unsafe { kanso_frame_header(core::ptr::null()) }.is_null());
    }
}
