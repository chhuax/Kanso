//! 连不上 App 时，转发程序要对客户端的第一条请求回一个 JSON-RPC 错误再退出（architecture §16.1），
//! 否则客户端只会看到"服务进程意外退出"，不知道该做什么。这是转发程序唯一需要理解的协议内容，
//! 为了不引入 JSON 库，这里只实现"取出顶层 `id` 的原始文本"这一件事。

use crate::hello::json_string;

/// MCP stdio 传输按行分帧：每条消息一行 JSON。取出顶层对象里 `id` 字段的原始 JSON 文本
/// （数字或字符串，原样保留，回复时照抄）。没有 `id`（通知）或不是 JSON 对象时返回 `None`。
pub fn request_id(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut i = skip_ws(bytes, 0);
    if bytes.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    loop {
        i = skip_ws(bytes, i);
        match bytes.get(i)? {
            b'}' => return None,
            b',' => {
                i += 1;
                continue;
            }
            b'"' => {}
            _ => return None,
        }
        let key_end = string_end(bytes, i)?;
        let key = &line[i + 1..key_end - 1];
        i = skip_ws(bytes, key_end);
        if bytes.get(i) != Some(&b':') {
            return None;
        }
        i = skip_ws(bytes, i + 1);
        let value_end = value_end(bytes, i)?;
        if key == "id" {
            return Some(line[i..value_end].to_owned());
        }
        i = value_end;
    }
}

/// 生成一条 JSON-RPC 错误回复（含结尾换行）。`id` 是 [`request_id`] 取出的原始文本。
pub fn error_response(id: &str, code: i32, message: &str) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"error\":{{\"code\":{code},\"message\":{}}}}}\n",
        json_string(message)
    )
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while matches!(b.get(i), Some(b' ' | b'\t' | b'\r' | b'\n')) {
        i += 1;
    }
    i
}

/// `b[i]` 是起始引号；返回结尾引号之后的位置。
fn string_end(b: &[u8], mut i: usize) -> Option<usize> {
    i += 1;
    loop {
        match b.get(i)? {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
}

/// 跳过一个 JSON 值（字符串、数字、字面量、对象、数组），返回其后的位置。
fn value_end(b: &[u8], i: usize) -> Option<usize> {
    match b.get(i)? {
        b'"' => string_end(b, i),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut j = i;
            loop {
                match b.get(j)? {
                    b'"' => {
                        j = string_end(b, j)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
        }
        _ => {
            let mut j = i;
            while let Some(c) = b.get(j) {
                if matches!(c, b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n') {
                    break;
                }
                j += 1;
            }
            (j > i).then_some(j)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_numeric_and_string_ids_in_any_position() {
        let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"id":"inner"}}"#;
        assert_eq!(request_id(init).as_deref(), Some("1"));
        let later = r#"{"method":"x","params":{"id":9,"a":[1,{"id":2}],"s":"}\""},"id":"abc"}"#;
        assert_eq!(request_id(later).as_deref(), Some("\"abc\""));
    }

    #[test]
    fn notifications_and_garbage_have_no_id() {
        assert_eq!(
            request_id(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            None
        );
        assert_eq!(request_id("not json"), None);
        assert_eq!(request_id(""), None);
    }

    #[test]
    fn error_response_echoes_raw_id() {
        assert_eq!(
            error_response("\"abc\"", -32000, "Kanso is not running"),
            "{\"jsonrpc\":\"2.0\",\"id\":\"abc\",\"error\":{\"code\":-32000,\"message\":\"Kanso is not running\"}}\n"
        );
    }
}
