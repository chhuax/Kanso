use crate::wire::{Frame, Handshake, header, packet, parse};

/// 发送文件每次向调用方请求的最大块大小。
pub const BLOCK_SIZE: usize = 1024;

/// 一个待传输文件的元数据。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileInfo {
    /// 文件名，不包含路径。
    pub name: String,
    /// 已知的文件大小。
    pub size: Option<u64>,
}

/// 由调用方顺序执行的协议动作。
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// 原样写往会话。
    Send(Vec<u8>),
    /// 让用户选择发送文件。
    PickFiles,
    /// 提供下一个文件的元数据；没有文件时用 None。
    NextFile,
    /// 为这个文件选择保存位置。
    Offer(FileInfo),
    /// 提供至多 BLOCK_SIZE 字节，空块表示 EOF。
    Read,
    /// 按当前协议块大小读取文件。
    ReadBlock(usize),
    /// 不属于协议的原始终端输出。
    Terminal(Vec<u8>),
    /// 写入文件；成功后调用 written，失败后调用 skip。
    Write(Vec<u8>),
    /// 当前文件完成。
    FileDone,
    /// 当前文件被跳过。
    Skipped,
    /// 最后等待对方确认。
    WaitingConfirm,
    /// 完成；最初的错误优先于后续协议错误。
    Finished {
        /// 成功接收的文件数。
        received: u32,
        /// 跳过的文件数。
        skipped: u32,
        /// 最先发生的错误。
        error: Option<String>,
        /// 用户或对端取消。
        cancelled: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Files,
    Position,
    Ack,
    Next,
    Header,
    Offer,
    Data,
    Write(u8, usize),
    Skip,
    Finish,
    Over,
}

/// ZMODEM 收发状态机。只保留当前读取批次和一个数据子包。
pub struct Machine {
    direction: Handshake,
    state: State,
    input: Vec<u8>,
    data_crc: Option<bool>,
    header_kind: u8,
    position: u32,
    file_accepted: bool,
    received: u32,
    skipped: u32,
    first_error: Option<String>,
    idle_ms: u64,
}

impl Machine {
    /// 由哨兵识别的握手创建状态机。
    pub fn new(direction: Handshake) -> Self {
        Self {
            direction,
            state: State::Header,
            input: Vec::new(),
            data_crc: None,
            header_kind: 0,
            position: 0,
            file_accepted: false,
            received: 0,
            skipped: 0,
            first_error: None,
            idle_ms: 0,
        }
    }

    /// 开始握手。发送方向先选择文件，接收方向宣告 CRC32 接收能力。
    pub fn start(&mut self) -> Vec<Action> {
        if self.direction == Handshake::Send {
            self.state = State::Files;
            vec![Action::PickFiles]
        } else {
            vec![Action::Send(header(1, 0x23000000))]
        }
    }

    /// 提供下一个待发送文件，None 表示整批文件已读完。
    pub fn next_file(&mut self, file: Option<FileInfo>) -> Vec<Action> {
        if self.state == State::Over {
            return vec![];
        }
        self.position = 0;
        if let Some(file) = file {
            self.state = State::Position;
            let mut metadata = file.name.into_bytes();
            metadata.push(0);
            if let Some(size) = file.size {
                metadata.extend_from_slice(size.to_string().as_bytes());
            }
            metadata.push(0);
            vec![
                Action::Send(header(4, 0)),
                Action::Send(packet(&metadata, b'k')),
            ]
        } else {
            self.state = State::Finish;
            vec![Action::WaitingConfirm, Action::Send(header(8, 0))]
        }
    }

    /// 保存位置已准备好，要求从零开始；本次不实现续传。
    pub fn accept(&mut self) -> Vec<Action> {
        self.state = State::Header;
        self.input.clear();
        self.position = 0;
        self.file_accepted = true;
        vec![Action::Send(header(9, 0))]
    }

    /// 跳过当前文件。磁盘错误保留至整批汇总，仍可继续接收其他文件。
    pub fn skip(&mut self, error: Option<String>) -> Vec<Action> {
        if self.first_error.is_none() {
            self.first_error = error;
        }
        self.skipped += 1;
        self.state = State::Skip;
        self.file_accepted = false;
        self.data_crc = None;
        self.input.clear();
        vec![Action::Skipped, Action::Send(header(5, 0))]
    }

