//! 流式编码转换，能处理跨块截断的多字节字符（sans-IO）。
//!
//! 本 crate 只负责**文本**的编码转换：
//!
//! - 输出方向（远端 → 终端）：[`StreamDecoder`] 把会话编码的字节流解码成 UTF-8；
//! - 输入方向（键盘、粘贴 → 远端）：[`encode_text`] 把 UTF-8 文本编码成会话编码。
//!
//! # 二进制写入永远不调用本 crate
//!
//! 传输协议（ZMODEM、XMODEM）的帧和终端回应（DA、CPR、焦点事件等）是二进制字节，
//! 经过编码器会被改写（例如不可映射字节变成 `?`，或多字节序列被重组），
//! 直接破坏协议。这类写入必须绕开本 crate，原样写入 PTY / SSH 通道。
//!
//! # 同一个解析结果
//!
//! 输入、输出两个方向必须使用同一个 [`TextEncoding`]：会话只调用一次
//! [`TextEncoding::resolve`]，把得到的值同时交给 [`StreamDecoder::new`] 和
//! [`encode_text`]。不要在两个方向各自解析标签，否则解析规则一旦演进就可能出现不一致。

use std::borrow::Cow;

use encoding_rs::{Decoder, EncoderResult, Encoding, UTF_8};
use kanso_types::profile::EncodingLabel;

/// 会话使用的文本编码，由 [`TextEncoding::resolve`] 一次解析得到。
///
/// 是 `Copy` 的轻量值（内部只是 `&'static Encoding`）。
/// 会话创建时解析一次，输入、输出两个方向共用。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextEncoding(&'static Encoding);

impl TextEncoding {
    /// UTF-8。
    pub fn utf8() -> Self {
        Self(UTF_8)
    }

    /// 按 WHATWG 标签解析。
    ///
    /// 以下情况一律回退 UTF-8，保证手工编辑或导入的配置不会让终端失效：
    /// - 未知标签、空标签；
    /// - UTF-16 系列：它无法承载终端的控制字节（ESC、换行等会被当成半个码元）；
    /// - replacement 系列（hz-gb-2312、iso-2022-cn 等）：解码结果没有可用内容；
    /// - `x-user-defined`：把高位字节映射到私有区码点，终端里显示不出任何可读内容。
    ///   会话对话框里选不到它，只可能来自手工编辑或导入的配置，按"不能让终端失效"的原则回退。
    /// - `iso-2022-jp`：靠 ESC 序列切换字符集，解码器会把终端自己的控制序列（`ESC [` 等）
    ///   当成非法字节替换掉，颜色和光标控制全部失效。它也不在会话对话框的可选列表里。
    ///
    /// 标签大小写和首尾空白不影响结果（WHATWG 的标签匹配本身就会忽略它们）。
    pub fn resolve(label: &EncodingLabel) -> Self {
        // for_label 内部已做 ASCII 大小写折叠与首尾空白裁剪；EncodingLabel 规范化后更不会有差异。
        let enc = Encoding::for_label(label.as_str().as_bytes()).unwrap_or(UTF_8);
        if enc == encoding_rs::X_USER_DEFINED || enc == encoding_rs::ISO_2022_JP {
            return Self(UTF_8);
        }
        // output_encoding() 对 UTF-16LE/BE 与 replacement 返回 UTF-8，其余返回自身，
        // 正好覆盖其余回退规则，不必手写枚举。
        Self(enc.output_encoding())
    }

    /// 是否为 UTF-8。
    pub fn is_utf8(self) -> bool {
        self.0 == UTF_8
    }

    /// 规范名称（WHATWG 编码名，如 `GBK`、`windows-1252`）。
    pub fn name(self) -> &'static str {
        self.0.name()
    }
}

/// 流式解码器：把会话编码的字节流解码成 UTF-8 文本。
///
/// 内部保留跨调用的未完成字节，所以多字节字符被拆在两次 [`decode`](Self::decode)
/// 之间也能正确拼接。UTF-8 会话同样走这里：远端的 UTF-8 也可能在块边界被截断。
pub struct StreamDecoder {
    encoding: TextEncoding,
    decoder: Decoder,
}

impl StreamDecoder {
    /// 创建解码器。
    pub fn new(encoding: TextEncoding) -> Self {
        Self {
            encoding,
            decoder: Self::new_inner(encoding),
        }
    }

    fn new_inner(encoding: TextEncoding) -> Decoder {
        // 必须用 without_bom_handling：终端流中间出现的 U+FEFF 字节序列
        // 不是 BOM，不能被吞掉或触发编码切换。
        encoding.0.new_decoder_without_bom_handling()
    }

