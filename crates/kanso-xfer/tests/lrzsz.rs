//! 只调用外部 lrzsz 二进制，不依赖其源码。
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use kanso_xfer::{Action, BLOCK_SIZE, FileInfo, Handshake, Machine, Xmodem};

fn program(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("KANSO_LRZSZ_BIN")
        .into_iter()
        .map(PathBuf::from)
        .chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ))
        .collect::<Vec<_>>();
    for directory in paths {
        for candidate in [name.to_string(), format!("l{name}")] {
            let path = directory.join(format!("{candidate}{}", std::env::consts::EXE_SUFFIX));
            if path.is_file() {
                return Some(path);
            }
        }
    }
    eprintln!("跳过 lrzsz 集成测试：找不到 {name}（可用 KANSO_LRZSZ_BIN 指定二进制目录）");
    None
}

fn exchange(direction: Handshake, skip_middle: bool, fail_middle: bool) {
    let Some(program) = program(if direction == Handshake::Receive {
        "sz"
    } else {
        "rz"
    }) else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let content: Vec<u8> = (0..100_003).map(|index| index as u8).collect();
    let names = ["first.bin", "second.bin", "third.bin"];
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("HOME", root.path())
        .env("PATH", "/usr/bin:/bin")
        .current_dir(root.path())
        .arg("-b")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if direction == Handshake::Receive {
        for name in names {
            std::fs::write(root.path().join(name), &content).unwrap();
            command.arg(name);
        }
    } else {
        command.arg("-y");
    }
    let (mut child, mut input, rx, reader) = connection(command);
    let mut machine = Machine::new(direction);
    let mut actions: VecDeque<_> = machine.start().into();
    let mut index = 0;
    let mut offset = 0;
    let mut saved = Vec::new();
    let mut received = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    let result = loop {
        if Instant::now() > deadline {
            break Err("lrzsz exchange timed out".to_string());
        }
        let more = if let Some(action) = actions.pop_front() {
            match action {
                Action::Send(bytes) => {
                    if let Err(error) = input.write_all(&bytes) {
                        break Err(error.to_string());
                    }
                    vec![]
                }
                Action::PickFiles | Action::NextFile => {
                    offset = 0;
                    let file = names.get(index).map(|name| FileInfo {
                        name: name.to_string(),
                        size: Some(content.len() as u64),
                    });
                    index += 1;
                    machine.next_file(file)
                }
                Action::Offer(file) => {
                    index += 1;
                    saved.clear();
                    assert_eq!(file.size, Some(content.len() as u64));
                    if skip_middle && index == 2 {
                        machine.skip(None)
                    } else {
                        machine.accept()
                    }
                }
                Action::Read => {
                    let end = (offset + BLOCK_SIZE).min(content.len());
                    let actions = machine.read(&content[offset..end]);
                    offset = end;
                    actions
                }
                Action::Write(bytes) => {
                    if fail_middle && index == 2 {
                        machine.skip(Some("disk full".into()))
                    } else {
                        saved.extend(bytes);
                        machine.written()
                    }
                }
                Action::FileDone if direction == Handshake::Receive => {
                    received.push(saved.clone());
                    vec![]
                }
                Action::Finished {
                    error, cancelled, ..
                } => {
                    break if fail_middle && error.as_deref() == Some("disk full") {
                        Ok(())
                    } else if let Some(error) = error {
                        Err(error)
                    } else if cancelled {
                        Err("cancelled".into())
                    } else {
                        Ok(())
                    };
                }
                _ => vec![],
            }
        } else {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(bytes) => machine.receive(&bytes),
                Err(mpsc::RecvTimeoutError::Timeout) => machine.tick(100),
                Err(_) => {
                    break Err("lrzsz stdout closed before transfer completed".into());
                }
            }
        };
        for action in more.into_iter().rev() {
            actions.push_front(action);
        }
    };
    drop(input);
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().unwrap();
    reader.join().unwrap();
    result.unwrap();
    assert!(status.success(), "{status}");
    if direction == Handshake::Receive {
        assert_eq!(
            received.len(),
            if skip_middle || fail_middle { 2 } else { 3 }
        );
        assert!(received.iter().all(|saved| *saved == content));
    } else {
        for name in names {
            assert_eq!(std::fs::read(root.path().join(name)).unwrap(), content);
        }
    }
}

