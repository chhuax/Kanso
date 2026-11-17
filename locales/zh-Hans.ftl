ui-shell-command = 命令行
command-duration-milliseconds = { $value }ms
command-duration-seconds = { $value }s
command-duration-minutes = { $minutes }m { $seconds }s
command-duration-hours = { $hours }h { $minutes }m
## 说明：本文件为 core 消息的简体中文目录。key、参数与取值分支必须与 kanso-types 的
## CATALOG / AUX_KEYS 一致，由 `cargo xtask check-locales` 校验，规则见 docs/i18n.md。
## 参数 $endpoint 与 $reason 由平台先用 endpoint-* / reason-* 子 key 渲染成字符串再代入。

## 子 key：失败原因

git-failed = Git：{ $detail }
git-summary-expired = 仓库摘要已过期，请刷新
git-commit-message-invalid = 提交说明不能为空且不能超过 16 KiB
git-entry-unavailable = 此条目已不可用
git-diff-unavailable = 二进制文件、非普通文件或内容超过 512 KiB，无法提供文本差异
git-diff-empty-file = 新增空文件，没有文本差异
git-diff-binary = 二进制文件，无法提供文本差异
git-diff-no-content-change = 相对当前提交没有净内容变化
git-diff-metadata-only = 仅元数据变化或无法提供文本差异
reason-not-found = 不存在
reason-permission-denied = 权限不足
reason-already-exists = 已存在
reason-timed-out = 超时
reason-connection-refused = 连接被拒绝
reason-connection-reset = 连接被重置
reason-host-unreachable = 主机不可达
reason-name-resolution = 域名解析失败
reason-other = { $detail }

## 子 key：连接对象

endpoint-direct = { $host_port }
endpoint-via = { $host_port }（经由 { $via }）

## Profile

profile-host-empty = 请填写主机。
profile-user-empty = 请填写用户名。
profile-locale-invalid = “{ $value }”不是有效的 locale。只能包含字母、数字和 _ - . @，长度不超过 64。

## SSH

ssh-connect-failed = 无法连接到 { $endpoint }：{ $reason }。
ssh-negotiation-failed = 与 { $endpoint } 没有共同的{ $category ->
        [kex] 密钥交换算法
        [host-key] 主机密钥算法
        [cipher] 加密算法
        [mac] MAC 算法
        [compression] 压缩算法
       *[other] 算法
    }。服务器提供的有：{ $server_offers }。
ssh-keepalive-timeout = { $endpoint } 没有响应，连接已关闭。
ssh-connection-lost = 与 { $endpoint } 的连接已中断。
ssh-shared-connection-closed = 与 { $endpoint } 的连接已关闭，无法复制此标签。请重新打开会话。
ssh-key-file-unreadable = 无法读取私钥文件 { $path }：{ $reason }。
ssh-key-decrypt-failed = 无法解密私钥 { $path }，请检查私钥口令。
ssh-proxy-command-failed = 代理命令执行失败（{ $command }）：{ $reason }。

## 主机密钥

hostkey-changed = { $endpoint } 的主机密钥已变化，新的 { $key_type } 密钥指纹是 { $fingerprint }。连接已被拒绝。只有确认这台主机确实被更换过，才应接受新密钥。
hostkey-store-write-failed = 无法把主机密钥保存到 { $path }：{ $reason }。

## 认证

auth-partial-success = { $endpoint } 已接受凭据，但用户 { $user } 还需要另一个验证因素，服务器只提供了：{ $methods }。
auth-failed = { $method ->
        [password] 用户 { $user } 在 { $endpoint } 上的密码认证失败。
        [private-key] 用户 { $user } 在 { $endpoint } 上的私钥认证失败。
        [agent] 用户 { $user } 在 { $endpoint } 上认证失败：agent 里没有被接受的身份。
       *[other] 用户 { $user } 在 { $endpoint } 上认证失败。
    }
auth-key-rejected = { $endpoint } 拒绝了用户 { $user } 的私钥 { $path }。
auth-agent-no-identity = ssh-agent 里没有可用于 { $endpoint }（用户 { $user }）的身份。
auth-agent-unavailable = 无法连接 ssh-agent：{ $details }。
auth-cancelled = 已取消对 { $endpoint } 的认证。
auth-too-many-rounds = { $endpoint } 要求的验证超过 { $max_rounds } 轮，连接已关闭。
auth-response-rejected = { $endpoint } 拒绝了验证响应。

