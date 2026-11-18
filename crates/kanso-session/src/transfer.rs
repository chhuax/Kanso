use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kanso_types::event::{NoticeLevel, TransferDirection, TransferOutcome};
use kanso_types::message::{ByteCount, Reason};
use kanso_types::transfer::XferProtocol;
use kanso_types::{CoreEvent, Message, PromptRequest, PromptResponse, SessionId, TransferId};
use kanso_xfer::{Action, BLOCK_SIZE, FileInfo, Handshake, Machine as Zmodem, Sentinel, Xmodem};

use crate::host::SessionHost;
use crate::local::lock;

static NEXT_TRANSFER: AtomicU64 = AtomicU64::new(1 << 63);
type Writer = Arc<dyn Fn(Vec<u8>) -> Result<(), String> + Send + Sync>;
type TerminalOutput = Arc<dyn Fn(Vec<u8>) + Send + Sync>;

#[derive(Default)]
struct Output {
    callback: Option<TerminalOutput>,
    pending: VecDeque<Vec<u8>>,
    delivering: bool,
}

enum Machine {
    Zmodem(Zmodem),
    Xmodem(Xmodem),
}

macro_rules! forward_machine {
    ($($name:ident($($arg:ident: $kind:ty),*) -> $result:ty;)*) => {
        $(fn $name(&mut self, $($arg: $kind),*) -> $result {
            match self {
                Self::Zmodem(machine) => machine.$name($($arg),*),
                Self::Xmodem(machine) => machine.$name($($arg),*),
            }
        })*
    };
}

impl Machine {
    forward_machine! {
        receive(bytes: &[u8]) -> Vec<Action>;
        tick(elapsed: u64) -> Vec<Action>;
        read(bytes: &[u8]) -> Vec<Action>;
        written() -> Vec<Action>;
        fail(error: String) -> Vec<Action>;
        cancel() -> Vec<Action>;
        take_remainder() -> Vec<u8>;
    }

    fn accept(&mut self) -> Vec<Action> {
        match self {
            Self::Zmodem(machine) => machine.accept(),
            Self::Xmodem(machine) => machine.start(),
        }
    }

    fn next_file(&mut self, file: Option<FileInfo>) -> Vec<Action> {
        match self {
            Self::Zmodem(machine) => machine.next_file(file),
            Self::Xmodem(machine) => machine.start(),
        }
    }

    fn skip(&mut self, error: Option<String>) -> Vec<Action> {
        match self {
            Self::Zmodem(machine) => machine.skip(error),
            Self::Xmodem(machine) => machine.fail(error.expect("XMODEM 写入失败有原因")),
        }
    }
}

struct State {
    sentinel: Sentinel,
    machine: Option<Machine>,
    writer: Option<Writer>,
    id: TransferId,
    direction: Handshake,
    paths: VecDeque<String>,
    file: Option<File>,
    name: String,
    path: String,
    total: Option<u64>,
    done: u64,
    progress_at: Instant,
    error: Option<Message>,
    prompt: Option<PromptRequest>,
    closed: bool,
    clock_id: Option<TransferId>,
    protocol: XferProtocol,
    selecting: bool,
    buffered: Vec<u8>,
}

pub(crate) struct TerminalTransfer {
    session: SessionId,
    host: Arc<dyn SessionHost>,
    state: Mutex<State>,
    terminal_output: Mutex<Output>,
}

impl TerminalTransfer {
    pub(crate) fn new(session: SessionId, host: Arc<dyn SessionHost>) -> Arc<Self> {
        Arc::new(Self {
            session,
            host,
            state: Mutex::new(State {
                sentinel: Sentinel::default(),
                machine: None,
                writer: None,
                id: TransferId::from_raw(0),
                direction: Handshake::Receive,
                paths: VecDeque::new(),
                file: None,
                name: String::new(),
                path: String::new(),
                total: None,
                done: 0,
                progress_at: Instant::now(),
                error: None,
                prompt: None,
                closed: false,
                clock_id: None,
                protocol: XferProtocol::Zmodem,
                selecting: false,
                buffered: Vec::new(),
            }),
            terminal_output: Mutex::new(Output::default()),
        })
    }

