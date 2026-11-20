//! `kanso_exec` 与 SFTP 工具的纯函数部分：内容编码、续页标记、路径拆分、core 错误与结果到 AI 可读 JSON
//! 和错误码的映射。不碰 core、不碰锁，便于单独测试。文字一律是给 AI 看的英文（spec/mcp.md §5）。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use kanso_core::CoreError;
use kanso_core::mcp::{ExecOutcome, UserNeed};
use kanso_encoding::{StreamDecoder, TextEncoding, encode_text};
use kanso_types::DirEntry;
use kanso_types::message::{Message, Reason, SftpOp};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::terminal::{capture_object, invalid};
use super::tools::{ToolError, internal_error};

/// `kanso_exec` 的超时默认值与上限，单位秒（spec/mcp.md §3.4）。
pub(super) const EXEC_TIMEOUT_DEFAULT: u64 = 60;
pub(super) const EXEC_TIMEOUT_MAX: u64 = 600;
/// `kanso_sftp_list` 一页的条目数上限（spec/mcp.md §3.5）。
pub(super) const LIST_PAGE_SIZE: usize = 1000;
/// `kanso_sftp_read` 单次读取上限：1 MiB（spec/mcp.md §3.5）。
pub(super) const READ_MAX_BYTES: u64 = 1024 * 1024;
/// `kanso_sftp_write` 解码后的内容上限：8 MiB（spec/mcp.md §3.5）。
pub(super) const WRITE_MAX_BYTES: usize = 8 * 1024 * 1024;

/// 文件内容的表示方式。
#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(super) enum ContentEncoding {
    /// 按会话编码与 UTF-8 互转。
    #[default]
    Text,
    /// 原始字节的 base64（标准字母表，带填充）。
    Base64,
}

impl ContentEncoding {
    pub(super) fn name(self) -> &'static str {
        match self {
            ContentEncoding::Text => "text",
            ContentEncoding::Base64 => "base64",
        }
    }
}

/// `kanso_sftp_write` 的写入模式（必填，spec/mcp.md §3.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum WriteMode {
    /// 目标已存在则失败（`CONFLICT`）。
    CreateNew,
    /// 先写临时文件再用 posix-rename 替换；服务器不支持时 `UNSUPPORTED`。
    Overwrite,
}

impl WriteMode {
    pub(super) fn name(self) -> &'static str {
        match self {
            WriteMode::CreateNew => "create_new",
            WriteMode::Overwrite => "overwrite",
        }
    }
}

/// 路径参数不能为空：空串在服务器上的含义不确定，让 AI 明确写 `.`（家目录）。
pub(super) fn check_path(name: &str, path: &str) -> Result<(), ToolError> {
    if path.is_empty() {
        return Err(invalid(format!(
            "{name} must not be empty. Use \".\" for the SFTP home directory."
        )));
    }
    Ok(())
}

// ---- 内容编码 ----

/// 把读到的字节按会话编码转成 UTF-8 文本（`kanso_sftp_read` 的 text 模式）。
///
/// 含 NUL 字节、或无法按会话编码无损解码，都返回 `INVALID_ARGUMENT` 并提示改用 base64。
/// - UTF-8 用严格校验。
/// - 其他编码用"解码后再编码，字节必须一致"校验：`StreamDecoder` 遇到非法序列会替换成 U+FFFD，
///   并且把末尾不完整的字符默默留在内部状态里（不报错、也不输出），单看解码结果发现不了。
///   这条回环检查同时挡住了这两种情况；不要简化成"检查结果里有没有 U+FFFD"。
/// - 一次读取可能把一个多字节字符切在边界上（按偏移 / 长度分段读取时），这时同样会报错；
///   说明里提示调整范围或改用 base64。
pub(super) fn decode_text(enc: TextEncoding, bytes: &[u8]) -> Result<String, ToolError> {
    let fail = || {
        invalid(
            "The content is not valid text in the session's encoding, or contains NUL bytes (a \
             multi-byte character may also have been cut at the range boundary). Read it again \
             with encoding \"base64\".",
        )
    };
    if bytes.contains(&0) {
        return Err(fail());
    }
    if enc.is_utf8() {
        return String::from_utf8(bytes.to_vec()).map_err(|_| fail());
    }
    let mut out = String::new();
    StreamDecoder::new(enc).decode(bytes, &mut out);
    if encode_text(enc, &out).as_ref() == bytes {
        Ok(out)
    } else {
        Err(fail())
    }
}

