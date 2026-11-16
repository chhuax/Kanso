use crate::wire::crc16;
use crate::{Action, Handshake};

const SOH: u8 = 1;
const STX: u8 = 2;
const EOT: u8 = 4;
const ACK: u8 = 6;
const NAK: u8 = 0x15;
const CAN: u8 = 0x18;
const BLOCK_TIMEOUT: u64 = 10_000;
const BYTE_TIMEOUT: u64 = 2_000;
const RETRIES: u8 = 10;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Opening,
    Reading,
    Ack,
    Data,
    Writing,
    Eot,
    Over,
}

/// 独立实现的 XMODEM / CRC / 1K 状态机；文件、字节流和时钟均由调用方提供。
pub struct Xmodem {
    direction: Handshake,
    one_k: bool,
    phase: Phase,
    crc: Option<bool>,
    number: u8,
    started: bool,
    input: Vec<u8>,
    tail: Vec<u8>,
    packet: Vec<u8>,
    elapsed: u64,
    gap: u64,
    retries: u8,
    requests: u8,
    cancel_byte: bool,
    header_seen: bool,
}

impl Xmodem {
    /// 接收时同时接受两种块大小；one_k 只控制发送。
    pub fn new(direction: Handshake, one_k: bool) -> Self {
        Self {
            direction,
            one_k,
            phase: Phase::Opening,
            crc: None,
            number: 1,
            started: false,
            input: Vec::new(),
            tail: Vec::new(),
            packet: Vec::new(),
            elapsed: 0,
            gap: 0,
            retries: 0,
            requests: 0,
            cancel_byte: false,
            header_seen: false,
        }
    }

    /// 接收方请求 CRC，发送方等待对方请求。
    pub fn start(&mut self) -> Vec<Action> {
        if self.direction == Handshake::Receive {
            self.phase = Phase::Data;
            self.requests = 1;
            vec![Action::Send(vec![b'C'])]
        } else {
            vec![]
        }
    }

    /// 输入原始字节，完成之后的字节保留给终端。
    pub fn receive(&mut self, bytes: &[u8]) -> Vec<Action> {
        self.input.extend_from_slice(bytes);
        self.gap = 0;
        self.pump()
    }

