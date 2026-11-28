//! macOS 端 GPUI 应用打包与 DMG 生成。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::{cargo, run};

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

    /// DMG 文件名里的写法。
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
}

fn parse_arch(s: &str) -> Result<Arch, String> {
    Arch::parse(s).ok_or_else(|| format!("未知架构：{s}（可选 arm64 / x86_64）"))
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

fn fail(msg: &str) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::FAILURE
}

/// `cargo xtask dmg [--arch arm64|x86_64]`：Release 构建 GPUI 版 `Kanso.app`（含 `kanso-mcp`）并生成 macOS DMG。
pub fn dmg(args: &[String]) -> ExitCode {
    if !cfg!(target_os = "macos") {
        return fail("dmg 只在 macOS 上运行，需要 macOS 的 codesign / iconutil / hdiutil。");
    }
    let mut arch = Arch::native();
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
    if let Err(error) = env::set_current_dir(repo_root()) {
        return fail(&format!("切换工作目录失败：{error}"));
    }
    let (app_bin, mcp_bin) = match build_gpui_binaries(arch) {
        Ok(bins) => bins,
        Err(error) => return fail(&error),
    };
    match disk_image(arch, &app_bin, &mcp_bin) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(&format!("生成 DMG 失败：{error}")),
    }
}

fn build_gpui_binaries(arch: Arch) -> Result<(PathBuf, PathBuf), String> {
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target"));
    let triple = arch.rust_target();

    let mut gpui_cmd = cargo();
    gpui_cmd
        .args([
            "build",
            "--release",
            "-p",
            "kanso-gpui",
            "--features",
            "direct",
            "--locked",
            "--target",
            triple,
            "--target-dir",
        ])
        .arg(&target_dir);
    if !run("Release 构建 kanso-gpui", &mut gpui_cmd) {
        return Err("kanso-gpui Release 构建失败".into());
    }

    let mut mcp_cmd = cargo();
    mcp_cmd
        .args([
            "build",
            "--release",
            "-p",
            "kanso-mcp",
            "--locked",
            "--target",
            triple,
            "--target-dir",
        ])
        .arg(&target_dir);
    if !run("Release 构建 kanso-mcp", &mut mcp_cmd) {
        return Err("kanso-mcp Release 构建失败".into());
    }

    let release_dir = target_dir.join(triple).join("release");
    let app_bin = release_dir.join("kanso-gpui");
    let mcp_bin = release_dir.join("kanso-mcp");
    if !app_bin.exists() {
        return Err(format!("找不到构建产物 {}", app_bin.display()));
    }
    if !mcp_bin.exists() {
        return Err(format!("找不到构建产物 {}", mcp_bin.display()));
    }
    Ok((app_bin, mcp_bin))
}

fn assemble_gpui_app(
    app_dir: &Path,
    app_bin: &Path,
    mcp_bin: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let macos_dir = app_dir.join("Contents/MacOS");
    let res_dir = app_dir.join("Contents/Resources");
    fs::create_dir_all(&macos_dir)?;
    fs::create_dir_all(&res_dir)?;

    let installed_app = macos_dir.join("Kanso");
    let installed_mcp = macos_dir.join("kanso-mcp");
    fs::copy(app_bin, &installed_app)?;
    fs::copy(mcp_bin, &installed_mcp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for bin in [&installed_app, &installed_mcp] {
            let mut perms = fs::metadata(bin)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(bin, perms)?;
        }
    }

    let iconset = repo_root().join("packaging/macos/AppIcon.iconset");
    let icns = res_dir.join("AppIcon.icns");
    if !run(
        "生成 AppIcon.icns",
        Command::new("iconutil")
            .args(["-c", "icns"])
            .arg(&iconset)
            .arg("-o")
            .arg(&icns),
    ) {
        return Err("iconutil 生成图标失败".into());
    }

    let version = env!("CARGO_PKG_VERSION");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleDisplayName</key>
	<string>Kanso</string>
	<key>CFBundleExecutable</key>
	<string>Kanso</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
	<key>CFBundleIdentifier</key>
	<string>com.chhuax.kanso</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>Kanso</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.developer-tools</string>
	<key>LSMinimumSystemVersion</key>
	<string>14.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSPrincipalClass</key>
	<string>NSApplication</string>
</dict>
</plist>
"#
    );
    fs::write(app_dir.join("Contents/Info.plist"), plist)?;

    if !run(
        "临时签名 kanso-mcp",
        Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(&installed_mcp),
    ) {
        return Err("codesign kanso-mcp 失败".into());
    }
    if !run(
        "临时签名 Kanso.app",
        Command::new("codesign")
            .args(["--force", "--deep", "--sign", "-"])
            .arg(app_dir),
    ) {
        return Err("codesign Kanso.app 失败".into());
    }
    Ok(())
}

fn disk_image(
    arch: Arch,
    app_bin: &Path,
    mcp_bin: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
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
    assemble_gpui_app(&contents.join("Kanso.app"), app_bin, mcp_bin)?;
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
                "-imagekey",
                "zlib-level=9",
                "-srcfolder",
            ])
            .arg(&contents)
            .arg("-ov")
            .arg(&image),
    ) {
        return Err("hdiutil 失败".into());
    }
    eprintln!("DMG 大小：{} 字节", fs::metadata(&image)?.len());
    if !run(
        "DMG SHA-256",
        Command::new("shasum").args(["-a", "256"]).arg(&image),
    ) {
        return Err("计算 SHA-256 失败".into());
    }
    fs::remove_dir_all(contents)?;
    Ok(())
}
