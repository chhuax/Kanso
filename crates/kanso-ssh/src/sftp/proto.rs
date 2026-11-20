//! SFTP v3 线格式（draft-ietf-secsh-filexfer-02）的编解码。
//!
//! 只实现本库用到的包类型；客户端（`client`）与测试服务器共用这里的常量与读写器，
//! 保证两端对协议的理解来自同一份代码。

use super::error::SftpError;

/// 本库说的协议版本。服务器回更低版本时拒绝：v3 之前没有 `READDIR` 的 attrs 与 `REALPATH`。
pub const PROTOCOL_VERSION: u32 = 3;

/// 单个包体的上限。
///
/// 对端报告更大的长度字段时直接判协议错误，而不是照长度分配内存：
/// 恶意或损坏的服务器只需一个 4 字节的长度就能让客户端分配 4 GiB。
/// 1 MiB 远大于 `READ` 能请求的最大块（见 `transfer::MAX_CHUNK`）加包头，也足够容纳一批 `NAME`。
pub const MAX_PACKET: usize = 1024 * 1024;

// 包类型。
pub const FXP_INIT: u8 = 1;
pub const FXP_VERSION: u8 = 2;
pub const FXP_OPEN: u8 = 3;
pub const FXP_CLOSE: u8 = 4;
pub const FXP_READ: u8 = 5;
pub const FXP_WRITE: u8 = 6;
pub const FXP_LSTAT: u8 = 7;
pub const FXP_FSTAT: u8 = 8;
pub const FXP_OPENDIR: u8 = 11;
pub const FXP_READDIR: u8 = 12;
pub const FXP_REMOVE: u8 = 13;
pub const FXP_MKDIR: u8 = 14;
pub const FXP_RMDIR: u8 = 15;
pub const FXP_REALPATH: u8 = 16;
pub const FXP_STAT: u8 = 17;
pub const FXP_RENAME: u8 = 18;
pub const FXP_SETSTAT: u8 = 9;
/// 扩展请求（`posix-rename@openssh.com` 等），请求体是扩展名 + 扩展自己的数据。
pub const FXP_EXTENDED: u8 = 200;
pub const FXP_STATUS: u8 = 101;
pub const FXP_HANDLE: u8 = 102;
pub const FXP_DATA: u8 = 103;
pub const FXP_NAME: u8 = 104;
pub const FXP_ATTRS: u8 = 105;

/// OpenSSH 的原子覆盖式改名扩展（OpenSSH `PROTOCOL` 文档）：与 POSIX `rename(2)` 语义一致，目标已存在时直接替换。
pub const EXT_POSIX_RENAME: &str = "posix-rename@openssh.com";

// STATUS 码。
pub const FX_OK: u32 = 0;
pub const FX_EOF: u32 = 1;
pub const FX_NO_SUCH_FILE: u32 = 2;
pub const FX_PERMISSION_DENIED: u32 = 3;
#[allow(dead_code)] // 测试服务器使用。
pub const FX_FAILURE: u32 = 4;
#[allow(dead_code)] // 测试服务器使用。
pub const FX_BAD_MESSAGE: u32 = 5;
pub const FX_OP_UNSUPPORTED: u32 = 8;

// OPEN 标志。
pub const OPEN_READ: u32 = 0x01;
pub const OPEN_WRITE: u32 = 0x02;
#[allow(dead_code)] // 测试服务器使用。
pub const OPEN_APPEND: u32 = 0x04;
pub const OPEN_CREAT: u32 = 0x08;
pub const OPEN_TRUNC: u32 = 0x10;
pub const OPEN_EXCL: u32 = 0x20;

// attrs 标志。
const ATTR_SIZE: u32 = 0x01;
const ATTR_UIDGID: u32 = 0x02;
const ATTR_PERMISSIONS: u32 = 0x04;
const ATTR_ACMODTIME: u32 = 0x08;
const ATTR_EXTENDED: u32 = 0x8000_0000;

