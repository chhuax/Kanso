# Kanso 的 zsh 集成 shim：.zlogout
#
# 本文件由 Kanso 自动生成，每次启动应用时按内容比对后才会重写，请勿手工修改。
# 它只做一件事：加载用户自己的 .zlogout（用户设置过 ZDOTDIR 就用它，否则用 HOME），
# 保证任何一个启动文件都不会因为 ZDOTDIR 被指向这里而被跳过。
# 删除整个 shim 目录即可彻底移除，不会在用户的任何文件里留下东西。
# 正常情况下 ZDOTDIR 已在 .zshrc / .zlogin 里还原，退出时 zsh 直接读用户自己的 .zlogout，
# 不会走到这里；保留本文件是为了在 ZDOTDIR 仍指向 shim 的异常路径上也不跳过用户的 .zlogout。
__kanso_shim_dir=$ZDOTDIR

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
if [[ -n $__kanso_user_dir && -f $__kanso_user_dir/.zlogout && -r $__kanso_user_dir/.zlogout ]]; then
  source $__kanso_user_dir/.zlogout
fi
# 用户文件（典型的是 ~/.zshenv 里写 ZDOTDIR=~/.config/zsh）可能改了 ZDOTDIR：
# 记下新值，后面的启动文件就会按 zsh 本来的语义去新目录找。
if [[ -n ${ZDOTDIR-} ]]; then
  KANSO_USER_ZDOTDIR=$ZDOTDIR
  export KANSO_USER_ZDOTDIR
fi
ZDOTDIR=$__kanso_shim_dir
# <<< kanso:load

unset __kanso_user_dir __kanso_shim_dir
