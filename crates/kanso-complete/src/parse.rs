#[derive(Clone, Debug)]
struct Word {
    start: usize,
    value: String,
    expand_home: bool,
    expansion: bool,
}

/// 已确认语义的光标前命令片段。
#[derive(Clone, Debug)]
pub struct Context {
    /// 光标前原始输入。
    pub input: String,
    /// 当前词的字节起点。
    pub word_start: usize,
    /// 当前命令片段的字节起点，包含简单前缀。
    pub command_start: usize,
    words: Vec<Word>,
    command_index: usize,
    kubeconfig: bool,
}

/// kubectl 配置名称类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameKind {
    /// 上下文名称。
    Context,
    /// 命名空间名称。
    Namespace,
}

/// 由调用方读取的目录与匹配条件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathRequest {
    /// 展开后的读取目录。
    pub directory: String,
    /// 文件名前缀。
    pub prefix: String,
    /// 接受时保留的路径前缀。
    pub insert_prefix: String,
    /// 是否仅显示目录。
    pub directories_only: bool,
    /// 首部的波浪线是否应交给 shell 展开。
    pub expand_home: bool,
}

/// 解析支持的简单 shell 输入；歧义输入整次省略。
pub fn parse(input: &str) -> Option<Context> {
    let mut words = Vec::new();
    let mut current: Option<Word> = None;
    let mut quote = None;
    let mut escaped = false;
    let mut kubeconfig = false;
    let mut characters = input.char_indices().peekable();
    while let Some((offset, character)) = characters.next() {
        if character.is_control() && character != '\t' {
            return None;
        }
        if escaped {
            if quote == Some('"') && !matches!(character, '$' | '`' | '"' | '\\') {
                current.as_mut()?.value.push('\\');
            }
            current.as_mut()?.value.push(character);
            escaped = false;
            continue;
        }
        if quote != Some('\'') && character == '\\' {
            current.get_or_insert_with(|| Word::new(offset));
            escaped = true;
            continue;
        }
        if quote != Some('\'')
            && (character == '`'
                || character == '$' && characters.peek().is_some_and(|(_, next)| *next == '('))
        {
            return None;
        }
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                if delimiter == '"' && character == '$' {
                    current.as_mut()?.expansion = true;
                }
                current.as_mut()?.value.push(character);
            }
            continue;
        }
        match character {
            '\'' | '"' => {
                current.get_or_insert_with(|| Word::new(offset));
                quote = Some(character);
            }
            '<' | '>' | '(' | ')' => return None,
            '#' if current.is_none() => return None,
            ';' | '|' | '&' => {
                if character == '&' && characters.peek().is_none_or(|(_, next)| *next != '&') {
                    return None;
                }
                if matches!(character, '|' | '&')
                    && characters
                        .peek()
                        .is_some_and(|(_, next)| *next == character)
                {
                    characters.next();
                }
                kubeconfig |= words
                    .iter()
                    .chain(current.iter())
                    .any(|word: &Word| is_kubeconfig(&word.value));
                current = None;
                words.clear();
            }
            character if character.is_whitespace() => {
                if let Some(word) = current.take() {
                    words.push(word);
                }
            }
            _ => {
                let word = current.get_or_insert_with(|| Word::new(offset));
                if word.start == offset && character == '~' {
                    word.expand_home = true;
                }
                if matches!(character, '$' | '*' | '?' | '[' | '{') {
                    word.expansion = true;
                }
                word.value.push(character);
            }
        }
    }
    if quote.is_some() || escaped {
        return None;
    }
    let current = current.unwrap_or_else(|| Word::new(input.len()));
    let word_start = current.start;
    words.push(current);
    kubeconfig |= words.iter().any(|word| is_kubeconfig(&word.value));
    let mut command_index = 0;
    while command_index + 1 < words.len() {
        let value = &words[command_index].value;
        let word = &words[command_index];
        let is_assignment = assignment(value)
            && input[word.start..].starts_with(&format!("{}=", value.split_once('=')?.0));
        if is_assignment || matches!(value.as_str(), "sudo" | "env" | "time" | "nohup") {
            command_index += 1;
            if words[command_index].value.starts_with('-') {
                return None;
            }
        } else {
            break;
        }
    }
    if words[command_index..].iter().any(|word| word.expansion) {
        return None;
    }
    if words.iter().any(|word| {
        let raw = word.value.as_str();
        let original = input[word.start..].trim_start_matches(['\'', '"']);
        [raw, original].iter().any(|raw| {
            raw.as_bytes().get(1) == Some(&b':')
                && raw
                    .as_bytes()
                    .get(2)
                    .is_some_and(|value| matches!(value, b'\\' | b'/'))
        })
    }) {
        return None;
    }
    Some(Context {
        input: input.to_owned(),
        word_start,
        command_start: words[0].start,
        words,
        command_index,
        kubeconfig,
    })
}

fn is_kubeconfig(value: &str) -> bool {
    value == "--kubeconfig" || value.starts_with("--kubeconfig=")
}

impl Word {
    fn new(start: usize) -> Self {
        Self {
            start,
            value: String::new(),
            expand_home: false,
            expansion: false,
        }
    }
}

