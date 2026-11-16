use std::collections::VecDeque;

use crate::wire::crc16;
use crate::{Action, Handshake, Xmodem};

fn block(number: u8, data: &[u8], crc: bool) -> Vec<u8> {
    let mut packet = vec![if data.len() == 1024 { 2 } else { 1 }, number, !number];
    packet.extend(data);
    if crc {
        packet.extend(crc16(data).to_be_bytes());
    } else {
        packet.push(data.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)));
    }
    packet
}

fn sends(actions: &[Action]) -> Vec<Vec<u8>> {
    actions
        .iter()
        .filter_map(|action| {
            if let Action::Send(bytes) = action {
                Some(bytes.clone())
            } else {
                None
            }
        })
        .collect()
}

fn exchange(
    one_k: bool,
    checksum: bool,
    damage: bool,
    lose_ack: bool,
    length: usize,
) -> Vec<usize> {
    let content: Vec<u8> = (0..length).map(|index| index as u8).collect();
    let mut sender = Xmodem::new(Handshake::Send, one_k);
    let mut receiver = Xmodem::new(Handshake::Receive, false);
    let mut queue = VecDeque::new();
    let mut request = receiver.start();
    if checksum {
        for _ in 0..3 {
            request = receiver.tick(10_000);
        }
        assert_eq!(sends(&request), [vec![0x15]]);
    }
    queue.extend(request.into_iter().map(|action| (false, action)));
    let mut offset = 0;
    let mut saved = Vec::new();
    let mut finished = [false; 2];
    let mut damaged = false;
    let mut lost_ack = false;
    let mut sizes = Vec::new();
    for _ in 0..100_000 {
        let Some((sending, action)) = queue.pop_front() else {
            if finished == [true, true] {
                break;
            }
            queue.extend(sender.tick(100).into_iter().map(|action| (true, action)));
            queue.extend(receiver.tick(100).into_iter().map(|action| (false, action)));
            continue;
        };
        let more = match action {
            Action::Send(mut bytes) if sending => {
                if matches!(bytes[0], 1 | 2) {
                    sizes.push(if bytes[0] == 1 { 128 } else { 1024 });
                    if damage && !damaged {
                        bytes[20] ^= 0x80;
                        damaged = true;
                    }
                }
                let mut actions = Vec::new();
                for fragment in bytes.chunks(17) {
                    actions.extend(receiver.receive(fragment));
                }
                for action in actions {
                    queue.push_back((false, action));
                }
                vec![]
            }
            Action::Send(bytes) => {
                if lose_ack && !lost_ack && bytes == [6] {
                    lost_ack = true;
                } else {
                    queue.extend(
                        sender
                            .receive(&bytes)
                            .into_iter()
                            .map(|action| (true, action)),
                    );
                }
                vec![]
            }
            Action::ReadBlock(size) => {
                let end = (offset + size).min(content.len());
                let actions = sender.read(&content[offset..end]);
                offset = end;
                actions
            }
            Action::Write(bytes) => {
                saved.extend(bytes);
                receiver.written()
            }
            Action::Finished {
                error, cancelled, ..
            } => {
                assert_eq!(error, None);
                assert!(!cancelled);
                finished[usize::from(sending)] = true;
                vec![]
            }
            _ => vec![],
        };
        for action in more.into_iter().rev() {
            queue.push_front((sending, action));
        }
    }
    assert_eq!(finished, [true, true]);
    assert_eq!(&saved[..content.len()], content);
    assert!(saved[content.len()..].iter().all(|byte| *byte == 0x1a));
    assert_eq!(saved.len(), content.len().div_ceil(128) * 128);
    sizes
}

#[test]
fn memory_crc_fragmentation_and_sequence_wrap() {
    assert!(
        exchange(false, false, false, false, 33_033)
            .iter()
            .all(|size| *size == 128)
    );
}

#[test]
fn memory_checksum_falls_back_from_one_k() {
    assert!(
        exchange(true, true, false, false, 33_033)
            .iter()
            .all(|size| *size == 128)
    );
}

#[test]
fn memory_one_k_uses_small_tail() {
    let sizes = exchange(true, false, false, false, 33_033);
    assert_eq!(sizes[0], 1024);
    assert_eq!(&sizes[sizes.len() - 3..], &[128; 3]);
}

#[test]
fn memory_corruption_retransmission() {
    exchange(true, false, true, false, 33_033);
}

#[test]
fn memory_lost_ack_does_not_duplicate_file() {
    exchange(false, false, false, true, 33_033);
}

#[test]
fn empty_files_and_block_boundaries() {
    for length in [0, 1, 128, 1023, 1024, 1025] {
        exchange(true, false, false, false, length);
    }
}

