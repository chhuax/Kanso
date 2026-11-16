//! Direct 版的 [`HostKeyStore`] 实现：读写 OpenSSH 格式的 `known_hosts` 文件（ssh.md §3）。
//!
//! 与系统自带的 ssh 共用同一个文件（生产路径是 `~/.ssh/known_hosts`，路径由调用方注入，
//! 测试给临时路径），所以格式、行号、哈希主机名都必须与 OpenSSH 兼容。
//!
//! 本模块只做存取，**不做策略**（accept-new、同算法变更拒绝都在 [`crate::hostkey`]）。
//!
//! 几个有意为之的取舍：
//! - **写入主机名一律明文，不哈希**：与 OpenSSH 的默认值 `HashKnownHosts no` 一致，
//!   用户用编辑器打开文件、按行号定位旧记录（ssh.md §3）时才看得懂。读取时仍支持别人写入的哈希行。
//! - **`@cert-authority` 行直接跳过**：本版本不支持主机证书。
//! - **`@revoked` 行不参与 [`HostKeyStore::lookup`]**（trait 无法表达"吊销"），
//!   由 [`KnownHostsFile::is_revoked`] 单独查询，握手流程用它拒绝被吊销的密钥。
//! - **`replace_all` 不碰 `@revoked` / `@cert-authority` 行**：吊销记录是安全信息，
//!   不应因为用户接受了新密钥就被抹掉。

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;

use crate::{HostKeyLocation, HostKeyStore, HostKeyStoreError, KnownHostKey};

/// 行首标记。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Marker {
    None,
    CertAuthority,
    Revoked,
}

/// 解析出的一条记录，附带主机字段在原行里的字节范围（供 `replace_all` 就地改写）。
struct ParsedLine {
    marker: Marker,
    hosts_start: usize,
    hosts_end: usize,
    key_type: String,
    key: Vec<u8>,
}

/// 按空格 / 制表符切字段，返回每个字段在行内的字节范围。
fn field_ranges(line: &str, max: usize) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() && out.len() < max {
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        while i < bytes.len() && bytes[i] != b' ' && bytes[i] != b'\t' {
            i += 1;
        }
        out.push((start, i));
    }
    out
}

/// 解析一行；注释、空行、无法解析的行返回 `None`（调用方照常计行号）。
fn parse_line(line: &str) -> Option<ParsedLine> {
    let line = line.trim_end_matches('\r');
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let f = field_ranges(line, 4);
    let (marker, base) = match f.first() {
        Some(&(s, e)) if line[s..e].starts_with('@') => {
            let m = match &line[s..e] {
                "@cert-authority" => Marker::CertAuthority,
                "@revoked" => Marker::Revoked,
                _ => return None,
            };
            (m, 1)
        }
        _ => (Marker::None, 0),
    };
    let hosts = *f.get(base)?;
    let ty = *f.get(base + 1)?;
    let b64 = *f.get(base + 2)?;
    let key = B64.decode(&line[b64.0..b64.1]).ok()?;
    if key.is_empty() {
        return None;
    }
    Some(ParsedLine {
        marker,
        hosts_start: hosts.0,
        hosts_end: hosts.1,
        key_type: line[ty.0..ty.1].to_owned(),
        key,
    })
}

/// `known_hosts` 里用来匹配的主机串：22 端口写裸主机名，其他端口写 `[host]:port`（与 OpenSSH 兼容）。
fn host_token(host: &str, port: u16) -> String {
    let h = host.to_ascii_lowercase();
    if port == 22 {
        h
    } else {
        format!("[{h}]:{port}")
    }
}

