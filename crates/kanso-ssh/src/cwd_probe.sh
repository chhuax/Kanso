# Kanso 远端工作目录探测（ssh.md §10）。
#
# 由 exec 通道里的 `sh` 从 stdin 读取并执行（不经用户的登录 shell 解析，csh / fish 也不受影响）。
# 只用 POSIX sh 与 busybox 也有的命令（tr、grep、readlink）。
#
# 思路：sshd 给同一个连接的所有进程设置相同的 SSH_CONNECTION。在 /proc 里找出带这个值（完全相同）、
# 且有控制终端的进程——那是 shell 通道（exec 通道没有终端，本脚本自己也被排除）；
# 再读这个终端前台进程组 leader 的 cwd，嵌套 shell 或 shell 里启动的程序也能查对。
#
# 输出恰好一行，带固定前缀（登录 rc 文件可能打印杂讯，调用方只认带前缀的行）：
#   __KANSO_CWD__=<路径>        成功
#   __KANSO_CWD__!<失败码>      noproc | nosshconn | noshell | otheruser | unavailable
#
# KANSO_PROC_ROOT 仅供测试把 /proc 换成伪造的目录树；生产环境不会设置它。
#
# 全部包进函数再调用：sh 从 stdin 读脚本时按行解析执行，函数定义保证整段解析完才开始运行，
# 也避免任何命令误读 stdin 吃掉后面的脚本（下面每个命令都有显式的重定向或管道输入）。

kanso_emit() {
    printf '__KANSO_CWD__%s\n' "$1"
}

kanso_main() {
    proc_root=${KANSO_PROC_ROOT:-/proc}
    if [ ! -d "$proc_root" ]; then
        kanso_emit '!noproc'
        return 0
    fi
    if [ -z "$SSH_CONNECTION" ]; then
        kanso_emit '!nosshconn'
        return 0
    fi
    for d in "$proc_root"/[0-9]*; do
        line=''
        # 进程可能在扫描途中退出，读不到就跳过。
        { read -r line < "$d/stat"; } 2>/dev/null || continue
        # stat 形如 `pid (comm) S ppid pgrp session tty_nr tpgid ...`；comm 可能含空格和括号，
        # 所以从最后一个 `) ` 之后开始切字段。
        rest=${line##*) }
        [ "$rest" != "$line" ] || continue
        # shellcheck disable=SC2086
        set -- $rest
        # 此时 $1=state $2=ppid $3=pgrp $4=session $5=tty_nr $6=tpgid
        [ "${5:-0}" != 0 ] || continue
        case "$6" in
            '' | *[!0-9]*) continue ;;
        esac
        [ "$6" -gt 0 ] || continue
        # environ 以 NUL 分隔；按行精确比较（-F 固定串、-x 整行），值里的空格与点号不做正则解释。
        # 别的用户的进程读不到 environ，同样跳过。
        { tr '\0' '\n' < "$d/environ"; } 2>/dev/null |
            grep -Fqx "SSH_CONNECTION=$SSH_CONNECTION" || continue
        # 找到 shell 通道的进程。同一连接有多个 shell 通道（复制标签）时无法区分，取第一个。
        tpgid=$6
        cwd=$(readlink "$proc_root/$tpgid/cwd" 2>/dev/null)
        if [ -n "$cwd" ]; then
            kanso_emit "=$cwd"
        elif [ -d "$proc_root/$tpgid" ]; then
            # 前台进程还在但 cwd 读不到：属于别的用户（sudo、su）。
            kanso_emit '!otheruser'
        else
            kanso_emit '!unavailable'
        fi
        return 0
    done
    kanso_emit '!noshell'
}

kanso_main
exit 0
