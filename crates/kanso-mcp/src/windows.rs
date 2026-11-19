//! Windows 端口与身份校验。通道是本机字节型命名管道，仅当前用户的 SID 有权限。

#![allow(unsafe_code)]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_SEM_TIMEOUT, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// 字节型命名管道没有半关闭操作。转发程序以 NUL 表示 stdin 结束，服务端仍可发送最后的回复。
/// 合法的 JSON 行里不会出现未转义的 NUL。
pub const PIPE_EOF: u8 = 0;

/// 两端共用的本机命名管道。App 运行期间保留 presence，只有开启服务时才监听 socket。
pub struct McpPaths {
    /// 由 App 在整个运行期独占的探测管道。
    pub presence: PathBuf,
    /// 启用 MCP 后监听的 stdio 管道。
    pub socket: PathBuf,
}

impl McpPaths {
    /// 按应用 ID 和当前用户 SID 算出两端使用的名字。
    pub fn for_bundle(bundle_id: &str) -> io::Result<Self> {
        Self::with_scope(bundle_id)
    }

    /// 单独的测试命名空间，不创建目录或访问用户的文件。
    pub fn in_dir(dir: &Path) -> io::Result<Self> {
        let mut hash = DefaultHasher::new();
        dir.hash(&mut hash);
        Self::with_scope(&format!("test-{:016x}", hash.finish()))
    }

    fn with_scope(scope: &str) -> io::Result<Self> {
        let user = current_sid()?;
        let prefix = format!(r"\\.\pipe\kanso-mcp-{scope}-{user}");
        Ok(Self {
            presence: PathBuf::from(format!("{prefix}-app")),
            socket: PathBuf::from(format!("{prefix}-stdio")),
        })
    }
}

/// App 持有这个管道实例直到退出。first 模式由内核原子地检查名称是否已被占用。
pub fn take_presence(name: &Path) -> io::Result<Option<OwnedHandle>> {
    match create_pipe(name, true) {
        Ok(handle) => Ok(Some(handle)),
        Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => Ok(None),
        Err(e) => Err(e),
    }
}

/// 探测 presence 不连接管道，也不抢占 App 的实例。
pub fn app_running(name: &Path) -> bool {
    match wait_for_pipe(name, 1) {
        Ok(()) => true,
        Err(e) => e.raw_os_error() == Some(ERROR_SEM_TIMEOUT as i32),
    }
}

/// 正在创建下一服务实例时，等待当前监听地址有空闲的管道实例。
pub fn wait_for_pipe(name: &Path, timeout_ms: u32) -> io::Result<()> {
    let name = wide_path(name);
    // SAFETY: name 是以 NUL 结尾的 UTF-16，调用期间仍存活。
    if unsafe { WaitNamedPipeW(name.as_ptr(), timeout_ms) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// 创建当前用户独享的本机管道，`first` 由内核保证是首个实例。
pub fn create_pipe(name: &Path, first: bool) -> io::Result<OwnedHandle> {
    let name = wide_path(name);
    // 仅当前用户，包括由同一用户以不同权限运行的进程。不开匿名或 Everyone 权限。
    let user = current_sid()?;
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;{user})")
        .encode_utf16()
        .chain([0])
        .collect();
    let mut descriptor = null_mut();
    // SAFETY: UTF-16 以 NUL 结尾，descriptor 是有效输出指针。
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let attrs = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let mode = PIPE_ACCESS_DUPLEX
        | FILE_FLAG_OVERLAPPED
        | if first {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
    // SAFETY: name 以 NUL 结尾，attrs 与其描述符在调用期间有效。API 会复制描述符。
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            mode,
            PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            8192,
            8192,
            0,
            &attrs,
        )
    };
    let result = if handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: CreateNamedPipeW 成功，句柄由当前调用方独占。
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    };
    // SAFETY: descriptor 由 ConvertStringSecurityDescriptorToSecurityDescriptorW 分配，尚未释放。
    unsafe { LocalFree(descriptor) };
    result
}

