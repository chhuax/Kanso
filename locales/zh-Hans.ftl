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

## Filer 与文件传输（GPUI，自动生成区块，改动请编辑生成脚本的清单）
ui-xfer-default-name = received.bin
ui-xfer-hint-pick-files = 对方请求接收文件，请选择要发送的文件（可多选）
ui-xfer-hint-pick-file = 请选择要用 XMODEM 发送的文件
ui-xfer-hint-save-sized = 对方要发送 { $name }（{ $size }），请选择保存位置
ui-xfer-hint-save = 对方要发送 { $name }，请选择保存位置
ui-xfer-hint-xmodem-save = 请选择 XMODEM 接收文件的保存位置
ui-xfer-done = 传输完成
ui-xfer-cancelled = 已取消
ui-xfer-failed = 失败：{ $reason }
ui-xfer-send = 发送
ui-xfer-receive = 接收
ui-filer-delete-link = 删除链接
ui-filer-delete-link-note = 只移除链接，不影响目标，无法撤销。
ui-filer-delete-dir = 删除目录
ui-filer-delete-dir-note = 只能删除空目录，无法撤销。
ui-filer-delete-file = 删除文件
ui-filer-delete-file-note = 文件将被移除，无法撤销。
ui-filer-delete-where = 位置：{ $location }
ui-filer-delete-many = 删除 { $count } 个项目
ui-filer-delete-more = 还有 { $count } 项
ui-filer-delete-many-note = 这些项目将被永久移除，无法撤销；非空目录会保留。
ui-filer-cancel = 取消
ui-filer-delete = 删除
ui-filer-new-file = 新建文件
ui-filer-new-folder = 新建目录
ui-filer-rename = 重命名
ui-filer-name-where = 位置：{ $location }
ui-filer-name = 名称
ui-filer-ok = 确定
ui-filer-name-empty = 名称不能为空
ui-filer-name-reserved = 名称不能是 . 或 ..
ui-filer-name-separator = 名称不能包含目录分隔符，只能是当前目录里的一个项目
ui-filer-name-invalid-char = 名称不能包含字符 { $ch }
ui-filer-conflict-title = 同名项目已存在
ui-filer-conflict-file = 目标位置已有名为“{ $name }”的文件。
ui-filer-conflict-dir = 目标位置已有名为“{ $name }”的目录。
ui-filer-conflict-merge-note = 目录会合并，其中的同名文件仍按你的选择处理。
ui-filer-skip = 跳过
ui-filer-skip-all = 全部跳过
ui-filer-overwrite = 覆盖
ui-filer-overwrite-all = 全部覆盖
ui-filer-err-not-found = 不存在
ui-filer-err-denied = 没有权限
ui-filer-err-not-dir = 不是目录
ui-filer-err-exists = 已经存在
ui-filer-err-not-empty = 目录不是空的，只能删除空目录
ui-filer-err-cancelled = 已取消
ui-filer-drive = 本地磁盘 ({ $letter }:)
ui-filer-err-copy-into-self = 不能把目录复制到它自己或它的子目录里
ui-filer-err-no-name = 无法取得名称
ui-filer-err-target-link = 目标位置是符号链接，已拒绝写入
ui-filer-err-target-dir = 目标位置已有同名目录
ui-filer-this-pc = 此电脑
ui-filer-connecting = 正在连接…
ui-filer-loading = 正在读取… 已收到 { $count } 项
ui-filer-disconnected-stats = 连接已断开（列表已过期，共 { $count } 项）
ui-filer-empty = { $count } 项
ui-filer-stats = { $total } 项：{ $dirs } 个目录，{ $files } 个文件
ui-filer-stats-links = { $total } 项：{ $dirs } 个目录，{ $files } 个文件，{ $links } 个链接
ui-filer-kind-link = 链接
ui-filer-kind-dir = 目录
ui-filer-kind-file = 文件
ui-filer-tip-kind = 类型：{ $kind }
ui-filer-tip-size = 大小：{ $size }
ui-filer-tip-perm = 权限：{ $perm }
ui-filer-tip-owner = 属主：{ $owner }
ui-filer-tip-group = 属组：{ $group }
ui-filer-op-upload = 上传
ui-filer-op-download = 下载
ui-filer-op-copy = 复制
ui-filer-busy = 正在处理，请等待当前传输完成
ui-filer-batch-done = 完成：{ $count } 项
ui-filer-cancelled = 已取消
ui-filer-op-failed = { $op }失败：{ $reason }
ui-filer-op-error = { $op } { $name }：{ $reason }
ui-filer-awaiting-conflict = 等待处理同名文件…
ui-filer-skipped = 跳过 { $count } 个同名项
ui-filer-local = 本地
ui-filer-identity = { $name } · { $host }
ui-filer-not-remote = 这一栏不是远端，无法上传下载
ui-filer-no-write-this-pc = “此电脑”不是真实目录，不能在这里新建、删除或接收文件
ui-filer-disconnected-write = 连接已断开，已停止写入操作
ui-filer-no-dir-open = 没有打开的目录，或目录读取失败
ui-filer-already-here = 文件已经在此目录，已跳过
ui-filer-link-broken = 无法打开链接“{ $name }”：{ $reason }
ui-filer-remote-edit-unsupported = 暂不支持用本地应用编辑远端文件；可在右键菜单里选择“下载”
ui-filer-menu-open = 打开
ui-filer-menu-open-with = 打开方式…
ui-filer-menu-download = 下载
ui-filer-menu-copy-path = 复制完整路径
ui-filer-menu-copy-name = 复制名称
ui-filer-menu-copy-dir = 复制当前目录路径
ui-filer-upload-files = 上传文件…
ui-filer-upload-dirs = 上传目录…
ui-filer-refresh = 刷新
ui-filer-op-error-at = { $what }“{ $name }”失败（{ $where }）：{ $reason }
ui-filer-tip-upload = 上传
ui-filer-tip-upload-remote-only = 只适用于已连接的 SSH 会话
ui-filer-tip-hidden = 显示 / 隐藏以点开头的项目
ui-filer-tip-locate = 定位终端目录
ui-filer-tip-up = 上一级
ui-filer-tip-home = 家目录
ui-filer-tip-cancel = 取消传输
ui-filer-col-name = 名称
ui-filer-col-date = 修改日期
ui-filer-col-size = 大小
ui-filer-col-time = 修改时间
ui-filer-folder-empty = 这个目录是空的
ui-filer-open-failed = 无法打开 { $target }：{ $reason }
ui-filer-still-showing = 下面仍是 { $path } 的内容
ui-filer-go-home = 回到家目录
ui-filer-disconnected-badge = 连接已断开
ui-filer-drop-upload = 放下以上传
ui-filer-drop-copy = 放下以复制到这里
ui-filer-dismiss = 关闭
ui-filer-send-to-remote = 上传到远端
ui-filer-get-from-remote = 下载到本地
ui-filer-no-session = 没有可浏览的会话
ui-filer-menu-locate = 在 Filer 中显示工作目录
ui-filer-menu-view = 文件浏览器
ui-filer-status-label = 文件
ui-filer-status-already-open = 文件浏览已经在当前标签打开
ui-filer-located = 已定位到 { $path }
ui-filer-locate-no-session = 没有可定位的会话
ui-filer-locate-sftp = 文件会话没有 shell 工作目录
ui-filer-locate-not-connected = 会话未连接
ui-filer-locate-foreign-host = 终端上报的目录属于另一台主机（{ $host }），不能当作本机路径
ui-filer-locate-failed = 无法取得“{ $name }”的工作目录：{ $reason }
ui-xfer-menu = 文件传输
ui-xfer-menu-send = 用 XMODEM 发送…
ui-xfer-menu-send-1k = 用 XMODEM-1K 发送…
ui-xfer-menu-receive = 用 XMODEM 接收…
ui-xfer-menu-cancel = 取消传输
ui-xfer-start-failed = 无法开始传输：{ $reason }
