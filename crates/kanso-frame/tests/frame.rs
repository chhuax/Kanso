//! 帧格式的集成测试：mock 往返、畸形输入、复用与边界。

use bytemuck::{bytes_of, cast_slice_mut};
use kanso_frame::mock::mock_frame;
use kanso_frame::*;

fn corrupt(frame: &Frame, f: impl FnOnce(&mut FrameHeader)) -> Vec<u64> {
    let mut words: Vec<u64> = frame
        .as_bytes()
        .chunks(8)
        .map(|c| u64::from_ne_bytes(c.try_into().unwrap()))
        .collect();
    let mut h = *frame.header();
    f(&mut h);
    cast_slice_mut::<u64, u8>(&mut words)[..HEADER_SIZE].copy_from_slice(bytes_of(&h));
    words
}

fn parse_words(w: &[u64]) -> Result<(), FrameError> {
    FrameView::parse(bytemuck::cast_slice(w)).map(|_| ())
}

#[test]
fn mock_parses_and_is_deterministic() {
    let a = mock_frame(80, 24, 3);
    let b = mock_frame(80, 24, 3);
    assert_eq!(a.as_bytes(), b.as_bytes());
    assert_ne!(a.as_bytes(), mock_frame(80, 24, 4).as_bytes());
    let v = a.view().unwrap();
    assert_eq!(v.header().seq, 3);
    assert_eq!(v.header().total_len as usize, a.as_bytes().len());
}

#[test]
fn mock_covers_features() {
    let f = mock_frame(80, 24, 0);
    let v = f.view().unwrap();
    assert_ne!(v.line_meta(0).unwrap().flags & LINE_FIRST_PROMPT, 0);
    assert_ne!(v.line_meta(6).unwrap().flags & LINE_PROMPT, 0);
    assert_eq!(v.line_meta(6).unwrap().flags & LINE_FIRST_PROMPT, 0);
    assert_ne!(v.line_meta(23).unwrap().flags & LINE_BLANK, 0);
    let all: Vec<&Cell> = (0..24).flat_map(|r| v.row(r)).collect();
    let any = |m: u16| all.iter().any(|c| c.flags & m != 0);
    for m in [
        CELL_WIDE,
        CELL_WIDE_SPACER,
        CELL_SELECTED,
        CELL_MATCH,
        CELL_MATCH_CURRENT,
        CELL_STRIKE,
        CELL_INVERSE,
    ] {
        assert!(any(m), "缺少 flag {m:#x}");
    }
    assert!(
        all.iter()
            .any(|c| c.fg >> COLOR_TAG_SHIFT == COLOR_TAG_SEMANTIC)
    );
    let zwj = all
        .iter()
        .filter_map(|c| v.extra(c))
        .filter_map(|e| v.grapheme(e))
        .any(|g| g.contains('\u{200D}'));
    assert!(zwj);
    assert!(
        all.iter()
            .filter_map(|c| v.extra(c))
            .any(|e| e.hyperlink_id != 0)
    );
    assert_ne!(v.header().flags & FRAME_CURSOR_VISIBLE, 0);
}

#[test]
fn reuse_matches_fresh() {
    let old = mock_frame(120, 40, 9);
    let mut b = FrameBuilder::reuse(old, 4, 3);
    let mut fresh = FrameBuilder::new(4, 3);
    for x in [&mut b, &mut fresh] {
        x.set_seq(5);
        x.set_cell(
            1,
            2,
            Cell {
                ch: 65,
                fg: 0,
                bg: 0,
                flags: 0,
                extra: 0,
            },
        );
    }
    assert_eq!(b.finish().as_bytes(), fresh.finish().as_bytes());
}

#[test]
fn tiny_and_empty_sizes() {
    for (c, r) in [(1, 1), (0, 0), (0, 5), (5, 0), (2, 1), (3, 3), (1, 24)] {
        for seq in [0, 1, 13] {
            mock_frame(c, r, seq).view().unwrap();
        }
    }
}