/// 只含 `*` / `?` 通配的简单匹配（不区分大小写由调用方先统一小写）。
fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    // 最近一个 `*` 的位置与它当前吞到的文本位置，用于回溯。
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((sp, st)) = star {
            p = sp + 1;
            t = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

/// 哈希主机名（`|1|salt|hash`，HMAC-SHA1）是否匹配。
fn hashed_matches(item: &str, token: &str) -> bool {
    let mut it = item.split('|');
    // 形如 "|1|salt|hash"：切开后依次是 ""、"1"、salt、hash。
    if it.next() != Some("") || it.next() != Some("1") {
        return false;
    }
    let (Some(salt), Some(hash), None) = (it.next(), it.next(), it.next()) else {
        return false;
    };
    let (Ok(salt), Ok(hash)) = (B64.decode(salt), B64.decode(hash)) else {
        return false;
    };
    let Ok(mut mac) = <Hmac<Sha1> as KeyInit>::new_from_slice(&salt) else {
        return false;
    };
    mac.update(token.as_bytes());
    // verify_slice 是常量时间比较。
    mac.verify_slice(&hash).is_ok()
}

fn has_wildcard(s: &str) -> bool {
    s.contains('*') || s.contains('?')
}

fn item_matches(pattern: &str, token: &str) -> bool {
    if pattern.starts_with("|1|") {
        hashed_matches(pattern, token)
    } else {
        glob_match(pattern.to_ascii_lowercase().as_bytes(), token.as_bytes())
    }
}

/// 逗号分隔的主机模式列表是否匹配：任一否定项命中则整体不匹配；否则任一肯定项命中即匹配。
fn list_matches(hosts: &str, token: &str) -> bool {
    let mut matched = false;
    for item in hosts.split(',').filter(|i| !i.is_empty()) {
        match item.strip_prefix('!') {
            Some(neg) => {
                if item_matches(neg, token) {
                    return false;
                }
            }
            None => {
                if item_matches(item, token) {
                    matched = true;
                }
            }
        }
    }
    matched
}

/// `replace_all` 时这一项能否被单独移除：只有"精确写着这个主机"或"哈希恰好是这个主机"的项可以；
/// 通配项（如 `*.corp`）还管着别的主机，不能拆。
fn removable_item(item: &str, token: &str) -> bool {
    if item.starts_with('!') {
        return false;
    }
    if item.starts_with("|1|") {
        return hashed_matches(item, token);
    }
    !has_wildcard(item) && item.eq_ignore_ascii_case(token)
}

enum LineAction {
    Keep,
    Drop,
    Replace(String),
}

/// 决定 `replace_all` 对一行的处理。
///
/// 同一行列了多个主机时，只从该行移除这个主机的模式，其余主机的记录原样保留
/// （整行删掉会让用户无关的主机丢记录，下次连接被当成未知主机）。
/// 如果移除精确项之后该行仍通过通配项命中这个主机，就整行删除：
/// 否则 `lookup` 还会返回那条旧密钥，刚接受的新密钥会立刻再被判为"变更"，用户陷入死循环。
fn rewrite_line(raw: &str, token: &str) -> LineAction {
    let Some(p) = parse_line(raw) else {
        return LineAction::Keep;
    };
    if p.marker != Marker::None {
        return LineAction::Keep;
    }
    let hosts = &raw[p.hosts_start..p.hosts_end];
    if !list_matches(hosts, token) {
        return LineAction::Keep;
    }
    let remaining: Vec<&str> = hosts
        .split(',')
        .filter(|i| !i.is_empty() && !removable_item(i, token))
        .collect();
    let has_positive = remaining.iter().any(|i| !i.starts_with('!'));
    if !has_positive || list_matches(&remaining.join(","), token) {
        return LineAction::Drop;
    }
    let mut out = String::with_capacity(raw.len());
    out.push_str(&raw[..p.hosts_start]);
    out.push_str(&remaining.join(","));
    out.push_str(&raw[p.hosts_end..]);
    LineAction::Replace(out)
}

fn io_err(path: &Path, e: &io::Error) -> HostKeyStoreError {
    HostKeyStoreError::Io {
        path: path.display().to_string(),
        detail: e.to_string(),
    }
}

/// 读写 OpenSSH `known_hosts` 文件的 [`HostKeyStore`]。
///
/// 调用约束：
/// - 路径由调用方给出（生产为 `~/.ssh/known_hosts`），本类型不读环境变量。
/// - 方法是阻塞 IO，kanso-ssh 的握手流程在 `spawn_blocking` 里调用；自行使用时同样不要在 async 线程直接调。
/// - 进程内用一把互斥锁串行化写入，防止同时打开同一主机的多个标签互相覆盖；
///   跨进程（例如用户同时在跑系统 ssh）与 OpenSSH 一样没有文件锁，`replace_all` 用原子 rename，
///   最坏情况是丢失对方刚追加的一行，不会写出半截文件。
pub struct KnownHostsFile {
    path: PathBuf,
    write_lock: Mutex<()>,
}

impl KnownHostsFile {
    /// 指向给定路径；文件和目录不存在没关系，首次写入时创建。
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            write_lock: Mutex::new(()),
        }
    }

    /// 文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        // 锁里只有文件 IO，没有会被中毒破坏的内存状态。
        self.write_lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 读取全文；文件不存在按空文件处理（ssh.md §3：当作未知主机）。
    fn read_all(&self) -> Result<String, HostKeyStoreError> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(io_err(&self.path, &e)),
        }
    }

    fn lookup_text(&self, text: &str, host: &str, port: u16) -> Vec<KnownHostKey> {
        let token = host_token(host, port);
        let path = self.path.display().to_string();
        let mut out = Vec::new();
        // 行号从 1 开始，注释行与空行都计数，与编辑器看到的一致（ssh.md §3）。
        for (idx, raw) in text.split('\n').enumerate() {
            let Some(p) = parse_line(raw) else { continue };
            if p.marker != Marker::None || !list_matches(&raw[p.hosts_start..p.hosts_end], &token) {
                continue;
            }
            out.push(KnownHostKey {
                key_type: p.key_type,
                key: p.key,
                location: Some(HostKeyLocation {
                    path: path.clone(),
                    line: u32::try_from(idx + 1).unwrap_or(u32::MAX),
                }),
            });
        }
        out
    }

    /// 该主机的这把密钥是否被 `@revoked` 行吊销。
    ///
    /// 被吊销的密钥必须拒绝连接且没有"接受"选项，所以握手流程在策略判断之前先调它。
    /// 与 OpenSSH 一致，只要有一行 `@revoked` 命中该主机并且密钥 blob 相同就算吊销。
    pub fn is_revoked(&self, host: &str, port: u16, key: &[u8]) -> Result<bool, HostKeyStoreError> {
        let text = self.read_all()?;
        let token = host_token(host, port);
        for raw in text.split('\n') {
            let Some(p) = parse_line(raw) else { continue };
            if p.marker == Marker::Revoked
                && p.key == key
                && list_matches(&raw[p.hosts_start..p.hosts_end], &token)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn ensure_parent_dir(&self) -> Result<(), HostKeyStoreError> {
        let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) else {
            return Ok(());
        };
        if parent.exists() {
            return Ok(());
        }
        let mut b = fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            // 新建的 ~/.ssh 必须是 0700，否则系统 ssh 会因权限过宽拒绝使用其中的文件。
            b.mode(0o700);
        }
        b.create(parent).map_err(|e| io_err(parent, &e))
    }

    fn format_line(host: &str, port: u16, key_type: &str, key: &[u8]) -> String {
        format!(
            "{} {} {}\n",
            host_token(host, port),
            key_type,
            B64.encode(key)
        )
    }

    /// 解析写入目标：路径本身是符号链接（常见于 dotfiles 管理）时写到链接指向的真实文件，
    /// 否则 rename 会把链接替换成普通文件。
    fn write_target(&self) -> PathBuf {
        match fs::symlink_metadata(&self.path) {
            Ok(m) if m.file_type().is_symlink() => {
                fs::canonicalize(&self.path).unwrap_or_else(|_| self.path.clone())
            }
            _ => self.path.clone(),
        }
    }
}