    pub(crate) fn set_writer(&self, writer: Writer) {
        lock(&self.state).writer = Some(writer);
    }

    pub(crate) fn set_terminal_output(&self, output: TerminalOutput) {
        lock(&self.terminal_output).callback = Some(output);
    }

    fn queue_output(&self, bytes: &mut Vec<u8>) {
        let mut output = lock(&self.terminal_output);
        if output.callback.is_some() && !bytes.is_empty() {
            output.pending.push_back(std::mem::take(bytes));
        }
    }

    fn output(&self) {
        let mut output = lock(&self.terminal_output);
        if output.delivering {
            return;
        }
        output.delivering = true;
        while let Some(bytes) = output.pending.pop_front() {
            let callback = output.callback.clone().expect("排队前已有终端出口");
            drop(output);
            callback(bytes);
            output = lock(&self.terminal_output);
        }
        output.delivering = false;
    }

    pub(crate) fn xmodem(
        self: &Arc<Self>,
        sending: bool,
        one_k: bool,
        path: Option<&str>,
    ) -> Result<TransferId, Message> {
        let mut state = lock(&self.state);
        if state.closed {
            return Err(Message::SenderTerminalUnavailable {});
        }
        if state.machine.is_some() {
            return Err(Message::SenderTerminalTransferring {});
        }
        state.id = TransferId::from_raw(NEXT_TRANSFER.fetch_add(1, Ordering::Relaxed));
        state.direction = if sending {
            Handshake::Send
        } else {
            Handshake::Receive
        };
        state.protocol = XferProtocol::Xmodem;
        state.machine = Some(Machine::Xmodem(Xmodem::new(state.direction, one_k)));
        state.selecting = true;
        state.error = None;
        state.done = 0;
        state.total = None;
        state.buffered = state.sentinel.flush();
        let id = state.id;
        self.host.emit(CoreEvent::TransferStarted {
            transfer: id,
            session: self.session,
            protocol: state.protocol,
            direction: if sending {
                TransferDirection::Upload
            } else {
                TransferDirection::Download
            },
            name: String::new(),
            total_bytes: None,
        });
        drop(state);
        if let Some(path) = path {
            self.respond(
                id,
                if sending {
                    PromptResponse::Files(vec![path.into()])
                } else {
                    PromptResponse::SavePath(path.into())
                },
            );
        } else {
            self.ask(Some(if sending {
                PromptRequest::XmodemPickFile
            } else {
                PromptRequest::XmodemSaveFile
            }));
        }
        Ok(id)
    }

    fn clock(self: &Arc<Self>, state: &mut State) {
        let id = state.id;
        if state.clock_id == Some(id) {
            return;
        }
        state.clock_id = Some(id);
        let weak = Arc::downgrade(self);
        std::thread::spawn(move || {
            while let Some(transfer) = weak.upgrade() {
                std::thread::sleep(Duration::from_millis(100));
                let mut state = lock(&transfer.state);
                if state.id != id {
                    break;
                }
                let mut terminal = Vec::new();
                if let Some(machine) = &mut state.machine {
                    let actions = machine.tick(100);
                    transfer.actions(&mut state, actions, &mut terminal);
                } else if state.sentinel.cancel_tail_pending() && !state.closed {
                    terminal = state.sentinel.poll_cancel_tail();
                } else {
                    state.clock_id = None;
                    break;
                }
                transfer.queue_output(&mut terminal);
                drop(state);
                transfer.output();
            }
        });
    }

    pub(crate) fn active(&self) -> bool {
        lock(&self.state).machine.is_some()
    }

    pub(crate) fn sender_input<R>(
        &self,
        send: impl FnOnce() -> Result<R, Message>,
    ) -> Result<R, Message> {
        let state = lock(&self.state);
        if state.machine.is_some() {
            return Err(Message::SenderTerminalTransferring {});
        }
        if state.closed {
            return Err(Message::SenderTerminalUnavailable {});
        }
        send()
    }