/// 解码写入内容（`kanso_sftp_write`），并检查大小上限。
///
/// - base64：先按长度粗判，超出上限的请求不必解码；不合法 → `INVALID_ARGUMENT`。
/// - text：按会话编码转换；目标编码里没有的字符 → `INVALID_ARGUMENT`。`encode_text` 对这类字符
///   会静默写成 `?`，那会悄悄改写文件内容，所以要回环解码核对，不一致就拒绝。
pub(super) fn decode_write_content(
    kind: ContentEncoding,
    enc: TextEncoding,
    content: &str,
) -> Result<Vec<u8>, ToolError> {
    let too_large = || {
        ToolError::new(
            "TOO_LARGE",
            format!(
                "At most {WRITE_MAX_BYTES} bytes (8 MiB) can be written per call. Split the \
                 content, or write it in several files."
            ),
        )
    };
    let data = match kind {
        ContentEncoding::Base64 => {
            // 8 MiB 的 base64 长度上限（带填充）；解码后的精确大小在下面再核对。
            if content.len() > WRITE_MAX_BYTES.div_ceil(3) * 4 {
                return Err(too_large());
            }
            BASE64
                .decode(content)
                .map_err(|e| invalid(format!("content is not valid base64: {e}.")))?
        }
        ContentEncoding::Text => {
            let bytes = encode_text(enc, content).into_owned();
            if !enc.is_utf8() {
                let mut back = String::new();
                StreamDecoder::new(enc).decode(&bytes, &mut back);
                if back != content {
                    return Err(invalid(format!(
                        "The text contains characters that cannot be represented in the \
                         session's encoding ({}). Use encoding \"base64\" to write exact bytes.",
                        enc.name()
                    )));
                }
            }
            bytes
        }
    };
    if data.len() > WRITE_MAX_BYTES {
        return Err(too_large());
    }
    Ok(data)
}

/// 审计里记录的写入大小（不解码、不记内容）：base64 按长度估算解码后的字节数，text 取 UTF-8 字节数。
pub(super) fn estimated_size(kind: ContentEncoding, content: &str) -> usize {
    match kind {
        ContentEncoding::Text => content.len(),
        ContentEncoding::Base64 => {
            let padding = content.bytes().rev().take_while(|b| *b == b'=').count();
            (content.len() / 4 * 3).saturating_sub(padding)
        }
    }
}

/// base64 编码（`kanso_sftp_read` 的 base64 模式）。
pub(super) fn encode_base64(bytes: &[u8]) -> String {
    BASE64.encode(bytes)
}

// ---- 续页标记 ----

/// 续页标记：已返回的条数，写成对 AI 不透明的字符串。
///
/// 下一页重新列目录并跳过这么多条，所以目录在两页之间发生变化（增删文件、服务器返回顺序变化）时，
/// 可能出现重复或遗漏。这是用"条数"而不是服务器游标的代价：SFTP 的 READDIR 没有可跨请求保存的游标。
pub(super) fn encode_cursor(returned: usize) -> String {
    format!("c{returned}")
}

pub(super) fn decode_cursor(cursor: &str) -> Result<usize, ToolError> {
    cursor
        .strip_prefix('c')
        .and_then(|n| n.parse::<usize>().ok())
        .ok_or_else(|| {
            invalid("cursor is not valid. Pass the next_cursor value returned by a previous call.")
        })
}

// ---- 条目与路径 ----

/// 目录条目的 JSON（字段同 ssh.md §8）。`permissions` 写成八进制字符串（如 `"0644"`，只取低 12 位），
/// 比十进制整数更便于 AI 阅读。
pub(super) fn entry_json(e: &DirEntry) -> Value {
    json!({
        "name": e.name,
        "path": e.path,
        "is_dir": e.is_dir,
        "is_symlink": e.is_symlink,
        "size": e.size,
        "mtime_ms": e.mtime_ms,
        "permissions": e.permissions.map(|p| format!("{:04o}", p & 0o7777)),
        "owner": e.owner,
        "group": e.group,
    })
}

/// `kanso_sftp_remove` 对一个条目该做什么。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RemoveAction {
    /// 删除空目录（`sftp_rmdir`）。
    Rmdir,
    /// 删除文件或符号链接（`sftp_remove(path, false)`）。
    RemoveFile,
}

/// 按 LSTAT 的结果（`follow_symlinks = false`）决定删除方式。
///
/// 绝不使用 `sftp_remove(path, true)`：那是递归删除，spec/mcp.md §3.5 明确不提供。
/// `DirEntry::is_dir` 对"指向目录的符号链接"也可能为真，所以必须同时排除符号链接：
/// 符号链接即使指向目录，也只能当作链接本身删除，不能对它调 rmdir。
pub(super) fn remove_action(entry: &DirEntry) -> RemoveAction {
    if entry.is_dir && !entry.is_symlink {
        RemoveAction::Rmdir
    } else {
        RemoveAction::RemoveFile
    }
}

