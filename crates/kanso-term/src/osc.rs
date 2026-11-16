//! OSC 序列扫描与解析：133（命令边界）、7 / 1337 CurrentDir / 9;9（工作目录）。
//!
//! 为什么要自己扫描：alacritty / vte 不把这几个 OSC 交给 `Handler`，而 OSC 133 A 必须记录在
//! “处理到这个序列那一刻光标所在的行”，不能等一批字节处理完再补。做法是在喂给 vte 之前先用
//! [`OscScanner`] 找到 OSC 的结束位置，把字节流切成“到该 OSC 结束为止”的一段喂给 vte，
//! 然后再处理这个 OSC（此时光标已经停在 OSC 之后，OSC 本身不移动光标）。
//!
//! 扫描器的状态机必须和 vte 的 `Parser` 对同一份字节流给出相同的 OSC 边界，规则（照 vte 0.15 写成）：
//! - 任何状态下的 ESC 都进入 Escape 状态（DCS/SOS/PM/APC 里的 ESC 也一样，不需要单独跟踪那些字符串）；
//!   Escape 状态下的 C0 控制字符不改变状态，`ESC ]` 开始 OSC；
//! - OSC 内部 0x00–0x06、0x08–0x17、0x19、0x1C–0x1F 被忽略；
//!   BEL(0x07)、CAN(0x18)、SUB(0x1A)、ESC(0x1B) 都结束 OSC（vte 对它们都会派发 OSC，ESC 同时开启新的 Escape）；
//! - vte 只支持 7 位控制码，所以不识别 C1 的 0x9D（否则 UTF-8 续字节会被误判）。
//!
//! 升级 vte 时要对照它的状态机确认这些规则没变（见 workspace Cargo.toml 的升级清单）。

use crate::types::CwdReport;

/// 我们关心的 OSC 编号：7、9、133、1337。其余（0/2 标题、8 链接、52 剪贴板…）由 alacritty 处理。
fn relevant(ps: u32) -> bool {
    matches!(ps, 7 | 9 | 133 | 1337)
}

/// 单个 OSC 负载上限。工作目录再长也不会超过它；超过的整条忽略，避免恶意输出撑爆内存。
const MAX_BODY: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum St {
    Ground,
    Esc,
    /// 正在读 OSC 编号（`ESC ]` 之后、第一个 `;` 之前）。
    Num,
    /// 相关 OSC 的负载。
    Body,
    /// 不关心的 OSC，只找结束位置。
    Skip,
}

/// 跨批次保持状态的 OSC 扫描器。
#[derive(Debug)]
pub(crate) struct OscScanner {
    st: St,
    ps: u32,
    ps_len: u8,
    overflow: bool,
    /// 最近一个完成的相关 OSC 的编号。
    pub done_ps: u32,
    /// 最近一个完成的相关 OSC 的负载（编号后的 `;` 之后的原始字节，不按 `;` 切分，
    /// 因为路径里可能有分号）。
    pub body: Vec<u8>,
}

impl OscScanner {
    pub fn new() -> Self {
        Self {
            st: St::Ground,
            ps: 0,
            ps_len: 0,
            overflow: false,
            done_ps: 0,
            body: Vec::new(),
        }
    }

    /// 扫描 `bytes`；遇到一个完整的相关 OSC 就返回“含终止符的消耗字节数”，
    /// 编号与负载在 `done_ps` / `body`。调用方处理完后用剩余字节再次调用。
    /// 返回 `None` 表示这段字节里没有完整的相关 OSC（状态会保留到下一批）。
    pub fn scan(&mut self, bytes: &[u8]) -> Option<usize> {
        let mut i = 0;
        while i < bytes.len() {
            if self.st == St::Ground {
                let marker = memchr::memchr(b']', &bytes[i..]).map(|offset| i + offset);
                let end = marker.unwrap_or(bytes.len());
                self.st = if bytes[i..end]
                    .iter()
                    .rfind(|&&byte| !matches!(byte, 0x00..=0x17 | 0x19 | 0x1c..=0x1f))
                    == Some(&0x1b)
                {
                    St::Esc
                } else {
                    St::Ground
                };
                marker?;
                i = end;
                if self.st == St::Ground {
                    i += 1;
                    continue;
                }
            }
            let b = bytes[i];
            i += 1;
            match self.st {
                St::Ground => unreachable!(),
                St::Esc => match b {
                    b']' => {
                        self.st = St::Num;
                        self.ps = 0;
                        self.ps_len = 0;
                        self.overflow = false;
                        self.body.clear();
                    }
                    0x1b | 0x00..=0x17 | 0x19 | 0x1c..=0x1f => {}
                    _ => self.st = St::Ground,
                },
                St::Num => match b {
                    b'0'..=b'9' => {
                        self.ps = self
                            .ps
                            .saturating_mul(10)
                            .saturating_add(u32::from(b - b'0'));
                        self.ps_len += 1;
                        if self.ps_len > 5 {
                            self.st = St::Skip;
                        }
                    }
                    b';' => {
                        self.st = if relevant(self.ps) {
                            St::Body
                        } else {
                            St::Skip
                        }
                    }
                    0x07 | 0x18 | 0x1a => self.st = St::Ground,
                    0x1b => self.st = St::Esc,
                    0x00..=0x06 | 0x08..=0x17 | 0x19 | 0x1c..=0x1f => {}
                    _ => self.st = St::Skip,
                },
                St::Skip => match b {
                    0x07 | 0x18 | 0x1a => self.st = St::Ground,
                    0x1b => self.st = St::Esc,
                    _ => {}
                },
                St::Body => match b {
                    0x07 | 0x18 | 0x1a | 0x1b => {
                        self.st = if b == 0x1b { St::Esc } else { St::Ground };
                        if !self.overflow {
                            self.done_ps = self.ps;
                            return Some(i);
                        }
                    }
                    0x00..=0x06 | 0x08..=0x17 | 0x19 | 0x1c..=0x1f => {}
                    _ => {
                        if self.body.len() >= MAX_BODY {
                            self.overflow = true;
                        } else {
                            self.body.push(b);
                        }
                    }
                },
            }
        }
        None
    }
}