    pub(crate) fn feed(self: &Arc<Self>, bytes: &[u8]) -> Vec<u8> {
        let mut terminal = Vec::new();
        let mut state = lock(&self.state);
        if state.closed {
            return terminal;
        }
        if state.selecting {
            state.buffered.extend_from_slice(bytes);
            return terminal;
        }
        let actions = if let Some(machine) = &mut state.machine {
            machine.receive(bytes)
        } else if let Some(direction) = state.sentinel.feed(bytes, &mut terminal) {
            state.id = TransferId::from_raw(NEXT_TRANSFER.fetch_add(1, Ordering::Relaxed));
            state.direction = direction;
            state.error = None;
            state.done = 0;
            state.total = None;
            state.protocol = XferProtocol::Zmodem;
            let mut machine = Zmodem::new(direction);
            let actions = machine.start();
            state.machine = Some(Machine::Zmodem(machine));
            self.host.emit(CoreEvent::TransferStarted {
                transfer: state.id,
                session: self.session,
                protocol: XferProtocol::Zmodem,
                direction: if direction == Handshake::Receive {
                    TransferDirection::Download
                } else {
                    TransferDirection::Upload
                },
                name: String::new(),
                total_bytes: None,
            });
            if direction == Handshake::Receive {
                self.notice(Message::ZmodemWaitingSender {});
            }
            self.clock(&mut state);
            actions
        } else {
            vec![]
        };
        if state.sentinel.cancel_tail_pending() {
            self.clock(&mut state);
        }
        self.actions(&mut state, actions, &mut terminal);
        let prompt = state.prompt.take();
        self.queue_output(&mut terminal);
        drop(state);
        self.output();
        self.ask(prompt);
        terminal
    }

    fn ask(self: &Arc<Self>, request: Option<PromptRequest>) {
        if let Some(request) = request {
            let state = lock(&self.state);
            if state.closed || state.machine.is_none() {
                return;
            }
            let id = state.id;
            let weak = Arc::downgrade(self);
            self.host.transfer_prompt(
                self.session,
                request,
                Box::new(move |response| {
                    if let Some(transfer) = weak.upgrade() {
                        transfer.respond(id, response);
                    }
                }),
            );
        }
    }

    fn respond(self: &Arc<Self>, id: TransferId, response: PromptResponse) {
        let mut state = lock(&self.state);
        if state.machine.is_none() || state.id != id {
            return;
        }
        let selecting = state.selecting;
        state.selecting = false;
        let selected = matches!((&state.direction, &response), (Handshake::Send, PromptResponse::Files(paths)) if paths.len() == 1)
            || matches!(
                (&state.direction, &response),
                (Handshake::Receive, PromptResponse::SavePath(_))
            );
        if selecting && !selected {
            let mut terminal = std::mem::take(&mut state.buffered);
            state.machine = None;
            self.host.emit(CoreEvent::TransferFinished {
                transfer: id,
                outcome: TransferOutcome::Cancelled,
            });
            self.queue_output(&mut terminal);
            drop(state);
            self.output();
            return;
        }
        let actions = match (state.direction, response) {
            (Handshake::Send, PromptResponse::Files(paths)) if !paths.is_empty() => {
                state.paths = paths.into();
                self.next_file(&mut state)
            }
            (Handshake::Receive, PromptResponse::SavePath(path)) => {
                state.path = path.clone();
                match File::create(&path) {
                    Ok(file) => {
                        state.file = Some(file);
                        state.machine.as_mut().expect("活动传输").accept()
                    }
                    Err(error) => self.disk_error(&mut state, error, true),
                }
            }
            (Handshake::Receive, _) => state.machine.as_mut().expect("活动传输").skip(None),
            _ => state.machine.as_mut().expect("活动传输").cancel(),
        };
        let mut terminal = Vec::new();
        self.actions(&mut state, actions, &mut terminal);
        if selecting && state.machine.is_some() {
            let bytes = std::mem::take(&mut state.buffered);
            let actions = state.machine.as_mut().expect("活动传输").receive(&bytes);
            self.actions(&mut state, actions, &mut terminal);
            self.clock(&mut state);
        }
        let prompt = state.prompt.take();
        self.queue_output(&mut terminal);
        drop(state);
        self.output();
        self.ask(prompt);
    }