/// 逐级创建目录用的前缀：`/a/b/c` → `/a`、`/a/b`、`/a/b/c`；相对路径不加前导 `/`。
/// 空组件（连续或结尾的 `/`）和 `.` 跳过。
pub(super) fn path_prefixes(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    if path.starts_with('/') {
        cur.push('/');
    }
    for part in path.split('/').filter(|p| !p.is_empty() && *p != ".") {
        if !cur.is_empty() && !cur.ends_with('/') {
            cur.push('/');
        }
        cur.push_str(part);
        out.push(cur.clone());
    }
    out
}

// ---- 错误映射 ----

fn reason_text(r: &Reason) -> String {
    match r {
        Reason::NotFound => "no such file or directory".to_owned(),
        Reason::PermissionDenied => "permission denied".to_owned(),
        Reason::AlreadyExists => "already exists".to_owned(),
        Reason::TimedOut => "timed out".to_owned(),
        Reason::ConnectionRefused => "connection refused".to_owned(),
        Reason::ConnectionReset => "connection reset".to_owned(),
        Reason::HostUnreachable => "host unreachable".to_owned(),
        Reason::NameResolution => "host name could not be resolved".to_owned(),
        // 服务器给的原始信息（含覆盖写失败时残留的临时文件路径）原样带出，spec/mcp.md §3.5、§9。
        Reason::Other { detail } => detail.clone(),
    }
}

/// 是不是"路径不存在"。逐级创建目录与重命名前的存在性检查靠它区分"不存在"与其他失败。
pub(super) fn is_not_found(e: &CoreError) -> bool {
    matches!(
        e,
        CoreError::User(Message::SftpOpFailed {
            reason: Reason::NotFound,
            ..
        })
    )
}

/// 把 core 在无界面会话上的 SFTP / exec 调用的错误映射成错误码。
///
/// `unsupported` 是 `NotSupported` 时给 AI 的说明：同一个错误码在不同操作下原因不同
/// （例如覆盖写入时是服务器缺 posix-rename 扩展）。
pub(super) fn remote_error(e: &CoreError, unsupported: &'static str) -> ToolError {
    match e {
        // 无界面会话是 MCP 自己管理的：调用中途它没了（连接被断开、借用的连接被用户关掉）属于中断。
        CoreError::NoSuchSession(_) => ToolError::new(
            "INTERRUPTED",
            "The connection used by this call is gone (it was closed or lost). Call again: Kanso \
             will reconnect if the session's access level allows it.",
        ),
        CoreError::NoSuchProfile(_) => ToolError::new("NOT_FOUND", "The session no longer exists."),
        CoreError::NotSupported => ToolError::new("UNSUPPORTED", unsupported),
        CoreError::User(Message::SftpFileExists { path }) => {
            ToolError::new("CONFLICT", format!("{path} already exists."))
        }
        CoreError::User(Message::SftpOpFailed { op, path, reason }) => {
            if *reason == Reason::AlreadyExists
                && matches!(op, SftpOp::Mkdir | SftpOp::Rename | SftpOp::CreateFile)
            {
                return ToolError::new("CONFLICT", format!("{path} already exists."));
            }
            ToolError::new(
                "REMOTE_ERROR",
                format!("SFTP {op:?} failed for {path}: {}.", reason_text(reason)),
            )
        }
        CoreError::User(Message::SftpSubsystemFailed { reason, .. }) => ToolError::new(
            "REMOTE_ERROR",
            format!(
                "The server did not open the SFTP subsystem: {}.",
                reason_text(reason)
            ),
        ),
        CoreError::User(Message::SshConnectionLost { .. }) => ToolError::new(
            "INTERRUPTED",
            "The SSH connection was lost during the call.",
        ),
        CoreError::User(m) => ToolError::new(
            "REMOTE_ERROR",
            format!("The operation failed ({}).", m.key()),
        ),
        other => {
            tracing::warn!("mcp remote call failed: {other}");
            internal_error()
        }
    }
}