#[test]
fn malformed_inputs_error() {
    let f = mock_frame(80, 24, 0);
    let h = *f.header();
    assert_eq!(
        parse_words(&corrupt(&f, |h| h.magic = 0)),
        Err(FrameError::BadMagic)
    );
    assert_eq!(
        parse_words(&corrupt(&f, |h| h.version = 9)),
        Err(FrameError::UnsupportedVersion(9))
    );
    assert_eq!(
        parse_words(&corrupt(&f, |h| h.header_size = 8)),
        Err(FrameError::BadHeaderSize)
    );
    assert_eq!(
        parse_words(&corrupt(&f, |h| h.total_len += 8)),
        Err(FrameError::TotalLenMismatch)
    );
    assert!(matches!(
        parse_words(&corrupt(&f, |h| h.cells_offset = h.total_len)),
        Err(FrameError::SectionOutOfBounds("cells"))
    ));
    assert!(matches!(
        parse_words(&corrupt(&f, |h| h.line_meta_offset = u32::MAX - 7)),
        Err(FrameError::SectionOutOfBounds("line_meta"))
    ));
    assert!(matches!(
        parse_words(&corrupt(&f, |h| h.extras_count = u32::MAX)),
        Err(FrameError::SectionOutOfBounds("extras"))
    ));
    assert!(matches!(
        parse_words(&corrupt(&f, |h| h.pool_len = h.total_len)),
        Err(FrameError::SectionOutOfBounds("pool"))
    ));
    assert_eq!(
        FrameView::parse(&f.as_bytes()[..4]).unwrap_err(),
        FrameError::TooShort
    );
    assert_eq!(
        FrameView::parse(&f.as_bytes()[..HEADER_SIZE]).unwrap_err(),
        FrameError::TotalLenMismatch
    );
    // 错位的起点必须报错而不是 panic。
    let shifted = &f.as_bytes()[1..];
    assert!(FrameView::parse(shifted).is_err());
    let _ = h;
}

#[test]
fn malformed_body_errors() {
    // 手工构造：extra 索引越界、池区间越界、非法 UTF-8。
    let mut b = FrameBuilder::new(1, 1);
    b.set_cell(
        0,
        0,
        Cell {
            ch: 0,
            fg: 0,
            bg: 0,
            flags: 0,
            extra: 1,
        },
    );
    assert_eq!(
        b.finish().view().unwrap_err(),
        FrameError::ExtraIndexOutOfRange
    );

    let mut b = FrameBuilder::new(1, 1);
    let e = b
        .push_extra(CellExtra {
            grapheme_offset: 0,
            grapheme_len: 4,
            hyperlink_id: 0,
            underline_color: 0,
        })
        .unwrap();
    b.set_cell(
        0,
        0,
        Cell {
            ch: 0,
            fg: 0,
            bg: 0,
            flags: 0,
            extra: e,
        },
    );
    assert_eq!(
        b.finish().view().unwrap_err(),
        FrameError::GraphemeOutOfRange
    );

    let mut b = FrameBuilder::new(1, 1);
    b.push_grapheme("ab").unwrap();
    let e = b
        .push_extra(CellExtra {
            grapheme_offset: 0,
            grapheme_len: 2,
            hyperlink_id: 0,
            underline_color: 0,
        })
        .unwrap();
    b.set_cell(
        0,
        0,
        Cell {
            ch: 0,
            fg: 0,
            bg: 0,
            flags: 0,
            extra: e,
        },
    );
    let f = b.finish();
    let mut words: Vec<u64> = f
        .as_bytes()
        .chunks(8)
        .map(|c| u64::from_ne_bytes(c.try_into().unwrap()))
        .collect();
    let pool_off = f.header().pool_offset as usize;
    cast_slice_mut::<u64, u8>(&mut words)[pool_off] = 0xFF;
    assert_eq!(parse_words(&words), Err(FrameError::InvalidUtf8));
}

#[test]
fn builder_limits_and_oob() {
    let mut b = FrameBuilder::new(2, 2);
    b.set_cell(
        5,
        5,
        Cell {
            ch: 1,
            fg: 0,
            bg: 0,
            flags: 0,
            extra: 0,
        },
    );
    assert!(b.line_meta_mut(2).is_none());
    for _ in 0..u16::MAX {
        b.push_extra(CellExtra {
            grapheme_offset: 0,
            grapheme_len: 0,
            hyperlink_id: 0,
            underline_color: 0,
        })
        .unwrap();
    }
    assert_eq!(
        b.push_extra(CellExtra {
            grapheme_offset: 0,
            grapheme_len: 0,
            hyperlink_id: 0,
            underline_color: 0
        }),
        Err(FrameError::ExtrasFull)
    );
    b.finish().view().unwrap();
}