## 跳板机

jump-cycle = 跳板机链出现循环，回到了“{ $profile }”。
jump-missing = 跳板机“{ $profile }”已不存在。
jump-not-ssh = 跳板机“{ $profile }”不是 SSH 会话。
jump-too-long = 跳板机链过长，最多 { $max_hops } 跳。
jump-unreachable = 跳板机 { $jump } 无法到达 { $target }：{ $reason }。
jump-proxy-command-conflict = 跳板机与代理命令不能同时使用，请二选一。

## 会话

session-exited = 会话已结束。
session-exited-with-code = 会话已结束（退出码 { $code }）。
session-connection-closed = 与 { $endpoint } 的连接已关闭。
session-spawn-failed = 无法启动 { $command }：{ $reason }。
session-not-terminal = 这是纯 SFTP 会话，没有终端。

## 当前目录

cwd-no-shell = 没有找到这个连接的 shell。会上报 OSC 7 的 shell 在任何系统上都能查到。
cwd-no-ssh-connection = 服务器没有设置 SSH_CONNECTION。会上报 OSC 7 的 shell 在任何系统上都能查到。
cwd-other-user = 前台进程属于其他用户（例如 sudo 或 su）。会上报 OSC 7 的 shell 在任何系统上都能查到。
cwd-no-proc = 服务器没有 /proc（只支持 Linux）。会上报 OSC 7 的 shell 在任何系统上都能查到。
cwd-timeout = 查询当前目录超时。会上报 OSC 7 的 shell 在任何系统上都能查到。
cwd-unavailable = 无法获取当前目录。会上报 OSC 7 的 shell 在任何系统上都能查到。

## SFTP

sftp-subsystem-failed = 无法在 { $endpoint } 上启动 SFTP：{ $reason }。服务器可能没有启用 SFTP。
sftp-op-failed = { $op ->
        [list] 无法列出 { $path } 的内容：{ $reason }。
        [stat] 无法读取 { $path } 的属性：{ $reason }。
        [mkdir] 无法创建文件夹 { $path }：{ $reason }。
        [remove] 无法删除 { $path }：{ $reason }。
        [rename] 无法重命名 { $path }：{ $reason }。
        [create-file] 无法创建文件 { $path }：{ $reason }。
        [open] 无法打开 { $path }：{ $reason }。
       *[other] 无法完成对 { $path } 的操作：{ $reason }。
    }
sftp-file-exists = { $path } 已存在。

## 文件传输

xfer-completed = 已传输 { $name }（{ $size }）。
xfer-cancelled = 传输已取消。
xfer-unsafe-name = 已拒绝文件名“{ $name }”，写到本地不安全。
xfer-size-overflow = 实际接收的大小与声明不符，或超出上限。
xfer-remote-not-dir = { $path } 不是文件夹。
xfer-local-write-failed = 无法写入 { $path }：{ $reason }。
xfer-local-read-failed = 无法读取 { $path }：{ $reason }。
xfer-session-ended = 传输过程中会话已结束。
xfer-stream-write-failed = 无法写入终端连接：{ $reason }
xfer-protocol-error = { $protocol ->
        [sftp] SFTP 协议错误：{ $detail }
        [zmodem] ZMODEM 协议错误：{ $detail }
        [xmodem] XMODEM 协议错误：{ $detail }
       *[other] 协议错误：{ $detail }
    }

## ZMODEM

zmodem-waiting-sender = 等待对方开始发送…
zmodem-file-offered = 对方要发送文件：{ $name }
zmodem-file-offered-with-size = 对方要发送文件：{ $name }（{ $size }）
zmodem-file-skipped = 已跳过 { $name }。
zmodem-receive-summary = 共接收 { $count } 个文件。
zmodem-select-files = 请选择要发送的文件。
zmodem-send-cancelled = 已取消发送。
zmodem-peer-skipped = 对方跳过了 { $name }。
zmodem-resume-unsupported = 对方请求断点续传，但不支持续传，已中止传输。
zmodem-waiting-confirm = 等待对方确认…

