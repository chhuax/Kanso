const ZDLE: u8 = 0x18;

pub(crate) fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = (crc << 1) ^ if crc & 0x8000 != 0 { 0x1021 } else { 0 };
        }
    }
    crc
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb88320 } else { 0 };
        }
    }
    !crc
}

fn escape(bytes: &[u8], output: &mut Vec<u8>) {
    for byte in bytes {
        if *byte == 0x7f {
            output.extend_from_slice(&[ZDLE, b'l']);
        } else if *byte == 0xff {
            output.extend_from_slice(&[ZDLE, b'm']);
        } else if byte & 0x60 == 0 {
            output.extend_from_slice(&[ZDLE, byte ^ 0x40]);
        } else {
            output.push(*byte);
        }
    }
}

pub(crate) fn header(kind: u8, position: u32) -> Vec<u8> {
    let mut body = vec![kind];
    body.extend_from_slice(&position.to_le_bytes());
    body.extend_from_slice(&crc16(&body).to_be_bytes());
    let mut output = b"**\x18B".to_vec();
    for byte in body {
        output.extend_from_slice(format!("{byte:02x}").as_bytes());
    }
    output.extend_from_slice(b"\r\n");
    if !matches!(kind, 3 | 8) {
        output.push(0x11);
    }
    output
}

pub(crate) fn packet(bytes: &[u8], end: u8) -> Vec<u8> {
    let mut checked = bytes.to_vec();
    checked.push(end);
    let mut output = Vec::new();
    escape(bytes, &mut output);
    output.extend_from_slice(&[ZDLE, end]);
    escape(&crc16(&checked).to_be_bytes(), &mut output);
    output
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Frame {
    Header(u8, u32, bool),
    Data(Vec<u8>, u8),
    Cancel,
}

fn unescape(bytes: &[u8], cursor: &mut usize) -> Option<u8> {
    loop {
        let byte = *bytes.get(*cursor)?;
        *cursor += 1;
        if matches!(byte, 0x11 | 0x13 | 0x91 | 0x93) {
            continue;
        }
        if byte != ZDLE {
            return Some(byte);
        }
        let next = *bytes.get(*cursor)?;
        *cursor += 1;
        return Some(match next {
            b'l' => 0x7f,
            b'm' => 0xff,
            _ => next ^ 0x40,
        });
    }
}

pub(crate) fn parse(bytes: &mut Vec<u8>, data: Option<bool>) -> Result<Option<Frame>, String> {
    if bytes.windows(5).any(|part| part == [ZDLE; 5]) {
        bytes.clear();
        return Ok(Some(Frame::Cancel));
    }
    if let Some(wide) = data {
        let mut cursor = 0;
        let mut body = Vec::new();
        loop {
            while bytes
                .get(cursor)
                .is_some_and(|byte| matches!(byte, 0x11 | 0x13 | 0x91 | 0x93))
            {
                cursor += 1;
            }
            if bytes.get(cursor) == Some(&ZDLE)
                && bytes
                    .get(cursor + 1)
                    .is_some_and(|byte| (b'h'..=b'k').contains(byte))
            {
                let end = bytes[cursor + 1];
                cursor += 2;
                let mut check = Vec::new();
                for _ in 0..if wide { 4 } else { 2 } {
                    let Some(byte) = unescape(bytes, &mut cursor) else {
                        return Ok(None);
                    };
                    check.push(byte);
                }
                let mut checked = body.clone();
                checked.push(end);
                let valid = if wide {
                    check == crc32(&checked).to_le_bytes()
                } else {
                    check == crc16(&checked).to_be_bytes()
                };
                bytes.drain(..cursor);
                return if valid {
                    Ok(Some(Frame::Data(body, end)))
                } else {
                    Err("invalid data CRC".into())
                };
            }
            let Some(byte) = unescape(bytes, &mut cursor) else {
                return Ok(None);
            };
            body.push(byte);
            if body.len() > 8192 {
                return Err("data subpacket exceeds 8192 bytes".into());
            }
        }
    }
    let Some(start) = bytes.windows(2).position(|pair| pair == [b'*', ZDLE]) else {
        let keep = usize::from(bytes.last() == Some(&b'*'));
        bytes.drain(..bytes.len() - keep);
        return Ok(None);
    };
    bytes.drain(..start);
    let Some(format) = bytes.get(2).copied() else {
        return Ok(None);
    };
    let wide = format == b'C';
    let mut cursor = 3;
    let mut body = Vec::new();
    match format {
        b'B' => {
            if bytes.len() < 19 {
                return Ok(None);
            }
            for pair in bytes[3..17].chunks_exact(2) {
                let digit = |byte: u8| (byte as char).to_digit(16).map(|value| value as u8);
                let Some(high) = digit(pair[0]) else {
                    return Err("invalid hex header".into());
                };
                let Some(low) = digit(pair[1]) else {
                    return Err("invalid hex header".into());
                };
                body.push(high * 16 + low);
            }
            cursor = 17;
            // 行尾可跨读取；残留的 CR/LF/XON 会在下次扫描时丢弃。
            while bytes
                .get(cursor)
                .is_some_and(|byte| matches!(byte, 0x0d | 0x0a | 0x8a | 0x11))
            {
                cursor += 1;
            }
        }
        b'A' | b'C' => {
            for _ in 0..if wide { 9 } else { 7 } {
                let Some(byte) = unescape(bytes, &mut cursor) else {
                    return Ok(None);
                };
                body.push(byte);
            }
        }
        _ => return Err("unsupported header format".into()),
    }
    let valid = if wide {
        body[5..] == crc32(&body[..5]).to_le_bytes()
    } else {
        body[5..] == crc16(&body[..5]).to_be_bytes()
    };
    bytes.drain(..cursor);
    if !valid {
        return Err("invalid header CRC".into());
    }
    Ok(Some(Frame::Header(
        body[0],
        u32::from_le_bytes(body[1..5].try_into().expect("四字节位置")),
        wide,
    )))
}

/// 对端启动的收发方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handshake {
    /// 对端运行 sz，本端接收。
    Receive,
    /// 对端运行 rz，本端发送。
    Send,
}

