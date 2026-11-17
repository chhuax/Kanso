//! macOS 端构建命令：Rust 静态库、XCFramework、Xcode 工程生成与 xcodebuild 封装。
//!
//! 约定：所有命令都以仓库根目录为工作目录；Xcode 的 Run Script 也是先 `cd` 到仓库根再调用，
//! 但这里仍会主动切换，避免从子目录手动运行时相对路径悄悄失效。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::{cargo, run, tokens};

/// 发行渠道，与 cargo feature、Xcode 配置后缀一一对应。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Direct,
    Mas,
    Setapp,
}

impl Channel {
    pub const ALL: [Channel; 3] = [Channel::Direct, Channel::Mas, Channel::Setapp];

    fn parse(s: &str) -> Option<Channel> {
        match s {
            "direct" => Some(Channel::Direct),
            "mas" => Some(Channel::Mas),
            "setapp" => Some(Channel::Setapp),
            _ => None,
        }
    }

    /// cargo feature 名，也是 `KANSO_CHANNEL` 的取值。
    fn feature(self) -> &'static str {
        match self {
            Channel::Direct => "direct",
            Channel::Mas => "mas",
            Channel::Setapp => "setapp",
        }
    }

    /// scheme 与 configuration 名里使用的写法。
    fn display(self) -> &'static str {
        match self {
            Channel::Direct => "Direct",
            Channel::Mas => "MAS",
            Channel::Setapp => "Setapp",
        }
    }
}

/// 目标架构。官网版按架构分别出包，不做 Universal 合并。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    Arm64,
    X86_64,
}

impl Arch {
    fn parse(s: &str) -> Option<Arch> {
        match s {
            "arm64" => Some(Arch::Arm64),
            "x86_64" => Some(Arch::X86_64),
            _ => None,
        }
    }

    /// 本机架构；非 x86_64 一律按 arm64 处理。
    fn native() -> Arch {
        if env::consts::ARCH == "x86_64" {
            Arch::X86_64
        } else {
            Arch::Arm64
        }
    }

    /// Xcode `ARCHS` 与 DMG 文件名里的写法。
    fn name(self) -> &'static str {
        match self {
            Arch::Arm64 => "arm64",
            Arch::X86_64 => "x86_64",
        }
    }

    fn rust_target(self) -> &'static str {
        match self {
            Arch::Arm64 => "aarch64-apple-darwin",
            Arch::X86_64 => "x86_64-apple-darwin",
        }
    }

    /// DerivedData 目录。arm64 保持原路径不变，x86_64 单独一个目录，两者产物互不覆盖。
    /// 目录名都带 `.noindex`，聚焦不收录。
    fn derived_data(self) -> &'static str {
        match self {
            Arch::Arm64 => "target/apple/DerivedData.noindex",
            Arch::X86_64 => "target/apple/DerivedData-x86_64.noindex",
        }
    }
}

fn parse_arch(s: &str) -> Result<Arch, String> {
    Arch::parse(s).ok_or_else(|| format!("未知架构：{s}（可选 arm64 / x86_64）"))
}

/// 解析 Xcode 传入的 `ARCHS`：只接受单个架构，不出 Universal 合并包。
fn parse_archs(s: &str) -> Result<Arch, String> {
    let mut parts = s.split_whitespace();
    let first = parts.next().ok_or("ARCHS 为空")?;
    if parts.next().is_some() {
        return Err(format!(
            "ARCHS=\"{s}\" 含多个架构：Kanso 按架构分别出包，不支持 Universal 合并包，请只指定一个架构"
        ));
    }
    parse_arch(first)
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask 位于仓库根目录之下")
        .to_path_buf()
}

/// 取 `--flag value` 形式参数的值；缺值时返回 Err。
fn take_value<'a>(it: &mut std::slice::Iter<'a, String>, flag: &str) -> Result<&'a str, String> {
    it.next()
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} 需要一个参数"))
}

fn parse_channel(s: &str) -> Result<Channel, String> {
    Channel::parse(s).ok_or_else(|| format!("未知渠道：{s}（可选 direct / mas / setapp）"))
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::FAILURE
}

