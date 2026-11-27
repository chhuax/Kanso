# Kanso 的 zsh 集成 shim：.zshrc
#
# 本文件由 Kanso 自动生成，每次启动应用时按内容比对后才会重写，请勿手工修改。
# 它的核心是加载用户自己的 .zshrc（用户设置过 ZDOTDIR 就用它，否则用 HOME），
# 保证任何一个启动文件都不会因为 ZDOTDIR 被指向这里而被跳过。
# 除此之外它只做两件事：把 HISTFILE 改回用户原来的位置，以及注册静默的 OSC 133 / OSC 7 钩子
# （保留用户原有的 PS1 样式不动，不覆盖用户的主题或提示符文字）。
# 删除整个 shim 目录即可彻底移除，不会在用户的任何文件里留下东西。

__kanso_shim_dir=$ZDOTDIR

# ---------------------------------------------------------------------------
# 提示符集成（KANSO_SHELL_INTEGRATION 为 0 时整段不定义，.zshrc shim 只做「加载用户文件」）。
# 这些函数在加载用户 .zshrc 之前定义：避免用户的 alias 在函数体被解析时展开、改变我们用到的内建命令；
# 注册 hook 则放在用户文件加载完之后（见文件末尾），这样我们的 precmd 排在用户 hook 之后。
# 序列格式见 docs/shell-integration.md；所有变量与函数都以 __kanso_ 开头，避免污染用户的命名空间。
# ---------------------------------------------------------------------------
if [[ ${KANSO_SHELL_INTEGRATION:-1} != 0 ]]; then

  # __kanso_first：还没有输出过第一个提示符；__kanso_cmd_ran：preexec 之后还没发过 D。
  # 用 preexec 置位而不是看 $?：空回车不会触发 preexec，也就不会发出没有对应命令的 D。
  typeset -g __kanso_first=1 __kanso_cmd_ran=0
  # 提示符末尾的 OSC 133 B。放进 %{ %} 告诉 zsh 这段是零宽度，否则 zsh 会把转义序列的字符
  # 算进提示符宽度，导致光标位置和换行计算出错。
  typeset -g __kanso_bmark=$'%{\e]133;B\a%}'

  # 百分号编码（RFC 3986 的 unreserved 字符与 / 不编码），结果放在 REPLY。
  # 逐字节编码以得到合法的 UTF-8 百分号序列，所以关掉 multibyte；用 $(( #ch )) 取字节值，
  # 比 printf "'$ch" 在高位字节上更可预测。
  __kanso_urlencode() {
    emulate -L zsh
    setopt extended_glob no_multibyte
    local s=$1 out= ch i code
    if [[ $s == [-A-Za-z0-9/._~]# ]]; then
      REPLY=$s
      return
    fi
    for (( i = 1; i <= ${#s}; ++i )); do
      ch=${s[i]}
      if [[ $ch == [-A-Za-z0-9/._~] ]]; then
        out+=$ch
      else
        (( code = #ch ))
        out+=$(printf '%%%02X' $(( code & 255 )))
      fi
    done
    REPLY=$out
  }

  # 保持用户原有的 PS1 不动，只在末尾确保附带零宽度的 OSC 133 B 标记。
  __kanso_ensure_bmark() {
    if [[ $PS1 != *"$__kanso_bmark" ]]; then
      PS1=${PS1//"$__kanso_bmark"/}$__kanso_bmark
    fi
  }

  # precmd：D（上一条命令的退出码）→ OSC 7 → A（0 空行，保留原生紧凑布局）→ 确保 PS1 末尾带 B。
  # 一次 print 输出，避免与别的输出交错。
  __kanso_precmd() {
    local ec=$?   # 必须是第一条语句：之后任何命令都会改写 $?
    emulate -L zsh
    setopt extended_glob
    local seq= host
    if (( __kanso_cmd_ran )); then
      seq+=$'\e]133;D;'${ec}$'\a'
      __kanso_cmd_ran=0
    fi
    __kanso_urlencode $HOST
    host=$REPLY
    __kanso_urlencode $PWD
    seq+=$'\e]7;file://'${host}${REPLY}$'\a'
    if (( __kanso_first )); then
      seq+=$'\e]133;A;kanso=first\a'
      __kanso_first=0
    else
      seq+=$'\e]133;A\a'
    fi
    __kanso_ensure_bmark
    print -rn -- $seq
  }

  # preexec：命令开始执行，输出从下一行起。
  __kanso_preexec() {
    __kanso_cmd_ran=1
    print -rn -- $'\e]133;C\a'
  }

  # 目录选择器静默切目录（状态栏点击目录时发送 ESC[9001~<hex>\n）：
  # 保留半行输入与光标、不写历史、触发 chpwd、更新 OSC 7 并重绘提示符。
  __kanso_change_directory() {
    emulate -L zsh
    local encoded= key= escaped= target= i
    local saved_buffer=$BUFFER saved_cursor=$CURSOR
    while read -rk 1 key; do
      [[ $key == $'\n' ]] && break
      encoded+=$key
    done
    [[ -n $encoded && $encoded != *[^0-9a-fA-F]* ]] || return 0
    (( ${#encoded} % 2 == 0 )) || return 0
    for (( i = 1; i <= ${#encoded}; i += 2 )); do
      escaped+="\\x${encoded[i,i+1]}"
    done
    printf -v target '%b' "$escaped"
    builtin cd -- "$target" >/dev/null 2>&1 || return 0
    BUFFER=$saved_buffer
    CURSOR=$saved_cursor
    __kanso_ensure_bmark
    __kanso_urlencode $HOST
    local host=$REPLY
    __kanso_urlencode $PWD
    print -rn -- $'\e]7;file://'${host}${REPLY}$'\a'
    zle reset-prompt
  }
fi

# macOS 的 /etc/zshrc 会把 HISTFILE 设成 ${ZDOTDIR:-$HOME}/.zsh_history；那时 ZDOTDIR 指向 shim 目录，
# 历史会被写进 shim 目录（删除 shim 就丢历史，也违反「不留痕迹」）。所以只在 HISTFILE 恰好等于
# shim 目录下的默认位置时，改回用户原来的位置：用户自己设置过其他值就不动，没设置过就保持未设置。
# 在加载用户 .zshrc 之前处理，用户文件里再设置的值自然生效。
if [[ -n ${HISTFILE-} && $HISTFILE == "$ZDOTDIR/.zsh_history" ]]; then
  HISTFILE=${KANSO_USER_ZDOTDIR:-$HOME}/.zsh_history
fi

# >>> kanso:load（五个 shim 文件里这一段完全相同，仅文件名不同）
# zsh 读取每个启动文件时都用「当时的 ZDOTDIR」定位文件，所以要让后续文件继续从 shim 目录加载，
# 就必须在加载完用户文件后把 ZDOTDIR 切回 shim 目录。加载用户文件期间则临时还原成用户自己的值
# （用户没设置过就 unset），这样用户文件里的 $ZDOTDIR 引用、"-z $ZDOTDIR" 判断都与没有 shim 时一致。
# 注意这里必须在顶层直接 source：放进函数里的话，用户文件里的 typeset 会变成函数局部变量。
__kanso_user_dir=${KANSO_USER_ZDOTDIR:-$HOME}
if [[ -n $KANSO_USER_ZDOTDIR ]]; then
  ZDOTDIR=$KANSO_USER_ZDOTDIR
else
  unset ZDOTDIR
fi
if [[ -n $__kanso_user_dir && -f $__kanso_user_dir/.zshrc && -r $__kanso_user_dir/.zshrc ]]; then
  source $__kanso_user_dir/.zshrc
fi
# 用户文件（典型的是 ~/.zshenv 里写 ZDOTDIR=~/.config/zsh）可能改了 ZDOTDIR：
# 记下新值，后面的启动文件就会按 zsh 本来的语义去新目录找。
if [[ -n ${ZDOTDIR-} ]]; then
  KANSO_USER_ZDOTDIR=$ZDOTDIR
  export KANSO_USER_ZDOTDIR
fi
ZDOTDIR=$__kanso_shim_dir
# <<< kanso:load


if [[ ${KANSO_SHELL_INTEGRATION:-1} != 0 ]]; then
  # 在用户 .zshrc 加载完之后注册，使我们的 hook 排在用户已有 hook 之后：precmd 里才能看到
  # 用户 / 主题最终设置的 PS1。add-zsh-hook 只追加、不覆盖用户已有的 hook，重复注册也会去重。
  autoload -Uz add-zsh-hook
  add-zsh-hook precmd __kanso_precmd
  add-zsh-hook preexec __kanso_preexec
  zle -N __kanso_change_directory
  bindkey -M emacs $'\e[9001~' __kanso_change_directory
  bindkey -M viins $'\e[9001~' __kanso_change_directory
  bindkey -M vicmd $'\e[9001~' __kanso_change_directory
fi

# 非 login 的交互式 shell 之后不会再读 .zlogin，在这里还原 ZDOTDIR（原因见 .zlogin shim）；
# login shell 留到 .zlogin 里还原，因为 zsh 还要用 ZDOTDIR 去定位它。
if [[ ! -o login ]]; then
  if [[ -n $KANSO_USER_ZDOTDIR ]]; then export ZDOTDIR=$KANSO_USER_ZDOTDIR; else unset ZDOTDIR; fi
fi
unset __kanso_user_dir __kanso_shim_dir
