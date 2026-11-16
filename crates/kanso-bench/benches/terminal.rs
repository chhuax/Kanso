//! VT、回滚搜索、帧快照与编码转换基准；测量方法见 docs/performance.md。

use std::fmt::Write;
use std::hint::black_box;
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term};
use criterion::{BatchSize, Criterion, Throughput};
use kanso_encoding::{StreamDecoder, TextEncoding, encode_text};
use kanso_frame::{Cell, FrameBuilder};
use kanso_term::{SearchQuery, TermConfig, Terminal};
use kanso_types::{geometry::GridSize, profile::EncodingLabel};
use vte::ansi::{Processor, Timeout};

const COLS: u16 = 120;
const ROWS: u16 = 40;
const HISTORY: usize = 10_000;
const CHUNK: usize = 8192;

struct Size;
impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        usize::from(ROWS)
    }
    fn screen_lines(&self) -> usize {
        usize::from(ROWS)
    }
    fn columns(&self) -> usize {
        usize::from(COLS)
    }
}

#[derive(Clone)]
struct Sink;
impl EventListener for Sink {
    fn send_event(&self, _: Event) {}
}

#[derive(Default)]
struct NoSync;
impl Timeout for NoSync {
    fn set_timeout(&mut self, _: Duration) {}
    fn clear_timeout(&mut self) {}
    fn pending_timeout(&self) -> bool {
        false
    }
}

fn terminal(history: usize) -> Terminal {
    Terminal::new(TermConfig {
        size: GridSize {
            cols: COLS,
            rows: ROWS,
        },
        scrollback_lines: history,
    })
}

fn output(kind: &str) -> Vec<u8> {
    let mut text = String::new();
    let count = if kind == "wrap" { 20_000 } else { 300_000 };
    for number in 1..=count {
        if number % 1024 == 1 {
            text.push_str("\x1b]133;A\x07$ seq\x1b]133;B\x07\r\n\x1b]133;C\x07");
        }
        match kind {
            "plain" => writeln!(text, "{number}\r").unwrap(),
            "sgr" => writeln!(text, "\x1b[32m{number}\x1b[0m\r").unwrap(),
            "wrap" => writeln!(text, "{number:06} {}\r", "abcdefgh".repeat(128)).unwrap(),
            _ => unreachable!(),
        }
        if number % 1024 == 0 {
            text.push_str("\x1b]133;D;0\x07");
        }
    }
    text.into_bytes()
}

fn vt(criterion: &mut Criterion) {
    for kind in ["plain", "sgr", "wrap"] {
        let data = output(kind);
        let mut group = criterion.benchmark_group(format!("vt_{kind}"));
        group.throughput(Throughput::Bytes(data.len() as u64));
        group.bench_function("bare", |bencher| {
            bencher.iter_batched_ref(
                || {
                    (
                        Term::new(
                            Config {
                                scrolling_history: HISTORY,
                                ..Config::default()
                            },
                            &Size,
                            Sink,
                        ),
                        Processor::<NoSync>::new(),
                    )
                },
                |(term, parser)| {
                    for chunk in data.chunks(CHUNK) {
                        parser.advance(term, black_box(chunk));
                    }
                    black_box(term.grid());
                },
                BatchSize::PerIteration,
            );
        });
        group.bench_function("metadata", |bencher| {
            bencher.iter_batched_ref(
                || terminal(HISTORY),
                |term| {
                    for chunk in data.chunks(CHUNK) {
                        term.feed(black_box(chunk), 1_700_000_000_000);
                    }
                    black_box(term.cursor_line());
                },
                BatchSize::PerIteration,
            );
        });
        group.finish();
    }
}

fn search(criterion: &mut Criterion) {
    let mut term = terminal(100_000);
    let mut text = String::new();
    for number in 0..100_040 {
        writeln!(
            text,
            "{number:06} INFO worker=17 request completed status=200 elapsed=12ms\r"
        )
        .unwrap();
    }
    for chunk in text.as_bytes().chunks(CHUNK) {
        term.feed(chunk, 1_700_000_000_000);
    }
    assert_eq!(term.line_meta_len(), 100_040);
    let mut group = criterion.benchmark_group("search_100k");
    for (name, needle, regex, expected) in [
        ("miss", "missing-request", false, 0),
        ("hit", "050000", false, 1),
        ("regex", "050[0-9]{3}.*status=200", true, 1000),
    ] {
        let query = SearchQuery {
            text: needle.into(),
            case_sensitive: true,
            regex,
        };
        assert_eq!(term.search(&query).total, expected);
        group.bench_function(name, |bencher| {
            bencher.iter(|| black_box(term.search(black_box(&query))))
        });
        term.search_close();
    }
    group.finish();
}