    /// 提供当前文件的一块内容。
    pub fn read(&mut self, bytes: &[u8]) -> Vec<Action> {
        if self.state == State::Over {
            return vec![];
        }
        if bytes.is_empty() {
            self.state = State::Next;
            vec![Action::Send(header(11, self.position))]
        } else {
            let position = self.position;
            let Some(next) = position.checked_add(bytes.len() as u32) else {
                return self.fail("file exceeds ZMODEM position range".into());
            };
            self.position = next;
            self.state = State::Ack;
            vec![
                Action::Send(header(10, position)),
                Action::Send(packet(bytes, b'k')),
            ]
        }
    }

    /// 文件块落盘成功之后才确认，确保 EOF 不会抢先覆盖磁盘错误。
    pub fn written(&mut self) -> Vec<Action> {
        let State::Write(end, length) = self.state else {
            return vec![];
        };
        let Some(position) = self.position.checked_add(length as u32) else {
            return self.fail("file exceeds ZMODEM position range".into());
        };
        self.position = position;
        self.state = if matches!(end, b'h' | b'k') {
            State::Header
        } else {
            State::Data
        };
        if self.state == State::Header {
            self.data_crc = None;
        }
        let mut actions = if matches!(end, b'j' | b'k') {
            vec![Action::Send(header(3, position))]
        } else {
            vec![]
        };
        actions.extend(self.receive(&[]));
        actions
    }

    /// 输入原始会话字节。Write 动作完成前暂停解析后续数据。
    pub fn receive(&mut self, bytes: &[u8]) -> Vec<Action> {
        if self.state == State::Over {
            return vec![];
        }
        self.idle_ms = 0;
        self.input.extend_from_slice(bytes);
        if self.input.windows(5).any(|part| part == [0x18; 5]) {
            self.input.clear();
            return self.finish(true);
        }
        if matches!(self.state, State::Offer | State::Files) {
            let trailing_cancel = self
                .input
                .iter()
                .rev()
                .take_while(|byte| **byte == 0x18)
                .count();
            self.input.drain(..self.input.len() - trailing_cancel);
            return vec![];
        }
        if self.direction == Handshake::Receive && self.state == State::Finish {
            return self.finish_receive();
        }
        let mut actions = Vec::new();
        while !matches!(
            self.state,
            State::Offer | State::Files | State::Write(..) | State::Over
        ) {
            let frame = match parse(&mut self.input, self.data_crc) {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(error) => {
                    if self.state == State::Skip {
                        if !self.input.is_empty() {
                            self.input.remove(0);
                        }
                        continue;
                    }
                    actions.extend(self.fail(error));
                    break;
                }
            };
            match frame {
                Frame::Cancel => {
                    actions.extend(self.finish(true));
                    break;
                }
                Frame::Header(kind, value, wide) => {
                    self.header_kind = kind;
                    match (self.direction, kind) {
                        (_, 16 | 7) => actions.extend(self.fail("peer aborted transfer".into())),
                        (Handshake::Receive, 0) => {
                            actions.push(Action::Send(header(1, 0x23000000)))
                        }
                        (Handshake::Receive, 2 | 4) => {
                            if kind == 4 && self.file_accepted {
                                actions.extend(self.fail("file offer before EOF".into()));
                            } else {
                                self.state = State::Header;
                                self.data_crc = Some(wide);
                            }
                        }
                        (Handshake::Receive, 10 | 11) if self.state == State::Skip => {}
                        (Handshake::Receive, 10) => {
                            if !self.file_accepted {
                                actions.extend(self.fail("data before file selection".into()));
                            } else if value != self.position {
                                actions.extend(self.fail("unexpected data position".into()));
                            } else {
                                self.state = State::Data;
                                self.data_crc = Some(wide);
                            }
                        }
                        (Handshake::Receive, 11) => {
                            if !self.file_accepted {
                                actions.push(Action::Send(header(1, 0x23000000)));
                            } else if value != self.position {
                                actions.extend(self.fail("unexpected EOF position".into()));
                            } else {
                                self.file_accepted = false;
                                self.received += 1;
                                actions.extend([
                                    Action::FileDone,
                                    Action::Send(header(1, 0x23000000)),
                                ]);
                            }
                        }
                        (Handshake::Receive, 8) => {
                            if self.file_accepted {
                                actions
                                    .extend(self.fail("session finished before file EOF".into()));
                            } else {
                                self.state = State::Finish;
                                actions.push(Action::Send(header(8, 0)));
                            }
                        }
                        (Handshake::Send, 1) if self.state == State::Next => {
                            actions.extend([Action::FileDone, Action::NextFile]);
                            self.state = State::Files;
                        }
                        (Handshake::Send, 9) => {
                            if value != 0 {
                                actions.extend(self.fail("resume is not supported".into()));
                            } else if self.state == State::Position {
                                self.state = State::Ack;
                                actions.push(Action::Read);
                            } else {
                                actions.extend(self.fail("peer requested retransmission".into()));
                            }
                        }
                        (Handshake::Send, 3) if self.state == State::Ack => {
                            if value == self.position {
                                actions.push(Action::Read);
                            } else {
                                actions.extend(self.fail("unexpected acknowledgement".into()));
                            }
                        }
                        (Handshake::Send, 5) => {
                            self.skipped += 1;
                            self.state = State::Files;
                            actions.extend([Action::Skipped, Action::NextFile]);
                        }
                        (Handshake::Send, 14) => actions.push(Action::Send(header(3, value))),
                        (Handshake::Send, 8) if self.state == State::Finish => {
                            actions.push(Action::Send(b"OO".to_vec()));
                            actions.extend(self.finish(false));
                        }
                        _ => {}
                    }
                }
                Frame::Data(body, end) => {
                    if self.header_kind == 4 {
                        let Some(split) = body.iter().position(|byte| *byte == 0) else {
                            actions.extend(self.fail("missing filename".into()));
                            break;
                        };
                        let name = String::from_utf8_lossy(&body[..split]).into_owned();
                        if name.is_empty()
                            || name.contains(['/', '\\', '\0'])
                            || matches!(name.as_str(), "." | "..")
                        {
                            actions.extend(self.fail("unsafe filename".into()));
                            break;
                        }
                        let size = std::str::from_utf8(&body[split + 1..])
                            .ok()
                            .and_then(|text| text.split([' ', '\0']).next())
                            .and_then(|text| text.parse().ok());
                        self.data_crc = None;
                        self.state = State::Offer;
                        actions.push(Action::Offer(FileInfo { name, size }));
                    } else if self.header_kind == 2 {
                        self.data_crc = None;
                        actions.push(Action::Send(header(3, 0)));
                    } else {
                        self.state = State::Write(end, body.len());
                        actions.push(Action::Write(body));
                    }
                }
            }
            if self.state == State::Finish && self.direction == Handshake::Receive {
                actions.extend(self.finish_receive());
                break;
            }
        }
        actions
    }

