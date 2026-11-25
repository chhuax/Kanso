//! 发布辅助命令（`cargo xtask release`）：版本升级与带硬校验的标签推送。
//!
//! 之所以收进 xtask 而不是手工拼命令行，是为了在发版前强制检查所有易错点：
//! - `bump` 同步更新 `Cargo.toml`、`apple/project.yml` 与 `Cargo.lock`，并用 `--locked` 兜底；
//! - `tag` 强制先从远端拉取最新 `main`，直接从目标 commit 对象读取 `Cargo.toml`、
//!   `apple/project.yml` 与 `Cargo.lock` 逐项核对版本号一致，并确认本地与远端都不存在同名标签，
//!   任何一步失败即中止，避免打在过期提交或触发受保护标签误推。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use crate::{cargo, run};

/// `cargo xtask release <bump|tag> ...` 入口。
pub fn dispatch(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("bump") => bump(&args[1..]),
        Some("tag") => tag(&args[1..]),
        _ => {
            release_usage();
            ExitCode::FAILURE
        }
    }
}

fn release_usage() {
    eprintln!(
        "用法：\n  \
         cargo xtask release bump <版本> [--build <构建号>]\n      \
             更新 Cargo.toml、apple/project.yml 与 Cargo.lock（默认将 CURRENT_PROJECT_VERSION 加 1）\n  \
         cargo xtask release tag <版本> [--remote <远端名>] [--dry-run]\n      \
             拉取远端 main，校验目标提交内版本号一致且标签不存在，再打 annotated tag 并推送"
    );
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask 位于仓库根目录之下")
        .to_path_buf()
}

/// 校验纯 SemVer 版本号（不带 `v` 前缀，如 `1.0.9` 或 `1.0.0-beta.1`）。
fn validate_version(ver: &str) -> Result<(), String> {
    if ver.starts_with('v') || ver.starts_with('V') {
        return Err(format!("请传入纯版本号，不要带 v 前缀：{ver}"));
    }
    let (core, pre) = match ver.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (ver, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        return Err(format!("版本号格式应为 MAJOR.MINOR.PATCH：{ver}"));
    }
    for part in parts {
        if part.is_empty()
            || !part.bytes().all(|b| b.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0'))
        {
            return Err(format!("版本号数字段不合法：{ver}"));
        }
    }
    if let Some(pre) = pre
        && (pre.is_empty()
            || pre.split('.').any(|seg| {
                seg.is_empty() || !seg.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }))
    {
        return Err(format!("预发布标识不合法：{ver}"));
    }
    Ok(())
}

/// 从 `Cargo.toml` 文本中读取 `[workspace.package]` 下的 `version`。
fn parse_workspace_version(content: &str) -> Result<String, String> {
    let mut in_section = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed == "[workspace.package]";
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("version") {
            let rest = rest.trim_start();
            if let Some(val) = rest.strip_prefix('=') {
                let val = val.trim();
                if let Some(inner) = val.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                    return Ok(inner.to_string());
                }
            }
        }
    }
    Err("Cargo.toml 中未找到 [workspace.package] 下的 version 字段".into())
}

/// 将 `Cargo.toml` 中 `[workspace.package]` 下的 `version` 替换为 `new_ver`。
fn rewrite_workspace_version(content: &str, new_ver: &str) -> Result<String, String> {
    let mut in_section = false;
    let mut replaced = false;
    let mut out = String::with_capacity(content.len() + 16);
    for segment in content.split_inclusive('\n') {
        let (line, ending) = match segment.strip_suffix("\r\n") {
            Some(l) => (l, "\r\n"),
            None => match segment.strip_suffix('\n') {
                Some(l) => (l, "\n"),
                None => (segment, ""),
            },
        };
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed == "[workspace.package]";
            out.push_str(line);
            out.push_str(ending);
            continue;
        }
        if in_section
            && !replaced
            && let Some(rest) = trimmed.strip_prefix("version")
            && rest.trim_start().starts_with('=')
        {
            out.push_str(&format!("version = \"{new_ver}\""));
            out.push_str(ending);
            replaced = true;
            continue;
        }
        out.push_str(line);
        out.push_str(ending);
    }
    if !replaced {
        return Err("Cargo.toml 中未找到可替换的 [workspace.package] version".into());
    }
    Ok(out)
}

