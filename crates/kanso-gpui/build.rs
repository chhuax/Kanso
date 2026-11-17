//! 仅在目标为 Windows 时，把图标、版本信息与应用清单编进 exe。
//!
//! 不引入额外依赖：用 Windows SDK 的 `rc.exe` 把生成的 `.rc` 编成 `.res`，再经
//! `cargo:rustc-link-arg-bins` 交给链接器（link.exe 与 rust-lld 的 lld-link 都直接接受 `.res`）。
//! macOS / Linux 构建（含 `cargo test`）只会走到最前面的目标判断就返回。
//!
//! 环境变量：
//! - `RC`：指定 rc.exe 的完整路径；
//! - `KANSO_REQUIRE_RESOURCES=1`：找不到 rc.exe 或编译失败时直接让构建失败（`cargo xtask win-package` 会设置），
//!   否则只给出警告并继续（本地开发构建没有图标 / 版本信息也能运行）；
//! - `KANSO_SKIP_MANIFEST=1`：不嵌入应用清单（排查与其他清单冲突时用）。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=windows/kanso.ico");
    println!("cargo:rerun-if-changed=windows/kanso.manifest");
    println!("cargo:rerun-if-env-changed=RC");
    println!("cargo:rerun-if-env-changed=KANSO_REQUIRE_RESOURCES");
    println!("cargo:rerun-if-env-changed=KANSO_SKIP_MANIFEST");

    let required = env::var("KANSO_REQUIRE_RESOURCES").is_ok_and(|v| v == "1");
    match compile_resources() {
        Ok(res) => println!("cargo:rustc-link-arg-bins={}", res.display()),
        Err(e) if required => panic!("嵌入 Windows 资源失败：{e}"),
        Err(e) => println!("cargo:warning=未嵌入 Windows 资源（图标 / 版本信息 / 清单）：{e}"),
    }
}

fn compile_resources() -> Result<PathBuf, String> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let rc_exe = find_rc().ok_or("找不到 rc.exe（需要 Windows SDK；可用环境变量 RC 指定路径）")?;

    let rc_path = out_dir.join("kanso.rc");
    fs::write(&rc_path, render_rc(&manifest_dir)).map_err(|e| format!("写入 .rc 失败：{e}"))?;
    let res_path = out_dir.join("kanso.res");
    let output = Command::new(&rc_exe)
        .args(["/nologo", "/c65001", "/fo"])
        .arg(&res_path)
        .arg(&rc_path)
        .output()
        .map_err(|e| format!("无法运行 {}：{e}", rc_exe.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} 失败：{}{}",
            rc_exe.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(res_path)
}

/// rc 字符串里的反斜杠要双写。
fn rc_string_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "\\\\")
}

fn render_rc(manifest_dir: &Path) -> String {
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let mut nums = version
        .split(['.', '-', '+'])
        .map_while(|part| part.parse::<u16>().ok());
    let (major, minor, patch) = (
        nums.next().unwrap_or(0),
        nums.next().unwrap_or(0),
        nums.next().unwrap_or(0),
    );
    let icon = rc_string_path(&manifest_dir.join("windows/kanso.ico"));
    let manifest = if env::var("KANSO_SKIP_MANIFEST").is_ok_and(|v| v == "1") {
        String::new()
    } else {
        format!(
            "1 24 \"{}\"\n",
            rc_string_path(&manifest_dir.join("windows/kanso.manifest"))
        )
    };
    // 不 #include winver.h：这里直接写数值（VOS_NT_WINDOWS32 = 0x40004，VFT_APP = 1）。
    // 图标 ID 1 是资源管理器与任务栏选用的应用图标（编号最小的图标组）。
    format!(
        r#"#pragma code_page(65001)
1 ICON "{icon}"
{manifest}
1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "chhuax"
      VALUE "FileDescription", "Kanso terminal, SSH and SFTP client"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "Kanso"
      VALUE "LegalCopyright", "Copyright (C) chhuax. GPL-3.0-or-later."
      VALUE "OriginalFilename", "Kanso.exe"
      VALUE "ProductName", "Kanso"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    )
}

/// 依次找：环境变量 `RC`、PATH、Windows Kits 目录（取版本号最高的 x64 rc.exe）。
fn find_rc() -> Option<PathBuf> {
    if let Some(rc) = env::var_os("RC").map(PathBuf::from)
        && rc.is_file()
    {
        return Some(rc);
    }
    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            let candidate = dir.join("rc.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let mut roots = Vec::new();
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(base) = env::var_os(var) {
            roots.push(PathBuf::from(base).join("Windows Kits"));
        }
    }
    roots.push(PathBuf::from(r"C:\Program Files (x86)\Windows Kits"));
    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    for root in roots {
        let Ok(kits) = fs::read_dir(&root) else {
            continue;
        };
        for kit in kits.flatten() {
            let bin = kit.path().join("bin");
            // 旧布局：bin\x64\rc.exe（没有版本号目录）。
            let flat = bin.join("x64").join("rc.exe");
            if flat.is_file() && best.is_none() {
                best = Some((Vec::new(), flat));
            }
            let Ok(versions) = fs::read_dir(&bin) else {
                continue;
            };
            for version in versions.flatten() {
                let name = version.file_name().to_string_lossy().into_owned();
                let parsed: Vec<u32> = name.split('.').map_while(|p| p.parse().ok()).collect();
                let candidate = version.path().join("x64").join("rc.exe");
                if parsed.is_empty() || !candidate.is_file() {
                    continue;
                }
                if best.as_ref().is_none_or(|(v, _)| parsed > *v) {
                    best = Some((parsed, candidate));
                }
            }
        }
    }
    best.map(|(_, path)| path)
}