/// 建立连接（`mcp_connect` / `mcp_open_session`）失败的错误映射。
pub(super) fn connect_error(e: &CoreError) -> ToolError {
    match e {
        CoreError::NoSuchProfile(_) => ToolError::new("NOT_FOUND", "The session no longer exists."),
        CoreError::NotSupported => ToolError::new(
            "UNSUPPORTED",
            "This kind of session cannot be used for this operation.",
        ),
        CoreError::User(m) => {
            let detail = match m {
                Message::SshConnectFailed { endpoint, reason } => {
                    format!("{}: {}", endpoint.host_port(), reason_text(reason))
                }
                other => other.key().to_owned(),
            };
            ToolError::new(
                "CONNECTION_FAILED",
                format!(
                    "Could not connect to the server ({detail}; message key {}). Check that the \
                     server is reachable and that the session's credentials work; the user can \
                     see the details by opening the session in Kanso.",
                    m.key()
                ),
            )
        }
        other => {
            tracing::warn!("mcp connect failed: {other}");
            internal_error()
        }
    }
}

/// 完全级别新建连接时需要人参与（spec/mcp.md §2.4）：说明具体原因和处理办法，让 AI 能转告用户。
pub(super) fn needs_user(need: UserNeed) -> ToolError {
    let reason = match need {
        UserNeed::KeyboardInteractive => {
            "the server asks for keyboard-interactive authentication (a verification code, MFA, \
             or a password prompt)"
        }
        UserNeed::Password => "no password is saved for this session",
        UserNeed::KeyPassphrase => "the private key is protected by a passphrase that is not saved",
        UserNeed::UnknownHostKey => "the server's host key has not been seen before",
        UserNeed::ChangedHostKey => "the server's host key has changed since it was last trusted",
    };
    ToolError::new(
        "NEEDS_USER",
        format!(
            "Connecting needs the user: {reason}. Kanso never prompts the user on behalf of MCP. \
             Ask the user to open this session in Kanso first; once it is connected, call again."
        ),
    )
}

/// 只读级别没有可用连接（spec/mcp.md §2.4）。
pub(super) fn not_connected() -> ToolError {
    ToolError::new(
        "NOT_CONNECTED",
        "This session has read-only MCP access, which never opens a new connection, and it has no \
         usable connection right now. Ask the user to open this session in Kanso first, then call \
         again.",
    )
}

// ---- exec 结果 ----

/// 把 `mcp_exec` 的结果映射成工具结果。超时是错误码 `TIMEOUT`，但仍带已收到的输出（放在错误 JSON 的顶层）。
pub(super) fn exec_result(outcome: &ExecOutcome, timeout_secs: u64) -> Result<Value, ToolError> {
    let mut obj = Map::new();
    obj.insert("stdout".to_owned(), capture_object(&outcome.stdout));
    obj.insert("stderr".to_owned(), capture_object(&outcome.stderr));
    obj.insert("exit_status".to_owned(), json!(outcome.exit_status));
    if outcome.timed_out {
        return Err(ToolError::with_details(
            "TIMEOUT",
            format!(
                "The command did not finish within {timeout_secs} seconds. Kanso closed the \
                 channel, but the remote process may still be running: many servers do not stop \
                 the process when the channel closes. The output received so far is included."
            ),
            obj,
        ));
    }
    Ok(Value::Object(obj))
}

#[cfg(test)]
mod tests {
    use kanso_core::mcp::CapturedText;
    use kanso_types::profile::EncodingLabel;

    use super::*;

    fn gbk() -> TextEncoding {
        TextEncoding::resolve(&EncodingLabel::new("gbk"))
    }

    fn entry(is_dir: bool, is_symlink: bool) -> DirEntry {
        DirEntry {
            name: "x".into(),
            path: "/x".into(),
            is_dir,
            is_symlink,
            size: 0,
            mtime_ms: None,
            permissions: Some(0o100644),
            owner: None,
            group: None,
        }
    }

    #[test]
    fn remove_never_rmdirs_a_symlink_and_never_recurses() {
        assert_eq!(remove_action(&entry(true, false)), RemoveAction::Rmdir);
        assert_eq!(
            remove_action(&entry(false, false)),
            RemoveAction::RemoveFile
        );
        // 指向目录的符号链接：DirEntry::is_dir 可能为真，但只能按链接本身删除。
        assert_eq!(remove_action(&entry(true, true)), RemoveAction::RemoveFile);
    }

    #[test]
    fn cursor_roundtrips_and_rejects_garbage() {
        assert_eq!(decode_cursor(&encode_cursor(1000)).unwrap(), 1000);
        assert_eq!(decode_cursor("x1").unwrap_err().code, "INVALID_ARGUMENT");
        assert_eq!(decode_cursor("c-1").unwrap_err().code, "INVALID_ARGUMENT");
        assert_eq!(decode_cursor("").unwrap_err().code, "INVALID_ARGUMENT");
    }

