# 终端帧快照 ABI

实现：`crates/kanso-frame`（布局、写入器、读取器、mock）；C 侧：`crates/kanso-ffi/include/kanso_frame.h`。
两处布局必须与本文一致，任何一处变动都要同步另外两处，并由编译期断言（Rust 的 `const _: () = assert!`、
C 的 `_Static_assert`）和 `cargo xtask ci` 的 `c-abi` 步骤兜底。

## 1. 设计目标

- **数据面零序列化**：core 把一帧写进一块连续缓冲区，Swift 通过指针直接读，帧内容不经过 UniFFI 编解码
  （取帧时只经 UniFFI 传一个 `FrameHandle` 对象，见 §8）。
- **Swift 只读**：缓冲区由 Rust 持有，Swift 不写、不释放内存，只通过 destroy 归还。
- **颜色不预解析**：单元格存“默认 / 调色板索引 / RGB / 语义类别”，不存最终像素色。切换深色/浅色主题只换调色板重画
  （architecture §5、§11）。
- **选区与搜索高亮由 core 直接标到单元格上**，渲染器不再计算区间。
- sans-IO：格式本身不涉及线程、锁和 IO。

## 2. 内存布局

一帧是一块连续缓冲区，起点 8 字节对齐（Rust 侧用 `Vec<u64>` 保证）。偏移均为字节、相对缓冲区起点，
每段起点 8 字节对齐。所有整数是本机字节序（只支持 aarch64，小端）。

| 段 | 起点 | 大小 |
|---|---|---|
| FrameHeader | 0 | 88 |
| LineMeta × rows | `line_meta_offset`（= header_size） | rows × 24 |
| Cell × (rows × cols)，行优先 | `cells_offset` | rows × cols × 16 |
| CellExtra × extras_count | `extras_offset` | extras_count × 16 |
| 字符串池（UTF-8） | `pool_offset` | `pool_len`，末尾补零到 8 字节 |

缓冲区总长 `total_len` = `pool_offset` + `pool_len` 向上取整到 8。

## 3. 结构

### FrameHeader（88 字节，对齐 8）

| 偏移 | 类型 | 字段 | 含义 |
|---|---|---|---|
| 0 | u32 | magic | `0x4D52464B`，小端读出为 ASCII "KFRM" |
| 4 | u16 | version | 布局版本，当前 1 |
| 6 | u16 | header_size | 写入方的帧头字节数 |
| 8 | u64 | seq | 帧序号，同一终端单调递增 |
| 16 | u16 | cols | 列数 |
| 18 | u16 | rows | 行数 |
| 20 | u32 | flags | `FRAME_*` |
| 24 | u64 | first_abs_line | 视口第 0 行的绝对行号（`AbsLine`） |
| 32 | u64 | line_number_base | 显示行号 = abs_line − line_number_base + 1 |
| 40 | u32 | display_offset | 向上滚动的行数，0 表示在底部 |
| 44 | u32 | scrollback_lines | 当前回滚总行数（滚动条用） |
| 48 | u16 | cursor_col | 光标列（视口内） |
| 50 | u16 | cursor_row | 光标行（视口内） |
| 52 | u8 | cursor_shape | 0 块、1 下划线、2 竖线、3 空心块（失焦） |
| 53 | u8[3] | _reserved0 | 保留，写 0 |
| 56 | u32 | line_meta_offset | |
| 60 | u32 | cells_offset | |
| 64 | u32 | extras_offset | |
| 68 | u32 | extras_count | |
| 72 | u32 | pool_offset | |
| 76 | u32 | pool_len | 有效字节数，不含对齐填充 |
| 80 | u32 | total_len | 整个缓冲区字节数 |
| 84 | u32 | _reserved1 | 保留，写 0 |

`FRAME_*`：`ALT_SCREEN` 1<<0（全屏程序期间：时间戳栏留空、不画分隔线、不做语义着色）、
`CURSOR_VISIBLE` 1<<1（光标不在视口内时清掉）、`CURSOR_BLINK` 1<<2、
`MOUSE_REPORTING` 1<<3（程序开启了鼠标上报，UI 据此决定点击交给程序还是按住 Shift 绕过）、`FOCUSED` 1<<4。

### LineMeta（24 字节，对齐 8）

