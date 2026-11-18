use super::*;
use crate::wire::{crc16, header, packet};
use std::collections::VecDeque;

#[test]
fn crc_and_split_sentinel() {
    assert_eq!(crc16(b"123456789"), 0x31c3);
    for split in 0..20 {
        let mut sentinel = Sentinel::default();
        let mut terminal = Vec::new();
        let handshake = header(0, 0);
        assert_eq!(sentinel.feed(&handshake[..split], &mut terminal), None);
        assert_eq!(
            sentinel.feed(&handshake[split..], &mut terminal),
            Some(Handshake::Receive)
        );
        assert!(terminal.ends_with(b"\r\x1b[2K"));
    }
}

#[test]
fn memory_transfer_every_byte_value_and_fragmentation() {
    let source: Vec<u8> = (0..65537).map(|index| index as u8).collect();
    for fragment in [1, 7, 4096] {
        let mut sender = Machine::new(Handshake::Send);
        let mut receiver = Machine::new(Handshake::Receive);
        let mut pending = VecDeque::new();
        let mut saved = Vec::new();
        let mut offset = 0;
        let mut offered = false;
        let mut finished = 0;
        pending.extend(sender.start().into_iter().map(|action| (true, action)));
        pending.extend(receiver.start().into_iter().map(|action| (false, action)));
        while let Some((sending, action)) = pending.pop_front() {
            let machine = if sending { &mut sender } else { &mut receiver };
            let actions = match action {
                Action::Send(bytes) => {
                    let peer = if sending { &mut receiver } else { &mut sender };
                    for chunk in bytes.chunks(fragment) {
                        pending.extend(
                            peer.receive(chunk)
                                .into_iter()
                                .map(|action| (!sending, action)),
                        );
                    }
                    vec![]
                }
                Action::PickFiles | Action::NextFile => {
                    let file = (!offered).then(|| FileInfo {
                        name: "数据.bin".into(),
                        size: Some(source.len() as u64),
                    });
                    offered = true;
                    machine.next_file(file)
                }
                Action::Offer(_) => machine.accept(),
                Action::Read => {
                    let end = (offset + BLOCK_SIZE).min(source.len());
                    let actions = machine.read(&source[offset..end]);
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
                    assert_eq!(error, None);
                    assert!(!cancelled);
                    finished += 1;
                    vec![]
                }
                _ => vec![],
            };
            pending.extend(actions.into_iter().map(|action| (sending, action)));
        }
        assert_eq!(finished, 2);
        assert_eq!(saved, source);
    }
}

#[test]
fn disk_error_precedes_eof_and_later_protocol_error() {
    let mut receiver = Machine::new(Handshake::Receive);
    receiver.start();
    receiver.receive(&header(4, 0));
    receiver.receive(&packet(b"a\0", b'k'));
    receiver.accept();
    let bytes = [header(10, 0), packet(b"content", b'h'), header(11, 7)].concat();
    assert_eq!(
        receiver.receive(&bytes),
        vec![Action::Write(b"content".to_vec())]
    );
    receiver.skip(Some("disk full".into()));
    let actions = receiver.fail("network closed".into());
    assert!(
        matches!(actions.last(), Some(Action::Finished { error: Some(error), .. }) if error == "disk full")
    );
    assert!(receiver.cancel().is_empty());
}

#[test]
fn peer_cancel_is_not_echoed_and_timeout_is_injected() {
    let mut receiver = Machine::new(Handshake::Receive);
    receiver.start();
    assert!(matches!(
        receiver.receive(&[0x18; 8]).as_slice(),
        [Action::Finished {
            cancelled: true,
            ..
        }]
    ));
    let mut receiver = Machine::new(Handshake::Receive);
    receiver.start();
    assert!(receiver.tick(59999).is_empty());
    assert!(matches!(
        receiver.tick(1).last(),
        Some(Action::Finished { error: Some(_), .. })
    ));
}

#[test]
fn finish_keeps_shell_output_in_same_read() {
    for suffix in [b"OO$ ".as_slice(), b"$ "] {
        let mut receiver = Machine::new(Handshake::Receive);
        receiver.start();
        let actions = receiver.receive(&[header(8, 0), suffix.to_vec()].concat());
        assert!(matches!(actions.last(), Some(Action::Finished { .. })));
        assert_eq!(receiver.take_remainder(), b"$ ");
    }
    let mut receiver = Machine::new(Handshake::Receive);
    receiver.start();
    receiver.receive(&header(8, 0));
    assert!(receiver.receive(b"O").is_empty());
    assert!(matches!(
        receiver.receive(b"O$ ").last(),
        Some(Action::Finished { .. })
    ));
    assert_eq!(receiver.take_remainder(), b"$ ");
}