    fn next_file(&self, state: &mut State) -> Vec<Action> {
        state.file = None;
        let Some(path) = state.paths.pop_front() else {
            return state.machine.as_mut().expect("活动传输").next_file(None);
        };
        state.path = path.clone();
        match File::open(&path).and_then(|file| file.metadata().map(|metadata| (file, metadata))) {
            Ok((file, metadata)) if metadata.is_file() => {
                state.name = std::path::Path::new(&path)
                    .file_name()
                    .expect("普通文件路径有名称")
                    .to_string_lossy()
                    .into_owned();
                state.total = Some(metadata.len());
                state.done = 0;
                state.file = Some(file);
                state
                    .machine
                    .as_mut()
                    .expect("活动传输")
                    .next_file(Some(FileInfo {
                        name: state.name.clone(),
                        size: state.total,
                    }))
            }
            Ok(_) => self.disk_error(state, std::io::Error::other("not a regular file"), false),
            Err(error) => self.disk_error(state, error, false),
        }
    }

    fn disk_error(&self, state: &mut State, error: std::io::Error, writing: bool) -> Vec<Action> {
        let message = if writing {
            Message::XferLocalWriteFailed {
                path: state.path.clone(),
                reason: Reason::Other {
                    detail: error.to_string(),
                },
            }
        } else {
            Message::XferLocalReadFailed {
                path: state.path.clone(),
                reason: Reason::Other {
                    detail: error.to_string(),
                },
            }
        };
        state.error.get_or_insert(message.clone());
        if state.protocol == XferProtocol::Zmodem {
            self.host.emit(CoreEvent::Notice {
                session: Some(self.session),
                level: NoticeLevel::Error,
                message,
            });
        }
        state.file = None;
        if writing {
            state
                .machine
                .as_mut()
                .expect("活动传输")
                .skip(Some(error.to_string()))
        } else {
            state
                .machine
                .as_mut()
                .expect("活动传输")
                .fail(error.to_string())
        }
    }

