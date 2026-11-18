//! Windows 发行包：`cargo xtask win-package`。
//!
//! 发行入口是 Windows x64 的 `kanso-gpui.exe`。暂存 `Kanso.exe`、`kanso-mcp.exe`、GPL 许可、
//! Windows 第三方声明，再用同一份暂存内容制作 ZIP 和 NSIS 安装程序，为每个产物写 `.sha256`。
//! 只支持在 Windows 上运行；详见 `docs/windows-release.md`。
//!
//! 签名钩子：环境变量 `KANSO_SIGN_CMD`（形如 `signtool sign /fd sha256 /a "%1"`，`%1` 换成待签名文件）。
//! 本版发布不签名，保持不设置即可；设置后会对两个暂存的可执行文件与安装程序各执行一次。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::{cargo, run};

const TARGET: &str = "x86_64-pc-windows-msvc";
const STATIC_CRT: &str = "-Ctarget-feature=+crt-static";

struct Options {
    skip_build: bool,
    no_installer: bool,
    offline: bool,
}

pub fn win_package(args: &[String]) -> ExitCode {
    if !cfg!(windows) {
        eprintln!(
            "win-package 只支持在 Windows 上运行（需要 MSVC 工具链、Windows SDK 的 rc.exe，可选 NSIS）；当前不是 Windows 主机。"
        );
        return ExitCode::FAILURE;
    }
    let mut options = Options {
        skip_build: false,
        no_installer: false,
        offline: false,
    };
    for arg in args {
        match arg.as_str() {
            "--skip-build" => options.skip_build = true,
            "--no-installer" => options.no_installer = true,
            "--offline" => options.offline = true,
            other => {
                eprintln!("未知参数：{other}（可用：--skip-build、--no-installer、--offline）");
                return ExitCode::FAILURE;
            }
        }
    }
    match package(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("win-package 失败：{error}");
            ExitCode::FAILURE
        }
    }
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn package(options: &Options) -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    env::set_current_dir(root)?;
    let version = env!("CARGO_PKG_VERSION");
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let release_dir = target_dir.join(TARGET).join("release");
    let exe = release_dir.join("kanso-gpui.exe");
    let helper = release_dir.join("kanso-mcp.exe");

    if options.skip_build {
        eprintln!(
            "--skip-build：使用 {} 中已有的可执行文件",
            release_dir.display()
        );
    } else {
        let mut build = cargo();
        build.args([
            "build",
            "--release",
            "--locked",
            "-p",
            "kanso-gpui",
            "-p",
            "kanso-mcp",
            "--target",
            TARGET,
        ]);
        if options.offline {
            build.arg("--offline");
        }
        enable_static_crt(&mut build, options.offline)?;
        // 找不到 rc.exe 时让构建失败，而不是悄悄发一个没有图标和版本信息的 exe。
        build.env("KANSO_REQUIRE_RESOURCES", "1");
        if !run("构建 Windows 前端与 MCP 转发程序（release）", &mut build) {
            return Err("cargo build 失败".into());
        }
    }
    for path in [&exe, &helper] {
        if !path.is_file() {
            return Err(format!("找不到 {}", path.display()).into());
        }
    }

    // 每次从空目录暂存 Windows 可执行文件及其许可，ZIP 与 NSIS 使用同一份内容。
    let out = target_dir.join("win-package");
    // 与 macOS 的 `Kanso-<版本>-macOS-<架构>` 同一规则：系统名放在版本和架构之间。
    let name = format!("Kanso-{version}-Windows-x64");
    let stage = out.join(&name);
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
    }
    fs::create_dir_all(&stage)?;
    fs::copy(&exe, stage.join("Kanso.exe"))?;
    fs::copy(&helper, stage.join("kanso-mcp.exe"))?;
    for (source, destination) in [
        ("LICENSE", "LICENSE.txt"),
        (
            "THIRD_PARTY_NOTICES.windows.md",
            "THIRD_PARTY_NOTICES.windows.md",
        ),
    ] {
        fs::copy(root.join(source), stage.join(destination)).map_err(|e| {
            format!("复制 {source} 失败：{e}（缺少 Windows 声明时先运行 cargo xtask notices）")
        })?;
    }
    let sign = env::var("KANSO_SIGN_CMD")
        .ok()
        .filter(|s| !s.trim().is_empty());
    if let Some(template) = &sign {
        sign_file(template, &stage.join("Kanso.exe"))?;
        sign_file(template, &stage.join("kanso-mcp.exe"))?;
    }

    // 绿色版 zip（内含同名顶层目录）。
    let zip = out.join(format!("{name}-portable.zip"));
    make_zip(&out, &name, &zip)?;
    write_sha256(&zip)?;
    let mut artifacts = vec![zip];

    // 安装程序。
    if options.no_installer {
        eprintln!("--no-installer：跳过安装程序");
    } else if let Some(makensis) = find_makensis() {
        let setup = out.join(format!("{name}-setup.exe"));
        make_installer(&makensis, root, &stage, &setup, version, sign.as_deref())?;
        if let Some(template) = &sign {
            // 安装程序已由 NSIS 的 !finalize 签过；这里不重复签名，仅提醒校验。
            eprintln!(
                "已通过 NSIS !finalize 签名（KANSO_SIGN_CMD：{template}），请用 signtool verify 校验。"
            );
        }
        write_sha256(&setup)?;
        artifacts.push(setup);
    } else {
        eprintln!(
            "\n!!!! 未找到 makensis，只生成了绿色版 zip，没有安装程序。\n     安装 NSIS 3.x（https://nsis.sourceforge.io/）后重试；也可设置环境变量 MAKENSIS 指向 makensis.exe，或用 --no-installer 隐藏此提示。"
        );
    }

    eprintln!("\n产物（{}）：", out.display());
    for artifact in &artifacts {
        eprintln!(
            "  {}（{} 字节）",
            artifact.display(),
            fs::metadata(artifact)?.len()
        );
        eprintln!("  {}.sha256", artifact.display());
    }
    Ok(())
}