    fn finish_receive(&mut self) -> Vec<Action> {
        if self.input.is_empty() || self.input == b"O" {
            return vec![];
        }
        if self.input.starts_with(b"OO") {
            self.input.drain(..2);
        }
        self.finish(false)
    }

    /// 注入经过的毫秒。依原始规范，长时间无通信后终止。
    pub fn tick(&mut self, elapsed_ms: u64) -> Vec<Action> {
        if matches!(self.state, State::Over | State::Offer | State::Files) {
            return vec![];
        }
        self.idle_ms = self.idle_ms.saturating_add(elapsed_ms);
        if self.state == State::Finish
            && self.direction == Handshake::Receive
            && self.idle_ms >= 1000
        {
            self.input.clear();
            return self.finish(false);
        }
        if self.idle_ms >= 60_000 {
            self.fail("ZMODEM timed out".into())
        } else {
            vec![]
        }
    }

    /// 本地错误先发取消序列，再报告最初原因。
    pub fn fail(&mut self, error: String) -> Vec<Action> {
        if self.state == State::Over {
            return vec![];
        }
        if self.first_error.is_none() {
            self.first_error = Some(error);
        }
        self.cancel()
    }

    /// 本端取消正在接收的文件时，将未完整的在途子包移交给终端握手哨兵。
    pub fn hand_off_cancelled_receive(&mut self, sentinel: &mut crate::Sentinel) {
        if self.direction == Handshake::Receive && self.file_accepted {
            sentinel.cancelled_receive(std::mem::take(&mut self.input), self.data_crc);
        }
    }

    /// 用户取消；已经结束时不重复发送。
    pub fn cancel(&mut self) -> Vec<Action> {
        if self.state == State::Over {
            return vec![];
        }
        let mut actions = vec![Action::Send([vec![0x18; 8], vec![8; 10]].concat())];
        actions.extend(self.finish(true));
        actions
    }

    fn finish(&mut self, cancelled: bool) -> Vec<Action> {
        self.state = State::Over;
        vec![Action::Finished {
            received: self.received,
            skipped: self.skipped,
            error: self.first_error.clone(),
            cancelled,
        }]
    }

    /// 结束后同批读到的 shell 提示符交还终端。
    pub fn take_remainder(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.input)
    }
}
