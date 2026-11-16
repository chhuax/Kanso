//! zsh 提示符集成 shim 的生成（local-shell.md §5.1）。
//!
//! 脚本本体维护在 `shim/zsh/*.zsh`（编译期 `include_str!` 嵌入）；加载机制与提示符规则
//! 的说明写在脚本注释和 `docs/local-shell-impl.md` 里。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::PtyError;

/// shim 文件名（zsh 启动文件）与内容。五个文件缺一不可：任何一个缺失，
/// 用户自己的同名文件就会因为 `ZDOTDIR` 指向 shim 目录而被跳过。
const FILES: &[(&str, &str)] = &[
    (".zshenv", include_str!("shim/zsh/zshenv.zsh")),
    (".zprofile", include_str!("shim/zsh/zprofile.zsh")),
    (".zshrc", include_str!("shim/zsh/zshrc.zsh")),
    (".zlogin", include_str!("shim/zsh/zlogin.zsh")),
    (".zlogout", include_str!("shim/zsh/zlogout.zsh")),
];

/// 在 `<data_dir>/shell-integration/zsh/` 下生成 shim，返回该目录（用作子进程的 `ZDOTDIR`）。
///
/// - 文件内容与已有的完全一致时**不重写**，不改变修改时间；内容不同（应用升级）才替换。
/// - 替换时先写同目录的临时文件再 `rename`：已经在运行的 zsh 可能正好在读 shim，
///   `rename` 保证它读到的要么是旧文件要么是新文件，不会读到写了一半的内容。
/// - 任何一步失败都返回 [`PtyError::Shim`]。**调用方必须据此不设置 `ZDOTDIR`**
///   （不要构造 [`crate::ZshIntegration`]），让 shell 按原样启动，只是没有提示符集成。
///   半成品的 shim 目录（例如只写了部分文件）绝不能被用作 `ZDOTDIR`。
/// - 卸载：删除 shim 目录即可，用户自己的文件从未被改动。
pub fn install_zsh_shim(data_dir: &Path) -> Result<PathBuf, PtyError> {
    let dir = data_dir.join("shell-integration").join("zsh");
    let wrap = |path: &Path, source: std::io::Error| PtyError::Shim {
        path: path.to_owned(),
        source,
    };
    fs::create_dir_all(&dir).map_err(|e| wrap(&dir, e))?;
    for (name, content) in FILES {
        let path = dir.join(name);
        if fs::read(&path).is_ok_and(|old| old == content.as_bytes()) {
            continue;
        }
        let tmp = dir.join(format!("{name}.tmp-{}", std::process::id()));
        let result = (|| {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(content.as_bytes())?;
            f.sync_all()?;
            fs::rename(&tmp, &path)
        })();
        if let Err(e) = result {
            let _ = fs::remove_file(&tmp);
            return Err(wrap(&path, e));
        }
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_five_files_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = install_zsh_shim(tmp.path()).unwrap();
        assert_eq!(dir, tmp.path().join("shell-integration/zsh"));
        let mut mtimes = Vec::new();
        for (name, content) in FILES {
            let p = dir.join(name);
            assert_eq!(fs::read_to_string(&p).unwrap(), *content);
            mtimes.push(fs::metadata(&p).unwrap().modified().unwrap());
        }
        // 留出足够的时间间隔，确保"重写"一定会体现为修改时间变化。
        std::thread::sleep(std::time::Duration::from_millis(50));
        let again = install_zsh_shim(tmp.path()).unwrap();
        assert_eq!(again, dir);
        for ((name, _), before) in FILES.iter().zip(mtimes) {
            assert_eq!(
                fs::metadata(dir.join(name)).unwrap().modified().unwrap(),
                before,
                "{name}"
            );
        }
        // 不留临时文件。
        assert_eq!(fs::read_dir(&dir).unwrap().count(), FILES.len());
    }

    #[test]
    fn changed_content_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = install_zsh_shim(tmp.path()).unwrap();
        fs::write(dir.join(".zshrc"), "stale").unwrap();
        install_zsh_shim(tmp.path()).unwrap();
        assert_eq!(fs::read_to_string(dir.join(".zshrc")).unwrap(), FILES[2].1);
    }

    #[test]
    fn failure_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        // data_dir 的位置被一个普通文件占用：create_dir_all 必然失败。
        let blocker = tmp.path().join("file");
        fs::write(&blocker, "x").unwrap();
        assert!(matches!(
            install_zsh_shim(&blocker),
            Err(PtyError::Shim { .. })
        ));
    }

    /// 取出 `# >>> kanso:load` 到 `# <<< kanso:load` 之间的段落，并把加载的用户文件名统一成占位符。
    fn load_block(content: &str, name: &str) -> String {
        let start = content.find("# >>> kanso:load").expect("缺少加载段起点");
        let end = content.find("# <<< kanso:load").expect("缺少加载段终点");
        content[start..end].replace(
            &format!("$__kanso_user_dir/{name}"),
            "$__kanso_user_dir/@FILE@",
        )
    }

    #[test]
    fn load_blocks_are_identical_across_files() {
        let reference = load_block(FILES[0].1, FILES[0].0);
        for (name, content) in FILES {
            assert_eq!(
                load_block(content, name),
                reference,
                "{name} 的加载段与其他文件不一致"
            );
        }
    }
}