/// `permissions` 里文件类型位的掩码与取值（与 POSIX `st_mode` 一致）。
pub const S_IFMT: u32 = 0o170_000;
pub const S_IFDIR: u32 = 0o040_000;
pub const S_IFREG: u32 = 0o100_000;
pub const S_IFLNK: u32 = 0o120_000;

/// 远端文件属性（v3：数字 uid/gid、秒级时间）。每个字段服务器都可以不提供。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileAttrs {
    /// 大小（字节）。
    pub size: Option<u64>,
    /// 属主 uid。
    pub uid: Option<u32>,
    /// 属组 gid。
    pub gid: Option<u32>,
    /// 权限位，含文件类型位。
    pub permissions: Option<u32>,
    /// 访问时间（Unix 秒）。
    pub atime: Option<u32>,
    /// 修改时间（Unix 秒）。
    pub mtime: Option<u32>,
}

impl FileAttrs {
    /// 是否目录。服务器没给权限位时无法判断，按"不是"处理。
    pub fn is_dir(&self) -> bool {
        self.permissions.is_some_and(|p| p & S_IFMT == S_IFDIR)
    }

    /// 是否符号链接（只有 LSTAT / READDIR 的结果才可能为真）。
    pub fn is_symlink(&self) -> bool {
        self.permissions.is_some_and(|p| p & S_IFMT == S_IFLNK)
    }

    /// 是否普通文件。
    pub fn is_regular(&self) -> bool {
        self.permissions.is_some_and(|p| p & S_IFMT == S_IFREG)
    }

    /// 编码到包体末尾。
    pub fn encode(&self, out: &mut Vec<u8>) {
        let mut flags = 0u32;
        if self.size.is_some() {
            flags |= ATTR_SIZE;
        }
        if self.uid.is_some() && self.gid.is_some() {
            flags |= ATTR_UIDGID;
        }
        if self.permissions.is_some() {
            flags |= ATTR_PERMISSIONS;
        }
        if self.atime.is_some() && self.mtime.is_some() {
            flags |= ATTR_ACMODTIME;
        }
        put_u32(out, flags);
        if let Some(s) = self.size {
            put_u64(out, s);
        }
        if flags & ATTR_UIDGID != 0 {
            put_u32(out, self.uid.unwrap_or(0));
            put_u32(out, self.gid.unwrap_or(0));
        }
        if let Some(p) = self.permissions {
            put_u32(out, p);
        }
        if flags & ATTR_ACMODTIME != 0 {
            put_u32(out, self.atime.unwrap_or(0));
            put_u32(out, self.mtime.unwrap_or(0));
        }
    }
}

pub fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

pub fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// 写一个 SSH string（长度前缀 + 字节）。
pub fn put_str(out: &mut Vec<u8>, s: &[u8]) {
    put_u32(out, s.len() as u32);
    out.extend_from_slice(s);
}

/// 起一个包：长度占位 + 类型（+ id）。`finish_packet` 回填长度。
pub fn begin_packet(ty: u8, id: Option<u32>, body_hint: usize) -> Vec<u8> {
    let mut p = Vec::with_capacity(9 + body_hint);
    p.extend_from_slice(&[0, 0, 0, 0, ty]);
    if let Some(id) = id {
        put_u32(&mut p, id);
    }
    p
}

/// 回填长度字段。
pub fn finish_packet(p: &mut [u8]) {
    let len = (p.len() - 4) as u32;
    p[..4].copy_from_slice(&len.to_be_bytes());
}

/// 包体读取器：越界读返回协议错误而不是 panic，对端发来的畸形数据不能让进程崩溃。
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