/// 本端取消时，对端可能已经在链路上放入后续子包。只剥离已确认的协议边界。
enum CancelPhase {
    Frame(Option<bool>),
    Padding(u8),
}

struct CancelTail {
    bytes: Vec<u8>,
    before_cancel: usize,
    phase: CancelPhase,
    progressed: bool,
}

impl CancelTail {
    fn into_terminal(mut self) -> Vec<u8> {
        self.bytes.drain(..self.before_cancel);
        self.bytes
    }

    /// 返回 true 表示首个普通终端字节已经在缓冲区开头。
    fn terminal_ready(&mut self) -> bool {
        while !self.bytes.is_empty() {
            match self.phase {
                CancelPhase::Padding(ref mut backspaces) => {
                    // 对端取消序列末尾可以跟退格；部分 sz 还会收尾一个空 ZCRCE 子包。
                    let mut n = 0;
                    while let Some(&byte) = self.bytes.get(n) {
                        if byte == 8 && *backspaces < 10 {
                            *backspaces += 1;
                        } else if byte == ZDLE && *backspaces == 0 {
                            match self.bytes.get(n + 1).copied() {
                                Some(8 | ZDLE) => {}
                                None => {
                                    self.bytes.drain(..n);
                                    return false;
                                }
                                _ => break,
                            }
                        } else {
                            break;
                        }
                        n += 1;
                    }
                    self.bytes.drain(..n);
                    if self.bytes.is_empty() {
                        return false;
                    }
                    if self.bytes[0] != ZDLE {
                        return true;
                    }
                    if self.bytes.len() == 1 {
                        return false;
                    }
                    if self.bytes[1] != b'h' {
                        return true;
                    }
                    for wide in [false, true] {
                        let mut candidate = self.bytes.clone();
                        if matches!(parse(&mut candidate, Some(wide)), Ok(Some(Frame::Data(body,b'h'))) if body.is_empty())
                        {
                            self.bytes = candidate;
                            return true;
                        }
                    }
                    // 空包最多是 ZDLE、结束符和四个转义后的 CRC 字节。
                    return self.bytes.len() >= 10;
                }
                CancelPhase::Frame(crc) => {
                    if let Some(end) = self.bytes.windows(5).position(|p| p == [ZDLE; 5]) {
                        self.bytes.drain(..end + 5);
                        self.before_cancel = 0;
                        self.progressed = true;
                        self.phase = CancelPhase::Padding(0);
                        continue;
                    }
                    if self.bytes.iter().all(|b| *b == ZDLE) {
                        return false;
                    }
                    if crc.is_none() {
                        let start = self.bytes.iter().position(|b| *b != 0x11);
                        let Some(start) = start else { return false };
                        let prefix = &self.bytes[start..];
                        if prefix[0] != b'*' {
                            if prefix.iter().all(|b| *b == ZDLE) {
                                return false;
                            }
                            // 十六进制头后面的 XON 可能晚于头本身到达：它是流控字节，不属于终端输出。
                            self.bytes.drain(..start);
                            return true;
                        }
                        let header = &prefix[1..];
                        let header = header.strip_prefix(b"*").unwrap_or(header);
                        if header.first().is_some_and(|b| *b != ZDLE) {
                            return true;
                        }
                    }
                    // parse 在 CRC 出错时也会消费输入，验证成功之前须保留原字节。
                    let mut candidate = self.bytes.clone();
                    match parse(&mut candidate, crc) {
                        // ZDATA 之外，对端发完最后一个子包后可能紧接着发 ZEOF（11），
                        // 甚至 ZFIN（8）；校验通过的协议头照样剥离，不能当二进制漏给终端。
                        // 不剥离 ZFIN 之后可能出现的 "OO"：那只在我们回 ZFIN 之后才会发，
                        // 本端取消后不会回，没有把握的字节宁可交还终端。
                        Ok(Some(Frame::Header(10, _, wide))) => {
                            self.phase = CancelPhase::Frame(Some(wide))
                        }
                        Ok(Some(Frame::Header(8 | 11, _, _))) => {
                            self.phase = CancelPhase::Frame(None)
                        }
                        Ok(Some(Frame::Data(_, end))) => {
                            if matches!(end, b'h' | b'k') {
                                self.phase = CancelPhase::Frame(None);
                            }
                        }
                        Ok(None) => return false,
                        _ => return true,
                    }
                    self.bytes = candidate;
                    self.before_cancel = 0;
                    self.progressed = true;
                }
            }
        }
        false
    }
}