/// 显式指定了 --target，Cargo 只把这些标志传给目标及其依赖，不影响宿主工具。
fn enable_static_crt(build: &mut Command, offline: bool) -> Result<()> {
    for (name, separator) in [("CARGO_ENCODED_RUSTFLAGS", "\x1f"), ("RUSTFLAGS", " ")] {
        if let Some(mut flags) = env::var_os(name) {
            if !flags.is_empty() {
                flags.push(separator);
            }
            flags.push(STATIC_CRT);
            build.env(name, flags);
            return Ok(());
        }
    }

    // Cargo 的 target rustflags 优先于 build.rustflags。用无普通依赖的工作区库查询实际生效层，
    // 再在同一层追加设置，同时保留开发者在 .cargo/config.toml 里配置的链接选项。
    let global = format!("build.rustflags=['{STATIC_CRT}']");
    let mut probe = cargo();
    probe.args([
        "rustc",
        "-p",
        "kanso-bench",
        "--lib",
        "--release",
        "--locked",
        "--target",
        TARGET,
    ]);
    if offline {
        probe.arg("--offline");
    }
    let result = probe
        .args(["--config", &global, "--", "--print=cfg"])
        .output()?;
    if !result.status.success() {
        return Err(format!(
            "检查 Windows 目标编译参数失败：{}",
            String::from_utf8_lossy(&result.stderr)
        )
        .into());
    }
    let static_crt = String::from_utf8_lossy(&result.stdout)
        .lines()
        .any(|line| line == "target_feature=\"crt-static\"");
    let config = if static_crt {
        global
    } else {
        format!("target.{TARGET}.rustflags=['{STATIC_CRT}']")
    };
    build.args(["--config", &config]);
    Ok(())
}

/// 用最高 Deflate 压缩生成资源管理器能直接解压的 ZIP；旧工具退回 PowerShell。
fn make_zip(out: &Path, folder: &str, zip: &Path) -> Result<()> {
    if zip.exists() {
        fs::remove_file(zip)?;
    }
    let tar_ok = run(
        "打包 zip（tar.exe）",
        Command::new("tar.exe")
            .args([
                "-a",
                "--options",
                "zip:compression=deflate,zip:compression-level=9",
                "-c",
                "-f",
            ])
            .arg(zip)
            .arg("-C")
            .arg(out)
            .arg(folder),
    );
    if tar_ok && zip.is_file() {
        return Ok(());
    }
    let _ = fs::remove_file(zip);
    let script = format!(
        "Compress-Archive -Path '{}' -DestinationPath '{}' -CompressionLevel Optimal -Force",
        out.join(folder).display(),
        zip.display()
    );
    if run(
        "打包 zip（Compress-Archive）",
        Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &script]),
    ) && zip.is_file()
    {
        return Ok(());
    }
    Err("生成 zip 失败（tar.exe 与 Compress-Archive 都不可用）".into())
}

fn make_installer(
    makensis: &Path,
    root: &Path,
    stage: &Path,
    setup: &Path,
    version: &str,
    sign: Option<&str>,
) -> Result<()> {
    if setup.exists() {
        fs::remove_file(setup)?;
    }
    let mut command = Command::new(makensis);
    command
        .arg("/V2")
        .arg(format!("/DVERSION={version}"))
        .arg(format!("/DVERSION_NUMERIC={}", numeric_version(version)))
        .arg(format!("/DSTAGE_DIR={}", stage.display()))
        .arg(format!("/DOUT_FILE={}", setup.display()))
        .arg(format!(
            "/DINSTALLER_ICON={}",
            root.join("crates/kanso-gpui/windows/installer.ico")
                .display()
        ));
    if let Some(template) = sign {
        command.arg(format!("/DSIGN_CMD={template}"));
    }
    command.arg(root.join("packaging/windows/kanso.nsi"));
    if !run("编译安装程序（makensis）", &mut command) || !setup.is_file() {
        return Err("makensis 失败".into());
    }
    Ok(())
}