fn short() -> SftpError {
    SftpError::Protocol {
        detail: "truncated packet".into(),
    }
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    #[allow(dead_code)] // 测试服务器使用。
    pub fn u8(&mut self) -> Result<u8, SftpError> {
        let b = *self.buf.get(self.pos).ok_or_else(short)?;
        self.pos += 1;
        Ok(b)
    }

    pub fn u32(&mut self) -> Result<u32, SftpError> {
        let s = self.buf.get(self.pos..self.pos + 4).ok_or_else(short)?;
        self.pos += 4;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }

    pub fn u64(&mut self) -> Result<u64, SftpError> {
        let s = self.buf.get(self.pos..self.pos + 8).ok_or_else(short)?;
        self.pos += 8;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Ok(u64::from_be_bytes(a))
    }

    /// 读一个 SSH string 的原始字节。
    pub fn bytes(&mut self) -> Result<&'a [u8], SftpError> {
        let n = self.u32()? as usize;
        // 先比较再切片：`pos + n` 在 32 位目标上可能溢出。
        if n > self.remaining() {
            return Err(short());
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    /// 读一个 SSH string 并按 UTF-8 解码（非法字节用替换符，文件名不保证是合法 UTF-8）。
    pub fn string(&mut self) -> Result<String, SftpError> {
        Ok(String::from_utf8_lossy(self.bytes()?).into_owned())
    }

    pub fn attrs(&mut self) -> Result<FileAttrs, SftpError> {
        let flags = self.u32()?;
        let mut a = FileAttrs::default();
        if flags & ATTR_SIZE != 0 {
            a.size = Some(self.u64()?);
        }
        if flags & ATTR_UIDGID != 0 {
            a.uid = Some(self.u32()?);
            a.gid = Some(self.u32()?);
        }
        if flags & ATTR_PERMISSIONS != 0 {
            a.permissions = Some(self.u32()?);
        }
        if flags & ATTR_ACMODTIME != 0 {
            a.atime = Some(self.u32()?);
            a.mtime = Some(self.u32()?);
        }
        if flags & ATTR_EXTENDED != 0 {
            let n = self.u32()?;
            // 扩展属性对我们没有用，但必须按长度跳过，后面的字段才对得上。
            for _ in 0..n {
                self.bytes()?;
                self.bytes()?;
            }
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attrs_roundtrip() {
        let a = FileAttrs {
            size: Some(1 << 40),
            uid: Some(501),
            gid: Some(20),
            permissions: Some(S_IFREG | 0o644),
            atime: Some(1),
            mtime: Some(1_700_000_000),
        };
        let mut b = Vec::new();
        a.encode(&mut b);
        let got = Reader::new(&b).attrs().unwrap();
        assert_eq!(got, a);
        assert!(got.is_regular() && !got.is_dir());
    }

    #[test]
    fn empty_attrs_roundtrip() {
        let mut b = Vec::new();
        FileAttrs::default().encode(&mut b);
        assert_eq!(b, [0, 0, 0, 0]);
        assert_eq!(Reader::new(&b).attrs().unwrap(), FileAttrs::default());
    }

    #[test]
    fn truncated_input_is_an_error_not_a_panic() {
        assert!(Reader::new(&[0, 0, 0]).u32().is_err());
        // 长度字段声称有 10 字节，实际只有 1 字节。
        assert!(Reader::new(&[0, 0, 0, 10, 1]).bytes().is_err());
        // u32::MAX 长度不应溢出。
        assert!(Reader::new(&[255, 255, 255, 255, 1]).bytes().is_err());
    }

    #[test]
    fn extended_attrs_are_skipped() {
        let mut b = Vec::new();
        put_u32(&mut b, ATTR_SIZE | ATTR_EXTENDED);
        put_u64(&mut b, 7);
        put_u32(&mut b, 1);
        put_str(&mut b, b"k");
        put_str(&mut b, b"v");
        put_u32(&mut b, 0xdead);
        let mut r = Reader::new(&b);
        assert_eq!(r.attrs().unwrap().size, Some(7));
        assert_eq!(r.u32().unwrap(), 0xdead);
    }
}