fn decorations(criterion: &mut Criterion) {
    for kind in ["plain", "sgr", "wrap"] {
        let data = output(kind);
        let mut group = criterion.benchmark_group(format!("decorations_{kind}"));
        group.throughput(Throughput::Bytes(data.len() as u64));
        for enabled in [false, true] {
            group.bench_function(if enabled { "all" } else { "off" }, |bencher| {
                bencher.iter_batched_ref(
                    || {
                        let mut term = terminal(HISTORY);
                        term.set_semantic_coloring(enabled);
                        term.feed(b"0\r\n", 1_700_000_000_000);
                        term.snapshot(None);
                        term
                    },
                    |term| {
                        let mut frame = None;
                        for frame_input in data.chunks(1_048_576) {
                            for chunk in frame_input.chunks(CHUNK) {
                                term.feed(black_box(chunk), 1_700_000_000_000);
                            }
                            let next = term.snapshot(frame.take());
                            if enabled {
                                let view = next.view().unwrap();
                                for row in 0..ROWS {
                                    let meta = view.line_meta(row).unwrap();
                                    black_box((meta.timestamp_ms, meta.abs_line, meta.flags));
                                }
                                black_box(&next.command_blocks);
                            }
                            black_box(next.as_bytes());
                            frame = Some(next);
                        }
                    },
                    BatchSize::PerIteration,
                );
            });
        }
        group.finish();
    }
}

fn frames(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("frame_200x60");
    let cell = Cell {
        ch: u32::from('W'),
        fg: 0x0100_0002,
        bg: 0,
        flags: 0,
        extra: 0,
    };
    for reuse in [false, true] {
        let mut previous = None;
        group.bench_function(if reuse { "reuse" } else { "new" }, |bencher| {
            bencher.iter(|| {
                let mut builder = match previous.take() {
                    Some(frame) => FrameBuilder::reuse(frame, 200, 60),
                    None => FrameBuilder::new(200, 60),
                };
                for row in 0..60 {
                    for col in 0..200 {
                        builder.set_cell(row, col, black_box(cell));
                    }
                }
                let frame = builder.finish();
                black_box(frame.as_bytes());
                if reuse {
                    previous = Some(frame);
                }
            })
        });
    }
    let mut term = terminal(HISTORY);
    term.resize(GridSize {
        cols: 200,
        rows: 60,
    });
    term.feed(output("sgr").as_slice(), 1_700_000_000_000);
    let mut previous = None;
    group.bench_function("terminal_snapshot", |bencher| {
        bencher.iter(|| {
            let frame = term.snapshot(previous.take());
            black_box(frame.as_bytes());
            previous = Some(frame);
        })
    });
    group.finish();
}

fn encoding(criterion: &mut Criterion) {
    let text = "日本語端末 performance 0123456789\r\n".repeat(32_768);
    for label in ["utf-8", "gbk", "shift_jis"] {
        let encoding = TextEncoding::resolve(&EncodingLabel::new(label));
        let data = encode_text(encoding, &text);
        let mut decoder = StreamDecoder::new(encoding);
        let mut decoded = String::new();
        decoder.decode(&data, &mut decoded);
        assert_eq!(decoded, text);
        let mut group = criterion.benchmark_group(format!("encoding_{label}"));
        group.throughput(Throughput::Bytes(data.len() as u64));
        group.bench_function("decode", |bencher| {
            bencher.iter(|| {
                decoder.reset();
                for chunk in data.chunks(CHUNK) {
                    decoded.clear();
                    decoder.decode(black_box(chunk), &mut decoded);
                    black_box(&decoded);
                }
            })
        });
        let name = if label == "utf-8" {
            group.throughput(Throughput::Elements(1));
            "encode_borrowed"
        } else {
            group.throughput(Throughput::Bytes(text.len() as u64));
            "encode"
        };
        group.bench_function(name, |bencher| {
            bencher.iter(|| black_box(encode_text(encoding, black_box(&text))))
        });
        group.finish();
    }
}

fn main() {
    let mut criterion = Criterion::default()
        .sample_size(20)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3))
        .configure_from_args();
    vt(&mut criterion);
    decorations(&mut criterion);
    search(&mut criterion);
    frames(&mut criterion);
    encoding(&mut criterion);
    criterion.final_summary();
}
