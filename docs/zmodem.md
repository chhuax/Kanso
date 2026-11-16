# 终端内 ZMODEM

在本地 shell 或 SSH 终端运行 `rz` 后，可多选文件发送；运行 `sz 文件…` 后，逐个选择保存位置。
取消某个保存面板会跳过该文件。通过“会话 → 文件传输 → 取消传输”取消整批传输。
传输期间终端输入暂停，完成后恢复。状态栏显示文件提示、进度、跳过、错误及接收汇总。
ZMODEM 不支持续传。

协议依据 Chuck Forsberg / Omen Technology 的《The ZMODEM Inter Application File Transfer Protocol》，
1988-10-14 版，第 7、8、11 章；公开规范副本：
https://techheap.packetizer.com/communications/modems/zmodem8.html
`kanso-xfer` 独立实现 CRC16 / CRC32 接收、CRC16 发送和 ZDLE 转义，仅输出动作，不访问文件或网络。
会话层按 1024 字节读取发送文件；接收按协议子包写入，每个子包最多 8192 字节。

验证：`cargo test -p kanso-xfer`、`cargo test -p kanso-core --test real_core zmodem`。
外部互通测试只调用 `sz` / `rz`（或 Homebrew 的 `lsz` / `lrz`）可执行文件，不读取其源码。
可通过 `KANSO_LRZSZ_BIN` 指定测试程序目录。缺少程序时打印跳过原因。
所有测试文件和 HOME 使用临时目录，未引入新的运行时依赖。

## XMODEM

先在终端启动对方的 `rx 文件`、`sx 文件` 或 bootloader 的 `loadx`，再从“会话 → 文件传输”
选择“用 XMODEM 发送…”、“用 XMODEM-1K 发送…”或“用 XMODEM 接收…”。发送只选一个文件，接收自行选择保存位置。
对话框期间暂存原始字节；取消选择会把这些字节还给终端。传输与 ZMODEM 共用进度显示和“取消传输”，
期间暂停键盘、粘贴、Sender 与补全输入。

协议依据为 Ward Christensen 于 1977 年提出的原始 XMODEM 协议及其 1982-01-01
《MODEM PROTOCOL OVERVIEW》公开说明，以及 Chuck Forsberg 编订的
《XMODEM/YMODEM Protocol Reference》（1988-06-18）第 7、8 章及 §4.1–4.3
中的 XMODEM、XMODEM/CRC、XMODEM-1K 说明；仅使用文字协议，不参考实现源码：
- https://www.menie.org/georges/embedded/xmodem_specification.html
- https://www.classic-computing.de/hellwie/pdf/XMODEM_YMODEM%20Protocol%20Reference.pdf

支持 128 字节累加和、CRC16 和 1 KiB CRC16 块，收到 NAK 开局请求时退回 128 字节累加和；
1K 发送的不足整块尾部改用 128 字节块。接收先发三次 `C`，无回应后改发 NAK，
实际校验方式由第一块决定。每块最多等待 10 秒，块内字节等待 2 秒；第一块只有一个校验字节时
等待这段间隔以区分累加和与尚未完整到达的 CRC。连续 10 次错误或超时后通知对方并结束。
连续两个 CAN 视为对方取消；主动取消发送八个 CAN。EOT 收到 ACK 后发送方才完成。

**XMODEM 不携带文件大小：收到的文件末尾保留 `^Z`（0x1A）填充，不能自动删除，
因为真实文件内容也可能以相同字节结尾。**

验证包括内存两端对传、分片、回退、重传与取消，以及 `tests/lrzsz.rs` 中调用实际 `sx` / `rx`
（或 `lsx` / `lrx`）的双向测试；未安装程序时打印跳过原因。