/// 从 `apple/project.yml` 文本中读取 `(MARKETING_VERSION, CURRENT_PROJECT_VERSION)`。
fn parse_apple_versions(content: &str) -> Result<(String, u32), String> {
    let mut marketing = None;
    let mut build = None;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("MARKETING_VERSION:") {
            let val = rest.trim().trim_matches('"');
            if !val.is_empty() {
                marketing = Some(val.to_string());
            }
        } else if let Some(rest) = trimmed.strip_prefix("CURRENT_PROJECT_VERSION:") {
            let val = rest.trim().trim_matches('"');
            let num: u32 = val
                .parse()
                .map_err(|_| format!("apple/project.yml 构建号不是正整数：{val}"))?;
            if num == 0 {
                return Err("apple/project.yml 构建号必须大于 0".into());
            }
            build = Some(num);
        }
    }
    let marketing =
        marketing.ok_or_else(|| "apple/project.yml 中未找到 MARKETING_VERSION".to_string())?;
    let build =
        build.ok_or_else(|| "apple/project.yml 中未找到 CURRENT_PROJECT_VERSION".to_string())?;
    Ok((marketing, build))
}

/// 替换 `apple/project.yml` 中的 `MARKETING_VERSION` 与 `CURRENT_PROJECT_VERSION`。
fn rewrite_apple_versions(content: &str, new_ver: &str, new_build: u32) -> Result<String, String> {
    let mut replaced_marketing = false;
    let mut replaced_build = false;
    let mut out = String::with_capacity(content.len() + 16);
    for segment in content.split_inclusive('\n') {
        let (line, ending) = match segment.strip_suffix("\r\n") {
            Some(l) => (l, "\r\n"),
            None => match segment.strip_suffix('\n') {
                Some(l) => (l, "\n"),
                None => (segment, ""),
            },
        };
        let indent_len = line.len() - line.trim_start().len();
        let indent = &line[..indent_len];
        let trimmed = &line[indent_len..];
        if !replaced_marketing && trimmed.starts_with("MARKETING_VERSION:") {
            out.push_str(&format!("{indent}MARKETING_VERSION: \"{new_ver}\""));
            out.push_str(ending);
            replaced_marketing = true;
            continue;
        }
        if !replaced_build && trimmed.starts_with("CURRENT_PROJECT_VERSION:") {
            out.push_str(&format!("{indent}CURRENT_PROJECT_VERSION: \"{new_build}\""));
            out.push_str(ending);
            replaced_build = true;
            continue;
        }
        out.push_str(line);
        out.push_str(ending);
    }
    if !replaced_marketing || !replaced_build {
        return Err("apple/project.yml 中缺少 MARKETING_VERSION 或 CURRENT_PROJECT_VERSION".into());
    }
    Ok(out)
}

/// 从 `Cargo.lock` 文本中读取指定工作区包的 `version`。
fn parse_lock_package_version(content: &str, pkg_name: &str) -> Result<String, String> {
    let mut in_pkg = false;
    let mut matched = false;
    let target = format!("name = \"{pkg_name}\"");
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            in_pkg = true;
            matched = false;
            continue;
        }
        if !in_pkg {
            continue;
        }
        if trimmed == target {
            matched = true;
            continue;
        }
        if matched
            && let Some(rest) = trimmed.strip_prefix("version = \"")
            && let Some(ver) = rest.strip_suffix('"')
        {
            return Ok(ver.to_string());
        }
    }
    Err(format!("Cargo.lock 中未找到包 {pkg_name} 的版本"))
}