    fn actions(&self, state: &mut State, actions: Vec<Action>, terminal: &mut Vec<u8>) {
        let mut pending: VecDeque<_> = actions.into();
        while let Some(action) = pending.pop_front() {
            let more = match action {
                Action::Send(bytes) => {
                    if let Some(writer) = &state.writer
                        && let Err(error) = writer(bytes)
                    {
                        // 写通道已断开，不再向它发送取消序列。
                        let actions = state
                            .machine
                            .as_mut()
                            .expect("活动传输")
                            .fail(error.clone());
                        if !actions.is_empty() {
                            state.error.get_or_insert(Message::XferStreamWriteFailed {
                                reason: Reason::Other { detail: error },
                            });
                            pending.clear();
                            pending.extend(
                                actions
                                    .into_iter()
                                    .filter(|action| !matches!(action, Action::Send(_))),
                            );
                        }
                    }
                    vec![]
                }
                Action::PickFiles => {
                    self.notice(Message::ZmodemSelectFiles {});
                    state.prompt = Some(PromptRequest::ZmodemPickFiles);
                    vec![]
                }
                Action::NextFile => self.next_file(state),
                Action::Offer(file) => {
                    state.name = file.name.clone();
                    state.total = file.size;
                    state.done = 0;
                    self.notice(if let Some(size) = file.size {
                        Message::ZmodemFileOfferedWithSize {
                            name: file.name.clone(),
                            size: ByteCount(size),
                        }
                    } else {
                        Message::ZmodemFileOffered {
                            name: file.name.clone(),
                        }
                    });
                    state.prompt = Some(PromptRequest::ZmodemSaveFile {
                        suggested_name: file.name,
                        size: file.size,
                    });
                    vec![]
                }
                Action::Read | Action::ReadBlock(_) => {
                    let size = if let Action::ReadBlock(size) = action {
                        size
                    } else {
                        BLOCK_SIZE
                    };
                    let mut buffer = Vec::with_capacity(size);
                    match state
                        .file
                        .as_mut()
                        .expect("发送文件已打开")
                        .take(size as u64)
                        .read_to_end(&mut buffer)
                    {
                        Ok(length) => {
                            state.done += length as u64;
                            self.progress(state, length == 0);
                            state
                                .machine
                                .as_mut()
                                .expect("活动传输")
                                .read(&buffer[..length])
                        }
                        Err(error) => self.disk_error(state, error, false),
                    }
                }
                Action::Write(bytes) => {
                    match state
                        .file
                        .as_mut()
                        .expect("保存文件已打开")
                        .write_all(&bytes)
                    {
                        Ok(()) => {
                            state.done += bytes.len() as u64;
                            self.progress(state, false);
                            state.machine.as_mut().expect("活动传输").written()
                        }
                        Err(error) => self.disk_error(state, error, true),
                    }
                }
                Action::FileDone => {
                    self.progress(state, true);
                    state.file = None;
                    vec![]
                }
                Action::Skipped => {
                    state.file = None;
                    self.notice(if state.direction == Handshake::Receive {
                        Message::ZmodemFileSkipped {
                            name: state.name.clone(),
                        }
                    } else {
                        Message::ZmodemPeerSkipped {
                            name: state.name.clone(),
                        }
                    });
                    vec![]
                }
                Action::WaitingConfirm => {
                    self.notice(Message::ZmodemWaitingConfirm {});
                    vec![]
                }
                Action::Terminal(bytes) => {
                    terminal.extend(bytes);
                    vec![]
                }
                Action::Finished {
                    received,
                    skipped,
                    error,
                    cancelled,
                } => {
                    let outcome = if let Some(message) = state.error.take().or_else(|| {
                        error.map(|detail| {
                            if state.protocol == XferProtocol::Xmodem && detail == "timeout" {
                                Message::XmodemTimeout {}
                            } else if detail == "resume is not supported" {
                                Message::ZmodemResumeUnsupported {}
                            } else {
                                Message::XferProtocolError {
                                    protocol: state.protocol,
                                    detail,
                                }
                            }
                        })
                    }) {
                        TransferOutcome::Failed {
                            message,
                            already_exists: false,
                        }
                    } else if cancelled {
                        TransferOutcome::Cancelled
                    } else {
                        TransferOutcome::Completed { skipped }
                    };
                    let user_cancelled = matches!(outcome, TransferOutcome::Cancelled);
                    terminal.extend(state.machine.as_mut().expect("活动传输").take_remainder());
                    terminal.append(&mut state.buffered);
                    state.selecting = false;
                    state.machine = None;
                    state.file = None;
                    state.paths.clear();
                    state.prompt = None;
                    self.host.transfer_prompt_dismiss(self.session);
                    if state.protocol == XferProtocol::Zmodem
                        && state.direction == Handshake::Receive
                        && !cancelled
                    {
                        self.notice(Message::ZmodemReceiveSummary {
                            count: i64::from(received),
                        });
                    }
                    self.host.emit(CoreEvent::TransferFinished {
                        transfer: state.id,
                        outcome,
                    });
                    if user_cancelled {
                        self.notice(
                            if state.protocol == XferProtocol::Zmodem
                                && state.direction == Handshake::Send
                            {
                                Message::ZmodemSendCancelled {}
                            } else {
                                Message::XferCancelled {}
                            },
                        );
                    }
                    vec![]
                }
            };
            for action in more.into_iter().rev() {
                pending.push_front(action);
            }
        }
    }

