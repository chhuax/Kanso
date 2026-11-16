# Kanso 的 zsh 集成 shim：.zshrc
#
# 本文件由 Kanso 自动生成，每次启动应用时按内容比对后才会重写，请勿手工修改。
# 它的核心是加载用户自己的 .zshrc（用户设置过 ZDOTDIR 就用它，否则用 HOME），
# 保证任何一个启动文件都不会因为 ZDOTDIR 被指向这里而被跳过。
# 除此之外它还做两件事：把 HISTFILE 改回用户原来的位置，以及注册提示符集成的 precmd / preexec hook。
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
  # __kanso_have：是否已经捕获过用户 PS1；__kanso_last_ps1：我们上次写入的 PS1，
  # 用来判断 PS1 是否被用户或主题改写过；__kanso_ins_*：上次插入的目录 / 分支文本，
  # 用来在别的脚本基于我们的 PS1 二次改写时把我们的部分准确摘掉，避免叠加。
  typeset -g __kanso_have=0 __kanso_last_ps1= __kanso_ins_dir= __kanso_ins_branch=
  # 对用户 PS1 的分析结果：__kanso_core 是去掉目录 / 用户名后的正文（`%#` 前有占位符 \x1f），
  # __kanso_trail 表示原提示符末尾有空格，__kanso_show_dir 表示是否由我们显示目录。
  typeset -g __kanso_core= __kanso_trail=0 __kanso_show_dir=1
  # 提示符里展示的目录 / 分支（已转义 %），promptsubst 开启时通过变量引用插入，见 __kanso_build_ps1。
  typeset -g __kanso_p_dir= __kanso_p_branch=
  # OSC 7 编码结果缓存：PWD 没变时不重复编码。
  typeset -g __kanso_enc_src= __kanso_enc_out=
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

  # 从 PWD 逐级向上找 .git，直接读 HEAD，不启动 git 进程；结果（分支名或提交号前 7 位）放在 REPLY，
  # 找不到或内容不认识时 REPLY 为空。
  __kanso_git_branch() {
    emulate -L zsh
    setopt extended_glob
    REPLY=
    local dir=$PWD gitpath line head gd
    while :; do
      gitpath=$dir/.git
      if [[ -d $gitpath ]]; then
        head=$gitpath/HEAD
        break
      elif [[ -f $gitpath ]]; then
        # worktree / submodule：.git 是文件，内容 `gitdir: <路径>`；相对路径以 .git 文件所在目录为基准。
        line=
        IFS= read -r line < $gitpath 2>/dev/null
        line=${line%%[[:space:]]#}
        [[ $line == 'gitdir: '* ]] || return 0
        gd=${line#gitdir: }
        [[ $gd == /* ]] || gd=$dir/$gd
        head=$gd/HEAD
        break
      fi
      [[ $dir == / || -z $dir ]] && return 0
      dir=${dir:h}
    done
    line=
    IFS= read -r line < $head 2>/dev/null
    line=${line%%[[:space:]]#}
    if [[ $line == 'ref: '* ]]; then
      line=${line#ref: }
      line=${line#refs/heads/}
      # HEAD 是数据，可能含控制字符（ESC 等）；直接放进提示符会被终端当成转义序列，所以全部丢弃。
      REPLY=${line//[[:cntrl:]]/}
    elif [[ ${#line} -eq 40 || ${#line} -eq 64 ]] && [[ $line == [0-9a-fA-F]## ]]; then
      REPLY=${line[1,7]}
    fi
  }

  # 目录显示文本放在 REPLY：~ 缩写主目录；超过 36 个字符只保留尾部若干完整路径段，前面加 `.../`
  # （含 `.../` 在内不超过 36，最后一段本身超长时也完整保留）。
  __kanso_fmt_dir() {
    emulate -L zsh
    setopt extended_glob
    local d=$PWD
    if [[ -n $HOME && $HOME != / ]]; then
      if [[ $d == $HOME ]]; then
        d='~'
      elif [[ $d == $HOME/* ]]; then
        d='~'${d#$HOME}
      fi
    fi
    # 目录名是数据，且可能含控制字符；替换成 ?，避免终端把它当转义序列。
    d=${d//[[:cntrl:]]/\?}
    if (( ${#d} > 36 )); then
      local -a segs=( ${(s:/:)d} )
      local tail=${segs[-1]} i
      for (( i = ${#segs} - 1; i >= 1; --i )); do
        (( ${#tail} + ${#segs[i]} + 5 > 36 )) && break
        tail=${segs[i]}/$tail
      done
      d=.../$tail
    fi
    REPLY=$d
  }

  # 分析用户 PS1，更新 __kanso_core / __kanso_trail / __kanso_show_dir。
  # 逐个 token 扫描（而不是整串做替换），这样 `%%~`（字面的 % 加 ~）不会被误当成 `%~`。
  __kanso_analyze_ps1() {
    emulate -L zsh
    setopt extended_glob
    local src=$1 out= tok c
    local strip_dir=1 hash_done=0
    __kanso_show_dir=1
    __kanso_trail=0
    [[ $src == *' ' ]] && __kanso_trail=1
    # 用户自己用 $PWD 之类表达式显示目录：不改写它，也不再重复显示目录。
    if [[ $src == *'$PWD'* || $src == *'${PWD'* || $src == *'$(pwd'* || $src == *'`pwd'* ]]; then
      __kanso_show_dir=0
      strip_dir=0
    fi
    while (( ${#src} )); do
      if [[ $src =~ '^@%-?[0-9]*[mM]' ]]; then
        # `@%m` / `@%M`：与用户名相连的主机名，整体丢弃。
        src=${src[${#MATCH}+1,-1]}
      elif [[ $src =~ '^%-?[0-9]*(.)' ]]; then
        tok=$MATCH
        c=$match[1]
        src=${src[${#tok}+1,-1]}
        case $c in
          # 目录转义（含 %1~、%-3~、%2d 等带数字写法）。%c / %C / %. 是"目录尾部若干段"
          # 的写法（zshmisc 提示符转义），同样会与我们前置的目录重复显示——
          # oh-my-zsh 默认主题 robbyrussell 就用 %c，不去掉会出现 `~ ➜ repo` 这样的重复。
          '~'|d|'/'|c|C|'.') (( strip_dir )) || out+=$tok ;;
          n|N) ;;                                       # 用户名
          '#')
            # 第一个 %# 前放占位符，分支稍后插在这里。
            if (( ! hash_done )); then out+=$'\x1f'; hash_done=1; fi
            out+=$tok
            ;;
          *) out+=$tok ;;
        esac
      else
        out+=${src[1]}
        src=${src[2,-1]}
      fi
    done
    out=${out//( )##/ }      # 合并连续空格
    out=${out##( )##}        # 去掉开头空格
    out=${out%%( )##}        # 末尾空格单独用 __kanso_trail 记录，组装时再补回
    __kanso_core=$out
  }

  # 组装最终 PS1，结果放在 REPLY。目录与分支都是数据：先转义 `%`；promptsubst 开启时
  # 提示符会先做参数 / 命令替换，数据里的 `$(...)` 会被执行，所以改为插入变量引用
  # `${__kanso_p_dir}`（替换结果不会被再次解析），变量里的 `%` 同样要转义，
  # 因为替换之后 zsh 还会再做一遍提示符转义。
  __kanso_build_ps1() {
    emulate -L zsh
    setopt extended_glob
    local dir_txt branch_txt dir_ins branch_ins pre= core=$__kanso_core out
    __kanso_git_branch
    branch_txt=${REPLY//\%/%%}
    __kanso_fmt_dir
    dir_txt=${REPLY//\%/%%}
    if [[ -o promptsubst ]]; then
      __kanso_p_dir=$dir_txt
      __kanso_p_branch=$branch_txt
      dir_ins='${__kanso_p_dir}'
      branch_ins='${__kanso_p_branch}'
    else
      dir_ins=$dir_txt
      branch_ins=$branch_txt
    fi
    # 目录用默认前景色；分支 2 绿；`%#` 1 红。用 ANSI 调色板索引，跟随浅色 / 深色主题，不写死 RGB。
    __kanso_ins_dir=
    __kanso_ins_branch=
    if (( __kanso_show_dir )); then
      __kanso_ins_dir=$dir_ins
      pre=$__kanso_ins_dir
    fi
    if [[ -n $branch_txt ]]; then
      __kanso_ins_branch="%F{2}${branch_ins}%f"
    fi
    # 只给第一个 `%#` 上色；替换文本放进变量，避免 `${…/…/…}` 里的花括号被当成参数展开的边界。
    local red_mark='%F{1}%#%f'
    core=${core/\%\#/$red_mark}
    if [[ $core == *$'\x1f'* ]]; then
      # 有 %#：分支插在它前面；占位符紧贴在非空格字符后面时（如 `$PWD%#`）先补一个空格，避免粘连。
      if [[ -n $__kanso_ins_branch ]]; then
        if [[ $core == *[^\ ]$'\x1f'* ]]; then
          core=${core/$'\x1f'/ $__kanso_ins_branch }
        else
          core=${core/$'\x1f'/$__kanso_ins_branch }
        fi
      else
        core=${core/$'\x1f'/}
      fi
    elif [[ -n $__kanso_ins_branch ]]; then
      pre+=${pre:+ }$__kanso_ins_branch
    fi
    out=$pre
    [[ -n $out && -n $core ]] && out+=' '
    out+=$core
    # 原提示符末尾的空格是光标与提示符的间距，保留；正文被全部去掉时也至少补一个。
    if (( __kanso_trail )) || [[ -z $core ]]; then
      out+=' '
    fi
    REPLY=$out$__kanso_bmark
  }

  # 更新 PS1。「用户原始 PS1」靠 __kanso_last_ps1 识别：当前 PS1 与我们上次写入的不同，
  # 说明用户 / 主题（如 oh-my-zsh 在 precmd 里重设 PS1）或某个脚本改过它，重新捕获并分析；
  # 相同则沿用上次的分析结果。若改写是基于我们的 PS1 叠加的（带有我们的 B 标记，
  # 例如 virtualenv activate 在 PS1 前面加 `(venv) `），先把我们插入的目录 / 分支 / B 标记
  # 摘掉再分析，避免目录与分支重复叠加。
  __kanso_update_ps1() {
    emulate -L zsh
    setopt extended_glob
    local p
    if (( ! __kanso_have )) || [[ $PS1 != "$__kanso_last_ps1" ]]; then
      p=$PS1
      if [[ $p == *"$__kanso_bmark"* ]]; then
        p=${p//"$__kanso_bmark"/}
        [[ -n $__kanso_ins_dir ]] && p=${p/"$__kanso_ins_dir"/}
        [[ -n $__kanso_ins_branch ]] && p=${p/"$__kanso_ins_branch"/}
      fi
      __kanso_analyze_ps1 $p
      __kanso_have=1
    fi
    __kanso_build_ps1
    PS1=$REPLY
    __kanso_last_ps1=$PS1
  }

  # precmd：D（上一条命令的退出码）→ OSC 7 → A（第一个提示符带 kanso=first 且不插空行；
  # 之后先输出一个空行、发 A、再输出第二个空行，分隔线画在两个空行正中间）→ 更新 PS1（末尾带 B）。
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
      seq+=$'\n\e]133;A\a\n'
    fi
    __kanso_update_ps1
    print -rn -- $seq
  }

  # preexec：命令开始执行，输出从下一行起。
  __kanso_preexec() {
    __kanso_cmd_ran=1
    print -rn -- $'\e]133;C\a'
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
fi

# 非 login 的交互式 shell 之后不会再读 .zlogin，在这里还原 ZDOTDIR（原因见 .zlogin shim）；
# login shell 留到 .zlogin 里还原，因为 zsh 还要用 ZDOTDIR 去定位它。
if [[ ! -o login ]]; then
  if [[ -n $KANSO_USER_ZDOTDIR ]]; then export ZDOTDIR=$KANSO_USER_ZDOTDIR; else unset ZDOTDIR; fi
fi
unset __kanso_user_dir __kanso_shim_dir
