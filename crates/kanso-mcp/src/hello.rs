//! 握手：转发程序连上 socket 后，在 MCP 字节流之前先发一行 JSON（architecture §16.1）。
//!
//! 格式：`{"v":1,"token":"…","tty":"/dev/ttys003","tty_dev":16777227}\n`。
//! `tty` 没有控制终端（或拿不到设备名）时为 `null`；`tty_dev` 是控制终端的设备号（`st_rdev`），
//! 拿不到时整个字段省略。`tty_dev` 是后加的可选字段，版本号不变：旧 App 的解析器忽略未知字段，
//! 新 App 遇到没有它的旧转发程序退回按 `tty` 路径比较。
//! App 读走这一行，校验令牌，然后把余下的字节流交给 MCP 协议处理。
//! 客户端所在标签的识别（spec/mcp.md §2.5）用到 `tty` / `tty_dev`；对端 pid 由 App 自己从 socket 取，不在握手里自报。

/// 握手格式版本。不兼容地改字段时加一，App 拒绝不认识的版本。
pub const VERSION: u32 = 1;
/// 握手行的最大字节数（含换行）。App 读到超过这个长度仍没有换行就断开，防止被灌入大量数据。
pub const MAX_LEN: usize = 8 * 1024;

/// 生成握手行（含结尾换行）。
///
/// `tty` 必须是控制终端的真实设备路径（如 `/dev/ttys003`），不能是 `/dev/tty` 这种别名；
/// `tty_dev` 是同一个终端的设备号，App 优先按它比较（路径写法可能不同，设备号不会）。
pub fn encode(token: &str, tty: Option<&str>, tty_dev: Option<u64>) -> String {
    let tty = tty.map_or_else(|| "null".to_owned(), json_string);
    let tty_dev = tty_dev.map_or_else(String::new, |d| format!(",\"tty_dev\":{d}"));
    format!(
        "{{\"v\":{VERSION},\"token\":{},\"tty\":{tty}{tty_dev}}}\n",
        json_string(token)
    )
}

/// 把字符串编码成 JSON 字符串字面量（含两侧引号）。
pub fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_with_and_without_tty() {
        assert_eq!(
            encode("abc", Some("/dev/ttys003"), Some(16_777_227)),
            "{\"v\":1,\"token\":\"abc\",\"tty\":\"/dev/ttys003\",\"tty_dev\":16777227}\n"
        );
        // 有设备号但没有设备名：两个字段互相独立。
        assert_eq!(
            encode("abc", None, Some(5)),
            "{\"v\":1,\"token\":\"abc\",\"tty\":null,\"tty_dev\":5}\n"
        );
        // 没有控制终端：tty_dev 整个省略，与旧转发程序的输出一致。
        assert_eq!(
            encode("abc", None, None),
            "{\"v\":1,\"token\":\"abc\",\"tty\":null}\n"
        );
    }

    #[test]
    fn escapes_control_and_quote_characters() {
        assert_eq!(json_string("a\"b\\c\n\u{1}"), "\"a\\\"b\\\\c\\n\\u0001\"");
    }
}