    #[test]
    fn prefixes_split_absolute_and_relative_paths() {
        assert_eq!(path_prefixes("/a/b//c/"), ["/a", "/a/b", "/a/b/c"]);
        assert_eq!(path_prefixes("a/./b"), ["a", "a/b"]);
        assert!(path_prefixes("/").is_empty());
    }

    #[test]
    fn text_decoding_rejects_nul_and_invalid_utf8() {
        let utf8 = TextEncoding::utf8();
        assert_eq!(decode_text(utf8, "héllo".as_bytes()).unwrap(), "héllo");
        assert_eq!(
            decode_text(utf8, b"a\0b").unwrap_err().code,
            "INVALID_ARGUMENT"
        );
        assert_eq!(
            decode_text(utf8, &[0xff, 0xfe]).unwrap_err().code,
            "INVALID_ARGUMENT"
        );
    }

    #[test]
    fn text_decoding_honours_the_session_encoding() {
        // "中" 的 GBK 编码是 D6 D0。
        assert_eq!(decode_text(gbk(), &[0xd6, 0xd0]).unwrap(), "中");
        // 末尾被截断的多字节字符不能被静默丢掉。
        assert!(decode_text(gbk(), &[b'a', 0xd6]).is_err());
    }

    #[test]
    fn write_content_converts_and_validates() {
        let utf8 = TextEncoding::utf8();
        assert_eq!(
            decode_write_content(ContentEncoding::Text, utf8, "中").unwrap(),
            "中".as_bytes()
        );
        assert_eq!(
            decode_write_content(ContentEncoding::Text, gbk(), "中").unwrap(),
            [0xd6, 0xd0]
        );
        // GBK 里没有 emoji：不能悄悄写成 `?`。
        assert_eq!(
            decode_write_content(ContentEncoding::Text, gbk(), "😀")
                .unwrap_err()
                .code,
            "INVALID_ARGUMENT"
        );
        assert_eq!(
            decode_write_content(ContentEncoding::Base64, utf8, "AAEC/w==").unwrap(),
            [0, 1, 2, 255]
        );
        assert_eq!(
            decode_write_content(ContentEncoding::Base64, utf8, "!!!")
                .unwrap_err()
                .code,
            "INVALID_ARGUMENT"
        );
    }

    #[test]
    fn write_content_size_limit_is_enforced_on_decoded_bytes() {
        let utf8 = TextEncoding::utf8();
        let exact = vec![b'a'; WRITE_MAX_BYTES];
        assert!(
            decode_write_content(
                ContentEncoding::Text,
                utf8,
                &String::from_utf8(exact.clone()).unwrap()
            )
            .is_ok()
        );
        let over = String::from_utf8(vec![b'a'; WRITE_MAX_BYTES + 1]).unwrap();
        assert_eq!(
            decode_write_content(ContentEncoding::Text, utf8, &over)
                .unwrap_err()
                .code,
            "TOO_LARGE"
        );
        let b64_over = encode_base64(&vec![0u8; WRITE_MAX_BYTES + 1]);
        assert_eq!(
            decode_write_content(ContentEncoding::Base64, utf8, &b64_over)
                .unwrap_err()
                .code,
            "TOO_LARGE"
        );
        let b64_exact = encode_base64(&exact);
        assert!(decode_write_content(ContentEncoding::Base64, utf8, &b64_exact).is_ok());
    }

    #[test]
    fn estimated_size_matches_decoded_size() {
        assert_eq!(estimated_size(ContentEncoding::Base64, "AAEC/w=="), 4);
        assert_eq!(estimated_size(ContentEncoding::Base64, "AAEC"), 3);
        assert_eq!(estimated_size(ContentEncoding::Text, "中"), 3);
    }

    #[test]
    fn permissions_are_octal_and_drop_file_type_bits() {
        assert_eq!(entry_json(&entry(false, false))["permissions"], "0644");
    }

    #[test]
    fn exec_timeout_keeps_output_in_details() {
        let outcome = ExecOutcome {
            stdout: CapturedText {
                text: "partial".into(),
                original_bytes: 7,
                truncated: false,
                cleared: false,
            },
            timed_out: true,
            ..ExecOutcome::default()
        };
        let err = exec_result(&outcome, 5).unwrap_err();
        assert_eq!(err.code, "TIMEOUT");
        assert!(err.message.contains("may still be running"));
        let details = err.details.unwrap();
        assert_eq!(details["stdout"]["text"], "partial");
        assert_eq!(details["exit_status"], Value::Null);
    }
}