/// App 用客户端进程的用户 SID 校验连接来源，作为管道 DACL 之外的检查。
pub fn client_pid(pipe: &impl AsRawHandle) -> io::Result<u32> {
    let mut pid = 0;
    // SAFETY: pipe 对应有效的已连接管道句柄，pid 是可写的 u32。
    if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    require_same_user(pid)?;
    Ok(pid)
}

/// 当前所有进程的 pid -> 父 pid 表（`CreateToolhelp32Snapshot` 快照），供 App 沿祖先链识别客户端所在标签。
///
/// 只能由 App 在需要判定时现取：Windows 的父 pid 是创建时记下的，父进程退出后 pid 可能被复用，
/// 所以表不能长期缓存。快照失败（权限、资源不足）返回错误，调用方按"识别不到"处理。
pub fn process_parents() -> io::Result<std::collections::HashMap<u32, u32>> {
    // SAFETY: 只创建只读的进程快照，参数是常量与 0（所有进程）。
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: 快照句柄由本函数独占，交给 OwnedHandle 保证任何返回路径都会关闭。
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    // SAFETY: PROCESSENTRY32W 是纯数据结构体，全零是合法初始值。
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    // 必须先填 dwSize，否则 Process32FirstW 会失败。
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut parents = std::collections::HashMap::new();
    // SAFETY: 句柄有效，entry 指向可写的、dwSize 已设置的结构体。
    let mut more = unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) } != 0;
    while more {
        parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
        // SAFETY: 同上。
        more = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) } != 0;
    }
    Ok(parents)
}

/// 转发程序先确认管道属于同一用户，再发送令牌。
pub fn require_same_user_server(pipe: &impl AsRawHandle) -> io::Result<()> {
    let mut pid = 0;
    // SAFETY: pipe 对应有效的已连接管道句柄，pid 是可写的 u32。
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    require_same_user(pid)
}

fn require_same_user(pid: u32) -> io::Result<()> {
    // SAFETY: OpenProcess 只读取 PID，不继承返回的句柄。
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcess 成功，进程句柄由当前调用方独占。
    let process = unsafe { OwnedHandle::from_raw_handle(handle) };
    if sid_of_process(process.as_raw_handle())? == current_sid()? {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "mcp peer has a different user SID",
        ))
    }
}

fn current_sid() -> io::Result<String> {
    // SAFETY: GetCurrentProcess 无参数，返回当前进程的伪句柄，不应关闭。
    sid_of_process(unsafe { GetCurrentProcess() })
}

fn sid_of_process(process: HANDLE) -> io::Result<String> {
    let mut token = null_mut();
    // SAFETY: process 是有效的进程句柄，token 是可写的句柄变量。
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcessToken 成功返回，句柄由当前调用方独占。
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut size = 0;
    // SAFETY: 长度为零、缓冲区为空，查询写入所需大小。
    unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut size) };
    if size == 0 {
        return Err(io::Error::last_os_error());
    }
    // TOKEN_USER 包含指针，需要机器字对齐。返回的 SID 指针始终在缓冲区存活期间读取。
    let mut data = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
    // SAFETY: data 是足量、对齐的可写内存，size 是 API 返回的实际所需大小。
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            data.as_mut_ptr().cast(),
            size,
            &mut size,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: TokenUser 调用成功，data 以 TOKEN_USER 开头，内含有效的 SID 指针。
    let sid = unsafe { (*data.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut text = null_mut();
    // SAFETY: sid 指向存活的 data，text 是有效输出指针。
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: 成功转换的 SID 字符串由 API 分配且以 NUL 结尾。
    let len = unsafe { (0..).take_while(|i| *text.add(*i) != 0).count() };
    // SAFETY: 上面已测量分配内的长度，在释放内存之前完成复制。
    let value = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
    // SAFETY: text 由 ConvertSidToStringSidW 分配，尚未释放。
    unsafe { LocalFree(text.cast()) };
    Ok(value)
}

fn wide_path(name: &Path) -> Vec<u16> {
    // 管道名由本模块构造。
    debug_assert!(!name.as_os_str().encode_wide().any(|c| c == 0));
    name.as_os_str().encode_wide().chain([0]).collect()
}