fn bump(args: &[String]) -> ExitCode {
    let Some(new_ver) = args.first() else {
        release_usage();
        return ExitCode::FAILURE;
    };
    if let Err(e) = validate_version(new_ver) {
        eprintln!("参数错误：{e}");
        return ExitCode::FAILURE;
    }

    let mut build_override: Option<u32> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--build" => {
                i += 1;
                let Some(raw) = args.get(i) else {
                    eprintln!("参数错误：--build 缺少正整数参数");
                    return ExitCode::FAILURE;
                };
                match raw.parse::<u32>() {
                    Ok(n) if n > 0 => build_override = Some(n),
                    _ => {
                        eprintln!("参数错误：--build 必须为正整数，收到 {raw}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            other => {
                eprintln!("未知参数：{other}");
                release_usage();
                return ExitCode::FAILURE;
            }
        }
        i += 1;
    }

    let root = repo_root();
    let cargo_path = root.join("Cargo.toml");
    let apple_path = root.join("apple/project.yml");

    let cargo_text = match fs::read_to_string(&cargo_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("无法读取 {}：{e}", cargo_path.display());
            return ExitCode::FAILURE;
        }
    };
    let apple_text = match fs::read_to_string(&apple_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("无法读取 {}：{e}", apple_path.display());
            return ExitCode::FAILURE;
        }
    };

    let old_cargo_ver = match parse_workspace_version(&cargo_text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let (old_apple_ver, old_build) = match parse_apple_versions(&apple_text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let new_build = build_override.unwrap_or(old_build + 1);

    let updated_cargo = match rewrite_workspace_version(&cargo_text, new_ver) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let updated_apple = match rewrite_apple_versions(&apple_text, new_ver, new_build) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(e) = fs::write(&cargo_path, updated_cargo) {
        eprintln!("写入 {} 失败：{e}", cargo_path.display());
        return ExitCode::FAILURE;
    }
    if let Err(e) = fs::write(&apple_path, updated_apple) {
        eprintln!("写入 {} 失败：{e}", apple_path.display());
        return ExitCode::FAILURE;
    }

    if !run(
        "同步 Cargo.lock 工作区版本",
        cargo()
            .current_dir(&root)
            .args(["update", "--workspace", "--offline"]),
    ) {
        return ExitCode::FAILURE;
    }
    if !run(
        "校验 Cargo.lock 一致性",
        cargo()
            .current_dir(&root)
            .args([
                "metadata",
                "--locked",
                "--offline",
                "--format-version",
                "1",
                "--no-deps",
            ])
            .stdout(Stdio::null()),
    ) {
        return ExitCode::FAILURE;
    }

    eprintln!(
        "\n版本已更新：\n  \
         Cargo.toml:        {old_cargo_ver} -> {new_ver}\n  \
         apple/project.yml: {old_apple_ver} (build {old_build}) -> {new_ver} (build {new_build})\n  \
         Cargo.lock:        已同步并通过 --locked 校验\n\n\
         下一步：检查 `git diff`、提交并经 PR 合并到 main，然后执行：\n  \
         cargo xtask release tag {new_ver}"
    );
    ExitCode::SUCCESS
}

/// 运行 git 并返回去首尾空白的 stdout；退出码非零时直接报错。
fn git_output(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|e| format!("无法启动 git：{e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("git {} 失败：{}", args.join(" "), stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn tag(args: &[String]) -> ExitCode {
    let Some(ver) = args.first() else {
        release_usage();
        return ExitCode::FAILURE;
    };
    if let Err(e) = validate_version(ver) {
        eprintln!("参数错误：{e}");
        return ExitCode::FAILURE;
    }

    let mut remote = "origin".to_string();
    let mut dry_run = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--remote" => {
                i += 1;
                let Some(r) = args.get(i) else {
                    eprintln!("参数错误：--remote 缺少远端名");
                    return ExitCode::FAILURE;
                };
                remote = r.clone();
            }
            "--dry-run" => dry_run = true,
            other => {
                eprintln!("未知参数：{other}");
                release_usage();
                return ExitCode::FAILURE;
            }
        }
        i += 1;
    }

    let root = repo_root();
    let tag_name = format!("v{ver}");
    let remote_main_ref = format!("refs/remotes/{remote}/main");
    let fetch_spec = format!("+refs/heads/main:{remote_main_ref}");

    // 1. 强制同步远端 main，绝不使用可能过期的本地追踪分支。
    if !run(
        "拉取远端 main",
        Command::new("git")
            .current_dir(&root)
            .args(["fetch", &remote, &fetch_spec]),
    ) {
        return ExitCode::FAILURE;
    }

    // 2. 锁定目标提交 SHA，后续所有校验与打标都绑定到这个确切提交。
    let sha = match git_output(&root, &["rev-parse", "--verify", &remote_main_ref]) {
        Ok(s) => s.trim().to_string(),
        Err(e) => {
            eprintln!("无法解析 {remote}/main：{e}");
            return ExitCode::FAILURE;
        }
    };
    let subject = match git_output(&root, &["show", "-s", "--format=%h %s", &sha]) {
        Ok(s) => s.trim().to_string(),
        Err(e) => {
            eprintln!("无法读取目标提交信息：{e}");
            return ExitCode::FAILURE;
        }
    };

    // 3. 直接从目标提交读取 Cargo.toml / apple/project.yml / Cargo.lock 校验版本号。
    let cargo_spec = format!("{sha}:Cargo.toml");
    let cargo_toml = match git_output(&root, &["show", &cargo_spec]) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let cargo_ver = match parse_workspace_version(&cargo_toml) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("目标提交 {sha} 的 Cargo.toml 校验失败：{e}");
            return ExitCode::FAILURE;
        }
    };
    if cargo_ver != *ver {
        eprintln!(
            "中止：远端 {remote}/main（{subject}）的 Cargo.toml 版本为 {cargo_ver}，与待打标签 {tag_name} 不一致。\n\
             请先通过 PR 合并版本更新提交到 main。"
        );
        return ExitCode::FAILURE;
    }

    let apple_spec = format!("{sha}:apple/project.yml");
    let apple_yml = match git_output(&root, &["show", &apple_spec]) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let (apple_ver, apple_build) = match parse_apple_versions(&apple_yml) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("目标提交 {sha} 的 apple/project.yml 校验失败：{e}");
            return ExitCode::FAILURE;
        }
    };
    if apple_ver != *ver {
        eprintln!(
            "中止：远端 {remote}/main（{subject}）的 apple/project.yml 版本为 {apple_ver}，与待打标签 {tag_name} 不一致。"
        );
        return ExitCode::FAILURE;
    }

    let lock_spec = format!("{sha}:Cargo.lock");
    let lock_text = match git_output(&root, &["show", &lock_spec]) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let lock_ver = match parse_lock_package_version(&lock_text, "kanso-core") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("目标提交 {sha} 的 Cargo.lock 校验失败：{e}");
            return ExitCode::FAILURE;
        }
    };
    if lock_ver != *ver {
        eprintln!(
            "中止：远端 {remote}/main（{subject}）的 Cargo.lock（kanso-core={lock_ver}）未与版本 {ver} 同步。"
        );
        return ExitCode::FAILURE;
    }

    // 4. 确认本地与远端都不存在同名标签（远端用 ls-remote 实时查询）。
    let tag_ref = format!("refs/tags/{tag_name}");
    let local_exists = Command::new("git")
        .current_dir(&root)
        .args(["rev-parse", "--verify", "--quiet", &tag_ref])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if local_exists {
        eprintln!(
            "中止：本地已存在标签 {tag_name}。如确认是残留标签，请先检查并执行 `git tag -d {tag_name}`。"
        );
        return ExitCode::FAILURE;
    }

    let remote_tags = match git_output(&root, &["ls-remote", "--tags", &remote, &tag_ref]) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("无法查询远端标签：{e}");
            return ExitCode::FAILURE;
        }
    };
    if !remote_tags.trim().is_empty() {
        eprintln!("中止：远端 {remote} 已存在标签 {tag_name}：\n{remote_tags}");
        return ExitCode::FAILURE;
    }

    eprintln!(
        "\n发布前校验全部通过：\n  \
         目标分支:          {remote}/main\n  \
         目标提交:          {subject} ({sha})\n  \
         Cargo.toml:        {cargo_ver}\n  \
         apple/project.yml: {apple_ver} (build {apple_build})\n  \
         Cargo.lock:        kanso-core {lock_ver}\n  \
         待建标签:          {tag_name}"
    );

    if dry_run {
        eprintln!("\ndry-run 模式：未创建或推送标签。");
        return ExitCode::SUCCESS;
    }

    let msg = format!("Kanso {ver}");
    if !run(
        "创建本地 annotated 标签",
        Command::new("git")
            .current_dir(&root)
            .args(["tag", "-a", &tag_name, "-m", &msg, &sha]),
    ) {
        return ExitCode::FAILURE;
    }

    if !run(
        "推送标签到远端",
        Command::new("git")
            .current_dir(&root)
            .args(["push", &remote, &tag_ref]),
    ) {
        eprintln!(
            "提示：本地标签 {tag_name} 已创建但推送失败；排查后如需撤销本地标签可运行 `git tag -d {tag_name}`。"
        );
        return ExitCode::FAILURE;
    }

    eprintln!("\n已推送标签 {tag_name} -> {sha}，Release 工作流已触发。");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_version_rules() {
        assert!(validate_version("1.0.9").is_ok());
        assert!(validate_version("0.1.0").is_ok());
        assert!(validate_version("1.0.0-beta.1").is_ok());
        assert!(validate_version("v1.0.9").is_err());
        assert!(validate_version("1.0").is_err());
        assert!(validate_version("1.0.9.1").is_err());
        assert!(validate_version("01.0.9").is_err());
        assert!(validate_version("1.0.9-").is_err());
        assert!(validate_version("").is_err());
    }

    #[test]
    fn rewrite_workspace_version_only_touches_workspace_package() {
        let src = "\
[workspace]
members = [\"crates/*\"]

[workspace.package]
version = \"1.0.10\"
edition = \"2024\"

[workspace.dependencies]
serde = { version = \"1\" }
";
        assert_eq!(parse_workspace_version(src).unwrap(), "1.0.10");
        let dst = rewrite_workspace_version(src, "1.0.9").unwrap();
        assert_eq!(parse_workspace_version(&dst).unwrap(), "1.0.9");
        assert!(dst.contains("serde = { version = \"1\" }"));
    }

    #[test]
    fn rewrite_apple_versions_preserves_indent() {
        let src = "\
settings:
  base:
    CODE_SIGN_STYLE: Manual
    MARKETING_VERSION: \"1.0.10\"
    CURRENT_PROJECT_VERSION: \"11\"
";
        assert_eq!(parse_apple_versions(src).unwrap(), ("1.0.10".into(), 11));
        let dst = rewrite_apple_versions(src, "1.0.9", 10).unwrap();
        assert_eq!(parse_apple_versions(&dst).unwrap(), ("1.0.9".into(), 10));
        assert!(dst.contains("    MARKETING_VERSION: \"1.0.9\""));
        assert!(dst.contains("    CURRENT_PROJECT_VERSION: \"10\""));
    }

    #[test]
    fn parse_lock_finds_kanso_core() {
        let lock = "\
[[package]]
name = \"other\"
version = \"0.1.0\"

[[package]]
name = \"kanso-core\"
version = \"1.0.9\"
dependencies = []
";
        assert_eq!(
            parse_lock_package_version(lock, "kanso-core").unwrap(),
            "1.0.9"
        );
        assert!(parse_lock_package_version(lock, "missing").is_err());
    }
}