## XMODEM

xmodem-timeout = 对方没有及时响应。
xmodem-peer-cancelled = 对方取消了传输。

## 导入

import-read-failed = 无法读取 { $path }：{ $reason }。
export-write-failed = 无法保存 { $path }：{ $reason }。
snippet-limit-reached = 最多只能保存 { $limit } 条命令，请先删除一些再添加。
sender-failed = Sender：{ $detail }
sender-empty-command = 命令不能为空白
sender-busy = 另一批命令正在发送
sender-no-sessions = 没有可发送会话
sender-repeat-replaced = 旧重复任务已停止
sender-stopped = 已发送 { $sent } / { $total } 行
sender-target-closed = 重复任务已停止：目标标签已关闭
sender-summary = 成功 { $success } / 共 { $total } 个对象{ $skipped ->
    [0] { "" }
   *[other] ，跳过 { $skipped } 个对象
    }
sender-repeat-completed = 重复任务完成（{ $rounds } 轮）
sender-terminal-closed = 终端已关闭
sender-terminal-unavailable = 终端已断线或锁定
sender-terminal-transferring = 终端正在传输文件
sender-write-failed = 终端写入失败
import-snippet-converted = 已转换“{ $name }”：纯 SFTP 类型作用域改为全局。
import-wrong-extension = 文件扩展名不正确。
import-not-kanso-file = 这不是 Kanso 导出文件。
import-version-too-new = 此文件由更新的版本导出（格式 { $version }），当前版本最高支持格式 { $supported }。
import-skipped-unsupported-kind = 已跳过“{ $name }”：不支持“{ $kind }”类型。
import-skipped-invalid = 已跳过“{ $name }”：{ $detail }
import-report = 新增 { $added } 个，更新 { $updated } 个，跳过 { $skipped } 个。
import-converted-suffix = ；已转换 { $count } 条
import-converted-and-skipped = 转换与跳过原因
sshconfig-multi-hop-ignored = “{ $alias }”使用了多跳 ProxyJump，已导入会话，但没有设置跳板机。
sshconfig-include-too-deep = “{ $file }”里的 Include 嵌套太深，没有读取“{ $include }”。
sshconfig-match-unsupported = “{ $file }”里有 Match 块。Match 块没有被求值，块里的设置不会生效。
sshconfig-invalid-port = “{ $alias }”的 Port“{ $value }”无效，已按 22 处理。
sshconfig-unknown-token = “{ $alias }”的 { $option } 里有不支持的替换符 %{ $token }，已原样保留。
sshconfig-import-report = 新增 { $added } 个，更新 { $updated } 个，额外导入 { $jumps_added } 个跳板机，忽略 { $multi_hop_ignored } 个多跳 ProxyJump。

## 凭据存储

secret-store-failed = 无法访问凭据存储：{ $detail }
ui-local-shell = Shell
ui-shell-default = 系统默认
ui-shell-custom = 自定义命令
ui-filter-path = 按路径筛选
ui-no-matching-paths = 没有匹配的路径

ui-session = 会话

ui-ssh-connection = SSH 连接

ui-local-session = 本地 Shell

ui-new-session = 新建会话

ui-edit-session = 编辑会话

ui-type = 类型

ui-name = 名称

ui-color = 颜色

ui-group = 分组

ui-start-directory = 起始目录

ui-host = 主机

ui-port = 端口

ui-user = 用户名

ui-authentication = 认证方式

ui-password = 密码

ui-private-key = 私钥

ui-key-passphrase = 私钥口令

ui-jump-host = 跳板机

ui-encoding = 编码

ui-locale = Locale

ui-proxy-command = ProxyCommand

ui-choose = 选择…

ui-cancel = 取消

ui-save = 保存

ui-connect = 连接

ui-automatic = 自动

ui-keep-saved = 留空则保持已保存的值

ui-search-sessions = 搜索已保存的会话

ui-saved-sessions = 已保存的会话

ui-no-sessions = 还没有保存的会话。选择“新建会话…”添加一个。

ui-no-matching-sessions = 没有匹配的会话

ui-empty-group = （空）

