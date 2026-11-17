//! 握手：转发程序连上 socket 后，在 MCP 字节流之前先发一行 JSON（architecture §16.1）。
//!
//! 格式：`{"v":1,"token":"…","tty":"/dev/ttys003"}\n`，`tty` 没有控制终端时为 `null`。
//! App 读走这一行，校验令牌，然后把余下的字节流交给 MCP 协议处理。
//! 客户端所在标签的识别（spec/mcp.md §2.5）用到 `tty`；对端 pid 由 App 自己从 socket 取，不在握手里自报。

/// 握手格式版本。不兼容地改字段时加一，App 拒绝不认识的版本。
pub const VERSION: u32 = 1;
/// 握手行的最大字节数（含换行）。App 读到超过这个长度仍没有换行就断开，防止被灌入大量数据。
pub const MAX_LEN: usize = 8 * 1024;

/// 生成握手行（含结尾换行）。
pub fn encode(token: &str, tty: Option<&str>) -> String {
    let tty = tty.map_or_else(|| "null".to_owned(), json_string);
    format!(
        "{{\"v\":{VERSION},\"token\":{},\"tty\":{tty}}}\n",
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
            encode("abc", Some("/dev/ttys003")),
            "{\"v\":1,\"token\":\"abc\",\"tty\":\"/dev/ttys003\"}\n"
        );
        assert_eq!(encode("abc", None), "{\"v\":1,\"token\":\"abc\",\"tty\":null}\n");
    }

    #[test]
    fn escapes_control_and_quote_characters() {
        assert_eq!(json_string("a\"b\\c\n\u{1}"), "\"a\\\"b\\\\c\\n\\u0001\"");
    }
}