/// OSC 133 解析结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Osc133 {
    /// A：提示符开始；`first` 表示带 `kanso=first`。
    PromptStart { first: bool },
    /// B：提示符结束、命令输入开始。
    InputStart,
    /// C：命令开始执行。
    CommandStart,
    /// D：命令结束，可带退出码。
    CommandFinished { exit_code: Option<i32> },
}

/// 解析 `133;` 之后的负载，如 `A;kanso=first`、`D;0`。其它 FinalTerm / iTerm2 选项（`aid=…`、`cl=…`）忽略。
pub(crate) fn parse_133(body: &[u8]) -> Option<Osc133> {
    let mut parts = body.split(|&b| b == b';');
    let kind = parts.next()?;
    match kind {
        b"A" => Some(Osc133::PromptStart {
            first: parts.any(|p| p == b"kanso=first"),
        }),
        b"B" => Some(Osc133::InputStart),
        b"C" => Some(Osc133::CommandStart),
        b"D" => {
            // 退出码是 D 后面第一个纯数字参数；`aid=` 之类带 `=` 的选项不会被误读。
            let exit_code = parts.find_map(|p| {
                std::str::from_utf8(p)
                    .ok()
                    .and_then(|s| s.parse::<i32>().ok())
            });
            Some(Osc133::CommandFinished { exit_code })
        }
        _ => None,
    }
}

/// 解析 OSC 7 的负载（`7;` 之后）：`file://host/path`，也容忍直接写绝对路径。
pub(crate) fn parse_osc7(body: &[u8]) -> Option<CwdReport> {
    let s = std::str::from_utf8(body).ok()?;
    if let Some(rest) = s.strip_prefix("file://") {
        let slash = rest.find('/')?;
        let (host, path) = rest.split_at(slash);
        // 忽略 `?query` 与 `#fragment`：路径里的 `?`/`#` 在合规 URL 中必须编码，
        // 不合规的提示符脚本直接写 $PWD 时才会出现，此时宁可保留原样（只在能解码时去掉）。
        let path = percent_decode(path).unwrap_or_else(|| path.to_owned());
        if path.is_empty() {
            return None;
        }
        let host = (!host.is_empty()).then(|| host.to_owned());
        Some(CwdReport { host, path })
    } else if s.starts_with('/') {
        Some(CwdReport {
            host: None,
            path: s.to_owned(),
        })
    } else {
        None
    }
}

/// 解析 OSC 1337 的负载（`1337;` 之后）：只认 `CurrentDir=路径`。
pub(crate) fn parse_osc1337(body: &[u8]) -> Option<CwdReport> {
    let s = std::str::from_utf8(body).ok()?;
    let path = s.strip_prefix("CurrentDir=")?;
    (!path.is_empty()).then(|| CwdReport {
        host: None,
        path: path.to_owned(),
    })
}

/// 解析 OSC 9 的负载（`9;` 之后）：只认 ConEmu 的 `9;路径`，路径两侧的双引号可有可无。
/// 其它 OSC 9 子命令（iTerm2 的通知、ConEmu 的进度 `9;4`）不是工作目录，返回 `None`。
pub(crate) fn parse_osc9(body: &[u8]) -> Option<CwdReport> {
    let s = std::str::from_utf8(body).ok()?;
    let path = s.strip_prefix("9;")?;
    let path = path.strip_prefix('"').unwrap_or(path);
    let path = path.strip_suffix('"').unwrap_or(path);
    (!path.is_empty()).then(|| CwdReport {
        host: None,
        path: path.to_owned(),
    })
}