    fn pump(&mut self) -> Vec<Action> {
        if self.phase == Phase::Over {
            return vec![];
        }
        let mut actions = Vec::new();
        if self.direction == Handshake::Send {
            let cancellation = if self.cancel_byte && self.input.first() == Some(&CAN) {
                Some(0)
            } else {
                self.input.windows(2).position(|bytes| bytes == [CAN, CAN])
            };
            if let Some(index) = cancellation {
                self.input.drain(..index);
                self.discard_cancel();
                return self.finish(None, true);
            }
            if self.phase == Phase::Opening
                && let Some(index) = self
                    .input
                    .iter()
                    .rposition(|byte| matches!(*byte, b'C' | NAK))
            {
                self.input.drain(..index);
            }
            while !self.input.is_empty() && self.phase != Phase::Over {
                let byte = self.input.remove(0);
                if self.cancelled_by(byte) {
                    self.discard_cancel();
                    actions.extend(self.finish(None, true));
                    break;
                }
                match (self.phase, byte) {
                    (Phase::Opening, b'C' | NAK) => {
                        self.crc = Some(byte == b'C');
                        self.phase = Phase::Reading;
                        self.elapsed = 0;
                        actions.push(Action::ReadBlock(self.read_size()));
                    }
                    (Phase::Ack, ACK) => {
                        self.number = self.number.wrapping_add(1);
                        self.started = true;
                        self.retries = 0;
                        self.elapsed = 0;
                        if self.tail.is_empty() {
                            self.phase = Phase::Reading;
                            actions.push(Action::ReadBlock(self.read_size()));
                        } else {
                            actions.extend(self.send_block());
                        }
                    }
                    (Phase::Ack | Phase::Eot, NAK) | (Phase::Ack, b'C')
                        if byte == NAK || !self.started =>
                    {
                        actions.extend(self.retry());
                    }
                    (Phase::Eot, ACK) => {
                        actions.push(Action::FileDone);
                        actions.extend(self.finish(None, false));
                    }
                    _ => {}
                }
            }
            return actions;
        }
        while self.phase == Phase::Data && !self.input.is_empty() {
            let head = self.input[0];
            if !matches!(head, SOH | STX) {
                self.input.remove(0);
                if self.cancelled_by(head) {
                    self.discard_cancel();
                    actions.extend(self.finish(None, true));
                    break;
                }
                if head == EOT {
                    actions.push(Action::Send(vec![ACK]));
                    actions.push(Action::FileDone);
                    actions.extend(self.finish(None, false));
                    break;
                }
                if !self.started && head != CAN {
                    actions.push(Action::Terminal(vec![head]));
                }
                continue;
            }
            self.cancel_byte = false;
            self.header_seen = true;
            let size = if head == SOH { 128 } else { 1024 };
            let minimum = size + 4;
            if self.input.len() < minimum {
                break;
            }
            let crc = match self.crc {
                Some(crc) => crc,
                None if self.input.len() > minimum => true,
                None if self.gap >= BYTE_TIMEOUT => false,
                None => break,
            };
            let length = minimum + usize::from(crc);
            if self.input.len() < length {
                break;
            }
            let packet: Vec<_> = self.input.drain(..length).collect();
            let data = &packet[3..size + 3];
            let valid = packet[1] ^ packet[2] == 0xff
                && if crc {
                    packet[size + 3..] == crc16(data).to_be_bytes()
                } else {
                    packet[size + 3] == data.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte))
                };
            if !valid {
                actions.extend(self.retry());
                continue;
            }
            self.crc = Some(crc);
            self.elapsed = 0;
            self.retries = 0;
            if packet[1] == self.number {
                self.phase = Phase::Writing;
                actions.push(Action::Write(data.to_vec()));
            } else if self.started && packet[1] == self.number.wrapping_sub(1) {
                actions.push(Action::Send(vec![ACK]));
            } else {
                actions.extend(self.fail("unexpected block number".into()));
            }
        }
        actions
    }

    fn cancelled_by(&mut self, byte: u8) -> bool {
        let cancelled = self.cancel_byte && byte == CAN;
        self.cancel_byte = byte == CAN;
        cancelled
    }

    fn discard_cancel(&mut self) {
        let count = self.input.iter().take_while(|byte| **byte == CAN).count();
        self.input.drain(..count);
    }

    fn read_size(&self) -> usize {
        if self.one_k && self.crc == Some(true) {
            1024
        } else {
            128
        }
    }

    /// 发送文件块；空块表示文件末尾。
    pub fn read(&mut self, bytes: &[u8]) -> Vec<Action> {
        if self.phase == Phase::Over {
            return vec![];
        }
        debug_assert!(self.phase == Phase::Reading);
        self.elapsed = 0;
        if bytes.is_empty() {
            self.phase = Phase::Eot;
            self.packet = vec![EOT];
            vec![Action::WaitingConfirm, Action::Send(self.packet.clone())]
        } else {
            self.tail = bytes.to_vec();
            self.send_block()
        }
    }

    fn send_block(&mut self) -> Vec<Action> {
        let size = if self.read_size() == 1024 && self.tail.len() == 1024 {
            1024
        } else {
            128
        };
        let length = size.min(self.tail.len());
        self.packet = vec![
            if size == 128 { SOH } else { STX },
            self.number,
            !self.number,
        ];
        self.packet.extend(self.tail.drain(..length));
        self.packet.resize(size + 3, 0x1a);
        let data = &self.packet[3..];
        if self.crc == Some(true) {
            self.packet.extend_from_slice(&crc16(data).to_be_bytes());
        } else {
            self.packet
                .push(data.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)));
        }
        self.phase = Phase::Ack;
        vec![Action::Send(self.packet.clone())]
    }

    /// 文件块写入完成后才发送 ACK，避免确认尚未落盘的数据。
    pub fn written(&mut self) -> Vec<Action> {
        if self.phase == Phase::Over {
            return vec![];
        }
        debug_assert!(self.phase == Phase::Writing);
        self.number = self.number.wrapping_add(1);
        self.started = true;
        self.phase = Phase::Data;
        self.elapsed = 0;
        let mut actions = vec![Action::Send(vec![ACK])];
        actions.extend(self.pump());
        actions
    }

    fn retry(&mut self) -> Vec<Action> {
        self.retries += 1;
        self.elapsed = 0;
        if self.retries >= RETRIES {
            return self.fail("timeout".into());
        }
        if self.direction == Handshake::Send {
            vec![Action::Send(self.packet.clone())]
        } else if !self.header_seen {
            let request = if self.requests < 3 { b'C' } else { NAK };
            self.requests += 1;
            vec![Action::Send(vec![request])]
        } else {
            vec![Action::Send(vec![NAK])]
        }
    }

    /// 调用方注入经过的毫秒数；每块最多等待十秒。
    pub fn tick(&mut self, elapsed_ms: u64) -> Vec<Action> {
        if matches!(self.phase, Phase::Over | Phase::Reading | Phase::Writing) {
            return vec![];
        }
        self.elapsed += elapsed_ms;
        self.gap += elapsed_ms;
        if self.direction == Handshake::Receive
            && !self.input.is_empty()
            && self.gap >= BYTE_TIMEOUT
        {
            let actions = self.pump();
            if !actions.is_empty() {
                return actions;
            }
            self.input.clear();
            return self.retry();
        }
        if self.elapsed < BLOCK_TIMEOUT {
            return vec![];
        }
        if self.phase == Phase::Opening {
            self.retries += 1;
            self.elapsed = 0;
            if self.retries < RETRIES {
                return vec![];
            }
            return self.fail("timeout".into());
        }
        self.input.clear();
        self.retry()
    }

    /// 用户取消，通知对方后结束。
    pub fn cancel(&mut self) -> Vec<Action> {
        if self.phase == Phase::Over {
            return vec![];
        }
        let mut actions = vec![Action::Send(vec![CAN; 8])];
        actions.extend(self.finish(None, true));
        actions
    }

    /// 首先向对方发送取消序列，再报告原始错误。
    pub fn fail(&mut self, error: String) -> Vec<Action> {
        if self.phase == Phase::Over {
            return vec![];
        }
        let mut actions = vec![Action::Send(vec![CAN; 8])];
        actions.extend(self.finish(Some(error), false));
        actions
    }

    fn finish(&mut self, error: Option<String>, cancelled: bool) -> Vec<Action> {
        self.phase = Phase::Over;
        vec![Action::Finished {
            received: u32::from(error.is_none() && !cancelled),
            skipped: 0,
            error,
            cancelled,
        }]
    }

    /// 完成、取消或失败后的剩余原始字节。
    pub fn take_remainder(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.input)
    }
}