    fn notice(&self, message: Message) {
        self.host.emit(CoreEvent::Notice {
            session: Some(self.session),
            level: NoticeLevel::Info,
            message,
        });
    }

    fn progress(&self, state: &mut State, force: bool) {
        if force || state.progress_at.elapsed() >= Duration::from_millis(50) {
            state.progress_at = Instant::now();
            self.host.emit(CoreEvent::TransferProgress {
                transfer: state.id,
                done_bytes: state.done,
                total_bytes: state.total,
            });
        }
    }

    pub(crate) fn cancel(&self, id: Option<TransferId>) {
        let mut state = lock(&self.state);
        if id.is_some_and(|id| id != state.id) {
            return;
        }
        let State {
            machine, sentinel, ..
        } = &mut *state;
        if let Some(machine) = machine {
            if let Machine::Zmodem(zmodem) = machine {
                zmodem.hand_off_cancelled_receive(sentinel);
            }
            let actions = machine.cancel();
            let mut terminal = Vec::new();
            self.actions(&mut state, actions, &mut terminal);
            self.queue_output(&mut terminal);
            drop(state);
            self.output();
        }
    }

    pub(crate) fn close(&self, reason: Message) {
        let mut state = lock(&self.state);
        state.closed = true;
        if let Some(machine) = &mut state.machine {
            let actions = machine.fail("session ended".into());
            state.error.get_or_insert(reason);
            self.actions(&mut state, actions, &mut Vec::new());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    type Reply = Box<dyn FnOnce(PromptResponse) + Send>;

    #[derive(Default)]
    struct Host(Mutex<Vec<CoreEvent>>, Mutex<Option<Reply>>);

    impl SessionHost for Host {
        fn emit(&self, event: CoreEvent) {
            lock(&self.0).push(event);
        }

        fn command_finished_in_background(&self, _: SessionId, _: Option<i32>) {}

        fn transfer_prompt(&self, _: SessionId, _: PromptRequest, reply: Reply) {
            *lock(&self.1) = Some(reply);
        }
    }

    #[test]
    fn sender_rejects_terminal_transfer_and_closed_input() {
        let transfer = TerminalTransfer::new(SessionId::from_raw(8), Arc::new(Host::default()));
        transfer.set_writer(Arc::new(|_| Ok(())));
        assert_eq!(transfer.sender_input(|| Ok(7)).unwrap(), 7);
        transfer.feed(b"**\x18B00000000000000\r\n\x11");
        assert!(
            transfer
                .sender_input(|| -> Result<(), Message> { panic!("传输期间不能输入") })
                .unwrap_err()
                .eq(&Message::SenderTerminalTransferring {})
        );
        transfer.cancel(None);
        assert!(transfer.sender_input(|| Ok(())).is_ok());
        transfer.close(Message::XferSessionEnded {});
        assert!(
            transfer
                .sender_input(|| -> Result<(), Message> { panic!("结束后不能输入") })
                .is_err()
        );
    }

    #[test]
    fn cancelled_zmodem_receive_releases_input_and_the_first_unterminated_shell_prompt() {
        let host = Arc::new(Host::default());
        let transfer = TerminalTransfer::new(SessionId::from_raw(22), host.clone());
        transfer.set_writer(Arc::new(|_| Ok(())));
        let (tx, rx) = std::sync::mpsc::channel();
        transfer.set_terminal_output(Arc::new(move |bytes| tx.send(bytes).unwrap()));
        transfer.feed(b"**\x18B00000000000000\r\n\x11");
        assert_eq!(rx.recv().unwrap(), b"\r\x1b[2K");
        let mut peer = Zmodem::new(Handshake::Send);
        for action in peer.next_file(Some(FileInfo {
            name: "test.bin".into(),
            size: Some(14),
        })) {
            if let Action::Send(bytes) = action {
                transfer.feed(&bytes);
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        lock(&host.1).take().unwrap()(PromptResponse::SavePath(path.to_str().unwrap().into()));
        let frames = peer.read(b"discarded data");
        let [Action::Send(header), Action::Send(packet)] = frames.as_slice() else {
            panic!("接收一个数据子包")
        };
        transfer.feed(header);
        transfer.feed(&packet[..3]);
        transfer.cancel(None);
        assert!(transfer.sender_input(|| Ok(())).is_ok());
        // 对端中止尚未发完的子包，不回 CAN，马上交回不带换行的提示符。
        transfer.feed(b"host$ ");
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), b"host$ ");
        assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
    }

    #[test]
    fn broken_writer_does_not_drop_finished_or_replace_first_error() {
        for closing in [false, true] {
            let host = Arc::new(Host::default());
            let transfer = TerminalTransfer::new(SessionId::from_raw(7), host.clone());
            let broken = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&broken);
            transfer.set_writer(Arc::new(move |_| {
                if flag.load(Ordering::Acquire) {
                    Err("closed writer".into())
                } else {
                    Ok(())
                }
            }));
            transfer.feed(b"**\x18B00000000000000\r\n\x11");
            assert!(transfer.active());
            broken.store(true, Ordering::Release);
            if closing {
                transfer.close(Message::XferSessionEnded {});
            } else {
                transfer.cancel(None);
            }
            assert!(!transfer.active());
            let events = lock(&host.0);
            let outcomes = events
                .iter()
                .filter_map(|event| match event {
                    CoreEvent::TransferFinished { outcome, .. } => Some(outcome),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(outcomes.len(), 1);
            if closing {
                assert!(matches!(
                    outcomes[0],
                    TransferOutcome::Failed {
                        message: Message::XferSessionEnded {},
                        ..
                    }
                ));
            } else {
                assert!(matches!(outcomes[0], TransferOutcome::Cancelled));
            }
        }
    }

    #[test]
    fn xmodem_dialog_cancel_restores_buffer_without_sending_cancel() {
        for sending in [false, true] {
            let host = Arc::new(Host::default());
            let transfer = TerminalTransfer::new(SessionId::from_raw(8), host.clone());
            transfer.set_writer(Arc::new(|_| panic!("取消文件选择不向对端发送数据")));
            let output = Arc::new(Mutex::new(Vec::new()));
            let saved = output.clone();
            transfer.set_terminal_output(Arc::new(move |bytes| lock(&saved).extend(bytes)));
            transfer.xmodem(sending, false, None).unwrap();
            assert!(transfer.feed(b"banner\r\nCC\x15").is_empty());
            assert_eq!(
                transfer.sender_input(|| Ok(())).unwrap_err(),
                Message::SenderTerminalTransferring {}
            );
            lock(&host.1).take().unwrap()(PromptResponse::Cancel);
            assert!(!transfer.active());
            assert_eq!(*lock(&output), b"banner\r\nCC\x15");
            assert!(transfer.sender_input(|| Ok(())).is_ok());
        }
    }

    #[test]
    fn restored_output_and_new_input_keep_stream_order_without_reentrant_lock() {
        let host = Arc::new(Host::default());
        let transfer = TerminalTransfer::new(SessionId::from_raw(8), host.clone());
        let output = Arc::new(Mutex::new(Vec::new()));
        let saved = output.clone();
        let weak = Arc::downgrade(&transfer);
        transfer.set_terminal_output(Arc::new(move |bytes| {
            lock(&saved).extend_from_slice(&bytes);
            if bytes == b"banner" {
                weak.upgrade().unwrap().feed(b"prompt");
            }
        }));
        transfer.xmodem(false, false, None).unwrap();
        transfer.feed(b"banner");
        lock(&host.1).take().unwrap()(PromptResponse::Cancel);
        assert_eq!(*lock(&output), b"bannerprompt");
    }

    #[test]
    fn xmodem_dialog_uses_latest_request_and_single_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("send.bin");
        std::fs::write(&path, [7; 1024]).unwrap();
        let host = Arc::new(Host::default());
        let transfer = TerminalTransfer::new(SessionId::from_raw(8), host.clone());
        let wire = Arc::new(Mutex::new(Vec::new()));
        let saved = wire.clone();
        transfer.set_writer(Arc::new(move |bytes| {
            lock(&saved).push(bytes);
            Ok(())
        }));
        transfer.xmodem(true, true, None).unwrap();
        transfer.feed(b"CCC\x15");
        lock(&host.1).take().unwrap()(PromptResponse::Files(vec![
            path.to_string_lossy().into_owned(),
        ]));
        let packet = lock(&wire)[0].clone();
        assert_eq!(packet.len(), 132);
        assert_eq!(&packet[..3], &[1, 1, 254]);
        transfer.cancel(None);
        assert_eq!(lock(&wire).last().unwrap(), &[0x18; 8]);
    }

    #[test]
    fn xmodem_receive_keeps_padding_and_releases_trailing_prompt() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("receive.bin");
        let host = Arc::new(Host::default());
        let transfer = TerminalTransfer::new(SessionId::from_raw(8), host.clone());
        transfer.set_writer(Arc::new(|_| Ok(())));
        transfer
            .xmodem(false, false, Some(path.to_str().unwrap()))
            .unwrap();
        let mut sender = Xmodem::new(Handshake::Send, false);
        sender.receive(b"C");
        let Action::Send(packet) = &sender.read(b"data")[0] else {
            panic!("发送首块");
        };
        assert_eq!(
            transfer.feed(&[b"banner\r\n".as_slice(), packet].concat()),
            b"banner\r\n"
        );
        assert_eq!(transfer.feed(b"\x04prompt"), b"prompt");
        let saved = std::fs::read(path).unwrap();
        assert_eq!(&saved[..4], b"data");
        assert_eq!(&saved[4..], &[0x1a; 124]);
        assert!(!transfer.active());
        assert!(lock(&host.0).iter().any(|event| matches!(
            event,
            CoreEvent::TransferFinished {
                outcome: TransferOutcome::Completed { .. },
                ..
            }
        )));
    }

    #[test]
    fn xmodem_session_close_and_file_error_keep_original_reason() {
        for closing in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let host = Arc::new(Host::default());
            let transfer = TerminalTransfer::new(SessionId::from_raw(8), host.clone());
            let wire = Arc::new(Mutex::new(Vec::new()));
            let saved = wire.clone();
            transfer.set_writer(Arc::new(move |bytes| {
                lock(&saved).push(bytes);
                Ok(())
            }));
            transfer.xmodem(false, false, None).unwrap();
            if closing {
                transfer.close(Message::XferSessionEnded {});
            } else {
                lock(&host.1).take().unwrap()(PromptResponse::SavePath(
                    root.path()
                        .join("missing/data")
                        .to_string_lossy()
                        .into_owned(),
                ));
            }
            assert!(!transfer.active());
            assert_eq!(lock(&wire).last().unwrap(), &[0x18; 8]);
            assert!(lock(&host.0).iter().any(|event| matches!(event, CoreEvent::TransferFinished { outcome: TransferOutcome::Failed { message, .. }, .. } if if closing { *message == Message::XferSessionEnded {} } else { matches!(message, Message::XferLocalWriteFailed {..}) })));
        }
    }

    #[test]
    fn xmodem_connection_write_error_is_not_a_protocol_error() {
        let root = tempfile::tempdir().unwrap();
        let host = Arc::new(Host::default());
        let transfer = TerminalTransfer::new(SessionId::from_raw(8), host.clone());
        transfer.set_writer(Arc::new(|_| Err("connection closed".into())));
        transfer
            .xmodem(
                false,
                false,
                Some(root.path().join("data").to_str().unwrap()),
            )
            .unwrap();
        assert!(!transfer.active());
        assert!(lock(&host.0).iter().any(|event| matches!(event, CoreEvent::TransferFinished {
            outcome: TransferOutcome::Failed { message: Message::XferStreamWriteFailed { reason: Reason::Other { detail } }, .. }, ..
        } if detail == "connection closed")));
    }
}
