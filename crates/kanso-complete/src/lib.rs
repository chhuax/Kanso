//! 命令补全的解析、匹配与排序；外部数据由调用方提供。

mod parse;
mod vocabulary;

pub use parse::{Context, NameKind, PathRequest, parse};

/// 弹窗最多显示的候选数。
pub const MAX_CANDIDATES: usize = 8;
/// 单个目录最多消费的条目数。
pub const MAX_DIRECTORY_ENTRIES: usize = 2000;

/// 候选类别，由界面本地化显示。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    /// 目录。
    Directory,
    /// 文件。
    File,
    /// 命令历史。
    History,
    /// Kubernetes 词汇与名称。
    Kubernetes,
    /// Docker 命令。
    Docker,
    /// Git 词汇。
    Git,
    /// 系统服务与日志命令。
    Systemd,
}

/// 一项接受后通过按键替换的候选。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// 要输入的文本。
    pub replacement: String,
    /// 来源类别。
    pub category: Category,
    /// 光标前需要删除的码点数。
    pub delete_codepoints: u32,
}

/// 参与排序的历史记录。
#[derive(Clone, Debug)]
pub struct History {
    /// 完整命令。
    pub command: String,
    /// 最近使用的毫秒时间。
    pub last_used_at: i64,
    /// 是否在当前主机使用过。
    pub same_host: bool,
}

/// 调用方读取的目录条目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryEntry {
    /// 单个文件名。
    pub name: String,
    /// 是否为目录（含已解析的目录链接）。
    pub directory: bool,
}

/// 本地 kubeconfig 中实际出现的名称。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KubeNames {
    /// 上下文名称。
    pub contexts: Vec<String>,
    /// 命名空间名称。
    pub namespaces: Vec<String>,
}

impl KubeNames {
    /// 合并配置；无效文档静默省略。
    pub fn from_configs(configs: &[String]) -> Self {
        let mut names = Self::default();
        for config in configs {
            let Ok(documents) = yaml_rust2::YamlLoader::load_from_str(config) else {
                continue;
            };
            for document in documents {
                let Some(contexts) = document["contexts"].as_vec() else {
                    continue;
                };
                for context in contexts {
                    if let Some(name) = context["name"].as_str() {
                        names.contexts.push(name.to_owned());
                    }
                    if let Some(namespace) = context["context"]["namespace"].as_str() {
                        names.namespaces.push(namespace.to_owned());
                    }
                }
            }
        }
        names.contexts.sort();
        names.contexts.dedup();
        names.namespaces.sort();
        names.namespaces.dedup();
        names
    }
}

/// 纯函数合并、排序并去重候选。
pub fn complete(
    context: &Context,
    history: &[History],
    path: Option<&PathRequest>,
    entries: &[DirectoryEntry],
    names: &KubeNames,
) -> Vec<Candidate> {
    let mut result = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut add = |from: usize, replacement: String, category: Category| {
        let completed = format!("{}{replacement}", &context.input[..from]);
        if completed != context.input && seen.insert(completed) && result.len() < MAX_CANDIDATES {
            result.push(Candidate {
                replacement,
                category,
                delete_codepoints: context.input[from..].chars().count() as u32,
            });
        }
    };
    for (word, category) in vocabulary::words(context) {
        if word.starts_with(context.word()) {
            add(context.word_start, word.to_owned(), category);
        }
    }
    if let Some(kind) = context.name_kind() {
        let values = match kind {
            NameKind::Context => &names.contexts,
            NameKind::Namespace => &names.namespaces,
        };
        let (prefix, value) = context.name_prefix();
        for name in values {
            if name.starts_with(value) {
                add(
                    context.word_start,
                    format!("{prefix}{}", quote_word(name)),
                    Category::Kubernetes,
                );
            }
        }
    }
    if let Some(path) = path {
        let mut matching: Vec<_> = entries
            .iter()
            .take(MAX_DIRECTORY_ENTRIES)
            .filter(|entry| {
                (!path.directories_only || entry.directory)
                    && entry.name.starts_with(&path.prefix)
                    && (!entry.name.starts_with('.') || path.prefix.starts_with('.'))
                    && !matches!(entry.name.as_str(), "." | "..")
                    && !entry.name.chars().any(char::is_control)
            })
            .collect();
        matching.sort_by(|left, right| {
            right
                .directory
                .cmp(&left.directory)
                .then_with(|| left.name.cmp(&right.name))
        });
        for entry in matching {
            let suffix = if entry.directory { "/" } else { "" };
            let word = format!("{}{}{suffix}", path.insert_prefix, entry.name);
            let mut quoted = quote_word(&word);
            if word.starts_with('~') && !path.expand_home {
                quoted.insert(0, '\\');
            }
            add(
                context.word_start,
                quoted,
                if entry.directory {
                    Category::Directory
                } else {
                    Category::File
                },
            );
        }
    }
    if context.command_position() {
        let query = context.word();
        if query
            .chars()
            .filter(|character| !character.is_whitespace())
            .count()
            >= 2
        {
            let worthwhile = |command: &str| {
                command.contains(query) && command.chars().count() >= query.chars().count() + 3
            };
            let mut history: Vec<_> = history
                .iter()
                .filter(|item| worthwhile(&item.command))
                .collect();
            history.sort_by(|left, right| {
                right
                    .command
                    .starts_with(query)
                    .cmp(&left.command.starts_with(query))
                    .then_with(|| right.same_host.cmp(&left.same_host))
                    .then_with(|| right.last_used_at.cmp(&left.last_used_at))
                    .then_with(|| left.command.cmp(&right.command))
            });
            for item in history {
                add(
                    context.command_start,
                    item.command.clone(),
                    Category::History,
                );
            }
            for &(command, category) in vocabulary::BUILTINS {
                if worthwhile(command) {
                    add(context.command_start, command.to_owned(), category);
                }
            }
        }
    }
    result
}

/// 将文件名编码成一个不会触发 shell 展开的词。
pub fn quote_word(word: &str) -> String {
    let mut quoted = String::new();
    for (index, character) in word.chars().enumerate() {
        let literal = character.is_alphanumeric()
            || matches!(character, '/' | '.' | '_' | '-' | ':' | '=' | ',')
            || index == 0 && character == '~' && word.starts_with("~/");
        if !literal {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted
}

#[cfg(test)]
mod tests;