| 偏移 | 类型 | 字段 | 含义 |
|---|---|---|---|
| 0 | u64 | abs_line | 该行绝对行号 |
| 8 | i64 | timestamp_ms | 首次写出时的 Unix 毫秒；0 表示无 |
| 16 | u32 | flags | `LINE_*` |
| 20 | u32 | _reserved | 保留 |

`LINE_*`：`WRAPPED` 1<<0（软换行到下一行）、`PROMPT` 1<<1（OSC 133 A 所在行，分隔线画在其上方，同一行只标一次）、
`CURSOR` 1<<2（光标所在行，时间戳栏高亮）、`BLANK` 1<<3（从未写过，不显示时间和行号）、
`DIRTY` 1<<4（自上一帧有变化）、`FIRST_PROMPT` 1<<5（会话第一个提示符，上方不画分隔线）。

### Cell（16 字节，对齐 4）

| 偏移 | 类型 | 字段 | 含义 |
|---|---|---|---|
| 0 | u32 | ch | Unicode 标量值；0 为空单元格（宽字符占位格也是 0） |
| 4 | u32 | fg | 前景色编码 |
| 8 | u32 | bg | 背景色编码 |
| 12 | u16 | flags | `CELL_*` |
| 14 | u16 | extra | 0 无；n 表示 `extras[n-1]` |

`CELL_*`：`BOLD` 1<<0、`DIM` 1<<1、`ITALIC` 1<<2、下划线样式 bits 3..=5
（`UNDERLINE_SHIFT`=3，`UNDERLINE_MASK`=0b111<<3；0 无、1 单线、2 双线、3 波浪、4 点线、5 虚线）、
`STRIKE` 1<<6、`INVERSE` 1<<7、`HIDDEN` 1<<8、`WIDE` 1<<9（宽字符前半）、`WIDE_SPACER` 1<<10（后半占位）、
`SELECTED` 1<<11、`MATCH` 1<<12、`MATCH_CURRENT` 1<<13（当前匹配项，同时带 `MATCH`）、`BLINK` 1<<14、bit 15 保留。

### CellExtra（16 字节，对齐 4）

| 偏移 | 类型 | 字段 | 含义 |
|---|---|---|---|
| 0 | u32 | grapheme_offset | 字形簇文本在字符串池内的偏移（相对池起点） |
| 4 | u32 | grapheme_len | 字节数；0 表示无，单元格只有 `ch` 一个码点 |
| 8 | u32 | hyperlink_id | OSC 8 链接 id，0 表示无；URL 由控制面按 id 查询，不放进帧里 |
| 12 | u32 | underline_color | 颜色编码；0 表示跟随前景 |

每帧最多 65535 个 CellExtra（`Cell::extra` 是 u16）、单元格总数不超过 2^24、字符串池不超过 16 MiB，
使所有偏移与 `total_len` 都落在 u32 内。

## 4. 颜色编码

`u32`，高 8 位标签，低 24 位负载。

| 标签 | 含义 | 负载 |
|---|---|---|
| 0 | 默认色（前景或背景由所在字段决定） | 忽略 |
| 1 | 调色板索引 | 低 8 位 |
| 2 | RGB | 低 24 位 0xRRGGBB |
| 3 | 语义着色类别 | 低 8 位类别号 |

语义着色只会出现在 `fg` 上，且只在程序没设前景色时由 core 填入；渲染器按当前主题映射成颜色。
读取方遇到未知标签应按默认色处理。

## 5. 版本兼容

- 每个读取方先读前 8 字节：`magic`（不符则拒绝）、`version`、`header_size`，这三个字段位置永远不变。
- **只在帧头末尾追加字段**：追加后 `header_size` 变大；旧读取方按自己已知的结构读，并用 `header_size`
  定位后续的段（段位置始终由偏移字段给出，不依赖 `sizeof`）。读取方要求 `header_size` 不小于自己已知的帧头。
- 修改、移动、删除已有字段，或改变任何段/结构的尺寸与语义，必须升 `version`；读取方遇到不认识的 `version` 直接拒绝。
- 升版本时同步：`kanso-frame` 的布局与断言、`kanso_frame.h`、本文。

## 6. 生命周期与线程

- 帧缓冲区由 Rust 创建并持有，销毁（`kanso_frame_destroy` 或 Rust 侧复用）之前一直有效，期间 Rust 不会修改它。
  因此只要销毁发生在所有读取结束之后，可以在任意线程读取；这一先后顺序由调用方保证。
