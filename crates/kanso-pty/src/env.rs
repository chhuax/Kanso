//! 子进程环境变量：继承、删除、覆盖三类的显式清单（local-shell.md §3）。
//!
//! 三类规则全部用常量表和 [`build_env`] 集中表达，不在别处零散处理。完整清单与依据见
//! `docs/local-shell-impl.md` §1。

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// 保存用户原来的 `ZDOTDIR` 的变量名。shim 靠它找到用户自己的 zsh 配置目录；
/// 用户没有设置过 `ZDOTDIR` 时该变量不存在，shim 回退到 `$HOME`。
pub const ENV_USER_ZDOTDIR: &str = "KANSO_USER_ZDOTDIR";

/// 提示符集成开关：`0` 时 shim 只加载用户文件，不注册 hook、不改提示符。
pub const ENV_SHELL_INTEGRATION: &str = "KANSO_SHELL_INTEGRATION";

/// **覆盖**：无条件设置为固定值（不论父进程里是什么）。
/// `TERM_PROGRAM_VERSION`、locale 与 zsh 集成的变量取决于调用方传入的选项，见 [`build_env`]。
pub const FORCED_ENV: &[(&str, &str)] = &[
    ("TERM", "xterm-256color"),
    ("COLORTERM", "truecolor"),
    ("TERM_PROGRAM", "Kanso"),
];

/// **删除（任何值）**：这些变量只要存在就会让常见工具关闭颜色，是启动 Kanso 的父进程
/// （构建工具、代理进程、CI 包装器）带来的约束，不应传给用户的交互式 shell。
///
/// - `NO_COLOR`：no-color.org 约定，存在且非空即禁用（local-shell.md §3 明确要求）。
/// - `NODE_DISABLE_COLORS`：Node.js 的 `tty` 颜色检测把它当作与 `NO_COLOR` 同类的开关。
/// - `ANSI_COLORS_DISABLED`：Python `termcolor` 等库认的同类开关（存在即禁用）。
pub const REMOVED_ALWAYS: &[&str] = &["NO_COLOR", "NODE_DISABLE_COLORS", "ANSI_COLORS_DISABLED"];

/// **有条件删除**：变量的值落在列表内才删除，其他值（例如 `CLICOLOR=1`）保留，
/// 因为那是在要求开启颜色。
///
/// - `CLICOLOR=0`：BSD / macOS `ls` 等的"不要颜色"约定。
/// - `FORCE_COLOR=0` / `false`：Node.js `supports-color` 家族把它们解释为强制无色。
pub const REMOVED_WHEN_FALSEY: &[(&str, &[&str])] =
    &[("CLICOLOR", &["0"]), ("FORCE_COLOR", &["0", "false"])];

/// 一次环境变量变更：`Some(值)` 表示设置，`None` 表示删除。
pub type EnvChange = (OsString, Option<OsString>);

/// 一份环境变量快照。`build_env` 等纯函数只读它，不碰进程自己的环境，方便表格测试。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvSnapshot {
    vars: BTreeMap<OsString, OsString>,
}

impl EnvSnapshot {
    /// 当前进程的环境。
    pub fn from_current() -> Self {
        Self::from_pairs(std::env::vars_os())
    }