#[test]
fn failed_file_ignores_in_flight_data_and_continues() {
    let mut receiver = Machine::new(Handshake::Receive);
    receiver.start();
    receiver.receive(&[header(4, 0), packet(b"a\0", b'k')].concat());
    receiver.accept();
    receiver.receive(&[header(10, 0), packet(b"content", b'i')].concat());
    assert!(matches!(
        receiver.skip(Some("disk full".into())).last(),
        Some(Action::Send(_))
    ));
    let bytes = [
        packet(b"discard", b'h'),
        header(11, 14),
        header(4, 0),
        packet(b"b\0", b'k'),
    ]
    .concat();
    assert_eq!(
        receiver.receive(&bytes),
        vec![Action::Offer(FileInfo {
            name: "b".into(),
            size: None
        })]
    );
    receiver.accept();
    assert_eq!(
        receiver.receive(&[header(10, 0), packet(b"ok", b'h')].concat()),
        vec![Action::Write(b"ok".to_vec())]
    );
    receiver.written();
    receiver.receive(&header(11, 2));
    let actions = receiver.receive(&[header(8, 0), b"OO".to_vec()].concat());
    assert!(
        matches!(actions.last(), Some(Action::Finished { received: 1, skipped: 1, error: Some(error), .. }) if error == "disk full")
    );
}

#[test]
fn cancellation_while_choosing_files_and_resume_rejection() {
    let mut sender = Machine::new(Handshake::Send);
    sender.start();
    assert!(sender.receive(&[0x18; 3]).is_empty());
    assert!(matches!(
        sender.receive(&[0x18; 2]).last(),
        Some(Action::Finished {
            cancelled: true,
            ..
        })
    ));
    assert!(sender.cancel().is_empty());
    let mut sender = Machine::new(Handshake::Send);
    sender.start();
    sender.next_file(Some(FileInfo {
        name: "a".into(),
        size: None,
    }));
    assert!(
        matches!(sender.receive(&header(9, 1)).last(), Some(Action::Finished { error: Some(error), .. }) if error == "resume is not supported")
    );
    let mut sender = Machine::new(Handshake::Send);
    sender.start();
    let actions = sender.cancel();
    assert!(
        matches!(actions.first(), Some(Action::Send(bytes)) if bytes == &[vec![0x18; 8], vec![8; 10]].concat())
    );
}

#[test]
fn ordinary_text_and_invalid_handshake() {
    let mut sentinel = Sentinel::default();
    let mut terminal = Vec::new();
    for bytes in [b"hello *".as_slice(), b"*", b" world"] {
        assert_eq!(sentinel.feed(bytes, &mut terminal), None);
    }
    assert_eq!(terminal, b"hello ** world");
    terminal.clear();
    assert_eq!(sentinel.feed(b"***", &mut terminal), None);
    assert_eq!(terminal, b"***");
    assert_eq!(sentinel.feed(b"text", &mut terminal), None);
    assert_eq!(terminal, b"***text");
    terminal.clear();
    for bytes in [b"**\x18".as_slice(), b"x*", b"y"] {
        assert_eq!(sentinel.feed(bytes, &mut terminal), None);
    }
    assert_eq!(terminal, b"**\x18x*y");
    let mut invalid = header(0, 0);
    invalid[10] = b'f';
    terminal.clear();
    assert_eq!(sentinel.feed(&invalid, &mut terminal), None);
    assert_eq!(terminal, b"\r\x1b[2K");
}

#[test]
fn star_batch_64k_compacts_once_and_preserves_split_handshake() {
    let mut sentinel = Sentinel::default();
    let mut terminal = Vec::new();
    let stars = vec![b'*'; 64 * 1024];
    assert_eq!(sentinel.feed(&stars, &mut terminal), None);
    assert_eq!(terminal, stars);
    assert_eq!(sentinel.compactions, 1);
    let handshake = header(0, 0);
    assert_eq!(
        sentinel.feed(&handshake[2..], &mut terminal),
        Some(Handshake::Receive)
    );
    assert_eq!(terminal.len(), stars.len() + b"\r\x1b[2K".len());
}

#[test]
fn sender_moves_to_next_file_when_peer_skips() {
    let mut sender = Machine::new(Handshake::Send);
    sender.start();
    sender.next_file(Some(FileInfo {
        name: "a".into(),
        size: None,
    }));
    assert_eq!(
        sender.receive(&header(5, 0)),
        vec![Action::Skipped, Action::NextFile]
    );
    sender.next_file(None);
    assert!(matches!(
        sender.receive(&header(8, 0)).last(),
        Some(Action::Finished {
            skipped: 1,
            cancelled: false,
            error: None,
            ..
        })
    ));
}

#[test]
fn data_before_file_selection_fails_without_requesting_a_write() {
    let mut receiver = Machine::new(Handshake::Receive);
    receiver.start();
    let actions = receiver.receive(&[header(10, 0), packet(b"payload", b'h')].concat());
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, Action::Write(_)))
    );
    assert!(matches!(
        actions.last(),
        Some(Action::Finished { error: Some(_), .. })
    ));
}