/// 渠道独立的 cargo target 目录。
///
/// 三个渠道的 feature 不同，共用一个 target 目录时每次切换 scheme 都会让 cargo 重新编译
/// kanso-core 等依赖并互相覆盖缓存；分开后切换 scheme 只做增量。
fn channel_target_dir(channel: Channel) -> PathBuf {
    PathBuf::from("target/apple").join(channel.feature())
}

/// 构建指定渠道、profile 与架构的 `libkanso_ffi.a`，返回其路径（相对仓库根）。
fn build_static_lib(channel: Channel, release: bool, arch: Arch) -> Result<PathBuf, String> {
    let target_dir = channel_target_dir(channel);
    let mut cmd = cargo();
    cmd.args([
        "build",
        "-p",
        "kanso-ffi",
        "--target",
        arch.rust_target(),
        "--no-default-features",
        "--features",
        channel.feature(),
        "--target-dir",
    ])
    .arg(&target_dir);
    if release {
        cmd.arg("--release");
    }
    if !run("构建 kanso-ffi 静态库", &mut cmd) {
        return Err("kanso-ffi 构建失败".into());
    }
    let profile = if release { "release" } else { "debug" };
    let lib = target_dir
        .join(arch.rust_target())
        .join(profile)
        .join("libkanso_ffi.a");
    if lib.exists() {
        Ok(lib)
    } else {
        Err(format!("找不到构建产物 {}", lib.display()))
    }
}

/// `cargo xtask apple-lib`：供 Xcode 的 Run Script 调用。
pub fn apple_lib(args: &[String]) -> ExitCode {
    let (mut channel, mut profile, mut out, mut arch) = (None, None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let r = match a.as_str() {
            "--channel" => take_value(&mut it, a)
                .and_then(parse_channel)
                .map(|c| channel = Some(c)),
            "--profile" => take_value(&mut it, a).map(|p| profile = Some(p.to_string())),
            "--out" => take_value(&mut it, a).map(|p| out = Some(PathBuf::from(p))),
            // Run Script 传 "$ARCHS"；含多个架构时报错。
            "--arch" => take_value(&mut it, a)
                .and_then(parse_archs)
                .map(|x| arch = Some(x)),
            other => Err(format!("未知参数：{other}")),
        };
        if let Err(e) = r {
            return fail(&e);
        }
    }
    let arch = arch.unwrap_or_else(Arch::native);
    let (Some(channel), Some(profile), Some(out)) = (channel, profile, out) else {
        return fail(
            "用法：cargo xtask apple-lib --channel <direct|mas|setapp> --profile <debug|release> --out <dir> [--arch <arm64|x86_64>]",
        );
    };
    let release = match profile.as_str() {
        "release" => true,
        "debug" => false,
        other => return fail(&format!("未知 profile：{other}（可选 debug / release）")),
    };

    // 先把 --out 转成绝对路径再切换目录，保证相对路径按调用方的工作目录解释。
    let out = match std::path::absolute(&out) {
        Ok(p) => p,
        Err(e) => return fail(&format!("无法解析 --out：{e}")),
    };
    if let Err(e) = env::set_current_dir(repo_root()) {
        return fail(&format!("无法切换到仓库根目录：{e}"));
    }

    let lib = match build_static_lib(channel, release, arch) {
        Ok(l) => l,
        Err(e) => return fail(&e),
    };
    match copy_if_changed(&lib, &out.join("libkanso_ffi.a")) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&format!("复制静态库失败：{e}")),
    }
}

/// 内容相同则不覆盖：覆盖会更新修改时间，导致 Xcode 每次都重新链接。
fn copy_if_changed(src: &Path, dst: &Path) -> std::io::Result<()> {
    if let (Ok(a), Ok(b)) = (fs::read(src), fs::read(dst))
        && a == b
    {
        return Ok(());
    }
    if let Some(dir) = dst.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::copy(src, dst).map(|_| ())
}

