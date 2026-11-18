#!/bin/sh
# 为自检新建 X11 显示、D-Bus、HOME 和 XDG 目录，不接触桌面用户的配置或剪贴板。
set -eu
binary=$(realpath "${1:-target/debug/kanso-gpui}")
output=$(realpath -m "${2:-target/linux-smoke}")
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
mkdir -p "$output"
for mode in selftest settings-test; do
    base="$scratch/$mode"
    mkdir -p "$base/home" "$base/config" "$base/data" "$base/cache" "$base/runtime"
    chmod 700 "$base/runtime"
    xvfb-run -a -s '-screen 0 1280x800x24' sh -c '
        exec env -i PATH="$PATH" DISPLAY="$DISPLAY" XAUTHORITY="$XAUTHORITY" \
            HOME="$1/home" XDG_CONFIG_HOME="$1/config" XDG_DATA_HOME="$1/data" \
            XDG_CACHE_HOME="$1/cache" XDG_RUNTIME_DIR="$1/runtime" \
            LANG=C.UTF-8 SHELL=/bin/bash \
            dbus-run-session -- sh -c '\''
                openbox --sm-disable >/dev/null 2>&1 &
                wm=$!
                trap "kill $wm 2>/dev/null || true" EXIT
                "$1" "--$2" --timeout 60 --log "$3"
            '\'' sh "$2" "$3" "$4"
    ' sh "$base" "$binary" "$mode" "$output/$mode.log" >"$output/$mode.stderr" 2>&1
done
grep 'SELFTEST PASS' "$output/selftest.log"
grep '\[test\] 设置窗口已打开=true' "$output/settings-test.log"
grep '\[test\] 已写入核心 display=' "$output/settings-test.log"