/// 百分号解码；遇到非法转义或结果不是合法 UTF-8 返回 `None`（调用方按原样使用）。
pub(crate) fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hi = hex(*b.get(i + 1)?)?;
            let lo = hex(*b.get(i + 2)?)?;
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_all(s: &mut OscScanner, mut b: &[u8]) -> Vec<(u32, Vec<u8>)> {
        let mut out = Vec::new();
        while let Some(n) = s.scan(b) {
            out.push((s.done_ps, s.body.clone()));
            b = &b[n..];
        }
        out
    }

    #[test]
    fn scans_bel_and_st_terminated() {
        let mut s = OscScanner::new();
        let r = scan_all(&mut s, b"a\x1b]133;A\x07b\x1b]7;file:///x\x1b\\c");
        assert_eq!(r, vec![(133, b"A".to_vec()), (7, b"file:///x".to_vec())]);
    }

    #[test]
    fn scans_across_chunks() {
        let mut s = OscScanner::new();
        assert!(scan_all(&mut s, b"\x1b").is_empty());
        assert!(scan_all(&mut s, b"]13").is_empty());
        assert!(scan_all(&mut s, b"3;A;kanso=").is_empty());
        let r = scan_all(&mut s, b"first\x07");
        assert_eq!(r, vec![(133, b"A;kanso=first".to_vec())]);
    }

    #[test]
    fn ignores_irrelevant_and_nested() {
        let mut s = OscScanner::new();
        // OSC 0 不关心；其中的 ESC 结束它并立刻开始新的 OSC。
        let r = scan_all(&mut s, b"\x1b]0;title\x1b]133;D;3\x07");
        assert_eq!(r, vec![(133, b"D;3".to_vec())]);
    }

    #[test]
    fn dcs_content_does_not_hide_osc() {
        let mut s = OscScanner::new();
        let r = scan_all(&mut s, b"\x1bPq...\x1b]133;C\x07");
        assert_eq!(r, vec![(133, b"C".to_vec())]);
    }

    #[test]
    fn sgr_skip_preserves_osc_boundaries_across_chunks() {
        let data = b"\x1b[32mgreen\x1b[0m\r\n\x1b\0\n]133;A\x07\x1b[1mtext\x1b[0m\x1b\x18]133;C\x07\x1b[0m\x1b\n\x1b\r]133;D;0\x07";
        let expected = vec![(133, b"A".to_vec()), (133, b"D;0".to_vec())];
        for split in 0..=data.len() {
            let mut scanner = OscScanner::new();
            let mut found = scan_all(&mut scanner, &data[..split]);
            found.extend(scan_all(&mut scanner, &data[split..]));
            assert_eq!(found, expected, "split={split}");
        }
        for width in 1..=data.len() {
            let mut scanner = OscScanner::new();
            let found: Vec<_> = data
                .chunks(width)
                .flat_map(|chunk| scan_all(&mut scanner, chunk))
                .collect();
            assert_eq!(found, expected, "width={width}");
        }
    }

    #[test]
    fn parse_cwd() {
        let c = parse_osc7(b"file://mac.local/Users/a%20b/%E4%B8%AD").unwrap();
        assert_eq!(c.host.as_deref(), Some("mac.local"));
        assert_eq!(c.path, "/Users/a b/中");
        let c = parse_osc7(b"file:///Users/a b").unwrap();
        assert_eq!((c.host, c.path.as_str()), (None, "/Users/a b"));
        // 非法转义按原样。
        assert_eq!(parse_osc7(b"file:///a%zz").unwrap().path, "/a%zz");
        assert_eq!(parse_osc1337(b"CurrentDir=/tmp/x").unwrap().path, "/tmp/x");
        assert_eq!(
            parse_osc9(b"9;\"C:\\Users\\a\"").unwrap().path,
            "C:\\Users\\a"
        );
        assert_eq!(parse_osc9(b"9;/home/a").unwrap().path, "/home/a");
        assert!(parse_osc9(b"4;1;50").is_none());
    }

    #[test]
    fn parse_133_variants() {
        assert_eq!(parse_133(b"A"), Some(Osc133::PromptStart { first: false }));
        assert_eq!(
            parse_133(b"A;aid=1;kanso=first"),
            Some(Osc133::PromptStart { first: true })
        );
        assert_eq!(
            parse_133(b"D"),
            Some(Osc133::CommandFinished { exit_code: None })
        );
        assert_eq!(
            parse_133(b"D;127"),
            Some(Osc133::CommandFinished {
                exit_code: Some(127)
            })
        );
        assert_eq!(parse_133(b"X"), None);
    }
}
