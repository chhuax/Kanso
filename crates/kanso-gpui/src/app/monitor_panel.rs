//! 服务器监控面板的纯逻辑（docs/spec/host-monitor.md）：
//! 目标会话判定、相邻两拍网卡速率计算、字节 / 速率格式化与代次状态机。
//!
//! 不依赖 GPUI 或窗口系统，可在全平台直接单元测试。

use std::collections::HashMap;
use std::time::Duration;

use kanso_types::{DiskUse, HostSample, SessionId};

use crate::app::tab::{TabConnection, TabItem, TabKind};

/// 上一拍结束后等待多久再采下一拍（host-monitor.md §2：约 2 秒）。
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// 活动标签对应的可监控目标；本地 shell、尚未连上、已断开、无标签均为 `None`。
pub fn target_for(item: Option<&TabItem>) -> Option<SessionId> {
    let item = item?;
    if item.kind == TabKind::Local {
        return None;
    }
    if !matches!(item.connection, TabConnection::Connected) {
        return None;
    }
    Some(item.id)
}

/// 一块网卡的展示数据：累计字节 + 第二拍起可用的实时速率（B/s）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetDisplay {
    /// 接口名。
    pub iface: String,
    /// 自开机累计接收字节。
    pub rx: u64,
    /// 自开机累计发送字节。
    pub tx: u64,
    /// 接收速率（字节/秒）；第一拍或计数倒退时为 `None`。
    pub rx_per_sec: Option<u64>,
    /// 发送速率（字节/秒）；第一拍或计数倒退时为 `None`。
    pub tx_per_sec: Option<u64>,
}

/// 渲染用的一拍快照。
#[derive(Clone, Debug, PartialEq)]
pub struct MonitorSnapshot {
    /// 1 / 5 / 15 分钟负载。
    pub load: [f32; 3],
    /// CPU 占用百分比（0–100）。
    pub cpu_percent: Option<u8>,
    /// 内存总量（字节）。
    pub mem_total: u64,
    /// 已用内存（字节）。
    pub mem_used: u64,
    /// 交换区总量（字节）。
    pub swap_total: u64,
    /// 已用交换区（字节）。
    pub swap_used: u64,
    /// 挂载点列表。
    pub disks: Vec<DiskUse>,
    /// 网卡列表（含速率）。
    pub nets: Vec<NetDisplay>,
}

/// 监控面板纯状态机（宿主负责驱动异步采样与定时）。
#[derive(Debug, Default)]
pub struct MonitorModel {
    target: Option<SessionId>,
    generation: u64,
    snapshot: Option<MonitorSnapshot>,
    error: Option<String>,
    sampling: bool,
    previous_nets: Option<HashMap<String, (u64, u64)>>,
    previous_at_ms: Option<u64>,
}

impl MonitorModel {
    /// 当前可采样的会话；`None` 表示空状态。
    pub fn target(&self) -> Option<SessionId> {
        self.target
    }

    /// 当前请求代次；切换会话或停止采样时递增，用于丢弃过时响应。
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// 最近一拍有效快照。
    pub fn snapshot(&self) -> Option<&MonitorSnapshot> {
        self.snapshot.as_ref()
    }

    /// 最近一次采样失败原因（就地展示，不关面板）。
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// 首次采样尚未回来。
    pub fn is_sampling(&self) -> bool {
        self.sampling
    }

    /// 跟随活动标签。目标改变时返回 `true`（清空旧主机数据并递增代次）。
    pub fn set_target(&mut self, next: Option<SessionId>) -> bool {
        if self.target == next {
            return false;
        }
        self.generation = self.generation.wrapping_add(1);
        self.target = next;
        self.snapshot = None;
        self.error = None;
        self.sampling = false;
        self.previous_nets = None;
        self.previous_at_ms = None;
        true
    }

    /// 面板隐藏或收起时递增代次，作废在途请求。
    pub fn pause(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.sampling = false;
    }

    /// 标记开始新一轮采样，返回本轮代次。
    pub fn begin_sample(&mut self) -> Option<(SessionId, u64)> {
        let target = self.target?;
        if self.snapshot.is_none() {
            self.sampling = true;
        }
        Some((target, self.generation))
    }