- Swift 只读，不得写入，不得在 destroy 后保留指针。
- Rust 侧 `FrameBuilder::reuse` 复用旧帧缓冲区，调用前必须确认没有读者。
- 所有 FFI 函数 `catch_unwind`，panic 时返回 NULL，不穿过 FFI 边界。
- 当前只导出 mock 帧（`kanso_mock_frame_create` / `kanso_frame_header` / `kanso_frame_destroy`）。
  按会话取帧的 acquire / release 接口待 core API 定下后加入。

## 7. 与 terminal.md 的对应

| 规格 | 帧里的体现 |
|---|---|
| §2 每行时间与累计行号 | `LineMeta.timestamp_ms`、`abs_line`、`header.line_number_base` |
| §2 空白行不显示时间和行号 | `LINE_BLANK` |
| §2 光标所在行高亮 | `LINE_CURSOR` |
| §2 全屏程序期间栏留空 | `FRAME_ALT_SCREEN` |
| §2、§3 `clear` 后行号从 1 重新开始，AbsLine 不回退 | 移动 `line_number_base` |
| §4 提示符上方分隔线，同一行只画一条 | `LINE_PROMPT`（core 去重）；会话首个提示符带 `LINE_FIRST_PROMPT` 不画线；全屏期间不画 |
| §7 语义着色 | fg 标签 3；全屏程序期间不填 |
| §8 选区 | `CELL_SELECTED` |
| §8.1 鼠标上报与 Shift 绕过 | `FRAME_MOUSE_REPORTING` |
| §10 链接（OSC 8） | `CellExtra.hyperlink_id` |
| §11 搜索高亮 | `CELL_MATCH`、`CELL_MATCH_CURRENT` |

## 8. 经 UniFFI 取帧（`FrameHandle`）

按会话取帧的接口在 UniFFI 控制面上（`crates/kanso-ffi/src/frame_handle.rs`、`core_object.rs`），内容仍然零拷贝：

```text
KansoCore.takeFrame(sessionId, reuse: FrameHandle?) -> FrameHandle?
FrameHandle.headerAddress() -> UInt64   // 缓冲区起点（即帧头）地址；帧已被回收时为 0
FrameHandle.byteLen()       -> UInt64   // 缓冲区字节数；帧已被回收时为 0
```

为什么不用 §6 的手写 C 函数：Swift 拿不到 UniFFI 对象的裸指针，无法把 `KansoCore` 传给 C 函数。
每帧多一次 UniFFI 调用（只传一个对象引用，不拷贝内容），换来不需要任何全局句柄。

**这是全部 FFI 中唯一的 unsafe 约定，调用方必须遵守：**

1. Swift 强引用某个 `FrameHandle` 期间，`headerAddress()` 的地址一直有效，缓冲区内容不会被修改；
   Swift 读取帧的整个期间必须持有句柄（`FrameSnapshot(coreFrame:)` 已做到）。
2. 把句柄作为 `reuse` 传回 `takeFrame` **并且这次调用返回了新帧**之后，旧缓冲区归 core 回收复用，
   旧句柄的 `headerAddress()` / `byteLen()` 此后返回 0，**不得再读取旧地址**。
3. `takeFrame` 返回 `nil`（没有新帧）时 `reuse` 原封不动、仍然有效。这条约束是内存安全的前提：
   `TerminalView` 在没有新帧时会继续重绘旧帧（窗口缩放等）。
   这条约束由 core 的契约直接保证，FFI 层不需要任何补偿：Rust 侧 `Core::take_frame` 的签名是
   `take_frame(&self, session, reuse: &mut Option<Frame>) -> Option<Frame>`，**有新帧时才从 `reuse` 取走缓冲区
   复用，没有新帧时 `reuse` 原样保留**；FFI 层只是把句柄里的缓冲区槽（`Mutex<Option<Frame>>`）借给 core。
   因此旧缓冲区在拿到新帧的**同一次调用**里就被复用（没有"备用槽、延迟一帧"），稳态下帧缓冲区本身不再分配。
4. Swift 只读，不得写入；地址 8 字节对齐，布局与校验规则同 §2、§5。
5. 句柄可以从任意线程读取，但"回收发生在所有读取结束之后"由调用方保证（目前只有主线程读取）。

core 返回的帧头校验失败属于 ABI 违约，Swift 侧直接终止进程（此时旧帧可能已被回收，继续运行只会悬垂读）。

