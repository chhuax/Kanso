//! 把监控脚本的 stdout 解析成 [`HostSample`]（host-monitor.md）。

use kanso_types::{DiskUse, HostSample, NetCounter};

/// 脚本第一行：主机没有 `/proc`。
const UNSUPPORTED: &str = "KANSO-unsupported";
/// 脚本第一行：后面是分段正文。
const OK: &str = "KANSO-1";

/// 固定探测命令。脚本里没有单引号，以便包进 `sh -c '…'`；`df -Pk` 固定以 1024 字节块输出。
pub(crate) const SAMPLE_COMMAND: &str = "sh -c 'if ! test -r /proc/stat; then echo KANSO-unsupported; exit 0; fi\n\
echo KANSO-1\n\
echo ---load---\n\
cat /proc/loadavg\n\
echo ---mem---\n\
cat /proc/meminfo\n\
echo ---cpu1---\n\
grep ^cpu /proc/stat | head -n 1\n\
sleep 1\n\
echo ---cpu2---\n\
grep ^cpu /proc/stat | head -n 1\n\
echo ---df---\n\
df -Pk\n\
echo ---net---\n\
cat /proc/net/dev'";

/// 单次输出上限（host-monitor.md §2）。
pub(crate) const SAMPLE_MAX_BYTES: usize = 64 * 1024;
/// 整次采样等待上限，含脚本里的 1 秒。
pub(crate) const SAMPLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

/// 解析失败的原因，给 `MonitorFailed` 用。
#[derive(Debug)]
pub(crate) enum SampleParse {
    /// 第一行是 `KANSO-unsupported`。
    Unsupported,
    /// 输出对不上约定。
    Bad(String),
}

pub(crate) fn parse_sample(text: &str) -> Result<HostSample, SampleParse> {
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.lines().map(str::trim_end);
    let head = lines.next().unwrap_or("").trim();
    if head == UNSUPPORTED {
        return Err(SampleParse::Unsupported);
    }
    if head != OK {
        return Err(SampleParse::Bad("missing sample header".into()));
    }
    let mut sections: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in lines {
        if let Some(name) = line.strip_prefix("---").and_then(|s| s.strip_suffix("---")) {
            sections.push((name, Vec::new()));
        } else if let Some((_, body)) = sections.last_mut() {
            body.push(line);
        }
    }
    let body = |name: &str| -> Result<&[&str], SampleParse> {
        sections
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, lines)| lines.as_slice())
            .ok_or_else(|| SampleParse::Bad(format!("missing {name}")))
    };
    let load = parse_load(body("load")?)?;
    let mem = parse_mem(body("mem")?)?;
    let cpu = cpu_percent(body("cpu1")?, body("cpu2")?)?;
    Ok(HostSample {
        load,
        cpu_percent: cpu,
        mem_total: mem.0,
        mem_available: mem.1,
        swap_total: mem.2,
        swap_free: mem.3,
        disks: parse_df(body("df")?)?,
        nets: parse_net(body("net")?)?,
    })
}

fn parse_load(lines: &[&str]) -> Result<[f32; 3], SampleParse> {
    let line = lines.first().copied().unwrap_or("");
    let mut nums = line.split_whitespace();
    let mut out = [0f32; 3];
    for slot in &mut out {
        let text = nums.next().ok_or_else(|| SampleParse::Bad("load".into()))?;
        *slot = text
            .parse()
            .map_err(|_| SampleParse::Bad(format!("load {text}")))?;
    }
    Ok(out)
}

fn mem_kb(lines: &[&str], key: &str) -> Result<u64, SampleParse> {
    let line = lines
        .iter()
        .find(|line| line.starts_with(key))
        .ok_or_else(|| SampleParse::Bad(key.into()))?;
    let kb = line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| SampleParse::Bad(key.into()))?;
    let kb: u64 = kb.parse().map_err(|_| SampleParse::Bad(key.into()))?;
    Ok(kb.saturating_mul(1024))
}

fn parse_mem(lines: &[&str]) -> Result<(u64, u64, u64, u64), SampleParse> {
    Ok((
        mem_kb(lines, "MemTotal:")?,
        mem_kb(lines, "MemAvailable:")?,
        mem_kb(lines, "SwapTotal:").unwrap_or(0),
        mem_kb(lines, "SwapFree:").unwrap_or(0),
    ))
}

fn cpu_fields(line: &str) -> Result<(u64, u64), SampleParse> {
    let mut nums = line
        .split_whitespace()
        .skip(1)
        .map(|n| n.parse::<u64>().map_err(|_| SampleParse::Bad("cpu".into())));
    let mut take = || nums.next().ok_or_else(|| SampleParse::Bad("cpu".into()))?;
    let user = take()?;
    let nice = take()?;
    let system = take()?;
    let idle = take()?;
    let iowait = take()?;
    let irq = take()?;
    let softirq = take()?;
    let steal = nums.next().transpose()?.unwrap_or(0);
    let busy = user + nice + system + irq + softirq + steal;
    let total = busy + idle + iowait;
    Ok((busy, total))
}

