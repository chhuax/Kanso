use serde::Serialize;

/// A detected shell's command line, ready for SessionProfile.shell.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalShell {
    pub name: String,
    pub command: String,
}

#[cfg(any(windows, test))]
fn detect_shell(
    name: &str,
    candidates: impl IntoIterator<Item = std::path::PathBuf>,
    args: &str,
) -> Option<LocalShell> {
    let path = candidates.into_iter().find(|path| path.is_file())?;
    // Windows paths cannot contain a double quote; quote even paths without
    // spaces so every detected command uses the same command-line convention.
    Some(LocalShell {
        name: name.into(),
        command: format!("\"{}\"{args}", path.display()),
    })
}

/// Only inspect files: opening the picker must not start a shell, run profile
/// scripts, or wake a WSL VM. Custom installs remain accessible by command line.
pub fn list() -> Vec<LocalShell> {
    #[cfg(not(windows))]
    {
        Vec::new()
    }
    #[cfg(windows)]
    {
        use std::path::PathBuf;

        let path: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default();
        let under = |variable: &str, suffix: &str| {
            std::env::var_os(variable).map(|root| PathBuf::from(root).join(suffix))
        };
        let candidates = |exe: &str, extra: Vec<Option<PathBuf>>| {
            path.iter()
                .map(|dir| dir.join(exe))
                .chain(extra.into_iter().flatten())
                .collect::<Vec<_>>()
        };
        [
            detect_shell(
                "PowerShell 7 (pwsh)",
                candidates(
                    "pwsh.exe",
                    vec![under("ProgramFiles", r"PowerShell\7\pwsh.exe")],
                ),
                " -NoLogo",
            ),
            detect_shell(
                "Windows PowerShell",
                candidates(
                    "powershell.exe",
                    vec![under(
                        "SystemRoot",
                        r"System32\WindowsPowerShell\v1.0\powershell.exe",
                    )],
                ),
                " -NoLogo",
            ),
            detect_shell(
                "Command Prompt (cmd)",
                candidates(
                    "cmd.exe",
                    vec![under("SystemRoot", r"System32\cmd.exe")],
                ),
                "",
            ),
            detect_shell(
                "WSL (default distribution)",
                candidates(
                    "wsl.exe",
                    vec![under("SystemRoot", r"System32\wsl.exe")],
                ),
                "",
            ),
            // Do not mistake Windows' legacy bash.exe WSL launcher for Git Bash.
            detect_shell(
                "Git Bash",
                vec![
                    under("ProgramFiles", r"Git\bin\bash.exe"),
                    under("ProgramFiles(x86)", r"Git\bin\bash.exe"),
                    under("LOCALAPPDATA", r"Programs\Git\bin\bash.exe"),
                ]
                .into_iter()
                .flatten(),
                " --login -i",
            ),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_skips_missing_files_and_directories_and_preserves_arguments() {
        let root = std::env::temp_dir().join(format!("kanso-shell-{}", uuid::Uuid::new_v4()));
        let program = root.join("PowerShell 7").join("pwsh.exe");
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, b"test executable").unwrap();
        let shell = detect_shell(
            "PowerShell 7 (pwsh)",
            [root.join("missing.exe"), root.clone(), program.clone()],
            " -NoLogo",
        )
        .unwrap();
        assert_eq!(shell.name, "PowerShell 7 (pwsh)");
        assert_eq!(
            crate::model::split_command_line(&shell.command, true).unwrap(),
            [program.to_string_lossy().into_owned(), "-NoLogo".into()]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unavailable_shell_is_not_offered() {
        assert!(detect_shell("Missing", [std::path::PathBuf::new()], "").is_none());
    }
}