/// `1.0.2` / `1.0.2-beta.1` → `1.0.2.0`（VIProductVersion 必须是四段数字）。
fn numeric_version(version: &str) -> String {
    let mut parts: Vec<u32> = version
        .split(['.', '-', '+'])
        .map_while(|part| part.parse().ok())
        .take(4)
        .collect();
    parts.resize(4, 0);
    parts
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

/// 依次找：环境变量 `MAKENSIS`、PATH、常见安装目录。
fn find_makensis() -> Option<PathBuf> {
    if let Some(path) = env::var_os("MAKENSIS").map(PathBuf::from)
        && path.is_file()
    {
        return Some(path);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = env::var_os("PATH") {
        candidates.extend(env::split_paths(&path).map(|dir| dir.join("makensis.exe")));
    }
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(base) = env::var_os(var) {
            candidates.push(PathBuf::from(base).join("NSIS").join("makensis.exe"));
        }
    }
    if let Some(base) = env::var_os("LOCALAPPDATA") {
        candidates.push(
            PathBuf::from(base)
                .join("Programs")
                .join("NSIS")
                .join("makensis.exe"),
        );
    }
    candidates.into_iter().find(|path| path.is_file())
}

/// 用 `certutil -hashfile` 算 SHA-256，写成 `<文件>.sha256`（`<哈希>  <文件名>`，与 `shasum -c` 兼容）。
fn write_sha256(file: &Path) -> Result<()> {
    let output = Command::new("certutil")
        .arg("-hashfile")
        .arg(file)
        .arg("SHA256")
        .output()?;
    if !output.status.success() {
        return Err(format!("certutil 失败：{}", String::from_utf8_lossy(&output.stdout)).into());
    }
    let hash = parse_certutil_hash(&String::from_utf8_lossy(&output.stdout))
        .ok_or("无法解析 certutil 的输出")?;
    let name = file.file_name().unwrap().to_string_lossy();
    let line = format!("{hash}  {name}\n");
    let mut checksum = file.as_os_str().to_owned();
    checksum.push(".sha256");
    fs::write(&checksum, &line)?;
    eprint!("SHA-256 {line}");
    Ok(())
}

/// certutil 输出第二行是（可能带空格分隔的）十六进制哈希。
fn parse_certutil_hash(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let compact: String = line.split_whitespace().collect();
        (compact.len() == 64 && compact.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| compact.to_ascii_lowercase())
    })
}

/// 把签名命令模板里的 `%1` 换成文件路径，交给 `cmd /C` 执行。
fn sign_file(template: &str, file: &Path) -> Result<()> {
    let command_line = template.replace("%1", &file.display().to_string());
    eprintln!("\n==> 签名：{command_line}");
    let mut command = Command::new("cmd.exe");
    command.args(["/D", "/S", "/C"]);
    raw_command_line(&mut command, &format!("\"{command_line}\""));
    match command.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("签名命令失败（{status}）").into()),
        Err(error) => Err(format!("无法运行签名命令：{error}").into()),
    }
}

#[cfg(windows)]
fn raw_command_line(command: &mut Command, line: &str) {
    use std::os::windows::process::CommandExt;
    command.raw_arg(line);
}

#[cfg(not(windows))]
fn raw_command_line(command: &mut Command, line: &str) {
    command.arg(line);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_version_pads_and_truncates() {
        assert_eq!(numeric_version("1.0.2"), "1.0.2.0");
        assert_eq!(numeric_version("1.0.2-beta.1"), "1.0.2.0");
        assert_eq!(numeric_version("2.10"), "2.10.0.0");
    }

    #[test]
    fn certutil_output_is_parsed() {
        let text = "SHA256 hash of x.zip:\r\n\
                    ab cd ef 01 23 45 67 89 ab cd ef 01 23 45 67 89 ab cd ef 01 23 45 67 89 ab cd ef 01 23 45 67 89\r\n\
                    CertUtil: -hashfile command completed successfully.\r\n";
        assert_eq!(
            parse_certutil_hash(text).unwrap(),
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        );
        let compact = format!("hash:\n{}\nok", "AB".repeat(32));
        assert_eq!(parse_certutil_hash(&compact).unwrap(), "ab".repeat(32));
        assert!(parse_certutil_hash("CertUtil: failed").is_none());
    }
}