impl HostKeyStore for KnownHostsFile {
    fn lookup(&self, host: &str, port: u16) -> Result<Vec<KnownHostKey>, HostKeyStoreError> {
        let text = self.read_all()?;
        Ok(self.lookup_text(&text, host, port))
    }

    fn add(
        &self,
        host: &str,
        port: u16,
        key_type: &str,
        key: &[u8],
    ) -> Result<(), HostKeyStoreError> {
        let _g = self.guard();
        // 锁内重新读取：并发打开同一主机的多个标签可能都判定为未知，这里保证幂等。
        let existing = self.lookup_text(&self.read_all()?, host, port);
        if existing
            .iter()
            .any(|k| k.key_type == key_type && k.key == key)
        {
            return Ok(());
        }
        if existing.iter().any(|k| k.key_type == key_type) {
            return Err(HostKeyStoreError::Other {
                detail: format!("add() called for changed {key_type} key of {host}:{port}"),
            });
        }
        self.ensure_parent_dir()?;
        let target = self.write_target();
        let mut opts = fs::OpenOptions::new();
        opts.read(true).append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&target).map_err(|e| io_err(&target, &e))?;
        // 文件末尾没有换行时先补一个，否则新记录会接在上一行后面，把两条记录写坏。
        let len = f.metadata().map_err(|e| io_err(&target, &e))?.len();
        let mut line = String::new();
        if len > 0 {
            f.seek(SeekFrom::End(-1)).map_err(|e| io_err(&target, &e))?;
            let mut last = [0u8; 1];
            f.read_exact(&mut last).map_err(|e| io_err(&target, &e))?;
            if last[0] != b'\n' {
                line.push('\n');
            }
        }
        line.push_str(&Self::format_line(host, port, key_type, key));
        // 单次 write_all：O_APPEND 下一次写入是原子追加，不会与系统 ssh 的追加交错。
        f.write_all(line.as_bytes())
            .map_err(|e| io_err(&target, &e))
    }

    fn replace_all(
        &self,
        host: &str,
        port: u16,
        key_type: &str,
        key: &[u8],
    ) -> Result<(), HostKeyStoreError> {
        let _g = self.guard();
        let token = host_token(host, port);
        let text = self.read_all()?;
        let mut out = String::with_capacity(text.len() + 128);
        for seg in text.split_inclusive('\n') {
            let (body, nl) = match seg.strip_suffix('\n') {
                Some(b) => (b, "\n"),
                None => (seg, ""),
            };
            match rewrite_line(body, &token) {
                LineAction::Keep => out.push_str(seg),
                LineAction::Drop => {}
                LineAction::Replace(new) => {
                    out.push_str(&new);
                    out.push_str(nl);
                }
            }
        }
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&Self::format_line(host, port, key_type, key));

        self.ensure_parent_dir()?;
        let target = self.write_target();
        let dir = target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        // 临时文件放在同目录：rename 必须在同一文件系统内才是原子的。
        let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| io_err(dir, &e))?;
        tmp.write_all(out.as_bytes())
            .map_err(|e| io_err(tmp.path(), &e))?;
        // 保留原文件权限（用户可能有意设成 0644 之类）；新文件沿用 tempfile 的 0600。
        if let Ok(meta) = fs::metadata(&target) {
            tmp.as_file()
                .set_permissions(meta.permissions())
                .map_err(|e| io_err(tmp.path(), &e))?;
        }
        tmp.as_file()
            .sync_all()
            .map_err(|e| io_err(tmp.path(), &e))?;
        tmp.persist(&target)
            .map_err(|e| io_err(&target, &e.error))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: &[u8] = b"key-a-blob";
    const KEY_B: &[u8] = b"key-b-blob";

    fn b64(k: &[u8]) -> String {
        B64.encode(k)
    }

    fn setup(content: Option<&str>) -> (tempfile::TempDir, KnownHostsFile) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        if let Some(c) = content {
            fs::write(&path, c).unwrap();
        }
        (dir, KnownHostsFile::new(path))
    }

    fn hashed(host_token: &str, salt: &[u8]) -> String {
        let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(salt).unwrap();
        mac.update(host_token.as_bytes());
        let h = mac.finalize().into_bytes();
        format!("|1|{}|{}", B64.encode(salt), B64.encode(h))
    }

    #[test]
    fn missing_file_is_empty_and_unknown_is_recorded() {
        let (_d, kh) = setup(None);
        assert!(kh.lookup("example.com", 22).unwrap().is_empty());
        kh.add("example.com", 22, "ssh-ed25519", KEY_A).unwrap();
        let got = kh.lookup("example.com", 22).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].key_type, "ssh-ed25519");
        assert_eq!(got[0].key, KEY_A);
        let loc = got[0].location.as_ref().unwrap();
        assert_eq!(loc.line, 1);
        assert_eq!(loc.path, kh.path().display().to_string());
    }

    #[test]
    fn non_default_port_uses_bracket_format_and_is_isolated() {
        let (_d, kh) = setup(None);
        kh.add("example.com", 2222, "ssh-ed25519", KEY_A).unwrap();
        let text = fs::read_to_string(kh.path()).unwrap();
        assert!(
            text.starts_with("[example.com]:2222 ssh-ed25519 "),
            "{text}"
        );
        assert!(kh.lookup("example.com", 22).unwrap().is_empty());
        assert_eq!(kh.lookup("example.com", 2222).unwrap().len(), 1);
    }

    #[test]
    fn line_numbers_count_comments_and_blank_lines() {
        let content = format!(
            "# comment\n\nother ssh-ed25519 {a}\n   \nexample.com ssh-ed25519 {b}\n",
            a = b64(KEY_A),
            b = b64(KEY_B)
        );
        let (_d, kh) = setup(Some(&content));
        let got = kh.lookup("example.com", 22).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].location.as_ref().unwrap().line, 5);
    }

    #[test]
    fn unparsable_lines_are_skipped_but_counted() {
        let content = format!(
            "garbage line here\nexample.com ssh-ed25519 !!!notbase64!!!\nexample.com ssh-ed25519 {}\n",
            b64(KEY_A)
        );
        let (_d, kh) = setup(Some(&content));
        let got = kh.lookup("example.com", 22).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].location.as_ref().unwrap().line, 3);
    }

    #[test]
    fn crlf_lines_parse() {
        let content = format!("example.com ssh-ed25519 {}\r\n", b64(KEY_A));
        let (_d, kh) = setup(Some(&content));
        assert_eq!(kh.lookup("example.com", 22).unwrap().len(), 1);
    }

    #[test]
    fn hashed_hostname_matches() {
        let h = hashed("example.com", b"0123456789abcdef0123");
        let hp = hashed("[example.com]:2222", b"saltsaltsaltsaltsalt");
        let content = format!(
            "{h} ssh-ed25519 {a}\n{hp} ssh-ed25519 {b}\n",
            a = b64(KEY_A),
            b = b64(KEY_B)
        );
        let (_d, kh) = setup(Some(&content));
        let got = kh.lookup("example.com", 22).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].key, KEY_A);
        let got = kh.lookup("example.com", 2222).unwrap();
        assert_eq!(got[0].key, KEY_B);
        assert!(kh.lookup("other.com", 22).unwrap().is_empty());
        // 大小写不敏感：调用方传 Example.COM 也应匹配。
        assert_eq!(kh.lookup("Example.COM", 22).unwrap().len(), 1);
    }

    #[test]
    fn wildcard_and_negation_patterns() {
        let content = format!(
            "*.corp.example,!secret.corp.example ssh-ed25519 {}\nh?st ssh-ed25519 {}\n",
            b64(KEY_A),
            b64(KEY_B)
        );
        let (_d, kh) = setup(Some(&content));
        assert_eq!(kh.lookup("a.corp.example", 22).unwrap().len(), 1);
        assert!(kh.lookup("secret.corp.example", 22).unwrap().is_empty());
        assert_eq!(kh.lookup("host", 22).unwrap().len(), 1);
        assert!(kh.lookup("hoost", 22).unwrap().is_empty());
    }

    #[test]
    fn multiple_comma_separated_hosts_all_match() {
        let content = format!("h1,h2,10.0.0.1 ssh-ed25519 {}\n", b64(KEY_A));
        let (_d, kh) = setup(Some(&content));
        for h in ["h1", "h2", "10.0.0.1"] {
            assert_eq!(kh.lookup(h, 22).unwrap().len(), 1, "{h}");
        }
    }

    #[test]
    fn cert_authority_skipped_and_revoked_not_in_lookup() {
        let content = format!(
            "@cert-authority example.com ssh-ed25519 {a}\n@revoked example.com ssh-ed25519 {b}\n",
            a = b64(KEY_A),
            b = b64(KEY_B)
        );
        let (_d, kh) = setup(Some(&content));
        assert!(kh.lookup("example.com", 22).unwrap().is_empty());
        assert!(kh.is_revoked("example.com", 22, KEY_B).unwrap());
        assert!(!kh.is_revoked("example.com", 22, KEY_A).unwrap());
        assert!(!kh.is_revoked("other.com", 22, KEY_B).unwrap());
    }

    #[test]
    fn add_is_idempotent_and_refuses_changed_key() {
        let (_d, kh) = setup(None);
        kh.add("h", 22, "ssh-ed25519", KEY_A).unwrap();
        kh.add("h", 22, "ssh-ed25519", KEY_A).unwrap();
        assert_eq!(fs::read_to_string(kh.path()).unwrap().lines().count(), 1);
        assert!(kh.add("h", 22, "ssh-ed25519", KEY_B).is_err());
        assert_eq!(kh.lookup("h", 22).unwrap()[0].key, KEY_A);
        // 新算法可以追加。
        kh.add("h", 22, "ssh-rsa", KEY_B).unwrap();
        assert_eq!(kh.lookup("h", 22).unwrap().len(), 2);
    }

    #[test]
    fn add_inserts_missing_trailing_newline() {
        let content = format!("other ssh-ed25519 {}", b64(KEY_A));
        let (_d, kh) = setup(Some(&content));
        kh.add("h", 22, "ssh-ed25519", KEY_B).unwrap();
        assert_eq!(kh.lookup("other", 22).unwrap().len(), 1);
        let got = kh.lookup("h", 22).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].location.as_ref().unwrap().line, 2);
    }

    #[cfg(unix)]
    #[test]
    fn creates_missing_directory_and_file_with_private_modes() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let kh = KnownHostsFile::new(d.path().join(".ssh").join("known_hosts"));
        kh.add("h", 22, "ssh-ed25519", KEY_A).unwrap();
        let dm = fs::metadata(d.path().join(".ssh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let fm = fs::metadata(kh.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(dm, 0o700);
        assert_eq!(fm, 0o600);
    }

    #[test]
    fn replace_all_removes_every_record_of_host_keeps_others() {
        let content = format!(
            "# header\nother ssh-ed25519 {o}\nh ssh-ed25519 {a}\nh ssh-rsa {b}\n[h]:2222 ssh-ed25519 {a}\n",
            o = b64(b"other"),
            a = b64(KEY_A),
            b = b64(KEY_B)
        );
        let (_d, kh) = setup(Some(&content));
        kh.replace_all("h", 22, "ecdsa-sha2-nistp256", b"new")
            .unwrap();
        let got = kh.lookup("h", 22).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].key_type, "ecdsa-sha2-nistp256");
        assert_eq!(got[0].key, b"new");
        // 其他主机、注释、其他端口的行原样保留。
        let text = fs::read_to_string(kh.path()).unwrap();
        assert!(text.starts_with("# header\nother ssh-ed25519 "));
        assert_eq!(kh.lookup("other", 22).unwrap().len(), 1);
        assert_eq!(kh.lookup("h", 2222).unwrap().len(), 1);
    }

    #[test]
    fn replace_all_strips_only_this_host_from_multi_host_line() {
        let content = format!("h1,h2 ssh-ed25519 {} comment\n", b64(KEY_A));
        let (_d, kh) = setup(Some(&content));
        kh.replace_all("h1", 22, "ssh-ed25519", KEY_B).unwrap();
        let text = fs::read_to_string(kh.path()).unwrap();
        assert!(
            text.starts_with(&format!("h2 ssh-ed25519 {} comment\n", b64(KEY_A))),
            "{text}"
        );
        assert_eq!(kh.lookup("h2", 22).unwrap()[0].key, KEY_A);
        assert_eq!(kh.lookup("h1", 22).unwrap()[0].key, KEY_B);
    }

    #[test]
    fn replace_all_handles_hashed_and_wildcard_records() {
        let h = hashed("h", b"0123456789abcdef0123");
        let content = format!(
            "{h} ssh-ed25519 {a}\n*.x,h ssh-rsa {b}\nkeep ssh-ed25519 {a}\n",
            a = b64(KEY_A),
            b = b64(KEY_B)
        );
        let (_d, kh) = setup(Some(&content));
        kh.replace_all("h", 22, "ssh-ed25519", b"new").unwrap();
        let got = kh.lookup("h", 22).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].key, b"new");
        // 通配项 `*.x` 还管着别的主机，这一行里只摘掉精确的 `h`。
        assert_eq!(kh.lookup("a.x", 22).unwrap().len(), 1);
        assert_eq!(kh.lookup("keep", 22).unwrap().len(), 1);
    }

    #[test]
    fn replace_all_drops_line_that_still_matches_via_wildcard() {
        let content = format!("h* ssh-ed25519 {}\n", b64(KEY_A));
        let (_d, kh) = setup(Some(&content));
        kh.replace_all("h1", 22, "ssh-ed25519", KEY_B).unwrap();
        let got = kh.lookup("h1", 22).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].key, KEY_B);
    }

    #[test]
    fn replace_all_leaves_revoked_lines() {
        let content = format!("@revoked h ssh-ed25519 {}\n", b64(KEY_A));
        let (_d, kh) = setup(Some(&content));
        kh.replace_all("h", 22, "ssh-ed25519", KEY_B).unwrap();
        assert!(kh.is_revoked("h", 22, KEY_A).unwrap());
        assert_eq!(kh.lookup("h", 22).unwrap()[0].key, KEY_B);
    }

    #[cfg(unix)]
    #[test]
    fn replace_all_preserves_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let content = format!("h ssh-ed25519 {}\n", b64(KEY_A));
        let (_d, kh) = setup(Some(&content));
        fs::set_permissions(kh.path(), fs::Permissions::from_mode(0o644)).unwrap();
        kh.replace_all("h", 22, "ssh-ed25519", KEY_B).unwrap();
        let mode = fs::metadata(kh.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
    }

    #[cfg(unix)]
    #[test]
    fn replace_all_writes_through_symlink() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real_hosts");
        let link = d.path().join("known_hosts");
        fs::write(&real, format!("h ssh-ed25519 {}\n", b64(KEY_A))).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let kh = KnownHostsFile::new(&link);
        kh.replace_all("h", 22, "ssh-ed25519", KEY_B).unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(fs::read_to_string(&real).unwrap().contains(&b64(KEY_B)));
    }

    #[test]
    fn replace_all_creates_file_when_missing() {
        let d = tempfile::tempdir().unwrap();
        let kh = KnownHostsFile::new(d.path().join("sub").join("known_hosts"));
        kh.replace_all("h", 22, "ssh-ed25519", KEY_A).unwrap();
        assert_eq!(kh.lookup("h", 22).unwrap().len(), 1);
    }

    #[test]
    fn glob_basics() {
        assert!(glob_match(b"*", b"anything"));
        assert!(glob_match(b"a*c", b"abbbc"));
        assert!(!glob_match(b"a*c", b"abbbd"));
        assert!(glob_match(b"a?c", b"abc"));
        assert!(!glob_match(b"a?c", b"ac"));
        assert!(glob_match(b"*.x*", b"a.xyz"));
    }
}