fn cpu_percent(first: &[&str], second: &[&str]) -> Result<Option<u8>, SampleParse> {
    let a = cpu_fields(first.first().copied().unwrap_or(""))?;
    let b = cpu_fields(second.first().copied().unwrap_or(""))?;
    let busy = b.0.saturating_sub(a.0);
    let total = b.1.saturating_sub(a.1);
    if total == 0 {
        return Ok(None);
    }
    Ok(Some(u8::try_from(busy * 100 / total).unwrap_or(100)))
}

fn parse_df(lines: &[&str]) -> Result<Vec<DiskUse>, SampleParse> {
    let mut disks = Vec::new();
    for line in lines.iter().copied().filter(|line| !line.is_empty()) {
        if line.starts_with("Filesystem") {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some(pct_at) = tokens.iter().position(|t| t.ends_with('%')) else {
            continue;
        };
        if pct_at < 3 || pct_at + 1 >= tokens.len() {
            continue;
        }
        // df -Pk 列序：Filesystem 1024-blocks Used Available Capacity Mounted on
        let size: u64 = tokens[pct_at - 3]
            .parse()
            .map_err(|_| SampleParse::Bad("df".into()))?;
        let used: u64 = tokens[pct_at - 2]
            .parse()
            .map_err(|_| SampleParse::Bad("df".into()))?;
        let percent: u8 = tokens[pct_at]
            .trim_end_matches('%')
            .parse()
            .map_err(|_| SampleParse::Bad("df".into()))?;
        disks.push(DiskUse {
            mount: tokens[pct_at + 1..].join(" "),
            size: size.saturating_mul(1024),
            used: used.saturating_mul(1024),
            percent,
        });
    }
    Ok(disks)
}

fn parse_net(lines: &[&str]) -> Result<Vec<NetCounter>, SampleParse> {
    let mut nets = Vec::new();
    for line in lines {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let nums: Vec<u64> = rest
            .split_whitespace()
            .filter_map(|n| n.parse().ok())
            .collect();
        if nums.len() < 9 {
            continue;
        }
        nets.push(NetCounter {
            iface: name.trim().to_owned(),
            rx: nums[0],
            tx: nums[8],
        });
    }
    Ok(nets)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
KANSO-1
---load---
0.10 0.20 0.30 1/2 3
---mem---
MemTotal: 2048 kB
MemAvailable: 1024 kB
SwapTotal: 512 kB
SwapFree: 256 kB
---cpu1---
cpu 10 0 10 80 0 0 0 0 0 0
---cpu2---
cpu 20 0 20 80 0 0 0 0 0 0
---df---
Filesystem 1024-blocks Used Available Capacity Mounted on
/dev/sda1 1000 400 600 40% /
tmpfs 500 50 450 10% /mnt/my data
---net---
 face
  eth0: 100 0 0 0 0 0 0 0 50 0 0 0 0 0 0 0
";

    #[test]
    fn parses_a_linux_sample() {
        let sample = parse_sample(SAMPLE).unwrap();
        assert_eq!(sample.load, [0.10, 0.20, 0.30]);
        assert_eq!(sample.mem_total, 2048 * 1024);
        assert_eq!(sample.mem_available, 1024 * 1024);
        assert_eq!(sample.swap_total, 512 * 1024);
        assert_eq!(sample.swap_free, 256 * 1024);
        // busy 20→40, total 100→120, 20/20 = 100%
        assert_eq!(sample.cpu_percent, Some(100));
        assert_eq!(sample.disks.len(), 2);
        assert_eq!(sample.disks[0].mount, "/");
        assert_eq!(sample.disks[0].size, 1000 * 1024);
        assert_eq!(sample.disks[0].used, 400 * 1024);
        assert_eq!(sample.disks[0].percent, 40);
        assert_eq!(sample.disks[1].mount, "/mnt/my data");
        assert_eq!(sample.nets[0].iface, "eth0");
        assert_eq!(sample.nets[0].rx, 100);
        assert_eq!(sample.nets[0].tx, 50);
    }

    #[test]
    fn unsupported_and_bad_headers_are_distinct() {
        assert!(matches!(
            parse_sample("KANSO-unsupported\n"),
            Err(SampleParse::Unsupported)
        ));
        assert!(matches!(
            parse_sample("unexpected output\n"),
            Err(SampleParse::Bad(_))
        ));
    }

    #[test]
    fn zero_cpu_delta_yields_none() {
        let same_cpu = SAMPLE.replace("cpu 20 0 20 80", "cpu 10 0 10 80");
        let sample = parse_sample(&same_cpu).unwrap();
        assert_eq!(sample.cpu_percent, None);
    }
}
