//! UniFFI Swift 绑定的生成（`bindgen`）与一致性校验（`check-bindings`）。
//!
//! 生成物入库（与终端配色 `TerminalThemes.swift` 同一策略）：SwiftPM 包要能被单独的
//! `swift build` 编译，不能依赖先跑 xtask。代价是改了 `kanso-ffi` 的导出接口却忘记重新生成，
//! 所以 `check-bindings` 重新生成到临时目录并与入库版本逐字节比较，不一致就失败，已加入 `ci`。
//!
//! 采用 library mode：从**已编译**的 `libkanso_ffi.a` 里读取 proc-macro 写入的接口元数据，
//! 不需要 UDL。生成物与具体渠道无关（导出接口不随 feature 变化），所以固定用默认 feature 的
//! host debug 构建。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::{cargo, run};

/// bindgen 输出的文件名与它们在仓库里的落点：(生成文件名, 入库相对路径)。
///
/// modulemap 必须叫 `module.modulemap`，SwiftPM 才会把 include 目录当作 C 模块的自定义映射。
const OUTPUTS: [(&str, &str); 3] = [
    (
        "KansoFFI.swift",
        "apple/Packages/KansoBridge/Sources/KansoBridge/Generated/KansoFFI.swift",
    ),
    (
        "KansoFFIC.h",
        "apple/Packages/KansoBridge/Sources/KansoFFIC/include/KansoFFIC.h",
    ),
    (
        "KansoFFIC.modulemap",
        "apple/Packages/KansoBridge/Sources/KansoFFIC/include/module.modulemap",
    ),
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask 位于仓库根目录之下")
        .to_path_buf()
}

/// 构建 `kanso-ffi` 并把绑定生成到 `out_dir`（先清空）。
fn generate_into(root: &Path, out_dir: &Path) -> Result<(), String> {
    if !run(
        "构建 kanso-ffi",
        cargo().current_dir(root).args(["build", "-p", "kanso-ffi"]),
    ) {
        return Err("kanso-ffi 构建失败".into());
    }
    let lib = root.join("target/debug/libkanso_ffi.a");
    if !lib.exists() {
        return Err(format!(
            "找不到 {}（若设置了 CARGO_TARGET_DIR 需要相应调整）",
            lib.display()
        ));
    }
    if out_dir.exists() {
        fs::remove_dir_all(out_dir).map_err(|e| format!("无法清理 {}：{e}", out_dir.display()))?;
    }
    fs::create_dir_all(out_dir).map_err(|e| format!("无法创建 {}：{e}", out_dir.display()))?;
    let ok = run(
        "uniffi-bindgen generate",
        cargo()
            .current_dir(root)
            .args(["run", "--quiet", "-p", "uniffi-bindgen", "--", "generate"])
            .arg("--library")
            .arg(&lib)
            .args(["--language", "swift"])
            .arg("--out-dir")
            .arg(out_dir),
    );
    if ok {
        Ok(())
    } else {
        Err("uniffi-bindgen 失败".into())
    }
}

/// `cargo xtask bindgen`：重新生成并覆盖入库的绑定。
pub fn generate() -> ExitCode {
    let root = repo_root();
    let staging = root.join("target/bindgen");
    if let Err(e) = generate_into(&root, &staging) {
        eprintln!("{e}");
        return ExitCode::FAILURE;
    }
    for (name, dest) in OUTPUTS {
        let src = staging.join(name);
        let dest = root.join(dest);
        let result = dest
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::copy(&src, &dest).map(|_| ()));
        if let Err(e) = result {
            eprintln!("无法写入 {}：{e}", dest.display());
            return ExitCode::FAILURE;
        }
    }
    eprintln!("已生成 Swift 绑定");
    ExitCode::SUCCESS
}

/// `cargo xtask check-bindings`：生成到临时目录并与入库版本逐字节比较。
pub fn check() -> ExitCode {
    let root = repo_root();
    let staging = root.join("target/bindgen-check");
    if let Err(e) = generate_into(&root, &staging) {
        eprintln!("{e}");
        return ExitCode::FAILURE;
    }
    let mut stale = vec![];
    for (name, dest) in OUTPUTS {
        let fresh = fs::read(staging.join(name));
        let committed = fs::read(root.join(dest));
        match (fresh, committed) {
            (Ok(a), Ok(b)) if a == b => {}
            (Ok(_), Ok(_)) => stale.push(format!("{dest}：与重新生成的内容不一致")),
            (Ok(_), Err(e)) => stale.push(format!("{dest}：无法读取入库版本（{e}）")),
            (Err(e), _) => stale.push(format!("{name}：bindgen 没有产出（{e}）")),
        }
    }
    if stale.is_empty() {
        eprintln!("check-bindings：Swift 绑定与 kanso-ffi 接口一致");
        return ExitCode::SUCCESS;
    }
    eprintln!("check-bindings 失败，入库的 Swift 绑定已过期：");
    for s in &stale {
        eprintln!("  - {s}");
    }
    eprintln!("请运行 `cargo xtask bindgen` 重新生成并提交。");
    ExitCode::FAILURE
}
