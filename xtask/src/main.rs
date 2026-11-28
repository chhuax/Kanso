//! 仓库级开发任务入口，用法见 `usage()`。
//!
//! token 与多语言校验需要 `kanso-types`、`serde_json`、`fluent-syntax`；`ci` 子命令本身只用 std。

mod apple;
mod bench;
mod color;
mod locales;
mod notices;
mod release;
mod tokens;
mod windows;

use std::env;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("ci") => ci(&args[1..]),
        Some("bench") => bench::run(&args[1..]),
        Some("bench-startup") => bench::startup(&args[1..]),
        Some("check-tokens") => tokens::check(),
        Some("check-locales") => locales::check(),
        Some("notices") => notices::generate(),
        Some("release") => release::dispatch(&args[1..]),
        Some("dmg") => apple::dmg(&args[1..]),
        Some("win-package") => windows::win_package(&args[1..]),
        _ => {
            usage();
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!(
        "用法：cargo xtask <子命令>\n\n\
         子命令：\n  \
         bench [Criterion 参数]      运行终端基准并打印摘要（不属于 ci）\n  \
         bench-startup [--runs N]     构建 Release，测量首帧可输入与空闲 RSS（不属于 ci）\n  \
         ci [--skip-deny]            依次运行 fmt、clippy、test、check-tokens、check-locales、渠道矩阵 MCP 测试与 cargo deny\n  \
         check-tokens                校验 design/tokens.json 与终端配色（格式、对比度）\n  \
         check-locales               校验 locales/*.ftl 与 kanso-types 消息清单一致\n  \
         notices                     重新生成产品依赖与素材的 THIRD_PARTY_NOTICES.md\n  \
         release bump <版本>         同步更新 Cargo.toml 与 Cargo.lock\n  \
         release tag <版本> [--remote <r>] [--dry-run]\n                                     拉取远端 main、核对提交内版本号与标签状态后打标并推送\n  \
         dmg [--arch <arm64|x86_64>]  （仅 macOS）Release 构建 GPUI 版 Kanso.app 并生成 DMG，输出 SHA-256（默认 arm64）\n  \
         win-package [--skip-build] [--no-installer] [--offline]\n                                     （仅 Windows）Release 构建 x64 GPUI，生成免安装 ZIP 与 NSIS 安装程序（找到 makensis 时），输出 SHA-256"
    );
}

/// 以环境变量 `CARGO`（缺省 `cargo`）构造子进程，保证与外层调用使用同一个 cargo。
fn cargo() -> Command {
    Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

/// 运行一个步骤，失败时打印步骤名并返回 false。
fn step(name: &str, args: &[&str]) -> bool {
    eprintln!("\n==> {name}: cargo {}", args.join(" "));
    match cargo().args(args).status() {
        Ok(s) if s.success() => true,
        Ok(s) => {
            eprintln!("步骤失败：{name}（{s}）");
            false
        }
        Err(e) => {
            eprintln!("步骤失败：{name}（无法启动 cargo：{e}）");
            false
        }
    }
}

/// 运行任意外部命令；失败时打印步骤名并返回 false。
fn run(name: &str, cmd: &mut Command) -> bool {
    eprintln!("\n==> {name}: {cmd:?}");
    match cmd.status() {
        Ok(s) if s.success() => true,
        Ok(s) => {
            eprintln!("步骤失败：{name}（{s}）");
            false
        }
        Err(e) => {
            eprintln!("步骤失败：{name}（无法启动：{e}）");
            false
        }
    }
}

fn ci(flags: &[String]) -> ExitCode {
    let mut skip_deny = false;
    for f in flags {
        match f.as_str() {
            "--skip-deny" => skip_deny = true,
            other => {
                eprintln!("未知参数：{other}");
                usage();
                return ExitCode::FAILURE;
            }
        }
    }

    let mut steps: Vec<(String, Vec<&str>)> = vec![
        ("fmt".into(), vec!["fmt", "--all", "--", "--check"]),
        (
            "clippy".into(),
            vec![
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        ),
        ("test".into(), vec!["test", "--workspace"]),
        // 经子进程再入口运行，保持每一步都是独立的 cargo 调用，失败输出与其他步骤一致。
        (
            "check-tokens".into(),
            vec!["run", "--quiet", "-p", "xtask", "--", "check-tokens"],
        ),
        (
            "check-locales".into(),
            vec!["run", "--quiet", "-p", "xtask", "--", "check-locales"],
        ),
    ];
    for channel in ["mas", "setapp"] {
        // MCP 服务的渠道限制（非 Direct 一律"不支持"）只在非 Direct 构建里才能测到，
        // 默认的 `cargo test --workspace` 跑的是 Direct，所以这里按渠道单独跑一遍。
        steps.push((
            format!("渠道矩阵 MCP 测试 ({channel})"),
            vec![
                "test",
                "-p",
                "kanso-mcp-server",
                "--no-default-features",
                "--features",
                channel,
            ],
        ));
    }

    for (name, args) in &steps {
        if !step(name, args) {
            return ExitCode::FAILURE;
        }
    }

    if skip_deny {
        eprintln!(
            "\n!!!!!!!! 警告：已跳过 cargo deny（--skip-deny），许可证与依赖来源未检查 !!!!!!!!"
        );
    } else {
        eprintln!("\n==> deny: cargo deny check");
        let has_deny = cargo()
            .args(["deny", "--version"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !has_deny {
            eprintln!(
                "步骤失败：deny（未安装 cargo-deny，请运行 `cargo install --locked cargo-deny`）"
            );
            return ExitCode::FAILURE;
        }
        if !step("deny", &["deny", "check"]) {
            return ExitCode::FAILURE;
        }
    }

    eprintln!("\nCI 全部通过");
    ExitCode::SUCCESS
}
