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

/// 在解码前检测握手；跨读取的候选头先暂存，不泄漏给终端。
#[derive(Default)]
pub struct Sentinel {
    pending: Vec<u8>,
    shown: usize,
    #[cfg(test)]
    pub(crate) compactions: usize,
}

impl Sentinel {
    /// 手动传输开始时释放尚未显示的候选握手字节。
    pub fn flush(&mut self) -> Vec<u8> {
        let bytes = self.pending.split_off(self.shown);
        self.pending.clear();
        self.shown = 0;
        bytes
    }

    /// 普通字节追加至 terminal；识别后丢弃本批剩余字节。
    pub fn feed(&mut self, bytes: &[u8], terminal: &mut Vec<u8>) -> Option<Handshake> {
        self.pending.extend_from_slice(bytes);
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