/// `cargo xtask xcframework`：只给发布流水线（T12）用，Xcode 日常构建走 `apple-lib`。
///
/// 始终用 release 构建（仅 arm64），产物为 `target/apple/<channel>/KansoCore.xcframework`。
pub fn xcframework(args: &[String]) -> ExitCode {
    let mut channel = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let r = match a.as_str() {
            "--channel" => take_value(&mut it, a)
                .and_then(parse_channel)
                .map(|c| channel = Some(c)),
            other => Err(format!("未知参数：{other}")),
        };
        if let Err(e) = r {
            return fail(&e);
        }
    }
    let Some(channel) = channel else {
        return fail("用法：cargo xtask xcframework --channel <direct|mas|setapp>");
    };
    if let Err(e) = env::set_current_dir(repo_root()) {
        return fail(&format!("无法切换到仓库根目录：{e}"));
    }
    let lib = match build_static_lib(channel, true, Arch::Arm64) {
        Ok(l) => l,
        Err(e) => return fail(&e),
    };
    let output = channel_target_dir(channel).join("KansoCore.xcframework");
    // xcodebuild 不会覆盖已存在的 xcframework，必须先删。
    if output.exists()
        && let Err(e) = fs::remove_dir_all(&output)
    {
        return fail(&format!("无法删除旧的 xcframework：{e}"));
    }
    let ok = run(
        "创建 xcframework",
        Command::new("xcodebuild")
            .args(["-create-xcframework", "-library"])
            .arg(&lib)
            .arg("-headers")
            .arg("crates/kanso-ffi/include")
            .arg("-output")
            .arg(&output),
    );
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// 工具是否可用。xcodebuild 不认识 `--version`（只有 `-version`），所以版本参数由调用方给。
fn has_tool(name: &str, version_arg: &str) -> bool {
    Command::new(name)
        .arg(version_arg)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `cargo xtask xcode`：生成 design token 的 Swift 产物，再用 XcodeGen 生成工程。
pub fn xcode() -> ExitCode {
    if let Err(e) = env::set_current_dir(repo_root()) {
        return fail(&format!("无法切换到仓库根目录：{e}"));
    }
    if !has_tool("xcodegen", "--version") {
        return fail("步骤失败：xcode（未找到 xcodegen，请运行 `brew install xcodegen`）");
    }
    // 必须先于 xcodegen：工程会把 Generated 目录里的文件加入 App target。
    if tokens::generate(&["--out".into(), "apple/Kanso/Generated".into()]) != ExitCode::SUCCESS {
        return ExitCode::FAILURE;
    }
    let ok = run(
        "xcodegen",
        Command::new("xcodegen").args(["generate", "--spec", "apple/project.yml"]),
    );
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// 把构建产物里的 App 从 LaunchServices 注销。
///
/// xcodebuild 每次构建都会把产物注册到 LaunchServices，久了"打开方式"、启动台和聚焦里会出现一堆同名 Kanso；
/// `.noindex` 只挡得住聚焦的文件索引，挡不住这一步注册。构建、测试结束后统一注销，失败不影响结果。
fn unregister_build_products(arch: Arch) {
    const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
    let Ok(configs) = std::fs::read_dir(Path::new(arch.derived_data()).join("Build/Products"))
    else {
        return;
    };
    for config in configs.flatten() {
        let Ok(items) = std::fs::read_dir(config.path()) else {
            continue;
        };
        for item in items.flatten() {
            let path = item.path();
            if path.extension().is_some_and(|e| e == "app") {
                let _ = Command::new(LSREGISTER).arg("-u").arg(&path).status();
            }
        }
    }
}

/// 构建单个渠道的 App；`release` 决定 configuration。
fn xcodebuild(channel: Channel, release: bool, arch: Arch) -> bool {
    let kind = if release { "Release" } else { "Debug" };
    let name = channel.display();
    run(
        &format!("xcodebuild {name} ({kind})"),
        Command::new("xcodebuild")
            .args(["-project", "apple/Kanso.xcodeproj"])
            .args(["-scheme", &format!("Kanso-{name}")])
            .args(["-configuration", &format!("{kind}-{name}")])
            // .noindex 后缀让聚焦不收录构建产物，避免搜索 Kanso 时出现多个同名 App。
            .args(["-derivedDataPath", arch.derived_data()])
            // 覆盖工程里的 ARCHS，保证产物就是请求的架构。
            .arg(format!("ARCHS={}", arch.name()))
            .arg("build"),
    )
}

/// `cargo xtask apple-build [--arch arm64|x86_64]`：先 `xcode` 再 xcodebuild，架构默认取本机。
///
/// x86_64 只支持 Direct 渠道的 Release（MAS / Setapp 的 Release 只出 arm64），Debug 三个渠道都可以。
pub fn apple_build(args: &[String]) -> ExitCode {
    let mut channels = vec![Channel::Direct];
    let mut release = false;
    let mut arch = Arch::native();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let r = match a.as_str() {
            "--arch" => take_value(&mut it, a)
                .and_then(parse_arch)
                .map(|x| arch = x),
            "--all" => {
                channels = Channel::ALL.to_vec();
                Ok(())
            }
            "--channel" => take_value(&mut it, a)
                .and_then(parse_channel)
                .map(|c| channels = vec![c]),
            "--configuration" => take_value(&mut it, a).and_then(|c| {
                release = match c {
                    "debug" => false,
                    "release" => true,
                    other => {
                        return Err(format!(
                            "未知 configuration：{other}（可选 debug / release）"
                        ));
                    }
                };
                Ok(())
            }),
            other => Err(format!("未知参数：{other}")),
        };
        if let Err(e) = r {
            return fail(&e);
        }
    }
    if release && arch == Arch::X86_64 && channels.iter().any(|&c| c != Channel::Direct) {
        return fail("x86_64 的 Release 只支持 direct 渠道（MAS / Setapp 只出 arm64）");
    }
    if !has_tool("xcodebuild", "-version") {
        return fail("步骤失败：apple-build（未找到 xcodebuild，请安装 Xcode）");
    }
    if xcode() != ExitCode::SUCCESS {
        return ExitCode::FAILURE;
    }
    let ok = channels.into_iter().all(|c| xcodebuild(c, release, arch));
    unregister_build_products(arch);
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// `xcodebuild test` 的运行目标。编译出的架构由 `ARCHS` 决定，运行目标只决定"在哪台机器上跑"。
///
/// Apple 芯片上**不能**写 `arch=arm64`：Xcode 27.2 在 M5 Max 上执行 `test` 动作时，本机只作为
/// `arch:arm64e` 目标出现（不带动作的 `-showdestinations` 却同时列出 arm64 与 arm64e），
/// 按 `arch=arm64` 过滤会找不到目标、测试直接失败。`arm64` 的测试包在 `arm64e` 目标上照常运行，
/// 所以只写 `platform=macOS`，让 xcodebuild 取本机原生的那个目标；旧版 Xcode 仍列 `arch:arm64`，同样匹配。
/// Intel 机器上保留 `arch=x86_64`，原生目标只有这一个，写明更清楚。
fn test_destination(arch: Arch) -> &'static str {
    match arch {
        Arch::Arm64 => "platform=macOS",
        Arch::X86_64 => "platform=macOS,arch=x86_64",
    }
}

/// `cargo xtask apple-test [--only-testing <标识>]`：先 `xcode`，再在 Direct scheme 下跑 `xcodebuild test`
/// （单元测试与截图测试，Debug 配置）。
///
/// 不提供 release 配置：Release 的 Hardened Runtime 会拒绝加载自签名的测试包（Team ID 不一致）。
/// 环境变量 `KANSO_RECORD_SNAPSHOTS` 会以 `TEST_RUNNER_` 前缀转发：xcodebuild 只把带这个前缀的变量
/// 传给测试进程，不转发的话录制开关到不了测试里。
pub fn apple_test(args: &[String]) -> ExitCode {
    let mut only: Option<String> = None;
    let arch = Arch::native();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let r = match a.as_str() {
            "--only-testing" => take_value(&mut it, a).map(|v| only = Some(v.to_string())),
            other => Err(format!("未知参数：{other}")),
        };
        if let Err(e) = r {
            return fail(&e);
        }
    }
    if !has_tool("xcodebuild", "-version") {
        return fail("步骤失败：apple-test（未找到 xcodebuild，请安装 Xcode）");
    }
    if xcode() != ExitCode::SUCCESS {
        return ExitCode::FAILURE;
    }
    let mut cmd = Command::new("xcodebuild");
    cmd.args(["-project", "apple/Kanso.xcodeproj"])
        .args(["-scheme", "Kanso-Direct"])
        .args(["-configuration", "Debug-Direct"])
        .args(["-derivedDataPath", arch.derived_data()])
        .args(["-destination", test_destination(arch)])
        .arg(format!("ARCHS={}", arch.name()));
    if let Some(o) = &only {
        cmd.arg(format!("-only-testing:{o}"));
    }
    cmd.arg("test");
    if let Ok(v) = env::var("KANSO_RECORD_SNAPSHOTS") {
        cmd.env("TEST_RUNNER_KANSO_RECORD_SNAPSHOTS", v);
    }
    let ok = run("xcodebuild test (Direct)", &mut cmd);
    unregister_build_products(arch);
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// `ci` 使用：xcodebuild 是否可用。
pub fn xcodebuild_available() -> bool {
    has_tool("xcodebuild", "-version")
}

/// `cargo xtask dmg [--arch arm64|x86_64]`：默认 arm64，与原流程一致；Intel 包需显式 `--arch x86_64`。
pub fn dmg(args: &[String]) -> ExitCode {
    let mut arch = Arch::Arm64;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let r = match a.as_str() {
            "--arch" => take_value(&mut it, a)
                .and_then(parse_arch)
                .map(|x| arch = x),
            other => Err(format!("未知参数：{other}")),
        };
        if let Err(e) = r {
            return fail(&e);
        }
    }
    if apple_build(&[
        "--channel".into(),
        "direct".into(),
        "--configuration".into(),
        "release".into(),
        "--arch".into(),
        arch.name().into(),
    ]) != ExitCode::SUCCESS
    {
        return ExitCode::FAILURE;
    }
    match disk_image(arch) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(&format!("生成 DMG 失败：{error}")),
    }
}