    /// 由键值对构造（主要给测试和调用方自己组装环境用）。
    pub fn from_pairs<K, V>(pairs: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<OsString>,
        V: Into<OsString>,
    {
        EnvSnapshot {
            vars: pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    /// 读取变量。Windows 的环境变量名不区分大小写，所以那里退化为忽略 ASCII 大小写的查找。
    pub fn get(&self, key: &str) -> Option<&OsStr> {
        if let Some(v) = self.vars.get(OsStr::new(key)) {
            return Some(v);
        }
        if cfg!(windows) {
            return self
                .vars
                .iter()
                .find(|(k, _)| k.to_string_lossy().eq_ignore_ascii_case(key))
                .map(|(_, v)| v.as_os_str());
        }
        None
    }

    /// 读取变量并要求是合法 UTF-8。
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(OsStr::to_str)
    }

    /// 变量存在且不是空串。
    pub fn get_non_empty(&self, key: &str) -> Option<&OsStr> {
        self.get(key).filter(|v| !v.is_empty())
    }

    /// 遍历全部变量。
    pub fn iter(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.vars
            .iter()
            .map(|(k, v)| (k.as_os_str(), v.as_os_str()))
    }

    /// 在快照上应用一组变更，得到最终要交给子进程的完整环境。
    pub fn apply(&self, changes: &[EnvChange]) -> EnvSnapshot {
        let mut vars = self.vars.clone();
        for (k, v) in changes {
            match v {
                Some(v) => {
                    vars.insert(k.clone(), v.clone());
                }
                None => {
                    vars.remove(k);
                }
            }
        }
        EnvSnapshot { vars }
    }
}

/// zsh 提示符集成的启用信息。只有 [`crate::install_zsh_shim`] 成功时才应构造它；
/// 安装失败时传 `None`，shell 按原样启动（local-shell.md §5.1）。
#[derive(Clone, Debug)]
pub struct ZshIntegration {
    /// `install_zsh_shim` 返回的 shim 目录，会成为子进程的 `ZDOTDIR`。
    pub shim_dir: PathBuf,
    /// 是否启用提示符集成。`false` 时 shim 仍负责加载用户文件，但不注册 hook。
    pub prompt_enabled: bool,
}

/// [`build_env`] 的输入选项。
#[derive(Clone, Debug)]
pub struct EnvOptions {
    /// 写入 `TERM_PROGRAM_VERSION` 的应用版本号。
    pub term_program_version: String,
    /// [`crate::decide_locale`] 的结果，`Some` 时写入 `LANG`。
    pub locale: Option<String>,
    /// zsh 集成；`None` 表示不设置 `ZDOTDIR`。
    pub zsh: Option<ZshIntegration>,
}

/// 计算相对 `base` 需要做的环境变更；没有出现在结果里的变量全部**继承**。
///
/// 1. 删除：[`REMOVED_ALWAYS`] 与 [`REMOVED_WHEN_FALSEY`]，只对 `base` 里确实存在的变量产生条目。
/// 2. 覆盖：[`FORCED_ENV`]、`TERM_PROGRAM_VERSION`、`LANG`（有 locale 决策结果时）。
/// 3. zsh 集成：`ZDOTDIR` 指向 shim；用户原来的 `ZDOTDIR` 存入 [`ENV_USER_ZDOTDIR`]。
///    若 `base` 的 `ZDOTDIR` 恰好已经是 shim 目录（从 Kanso 里的 shell 再启动 Kanso，
///    而 shim 没来得及还原的情形），它不是"用户原来的"，改从 `base` 的 `KANSO_USER_ZDOTDIR`
///    取；用户没有设置过时必须**删除**可能残留的旧值，否则 shim 会去加载过期目录。
///    `base` 有 `TERM`（从终端里启动）时不信任继承的 `ZDOTDIR`，视同用户没有设置过。
pub fn build_env(base: &EnvSnapshot, opts: &EnvOptions) -> Vec<EnvChange> {
    let mut out: Vec<EnvChange> = Vec::new();

    for key in REMOVED_ALWAYS {
        if base.get(key).is_some() {
            out.push((OsString::from(*key), None));
        }
    }
    for (key, falsey) in REMOVED_WHEN_FALSEY {
        if let Some(v) = base.get_str(key)
            && falsey.contains(&v)
        {
            out.push((OsString::from(*key), None));
        }
    }

    for (k, v) in FORCED_ENV {
        out.push((OsString::from(*k), Some(OsString::from(*v))));
    }
    out.push((
        OsString::from("TERM_PROGRAM_VERSION"),
        Some(OsString::from(&opts.term_program_version)),
    ));
    if let Some(locale) = &opts.locale {
        out.push((OsString::from("LANG"), Some(OsString::from(locale))));
    }

    if let Some(z) = &opts.zsh {
        let user = user_zdotdir(base, &z.shim_dir);
        out.push((
            OsString::from("ZDOTDIR"),
            Some(z.shim_dir.clone().into_os_string()),
        ));
        match user {
            Some(u) => out.push((OsString::from(ENV_USER_ZDOTDIR), Some(u))),
            None if base.get(ENV_USER_ZDOTDIR).is_some() => {
                out.push((OsString::from(ENV_USER_ZDOTDIR), None));
            }
            None => {}
        }
        out.push((
            OsString::from(ENV_SHELL_INTEGRATION),
            Some(OsString::from(if z.prompt_enabled { "1" } else { "0" })),
        ));
    }
    out
}

fn user_zdotdir(base: &EnvSnapshot, shim_dir: &Path) -> Option<OsString> {
    // `base` 里有 `TERM` 说明 Kanso 是从某个终端里用 `open` 等方式启动的（Dock / Finder
    // 启动时继承的 launchd 环境没有 `TERM`）。这时继承来的 `ZDOTDIR` 属于那个终端的会话，
    // 可能是它自己的 shim 目录：那套 shim 若也依赖 `KANSO_USER_ZDOTDIR` 之类的未导出变量，
    // 我们再把它当成"用户目录"交给自己的 shim 加载，就会自己 source 自己直到 fd 耗尽。
    // 所以这种情况一律不信任继承值，回退到 `$HOME`，与从 Dock 启动的行为一致；用户在
    // `~/.zshenv` 里设置的 `ZDOTDIR` 仍会被 shim 的 `.zshenv` 照常识别（local-shell-impl.md §3.2 步骤 4）。
    if base.get("TERM").is_some() {
        return None;
    }
    let cur = base.get_non_empty("ZDOTDIR")?;
    if Path::new(cur) != shim_dir {
        return Some(cur.to_owned());
    }
    base.get_non_empty(ENV_USER_ZDOTDIR).map(OsStr::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(p: &[(&str, &str)]) -> EnvSnapshot {
        EnvSnapshot::from_pairs(p.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    }

    fn opts() -> EnvOptions {
        EnvOptions {
            term_program_version: "1.2.3".into(),
            locale: None,
            zsh: None,
        }
    }

    fn removed(changes: &[EnvChange], key: &str) -> bool {
        changes.iter().any(|(k, v)| k == key && v.is_none())
    }

    fn set_to(changes: &[EnvChange], key: &str) -> Option<String> {
        changes
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.as_ref().map(|v| v.to_string_lossy().into_owned()))
    }

    #[test]
    fn color_disabling_variables_table() {
        // (环境, 变量, 是否应被删除)
        type Case<'a> = (&'a [(&'a str, &'a str)], &'a str, bool);
        let cases: &[Case] = &[
            (&[("NO_COLOR", "1")], "NO_COLOR", true),
            (&[("NO_COLOR", "")], "NO_COLOR", true),
            (&[("NO_COLOR", "0")], "NO_COLOR", true),
            (&[("NODE_DISABLE_COLORS", "1")], "NODE_DISABLE_COLORS", true),
            (
                &[("ANSI_COLORS_DISABLED", "1")],
                "ANSI_COLORS_DISABLED",
                true,
            ),
            (&[("CLICOLOR", "0")], "CLICOLOR", true),
            (&[("CLICOLOR", "1")], "CLICOLOR", false),
            (&[("FORCE_COLOR", "0")], "FORCE_COLOR", true),
            (&[("FORCE_COLOR", "false")], "FORCE_COLOR", true),
            (&[("FORCE_COLOR", "1")], "FORCE_COLOR", false),
            (&[("FORCE_COLOR", "3")], "FORCE_COLOR", false),
            // 不存在的变量不产生条目。
            (&[], "NO_COLOR", false),
        ];
        for (env, key, want) in cases {
            let out = build_env(&snap(env), &opts());
            assert_eq!(removed(&out, key), *want, "{env:?} {key}");
        }
    }

    #[test]
    fn forced_overrides_and_inheritance() {
        let base = snap(&[
            ("TERM", "dumb"),
            ("COLORTERM", "24bit"),
            ("PATH", "/bin"),
            ("FOO", "x"),
        ]);
        let out = build_env(&base, &opts());
        assert_eq!(set_to(&out, "TERM").as_deref(), Some("xterm-256color"));
        assert_eq!(set_to(&out, "COLORTERM").as_deref(), Some("truecolor"));
        assert_eq!(set_to(&out, "TERM_PROGRAM").as_deref(), Some("Kanso"));
        assert_eq!(
            set_to(&out, "TERM_PROGRAM_VERSION").as_deref(),
            Some("1.2.3")
        );
        // 其余变量不产生条目，即继承。
        assert!(out.iter().all(|(k, _)| k != "PATH" && k != "FOO"));
        let fin = base.apply(&out);
        assert_eq!(fin.get_str("PATH"), Some("/bin"));
        assert_eq!(fin.get_str("FOO"), Some("x"));
        assert_eq!(fin.get_str("TERM"), Some("xterm-256color"));
    }

    #[test]
    fn locale_becomes_lang() {
        let mut o = opts();
        o.locale = Some("zh_CN.UTF-8".into());
        assert_eq!(
            set_to(&build_env(&snap(&[]), &o), "LANG").as_deref(),
            Some("zh_CN.UTF-8")
        );
        assert_eq!(set_to(&build_env(&snap(&[]), &opts()), "LANG"), None);
    }

    fn zsh_opts(enabled: bool) -> EnvOptions {
        EnvOptions {
            zsh: Some(ZshIntegration {
                shim_dir: "/data/shim/zsh".into(),
                prompt_enabled: enabled,
            }),
            ..opts()
        }
    }

    #[test]
    fn zsh_integration_variables() {
        // 用户没有 ZDOTDIR。
        let out = build_env(&snap(&[]), &zsh_opts(true));
        assert_eq!(set_to(&out, "ZDOTDIR").as_deref(), Some("/data/shim/zsh"));
        assert_eq!(set_to(&out, ENV_USER_ZDOTDIR), None);
        assert_eq!(set_to(&out, ENV_SHELL_INTEGRATION).as_deref(), Some("1"));
        // 用户有 ZDOTDIR：保存下来。
        let out = build_env(
            &snap(&[("ZDOTDIR", "/home/u/.config/zsh")]),
            &zsh_opts(false),
        );
        assert_eq!(
            set_to(&out, ENV_USER_ZDOTDIR).as_deref(),
            Some("/home/u/.config/zsh")
        );
        assert_eq!(set_to(&out, ENV_SHELL_INTEGRATION).as_deref(), Some("0"));
        // 残留的过期 KANSO_USER_ZDOTDIR 必须删除。
        let out = build_env(&snap(&[(ENV_USER_ZDOTDIR, "/stale")]), &zsh_opts(true));
        assert!(removed(&out, ENV_USER_ZDOTDIR));
        // base 的 ZDOTDIR 已经是 shim：用保存的用户值，而不是把 shim 当用户目录。
        let out = build_env(
            &snap(&[
                ("ZDOTDIR", "/data/shim/zsh/"),
                (ENV_USER_ZDOTDIR, "/home/u/zsh"),
            ]),
            &zsh_opts(true),
        );
        assert_eq!(
            set_to(&out, ENV_USER_ZDOTDIR).as_deref(),
            Some("/home/u/zsh")
        );
        // 从终端启动（有 TERM）：继承的 ZDOTDIR 可能是别的终端的 shim，不当作用户目录，
        // 继承来的 KANSO_USER_ZDOTDIR 同样删除。
        let out = build_env(
            &snap(&[
                ("TERM", "xterm-256color"),
                (
                    "ZDOTDIR",
                    "/Users/u/Library/Application Support/Other/shell/zsh",
                ),
                (
                    ENV_USER_ZDOTDIR,
                    "/Users/u/Library/Application Support/Other/shell/zsh",
                ),
            ]),
            &zsh_opts(true),
        );
        assert_eq!(set_to(&out, "ZDOTDIR").as_deref(), Some("/data/shim/zsh"));
        assert!(removed(&out, ENV_USER_ZDOTDIR));
        // 不启用 zsh 集成时完全不碰 ZDOTDIR。
        let out = build_env(&snap(&[("ZDOTDIR", "/x")]), &opts());
        assert!(out.iter().all(|(k, _)| k != "ZDOTDIR"));
    }
}