/// 在解码前检测握手；跨读取的候选头先暂存，不泄漏给终端。
#[derive(Default)]
pub struct Sentinel {
    pending: Vec<u8>,
    shown: usize,
    cancelled: Option<CancelTail>,
    #[cfg(test)]
    pub(crate) compactions: usize,
}

impl Sentinel {
    pub(crate) fn cancelled_receive(&mut self, bytes: Vec<u8>, crc: Option<bool>) {
        self.cancelled = Some(CancelTail {
            before_cancel: bytes.len(),
            bytes,
            phase: CancelPhase::Frame(crc),
            progressed: true,
        });
    }

    /// 待定字节需要一次空闲检查；没有待定字节时无需后台计时。
    pub fn cancel_tail_pending(&self) -> bool {
        self.cancelled
            .as_ref()
            .is_some_and(|tail| tail.bytes.len() > tail.before_cancel)
    }

    /// 对端可以直接退回 shell。若连续两次检查无已验证的协议进展，就交还待定字节。
    pub fn poll_cancel_tail(&mut self) -> Vec<u8> {
        if let Some(tail) = &mut self.cancelled
            && tail.bytes.len() > tail.before_cancel
            && !std::mem::take(&mut tail.progressed)
        {
            return self.cancelled.take().unwrap().into_terminal();
        }
        Vec::new()
    }

    /// 手动传输开始时释放尚未显示的候选握手字节。
    pub fn flush(&mut self) -> Vec<u8> {
        let mut bytes = self
            .cancelled
            .take()
            .map(CancelTail::into_terminal)
            .unwrap_or_default();
        bytes.extend_from_slice(&self.pending[self.shown..]);
        self.pending.clear();
        self.shown = 0;
        bytes
    }

    /// 普通字节追加至 terminal；识别后丢弃本批剩余字节。
    pub fn feed(&mut self, bytes: &[u8], terminal: &mut Vec<u8>) -> Option<Handshake> {
        if let Some(tail) = &mut self.cancelled {
            // 新的未定前缀开始计时。后续普通输出不能靠不断到达延长等待。
            if tail.bytes.len() == tail.before_cancel && !bytes.is_empty() {
                tail.progressed = true;
            }
            tail.bytes.extend_from_slice(bytes);
            if !tail.terminal_ready() {
                return None;
            }
            self.pending
                .extend(self.cancelled.take().unwrap().into_terminal());
        } else {
            self.pending.extend_from_slice(bytes);
        }
        let mut consumed = 0;
        loop {
            let Some(start) = self.pending[consumed..]
                .iter()
                .position(|byte| *byte == b'*')
            else {
                terminal.extend_from_slice(&self.pending[consumed..]);
                consumed = self.pending.len();
                break;
            };
            terminal.extend_from_slice(&self.pending[consumed..consumed + start]);
            consumed += start;
            let pending = &self.pending[consumed..];
            let prefix = b"**\x18B";
            if pending.len() < prefix.len() && prefix.starts_with(pending) {
                let visible = pending.len().min(2);
                terminal.extend_from_slice(&pending[self.shown..visible]);
                self.shown = visible;
                break;
            }
            if pending.starts_with(prefix) {
                if pending.len() < 20 {
                    break;
                }
                let mut candidate = pending.to_vec();
                match parse(&mut candidate, None) {
                    Ok(Some(Frame::Header(kind @ (0 | 1), _, _))) => {
                        self.pending.clear();
                        self.shown = 0;
                        terminal.extend_from_slice(b"\r\x1b[2K");
                        return Some(if kind == 0 {
                            Handshake::Receive
                        } else {
                            Handshake::Send
                        });
                    }
                    Err(_) => {
                        self.pending.clear();
                        self.shown = 0;
                        terminal.extend_from_slice(b"\r\x1b[2K");
                        return None;
                    }
                    _ => {}
                }
            }
            let byte = self.pending[consumed];
            consumed += 1;
            if self.shown == 0 {
                terminal.push(byte);
            } else {
                self.shown -= 1;
            }
        }
        if consumed != 0 {
            self.pending.drain(..consumed);
            #[cfg(test)]
            {
                self.compactions += 1;
            }
        }
        None
    }
}