    /// 解码一块字节并追加到 `out`。
    ///
    /// 非法字节序列替换成 U+FFFD。`out` 由调用方持有并复用，
    /// 本方法只在容量不足时扩容，不为每块单独分配。
    pub fn decode(&mut self, bytes: &[u8], out: &mut String) {
        // 精确的最坏情况容量（含上一次遗留的半个字符）；
        // 只有预留足够，decode_to_string 才不会返回 OutputFull，单次调用即可处理完。
        let need = self
            .decoder
            .max_utf8_buffer_length(bytes.len())
            .unwrap_or(bytes.len().saturating_mul(3).saturating_add(16));
        out.reserve(need);
        let (_result, _read, _had_errors) = self.decoder.decode_to_string(bytes, out, false);
    }

    /// 重置解码状态，丢弃残留的半个字符。重新连接时必须调用，
    /// 否则上一次会话被截断的字节会和新会话的开头拼成乱码。
    pub fn reset(&mut self) {
        self.decoder = Self::new_inner(self.encoding);
    }
}

/// 把文本编码成会话编码的字节，用于键盘输入与粘贴。
///
/// - UTF-8：直接借用 `text`，不拷贝。
/// - 其他编码：目标编码里没有的字符编码成单个 `?`。
///   WHATWG 默认会输出 HTML 数字实体（如 `&#128512;`），这些字符会原样进入 shell
///   被当成命令文本执行，所以不能用默认行为。
///
/// 任意长度的输入都会完整编码，不截断。
pub fn encode_text(enc: TextEncoding, text: &str) -> Cow<'_, [u8]> {
    if enc.is_utf8() {
        return Cow::Borrowed(text.as_bytes());
    }
    let mut encoder = enc.0.new_encoder();
    // 多数字符一个字符对应 1~2 字节；初始容量取字节数加余量，不够再扩容。
    let mut out = vec![0u8; text.len().saturating_add(16)];
    let mut written = 0;
    let mut rest = text;
    loop {
        let (result, read, w) =
            encoder.encode_from_utf8_without_replacement(rest, &mut out[written..], true);
        written += w;
        rest = &rest[read..];
        match result {
            EncoderResult::InputEmpty => break,
            // 输出缓冲不足：翻倍扩容后继续，不能把它当成结束，否则长输入会被截断。
            EncoderResult::OutputFull => {
                let new_len = out.len().saturating_mul(2).max(64);
                out.resize(new_len, 0);
            }
            EncoderResult::Unmappable(_) => {
                // 缓冲区刚好写满时 `?` 没有位置，先扩容。
                if written == out.len() {
                    let new_len = out.len().saturating_mul(2).max(64);
                    out.resize(new_len, 0);
                }
                out[written] = b'?';
                written += 1;
            }
        }
    }
    out.truncate(written);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(label: &str) -> TextEncoding {
        TextEncoding::resolve(&EncodingLabel::new(label))
    }

    /// 按每个字节切块解码。
    fn decode_bytewise(enc: TextEncoding, bytes: &[u8]) -> String {
        let mut dec = StreamDecoder::new(enc);
        let mut out = String::new();
        for b in bytes {
            dec.decode(std::slice::from_ref(b), &mut out);
        }
        out
    }

    #[test]
    fn gbk_encodes_ascii_and_chinese() {
        let enc = resolve("gbk");
        assert_eq!(
            encode_text(enc, "ls 你好").as_ref(),
            &[b'l', b's', b' ', 0xC4, 0xE3, 0xBA, 0xC3]
        );
    }

    #[test]
    fn gbk_emoji_becomes_question_mark() {
        let enc = resolve("gbk");
        assert_eq!(encode_text(enc, "a😀b").as_ref(), b"a?b");
        // 每个不可映射字符恰好一个 `?`，不是数字实体。
        assert_eq!(encode_text(enc, "😀😀").as_ref(), b"??");
    }

    #[test]
    fn gb18030_emoji_roundtrips() {
        let enc = resolve("gb18030");
        let text = "你好 😀 ls";
        let bytes = encode_text(enc, text);
        assert!(!bytes.contains(&b'?'));
        assert_eq!(decode_bytewise(enc, &bytes), text);
    }

    #[test]
    fn bytewise_roundtrip_for_all_listed_encodings() {
        let cases: &[(&str, &str)] = &[
            ("gb18030", "你好，世界 ls -l 😀"),
            ("gbk", "你好，世界 ls -l"),
            ("big5", "繁體中文測試 ls"),
            ("shift_jis", "こんにちは世界 ｱｲｳ ls"),
            ("euc-jp", "こんにちは世界 ls"),
            ("euc-kr", "안녕하세요 세계 ls"),
            ("windows-1252", "café déjà vu € ls"),
            ("windows-1251", "Привет, мир ls"),
            ("koi8-r", "Привет, мир ls"),
        ];
        for (label, text) in cases {
            let enc = resolve(label);
            let bytes = encode_text(enc, text);
            assert!(!bytes.contains(&b'?'), "{label} 不应出现不可映射字符");
            assert_eq!(&decode_bytewise(enc, &bytes), text, "{label}");
        }
    }

    #[test]
    fn utf8_stream_handles_split_characters() {
        let enc = TextEncoding::utf8();
        let text = "a你😀b";
        assert_eq!(decode_bytewise(enc, text.as_bytes()), text);
    }

    #[test]
    fn invalid_bytes_become_replacement_char() {
        let mut dec = StreamDecoder::new(TextEncoding::utf8());
        let mut out = String::new();
        dec.decode(&[b'a', 0xFF, b'b'], &mut out);
        assert_eq!(out, "a\u{FFFD}b");
    }

    #[test]
    fn bom_is_not_stripped() {
        let mut dec = StreamDecoder::new(TextEncoding::utf8());
        let mut out = String::new();
        dec.decode("\u{FEFF}x".as_bytes(), &mut out);
        assert_eq!(out, "\u{FEFF}x");
    }

    #[test]
    fn labels_fall_back_to_utf8() {
        for label in [
            "no-such-encoding",
            "utf-16",
            "utf-16le",
            "utf-16be",
            "hz-gb-2312",
            "iso-2022-cn",
            "replacement",
            "x-user-defined",
            "iso-2022-jp",
            "",
        ] {
            let enc = resolve(label);
            assert!(enc.is_utf8(), "{label}");
            assert_eq!(enc.name(), "UTF-8");
        }
    }

    #[test]
    fn label_aliases_and_normalization() {
        assert_eq!(resolve("iso-8859-1").name(), "windows-1252");
        assert!(resolve("iso-8859-1") == resolve("windows-1252"));
        assert_eq!(resolve("  GBK ").name(), "GBK");
        assert_eq!(resolve("Shift_JIS").name(), "Shift_JIS");
        assert!(resolve("UTF-8").is_utf8());
        assert!(!resolve("gbk").is_utf8());
    }

    #[test]
    fn reset_discards_partial_character() {
        let enc = resolve("gbk");
        let mut dec = StreamDecoder::new(enc);
        let mut out = String::new();
        // "你" = C4 E3，只喂入前半个。
        dec.decode(&[0xC4], &mut out);
        assert_eq!(out, "");
        dec.reset();
        dec.decode(b"ok", &mut out);
        assert_eq!(out, "ok");

        // 对照：不 reset 时残留字节会和后续字节拼成一个字符。
        let mut dec = StreamDecoder::new(enc);
        let mut out = String::new();
        dec.decode(&[0xC4], &mut out);
        dec.decode(b"o", &mut out);
        assert_ne!(out, "o");
    }

    #[test]
    fn long_input_is_not_truncated() {
        let enc = resolve("gbk");
        let text = "汉".repeat(100_000);
        let bytes = encode_text(enc, &text);
        assert_eq!(bytes.len(), 200_000);
        let mut dec = StreamDecoder::new(enc);
        let mut out = String::new();
        dec.decode(&bytes, &mut out);
        assert_eq!(out, text);
    }

    #[test]
    fn long_input_with_many_unmappable_chars() {
        // 全是不可映射字符时输出长度等于字符数，缓冲区边界处也不能丢字节。
        let enc = resolve("gbk");
        let text = "😀".repeat(50_000);
        let bytes = encode_text(enc, &text);
        assert_eq!(bytes.len(), 50_000);
        assert!(bytes.iter().all(|b| *b == b'?'));
    }

    #[test]
    fn utf8_encode_borrows() {
        let text = "ls 你好";
        let out = encode_text(TextEncoding::utf8(), text);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out.as_ref(), text.as_bytes());
    }

    #[test]
    fn decoder_reuses_output_buffer() {
        let enc = resolve("gbk");
        let mut dec = StreamDecoder::new(enc);
        let mut out = String::with_capacity(64);
        for _ in 0..10 {
            out.clear();
            dec.decode(b"hello", &mut out);
        }
        assert_eq!(out, "hello");
        assert!(out.capacity() >= 64);
    }
}
