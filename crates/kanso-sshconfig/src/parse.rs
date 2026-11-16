//! 配置文件解析：把文件（含 Include）读成条目树。

use std::path::{Path, PathBuf};

use crate::fs::ConfigFs;
use crate::text::{expand_percent, expand_tilde, has_wildcard, split_line};
use crate::{Env, Warning};

/// Include 最大嵌套层数：主配置文件为第 0 层，第 17 层的文件不再读取
/// （`ssh -G` 实验：16 层链可读，17 层报 “Too many recursive configuration includes”）。
pub(crate) const MAX_INCLUDE_DEPTH: usize = 16;

/// 求值关心的选项。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Key {
    HostName,
    Port,
    User,
    IdentityFile,
    ProxyJump,
}

pub(crate) enum Item {
    /// `Host` 行及其全部模式（原文）。
    Host(Vec<String>),
    /// `Match` 行：本实现不求值，等价于“不选中任何主机”的块。
    Match,
    Opt(Key, String),
    /// 一行 `Include` 展开出的各文件，已按字典序排列；每个文件各自是一个作用域。
    Include(Vec<Vec<Item>>),
}

pub(crate) struct Parser<'a> {
    pub fs: &'a dyn ConfigFs,
    pub env: &'a Env,
    pub warnings: Vec<Warning>,
}

impl Parser<'_> {
    /// 读取并解析一个文件；读不到返回 `None`。`depth` 是该文件的嵌套层数。
    pub fn parse_file(&mut self, path: &Path, depth: usize) -> Option<Vec<Item>> {
        let text = self.fs.read_to_string(path)?;
        let mut items = Vec::new();
        // Match 警告每个文件只报一次，避免一个大文件刷屏。
        let mut match_warned = false;
        for raw in text.lines() {
            let Some(line) = split_line(raw) else {
                continue;
            };
            let kw = line.keyword.to_ascii_lowercase();
            match kw.as_str() {
                "host" => items.push(Item::Host(line.words)),
                "match" => {
                    if !match_warned {
                        match_warned = true;
                        self.warnings.push(Warning::MatchUnsupported {
                            file: path.to_path_buf(),
                        });
                    }
                    items.push(Item::Match);
                }
                "include" => {
                    let files = self.expand_include(&line.words, path, depth);
                    items.push(Item::Include(files));
                }
                _ => {
                    let key = match kw.as_str() {
                        "hostname" => Key::HostName,
                        "port" => Key::Port,
                        "user" => Key::User,
                        "identityfile" => Key::IdentityFile,
                        "proxyjump" => Key::ProxyJump,
                        _ => continue,
                    };
                    // 没有值的行 ssh 会报错；我们当作不存在，不占用“第一次出现”的名额。
                    if let Some(v) = line.words.into_iter().next() {
                        items.push(Item::Opt(key, v));
                    }
                }
            }
        }
        Some(items)
    }

    fn expand_include(&mut self, words: &[String], from: &Path, depth: usize) -> Vec<Vec<Item>> {
        let mut out = Vec::new();
        for word in words {
            let env = self.env;
            // Include 路径里的 %h/%n/%p/%r 依赖具体主机，而文件是在求值前统一读入的，
            // 所以只支持与主机无关的 token；其余按未知处理并警告。
            let (expanded, bad) = expand_percent(word, &|c| match c {
                'd' => Some(env.home.to_string_lossy().into_owned()),
                'u' => Some(env.local_user.clone()),
                'l' => Some(env.local_host.clone()),
                'L' => Some(short_host(&env.local_host)),
                'i' => Some(env.local_uid.to_string()),
                _ => None,
            });
            for token in bad {
                self.warnings.push(Warning::UnknownToken {
                    option: "Include",
                    value: word.clone(),
                    token,
                });
            }
            let mut path = expand_tilde(&expanded, &env.home);
            if path.is_relative() {
                path = env.home.join(".ssh").join(path);
            }
            let mut files: Vec<PathBuf> = if has_wildcard(&path.to_string_lossy()) {
                self.fs.glob(&path)
            } else {
                vec![path]
            };
            files.sort();
            for f in files {
                if depth + 1 > MAX_INCLUDE_DEPTH {
                    self.warnings.push(Warning::IncludeTooDeep {
                        file: from.to_path_buf(),
                        include: f,
                    });
                    continue;
                }
                if let Some(items) = self.parse_file(&f, depth + 1) {
                    out.push(items);
                }
            }
        }
        out
    }
}

/// 本机主机名去掉域名后的短名（`%L`）。
pub(crate) fn short_host(h: &str) -> String {
    h.split('.').next().unwrap_or(h).to_string()
}