fn disk_image(arch: Arch) -> Result<(), Box<dyn std::error::Error>> {
    // arm64 沿用原目录名；x86_64 另用一个，避免互相冲突。
    let contents_name = match arch {
        Arch::Arm64 => "dmg-contents",
        Arch::X86_64 => "dmg-contents-x86_64",
    };
    let contents = repo_root().join("target/dist").join(contents_name);
    if contents.exists() {
        fs::remove_dir_all(&contents)?;
    }
    fs::create_dir_all(&contents)?;
    let app = Path::new(arch.derived_data()).join("Build/Products/Release-Direct/Kanso.app");
    if !run(
        "复制 Kanso.app",
        Command::new("ditto")
            .arg(&app)
            .arg(contents.join("Kanso.app")),
    ) {
        return Err("复制 App 失败".into());
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink("/Applications", contents.join("Applications"))?;
    for (source, destination) in [
        ("LICENSE", "LICENSE.txt"),
        ("THIRD_PARTY_NOTICES.md", "THIRD_PARTY_NOTICES.md"),
    ] {
        fs::copy(source, contents.join(destination))?;
    }
    let image = repo_root().join(format!(
        "target/dist/Kanso-{}-macOS-{}.dmg",
        env!("CARGO_PKG_VERSION"),
        arch.name()
    ));
    if !run(
        "创建 DMG",
        Command::new("hdiutil")
            .args([
                "create",
                "-volname",
                "Kanso",
                "-format",
                "UDZO",
                "-srcfolder",
            ])
            .arg(&contents)
            .arg("-ov")
            .arg(&image),
    ) {
        return Err("hdiutil 失败".into());
    }
    if !run(
        "DMG SHA-256",
        Command::new("shasum").args(["-a", "256"]).arg(&image),
    ) {
        return Err("计算 SHA-256 失败".into());
    }
    fs::remove_dir_all(contents)?;
    Ok(())
}