#[test]
fn peer_cancel_in_same_batch_as_request_does_not_read_or_send() {
    for opening in [true, false] {
        let mut sender = Xmodem::new(Handshake::Send, false);
        if !opening {
            sender.receive(b"C");
            sender.read(&[0; 128]);
        }
        let actions = sender.receive(b"C\x18\x18prompt");
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            actions[0],
            Action::Finished {
                cancelled: true,
                ..
            }
        ));
        assert_eq!(sender.take_remainder(), b"prompt");
        assert!(sender.receive(&[0x18; 2]).is_empty());
    }
    let mut sender = Xmodem::new(Handshake::Send, false);
    sender.receive(&[0x18]);
    assert!(matches!(
        sender.receive(&[0x18])[0],
        Action::Finished {
            cancelled: true,
            ..
        }
    ));
}

#[test]
fn first_block_determines_checksum_even_when_crc_requested() {
    for crc in [false, true] {
        let mut receiver = Xmodem::new(Handshake::Receive, false);
        receiver.start();
        if crc {
            for _ in 0..3 {
                receiver.tick(10_000);
            }
        }
        let data = vec![0x1a; 128];
        let mut actions = receiver.receive(&block(1, &data, crc));
        if !crc {
            actions.extend(receiver.tick(2_000));
        }
        assert!(actions.contains(&Action::Write(data)));
    }
}

#[test]
fn sender_latest_request_and_opening_c_only_retries_first_block() {
    let mut sender = Xmodem::new(Handshake::Send, true);
    assert_eq!(sender.receive(b"CC\x15"), [Action::ReadBlock(128)]);
    let first = sender.read(&[0; 128]);
    assert_eq!(sender.receive(b"C"), first);
    sender.receive(&[6]);
    sender.read(&[1; 128]);
    assert!(sender.receive(b"C").is_empty());
}

#[test]
fn eot_requires_ack_and_retries_after_nak_or_timeout() {
    let mut sender = Xmodem::new(Handshake::Send, false);
    sender.receive(b"C");
    assert_eq!(sends(&sender.read(&[])), [vec![4]]);
    assert_eq!(sends(&sender.receive(&[0x15])), [vec![4]]);
    assert_eq!(sends(&sender.tick(10_000)), [vec![4]]);
    let actions = sender.receive(b"\x06prompt");
    assert!(actions.iter().any(|action| matches!(
        action,
        Action::Finished {
            error: None,
            cancelled: false,
            ..
        }
    )));
    assert_eq!(sender.take_remainder(), b"prompt");
    assert!(sender.cancel().is_empty());
}

#[test]
fn banners_cancel_and_trailing_prompt_are_terminal_bytes() {
    let mut receiver = Xmodem::new(Handshake::Receive, false);
    receiver.start();
    let actions = receiver.receive(b"banner\r\n");
    let output: Vec<_> = actions
        .into_iter()
        .flat_map(|action| {
            if let Action::Terminal(bytes) = action {
                bytes
            } else {
                vec![]
            }
        })
        .collect();
    assert_eq!(output, b"banner\r\n");
    let actions = receiver.receive(b"\x18\x18\x18\x18prompt");
    assert!(sends(&actions).is_empty());
    assert!(actions.iter().any(|action| matches!(
        action,
        Action::Finished {
            cancelled: true,
            ..
        }
    )));
    assert_eq!(receiver.take_remainder(), b"prompt");
    assert!(receiver.cancel().is_empty());
}

#[test]
fn local_cancel_and_timeout_notify_before_finishing() {
    for direction in [Handshake::Send, Handshake::Receive] {
        let mut machine = Xmodem::new(direction, false);
        machine.start();
        let actions = machine.cancel();
        assert_eq!(sends(&actions), [vec![0x18; 8]]);
        assert!(matches!(
            actions.last(),
            Some(Action::Finished {
                cancelled: true,
                ..
            })
        ));
        assert!(machine.read(b"late file read").is_empty());
        assert!(machine.written().is_empty());
        let mut machine = Xmodem::new(direction, false);
        machine.start();
        let mut actions = Vec::new();
        for _ in 0..10 {
            actions = machine.tick(10_000);
        }
        assert_eq!(sends(&actions), [vec![0x18; 8]]);
        assert!(
            matches!(actions.last(), Some(Action::Finished { error: Some(error), .. }) if error == "timeout")
        );
    }
}

#[test]
fn partial_block_times_out_and_can_bytes_in_payload_are_data() {
    let mut receiver = Xmodem::new(Handshake::Receive, false);
    receiver.start();
    receiver.receive(&[1, 1, 254, 0]);
    assert!(receiver.tick(1_999).is_empty());
    assert_eq!(sends(&receiver.tick(1)), [vec![0x15]]);
    assert!(
        receiver
            .receive(&block(1, &[0x18; 128], true))
            .contains(&Action::Write(vec![0x18; 128]))
    );
}