fn assignment(value: &str) -> bool {
    value.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name.chars().enumerate().all(|(index, character)| {
                character == '_'
                    || character.is_ascii_alphabetic()
                    || index > 0 && character.is_ascii_digit()
            })
    })
}

impl Context {
    /// 当前词是否是命令本身。
    pub fn command_position(&self) -> bool {
        self.command_index + 1 == self.words.len()
    }
    /// 去掉简单前缀后的命令。
    pub fn command(&self) -> &str {
        &self.words[self.command_index].value
    }
    /// 当前词解码后的内容。
    pub fn word(&self) -> &str {
        &self.words.last().expect("解析结果总有当前词").value
    }
    /// 命令参数（含当前词）。
    pub fn arguments(&self) -> Vec<&str> {
        self.words[self.command_index + 1..]
            .iter()
            .map(|word| word.value.as_str())
            .collect()
    }

    /// 当前是否在 Kubernetes 配置名称值位置。
    pub fn name_kind(&self) -> Option<NameKind> {
        let arguments = self.arguments();
        if self.command_position() || self.kubeconfig {
            return None;
        }
        match self.command() {
            "kubectx" if !self.word().starts_with('-') => return Some(NameKind::Context),
            "kubens" if !self.word().starts_with('-') => return Some(NameKind::Namespace),
            "kubectl" => {}
            _ => return None,
        }
        let previous = arguments.iter().rev().nth(1).copied().unwrap_or_default();
        if self.word().starts_with("--context=") || previous == "--context" {
            return Some(NameKind::Context);
        } else if self.word().starts_with("--namespace=")
            || matches!(previous, "-n" | "--namespace")
        {
            return Some(NameKind::Namespace);
        }
        if self.word().starts_with('-') {
            return None;
        }
        let mut positional = Vec::new();
        let mut skip = false;
        for argument in &arguments[..arguments.len() - 1] {
            if skip {
                skip = false;
            } else if matches!(
                *argument,
                "--context"
                    | "--namespace"
                    | "-n"
                    | "--kubeconfig"
                    | "--cluster"
                    | "--user"
                    | "--server"
                    | "--request-timeout"
            ) {
                skip = true;
            } else if !argument.starts_with('-') {
                positional.push(*argument);
            }
        }
        (!skip
            && matches!(
                positional.as_slice(),
                ["config", "use-context" | "set-context" | "delete-context"]
            ))
        .then_some(NameKind::Context)
    }

    /// 等号选项的保留前缀与名称前缀。
    pub fn name_prefix(&self) -> (&str, &str) {
        if self.command() == "kubectl"
            && (self.word().starts_with("--context=") || self.word().starts_with("--namespace="))
        {
            let (option, value) = self.word().split_once('=').expect("已确认名称选项含等号");
            (&self.word()[..option.len() + 1], value)
        } else {
            ("", self.word())
        }
    }

    /// 按命令语境与可信目录生成读取请求，不执行 IO。
    pub fn path_request(&self, cwd: Option<&str>, home: Option<&str>) -> Option<PathRequest> {
        let word = self.word();
        let current = self.words.last().expect("解析结果总有当前词");
        if word.starts_with('-') || current.expansion || word.contains(':') {
            return None;
        }
        let arguments = self.arguments();
        let directories_only = matches!(self.command(), "cd" | "pushd" | "mkdir");
        let explicit = word.contains('/') || word.starts_with(['~', '.']);
        let allowed = match self.command() {
            "git" => {
                matches!(
                    arguments.first().copied(),
                    Some("add" | "restore" | "rm" | "diff")
                ) || arguments.first() == Some(&"checkout") && arguments.contains(&"--")
            }
            "kubectl" | "kubectx" | "kubens" => false,
            "cd" | "pushd" | "ls" | "cat" | "less" | "tail" | "head" | "vim" | "vi" | "nano"
            | "code" | "cp" | "mv" | "rm" | "mkdir" | "touch" | "chmod" | "chown" | "source"
            | "tar" | "scp"
                if !self.command_position() =>
            {
                true
            }
            _ => explicit,
        };
        if !allowed || self.command() == "git" && arguments.len() < 2 {
            return None;
        }
        let (insert_prefix, prefix) = if matches!(word, "." | ".." | "~") {
            (format!("{word}/"), String::new())
        } else if let Some(index) = word.rfind('/') {
            (word[..=index].to_owned(), word[index + 1..].to_owned())
        } else {
            (String::new(), word.to_owned())
        };
        let directory = if current.expand_home && insert_prefix.starts_with("~/") {
            let relative = &insert_prefix[2..];
            format!("{}/{relative}", home?.trim_end_matches('/'))
        } else if current.expand_home && insert_prefix.starts_with('~') {
            return None;
        } else if insert_prefix.starts_with('/') {
            insert_prefix.clone()
        } else {
            format!(
                "{}/{}",
                cwd.filter(|value| value.starts_with('/'))?
                    .trim_end_matches('/'),
                insert_prefix
            )
        };
        Some(PathRequest {
            directory,
            prefix,
            insert_prefix,
            directories_only,
            expand_home: current.expand_home,
        })
    }
}
