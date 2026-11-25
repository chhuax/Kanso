//! 仓库级开发任务入口，用法见 `usage()`。
//!
//! token 与多语言校验需要 `kanso-types`、`serde_json`、`fluent-syntax`；`ci` 子命令本身只用 std。

mod apple;
mod bench;
mod bindings;
mod color;
mod locales;
mod notices;
mod release;
mod tokens;
mod windows;

use std::env;
use std::path::Path;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("ci") => ci(&args[1..]),
        Some("bench") => bench::run(&args[1..]),
        Some("bench-startup") => bench::startup(&args[1..]),
        Some("check-tokens") => tokens::check(),
        Some("gen-tokens") => tokens::generate(&args[1..]),
        Some("check-locales") => locales::check(),
        Some("notices") => notices::generate(),
        Some("release") => release::dispatch(&args[1..]),
        Some("dmg") => apple::dmg(&args[1..]),
        Some("win-package") => windows::win_package(&args[1..]),
        Some("bindgen") => bindings::generate(),
        Some("check-bindings") => bindings::check(),
        Some("apple-lib") => apple::apple_lib(&args[1..]),
        Some("xcframework") => apple::xcframework(&args[1..]),
        Some("xcode") => apple::xcode(),
        Some("apple-build") => apple::apple_build(&args[1..]),
        Some("apple-test") => apple::apple_test(&args[1..]),
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
         ci [--skip-deny] [--skip-apple]\n                                     依次运行 fmt、clippy、test、c-abi、check-tokens、check-locales、check-bindings、渠道矩阵、\n                                     apple-build --all、apple-test 与 cargo deny\n  \
         check-tokens                校验 design/tokens.json 与终端配色（格式、对比度）\n  \
         gen-tokens [--out <dir>]    生成 Xcode 颜色集与 DesignTokens.swift（默认 apple/Kanso/Generated）\n  \
         check-locales               校验 locales/*.ftl 与 kanso-types 消息清单一致，并检查 Localizable.xcstrings 中英齐全\n  \
         notices                     重新生成产品依赖与素材的 THIRD_PARTY_NOTICES.md\n  \
         release bump <版本> [--build <N>]\n                                     同步更新 Cargo.toml、apple/project.yml 与 Cargo.lock\n  \
         release tag <版本> [--remote <r>] [--dry-run]\n                                     拉取远端 main、核对提交内版本号与标签状态后打标并推送\n  \
         dmg [--arch <arm64|x86_64>]  （仅 macOS）用 Xcode 构建 Direct AppKit 版并生成 DMG，输出 SHA-256（默认 arm64）\n  \
         win-package [--skip-build] [--no-installer] [--offline]\n                                     （仅 Windows）Release 构建 x64 GPUI，生成免安装 ZIP 与 NSIS 安装程序（找到 makensis 时），输出 SHA-256\n  \
         bindgen                     用 UniFFI library mode 重新生成 Swift 绑定（入库于 apple/Packages/KansoBridge）\n  \
         check-bindings              重新生成到临时目录并与入库绑定逐字节比较，不一致即失败\n  \
         xcode                       生成 design token 产物并用 XcodeGen 生成 apple/Kanso.xcodeproj\n  \
         apple-build [--channel <direct|mas|setapp>|--all] [--configuration debug|release] [--arch <arm64|x86_64>]\n                                     先运行 xcode，再用 xcodebuild 构建（默认 direct / debug / 本机架构；--all 构建三个渠道；x86_64 的 Release 只支持 direct）\n  \
         apple-test [--only-testing <标识>] [--skip-testing <标识>]...\n                                     先运行 xcode，再在 Direct scheme 下跑 xcodebuild test（KANSO_RECORD_SNAPSHOTS=1 重录参考截图）\n  \
         apple-lib --channel <c> --profile <debug|release> --out <dir> [--arch <arm64|x86_64>]\n                                     构建 libkanso_ffi.a 并复制到 <dir>（Xcode 的 Run Script 调用）\n  \
         xcframework --channel <c>   release 构建并打包 KansoCore.xcframework（仅供发布流水线，Xcode 日常构建不用）"
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

/// 校验手写 C 头文件并运行 C 冒烟测试，防止头文件与 Rust 布局漂移。
fn c_abi() -> bool {
    let has_clang = Command::new("clang")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_clang {
        eprintln!(
            "步骤失败：c-abi（未找到 clang，请安装 Xcode Command Line Tools：xcode-select --install）"
        );
        return false;
    }
    let header = "crates/kanso-ffi/include/kanso_frame.h";
    if !run(
        "c-abi 头文件语法",
        Command::new("clang").args([
            "-fsyntax-only",
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            header,
        ]),
    ) {
        return false;
    }
    if !step("c-abi 构建 kanso-ffi", &["build", "-p", "kanso-ffi"]) {
        return false;
    }
    let lib = "target/debug/libkanso_ffi.a";
    if !Path::new(lib).exists() {
        eprintln!("步骤失败：c-abi（找不到 {lib}；若设置了 CARGO_TARGET_DIR 需要相应调整）");
        return false;
    }
    // 静态库里的 Rust std 在 macOS 上不需要额外 framework；若以后 kanso-ffi 引入
    // 系统库依赖，需要在这里补充对应的 -framework / -l 参数。
    if !run(
        "c-abi 编译冒烟测试",
        Command::new("clang").args([
            "-std=c11",
            "-Wall",
            "-Werror",
            "-I",
            "crates/kanso-ffi/include",
            "crates/kanso-ffi/tests/c/frame_smoke.c",
            lib,
            "-o",
            "target/frame_smoke",
        ]),
    ) {
        return false;
    }
    run(
        "c-abi 运行冒烟测试",
        &mut Command::new("target/frame_smoke"),
    )
}

fn ci(flags: &[String]) -> ExitCode {
    let mut skip_deny = false;
    let mut skip_apple = false;
    for f in flags {
        match f.as_str() {
            "--skip-deny" => skip_deny = true,
            "--skip-apple" => skip_apple = true,
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
        (
            "check-bindings".into(),
            vec!["run", "--quiet", "-p", "xtask", "--", "check-bindings"],
        ),
    ];
    // architecture.md §9 要求 CI 从第一天起就编译 mas 与 setapp 两种配置，
    // 防止平台抽象被默认的 direct 渠道绕过。
    for channel in ["mas", "setapp"] {
        steps.push((
            format!("渠道矩阵 ({channel})"),
            vec![
                "clippy",
                "-p",
                "kanso-ffi",
                "--no-default-features",
                "--features",
                channel,
                "--",
                "-D",
                "warnings",
            ],
        ));
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
        // c-abi 夹在 test 与渠道矩阵之间，因此在 test 步骤后立即执行。
        if name == "test" && !c_abi() {
            return ExitCode::FAILURE;
        }
    }

    // 放在渠道矩阵之后：Rust 侧先过 clippy，再花时间跑 Xcode 构建。
    if skip_apple {
        eprintln!(
            "\n!!!!!!!! 警告：已跳过 Apple 端构建与测试（--skip-apple），Xcode 工程与 Swift 代码未验证 !!!!!!!!"
        );
    } else {
        if !apple::xcodebuild_available() {
            eprintln!(
                "步骤失败：apple-build（未找到 xcodebuild；没有安装 Xcode 时请使用 --skip-apple）"
            );
            return ExitCode::FAILURE;
        }
        if !run(
            "apple-build --all",
            cargo().args([
                "run",
                "--quiet",
                "-p",
                "xtask",
                "--",
                "apple-build",
                "--all",
            ]),
        ) {
            return ExitCode::FAILURE;
        }
        if !run(
            "apple-test",
            cargo().args(["run", "--quiet", "-p", "xtask", "--", "apple-test"]),
        ) {
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