#[test]
fn empty_file_accepts_flow_control_before_subpacket_end() {
    let mut receiver = Machine::new(Handshake::Receive);
    receiver.start();
    receiver.receive(&[header(4, 0), packet(b"empty\0", b'k')].concat());
    receiver.accept();
    let mut data_header = header(10, 0);
    data_header.pop();
    assert!(receiver.receive(&data_header).is_empty());
    assert_eq!(
        receiver.receive(&[vec![0x11], packet(b"", b'h'), header(11, 0)].concat()),
        vec![Action::Write(vec![])]
    );
    assert!(receiver.written().contains(&Action::FileDone));
    assert_eq!(
        receiver.receive(&header(11, 0)),
        vec![Action::Send(header(1, 0x23000000))]
    );
    assert!(matches!(
        receiver
            .receive(&[header(8, 0), b"OO".to_vec()].concat())
            .last(),
        Some(Action::Finished {
            received: 1,
            error: None,
            ..
        })
    ));
}

#[test]
fn local_cancel_filters_split_subpackets_echo_and_empty_crc_trailer() {
    let mut packet = packet(b"file bytes including \r\n and $ ", b'i');
    for width in [1, 3, 4096] {
        let mut sentinel = Sentinel::default();
        let mut receiver = Machine::new(Handshake::Receive);
        receiver.receive(&header(4, 0));
        receiver.receive(&crate::wire::packet(b"a\0", b'k'));
        receiver.accept();
        receiver.receive(&header(10, 0));
        receiver.receive(&packet[..4]);
        receiver.hand_off_cancelled_receive(&mut sentinel);
        receiver.cancel();
        assert!(receiver.take_remainder().is_empty());
        let pending = [
            &packet[4..],
            &packet,
            &[0x18; 10],
            &[8; 10],
            &[0x18, b'h', 0xe7, 0x06, b'k', 0x18, 0xd1],
            b"\x1b[32muser@host $ ",
        ]
        .concat();
        let mut terminal = Vec::new();
        for chunk in pending.chunks(width) {
            sentinel.feed(chunk, &mut terminal);
        }
        assert_eq!(terminal, b"\x1b[32muser@host $ ");
    }
    // 没有取消回声的对端也可以直接回到 shell，首条提示符不必带换行。
    let mut sentinel = Sentinel::default();
    sentinel.cancelled_receive(packet.drain(..4).collect(), Some(false));
    let mut terminal = Vec::new();
    sentinel.feed(b"first prompt$ ", &mut terminal);
    assert!(sentinel.poll_cancel_tail().is_empty());
    terminal.extend(sentinel.poll_cancel_tail());
    assert_eq!(terminal, b"first prompt$ ");
}

#[test]
fn local_cancel_filters_trailing_zeof_and_zfin_headers() {
    // 最后一个子包以 ZCRCE 结尾，随后才是 ZEOF。
    let packet = packet(b"last bytes", b'h');
    let mut eof = header(11, 10);
    assert_eq!(eof.pop(), Some(0x11));
    for fin in [false, true] {
        for width in [1, 3, 4096] {
            let mut sentinel = Sentinel::default();
            let mut receiver = Machine::new(Handshake::Receive);
            receiver.receive(&header(4, 0));
            receiver.receive(&crate::wire::packet(b"a\0", b'k'));
            receiver.accept();
            receiver.receive(&header(10, 0));
            receiver.receive(&packet[..4]);
            receiver.hand_off_cancelled_receive(&mut sentinel);
            receiver.cancel();
            // ZEOF 的 XON 单独晚到；ZFIN 后的 OO 则要原样交给终端。
            let mut terminal = Vec::new();
            for chunk in [&packet[4..], eof.as_slice()].concat().chunks(width) {
                sentinel.feed(chunk, &mut terminal);
            }
            sentinel.feed(&[0x11], &mut terminal);
            if fin {
                for chunk in header(8, 0).chunks(width) {
                    sentinel.feed(chunk, &mut terminal);
                }
            }
            let prompt: &[u8] = if fin {
                b"OOuser@host $ "
            } else {
                b"user@host $ "
            };
            for chunk in prompt.chunks(width) {
                sentinel.feed(chunk, &mut terminal);
            }
            terminal.extend(sentinel.poll_cancel_tail());
            terminal.extend(sentinel.poll_cancel_tail());
            assert_eq!(terminal, prompt, "fin={fin} width={width}");
        }
    }
}

#[test]
fn continuous_plain_output_after_cancel_is_released_without_waiting_for_silence() {
    let mut sentinel = Sentinel::default();
    sentinel.cancelled_receive(b"old".to_vec(), Some(false));
    assert!(!sentinel.cancel_tail_pending());
    let mut terminal = Vec::new();
    sentinel.feed(b"prompt$ ", &mut terminal);
    assert!(sentinel.cancel_tail_pending());
    terminal.extend(sentinel.poll_cancel_tail());
    assert!(terminal.is_empty());
    sentinel.feed(b"still printing", &mut terminal);
    terminal.extend(sentinel.poll_cancel_tail());
    sentinel.feed(b"!", &mut terminal);
    assert_eq!(terminal, b"prompt$ still printing!");
    assert!(!sentinel.cancel_tail_pending());
}