    /// 写入采样成功结果；若会话或代次已过时则忽略并返回 `false`。
    pub fn apply_sample(
        &mut self,
        session: SessionId,
        generation: u64,
        raw: HostSample,
        at_ms: u64,
    ) -> bool {
        if self.target != Some(session) || self.generation != generation {
            return false;
        }
        let dt_secs = self
            .previous_at_ms
            .and_then(|prev| at_ms.checked_sub(prev))
            .filter(|&ms| ms >= 50)
            .map(|ms| ms as f64 / 1000.0);
        let nets = raw
            .nets
            .iter()
            .map(|net| {
                let (rx_per_sec, tx_per_sec) = match (
                    dt_secs,
                    self.previous_nets
                        .as_ref()
                        .and_then(|map| map.get(&net.iface)),
                ) {
                    (Some(dt), Some(&(prx, ptx))) => (
                        net.rx.checked_sub(prx).map(|d| (d as f64 / dt) as u64),
                        net.tx.checked_sub(ptx).map(|d| (d as f64 / dt) as u64),
                    ),
                    _ => (None, None),
                };
                NetDisplay {
                    iface: net.iface.clone(),
                    rx: net.rx,
                    tx: net.tx,
                    rx_per_sec,
                    tx_per_sec,
                }
            })
            .collect();
        self.previous_nets = Some(
            raw.nets
                .into_iter()
                .map(|n| (n.iface, (n.rx, n.tx)))
                .collect(),
        );
        self.previous_at_ms = Some(at_ms);
        self.snapshot = Some(MonitorSnapshot {
            load: raw.load,
            cpu_percent: raw.cpu_percent,
            mem_total: raw.mem_total,
            mem_used: raw.mem_total.saturating_sub(raw.mem_available),
            swap_total: raw.swap_total,
            swap_used: raw.swap_total.saturating_sub(raw.swap_free),
            disks: raw.disks,
            nets,
        });
        self.error = None;
        self.sampling = false;
        true
    }

    /// 写入采样失败信息；过时结果忽略。
    pub fn apply_error(&mut self, session: SessionId, generation: u64, message: String) -> bool {
        if self.target != Some(session) || self.generation != generation {
            return false;
        }
        self.error = Some(message);
        self.sampling = false;
        true
    }
}

/// 二进制单位格式化字节数（B / KiB / MiB / GiB / TiB）。
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut idx = 0;
    while value >= 1024.0 && idx + 1 < UNITS.len() {
        value /= 1024.0;
        idx += 1;
    }
    format!("{value:.1} {}", UNITS[idx])
}

/// 格式化网卡速率。
pub fn format_rate(bytes_per_sec: u64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

#[cfg(test)]
mod tests {
    use kanso_types::NetCounter;

    use super::*;

    fn sample(rx: u64, tx: u64, cpu: Option<u8>) -> HostSample {
        HostSample {
            load: [0.15, 0.25, 0.35],
            cpu_percent: cpu,
            mem_total: 8 * 1024 * 1024 * 1024,
            mem_available: 2 * 1024 * 1024 * 1024,
            swap_total: 1024 * 1024 * 1024,
            swap_free: 512 * 1024 * 1024,
            disks: vec![DiskUse {
                mount: "/".into(),
                size: 100 * 1024,
                used: 40 * 1024,
                percent: 40,
            }],
            nets: vec![NetCounter {
                iface: "eth0".into(),
                rx,
                tx,
            }],
        }
    }

    #[test]
    fn target_only_accepts_connected_ssh_and_sftp() {
        let mut local = TabItem::new(SessionId::from_raw(1), TabKind::Local, "zsh");
        local.connection = TabConnection::Connected;
        assert_eq!(target_for(Some(&local)), None);

        let mut ssh = TabItem::new(SessionId::from_raw(2), TabKind::Ssh, "prod");
        ssh.connection = TabConnection::Connecting;
        assert_eq!(target_for(Some(&ssh)), None);
        ssh.connection = TabConnection::Connected;
        assert_eq!(target_for(Some(&ssh)), Some(SessionId::from_raw(2)));

        let mut sftp = TabItem::new(SessionId::from_raw(3), TabKind::Sftp, "files");
        sftp.connection = TabConnection::Connected;
        assert_eq!(target_for(Some(&sftp)), Some(SessionId::from_raw(3)));
    }

    #[test]
    fn net_rates_and_stale_generation_guard() {
        let mut model = MonitorModel::default();
        let s1 = SessionId::from_raw(1);
        let s2 = SessionId::from_raw(2);
        assert!(model.set_target(Some(s1)));
        let (_, g1) = model.begin_sample().unwrap();
        assert!(model.apply_sample(s1, g1, sample(1000, 500, Some(20)), 10_000));
        assert_eq!(model.snapshot().unwrap().nets[0].rx_per_sec, None);

        assert!(model.apply_sample(s1, g1, sample(5096, 2548, Some(45)), 12_000));
        assert_eq!(model.snapshot().unwrap().nets[0].rx_per_sec, Some(2048));
        assert_eq!(model.snapshot().unwrap().nets[0].tx_per_sec, Some(1024));

        // 计数倒退不崩溃且不输出假速率。
        assert!(model.apply_sample(s1, g1, sample(100, 10, Some(10)), 14_000));
        assert_eq!(model.snapshot().unwrap().nets[0].rx_per_sec, None);

        // 切会话后旧代次响应必须被丢弃。
        assert!(model.set_target(Some(s2)));
        assert!(!model.apply_sample(s1, g1, sample(9999, 9999, Some(99)), 16_000));
        assert!(model.snapshot().is_none());
    }

    #[test]
    fn formats_bytes_and_rates() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_rate(2 * 1024 * 1024), "2.0 MiB/s");
    }
}
