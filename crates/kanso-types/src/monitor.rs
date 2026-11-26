//! 服务器监控的一拍结果（host-monitor.md）。解析在 `kanso-core`。

/// 一个挂载点的用量。`size` 与 `used` 是字节。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskUse {
    /// 挂载点。
    pub mount: String,
    /// 总容量。
    pub size: u64,
    /// 已用。
    pub used: u64,
    /// `df` 给出的百分比，0–100。
    pub percent: u8,
}

/// 一块网卡自开机以来的收发字节。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetCounter {
    /// 接口名。
    pub iface: String,
    /// 接收字节。
    pub rx: u64,
    /// 发送字节。
    pub tx: u64,
}

/// 一拍采样。CPU 是这一拍内部约 1 秒的占用；算不出来时为 `None`。
#[derive(Clone, Debug, PartialEq)]
pub struct HostSample {
    /// 1 / 5 / 15 分钟负载。
    pub load: [f32; 3],
    /// CPU 占用百分比，0–100。
    pub cpu_percent: Option<u8>,
    /// 内存总量与可用量（字节）。
    pub mem_total: u64,
    /// 可用内存（字节），来自 MemAvailable。
    pub mem_available: u64,
    /// 交换区总量（字节）。
    pub swap_total: u64,
    /// 空闲交换区（字节）。
    pub swap_free: u64,
    /// 挂载点。
    pub disks: Vec<DiskUse>,
    /// 网卡计数。
    pub nets: Vec<NetCounter>,
}