ui-general = 通用

ui-display = 显示

ui-appearance = 外观

ui-theme = 模式

ui-light-theme = 浅色终端配色

ui-dark-theme = 深色终端配色

ui-font = 字体

ui-terminal-font = 终端字体

ui-font-size = 字号

ui-line-height = 行距

ui-font-custom = 自定义字体

ui-font-hint = 留空使用系统等宽字体。字体缺少的图标会自动使用已安装的 Nerd Font。

ui-font-probing = 正在检测本机等宽字体…

ui-cursor = 光标

ui-shape = 形状

ui-cursor-blink = 光标闪烁

ui-semantic-color = 语义着色

ui-scrollback = 回滚

ui-scrollback-lines = 回滚行数

ui-scrollback-hint = 调小以后立即丢弃超出的最早的行。

ui-contrast = 对比度

ui-contrast-enabled = 最低对比度保护

ui-contrast-ratio = 对比度比例

ui-contrast-hint = 文字与背景的对比度低于此比例时，会自动调亮或调暗文字。

ui-timeline = 时间轴

ui-interface-font = 界面字体

ui-settings = 设置

ui-sender-title = Sender
ui-sender-run = 运行
ui-sender-stop = 停止
ui-sender-clear = 清空
ui-sender-repeat = 重复发送
ui-sender-save = 保存
ui-sender-update = 更新
ui-sender-cancel-edit = 取消编辑
ui-sender-name-placeholder = 名称（可选）
ui-sender-input-placeholder = 输入命令，Enter 发送，Shift+Enter 换行
ui-sender-interval = 间隔（秒）
ui-sender-count = 次数（0 为不限）
ui-sender-library-empty = 尚无已保存的命令
ui-sender-library-none = 没有适用于当前会话的命令
ui-sender-prev-page = 上一页
ui-sender-next-page = 下一页
ui-sender-ending-none = 不追加行尾
ui-sender-target-current = 当前会话（{ $count }）
ui-sender-target-all = 全部会话（{ $count }）
ui-sender-target-all-label = 全部会话
ui-sender-target-current-label = 当前会话 · { $name }
ui-sender-scope-global = 全局
ui-sender-scope-ssh = 所有 SSH 会话
ui-sender-scope-local = 所有本地 Shell 会话
ui-sender-scope-group = 分组 · { $name }
ui-sender-scope-profile = 会话 · { $name }
ui-sender-menu-edit = 编辑
ui-sender-menu-repeat = 重复发送
ui-sender-menu-ending = 行尾
ui-sender-menu-scope = 作用域
ui-sender-menu-delete = 删除
ui-sender-rounds = 已发 { $rounds } 轮
ui-sender-rounds-of = 已发 { $rounds } / { $count } 轮
ui-sender-sending = { $progress } · 正在发送
ui-sender-next-in = { $progress } · { $wait } 秒后发送
ui-sender-repeat-bar = 重复发送：{ $title } · 每 { $interval } 秒 · { $dest } · { $progress }
ui-sender-chip = 重复发送 · { $rounds } 轮
ui-sender-chip-of = 重复发送 · { $rounds }/{ $count }
ui-sender-join = ；
ui-sender-save-failed = 保存命令失败：{ $detail }
ui-sender-update-failed = 更新命令失败：{ $detail }
ui-sender-delete-failed = 删除命令失败：{ $detail }
ui-sender-send-failed = 发送失败：{ $detail }
ui-sender-limit = 命令库已达 { $max } 条上限，不能再新增（仍可更新或删除）
ui-completion-directory = 目录
ui-completion-file = 文件
ui-completion-history = 历史
ui-completion-kubernetes = Kubernetes
ui-completion-docker = Docker
ui-completion-git = Git
ui-completion-systemd = systemd
ui-menu-command-suggestions = 命令建议
ui-menu-clear-history = 清除命令历史…
ui-menu-sender = Sender
ui-history-clear-title = 清除命令历史？
ui-history-clear-body = 将清除所有终端记录的命令历史，此操作无法撤销。
ui-history-clear-confirm = 清除
ui-history-clear-cancel = 取消
ui-history-clear-failed = 清除命令历史失败：{ $detail }