#[test]
fn receive_from_real_sz_and_skip_second_file() {
    exchange(Handshake::Receive, true, false);
}

#[test]
fn send_to_real_rz() {
    exchange(Handshake::Send, false, false);
}

#[test]
fn receive_from_real_sz_continues_after_write_error() {
    exchange(Handshake::Receive, false, true);
}

fn connection(
    mut command: Command,
) -> (
    std::process::Child,
    std::process::ChildStdin,
    mpsc::Receiver<Vec<u8>>,
    std::thread::JoinHandle<()>,
) {
    let mut child = command.spawn().unwrap();
    let input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut buffer = [0; 4096];
        while let Ok(length) = output.read(&mut buffer) {
            if length == 0 {
                break;
            }
            if tx.send(buffer[..length].to_vec()).is_err() {
                break;
            }
        }
    });
    (child, input, rx, reader)
}

fn xmodem_exchange(sending: bool, one_k: bool, checksum: bool) {
    let Some(program) = program(if sending { "rx" } else { "sx" }) else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("data.bin");
    let content: Vec<u8> = (0..100_003).map(|index| index as u8).collect();
    if !sending {
        std::fs::write(&path, &content).unwrap();
    }
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("HOME", root.path())
        .env("PATH", "/usr/bin:/bin")
        .current_dir(root.path())
        .arg("-b")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if sending {
        command.arg("-y");
        if !checksum {
            command.arg("-c");
        }
    } else if one_k {
        command.arg("-k");
    }
    command.arg("data.bin");
    let (mut child, mut input, rx, reader) = connection(command);
    let direction = if sending {
        Handshake::Send
    } else {
        Handshake::Receive
    };
    let mut machine = Xmodem::new(direction, one_k);
    let mut actions: VecDeque<_> = machine.start().into();
    let mut offset = 0;
    let mut saved = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    let result = loop {
        if Instant::now() > deadline {
            break Err("XMODEM lrzsz exchange timed out".to_owned());
        }
        let more = if let Some(action) = actions.pop_front() {
            match action {
                Action::Send(bytes) => {
                    if let Err(error) = input.write_all(&bytes) {
                        break Err(error.to_string());
                    }
                    vec![]
                }
                Action::ReadBlock(size) => {
                    let end = (offset + size).min(content.len());
                    let actions = machine.read(&content[offset..end]);
                    offset = end;
                    actions
                }
                Action::Write(bytes) => {
                    saved.extend(bytes);
                    machine.written()
                }
                Action::Finished {
                    error, cancelled, ..
                } => {
                    break if let Some(error) = error {
                        Err(error)
                    } else if cancelled {
                        Err("cancelled".into())
                    } else {
                        Ok(())
                    };
                }
                _ => vec![],
            }
        } else {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(bytes) => machine.receive(&bytes),
                Err(mpsc::RecvTimeoutError::Timeout) => machine.tick(100),
                Err(_) => break Err("lrzsz stdout closed before transfer completed".into()),
            }
        };
        for action in more.into_iter().rev() {
            actions.push_front(action);
        }
    };
    drop(input);
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().unwrap();
    reader.join().unwrap();
    result.unwrap();
    assert!(status.success(), "{status}");
    if sending {
        saved = std::fs::read(path).unwrap();
    }
    assert_eq!(&saved[..content.len()], content);
    assert!(saved[content.len()..].iter().all(|byte| *byte == 0x1a));
    assert_eq!(saved.len(), content.len().div_ceil(128) * 128);
}

#[test]
fn xmodem_send_to_real_rx_crc() {
    xmodem_exchange(true, false, false);
}

#[test]
fn xmodem_send_to_real_rx_checksum_falls_back_from_one_k() {
    xmodem_exchange(true, true, true);
}

#[test]
fn xmodem_one_k_send_to_real_rx() {
    xmodem_exchange(true, true, false);
}

#[test]
fn xmodem_receive_from_real_sx() {
    xmodem_exchange(false, false, false);
}

#[test]
fn xmodem_receive_from_real_sx_one_k() {
    xmodem_exchange(false, true, false);
}
