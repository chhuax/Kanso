//! 多语言目录校验（`check-locales`）：用 `kanso-types` 的 `CATALOG` / `AUX_KEYS` 对照 `locales/*.ftl`。
//!
//! 规则见 `docs/i18n.md`。所有问题收集完再一次性输出，避免改一个报一个。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::process::ExitCode;

use fluent_syntax::ast::{
    CallArguments, Entry, Expression, InlineExpression, Pattern, PatternElement,
};
use fluent_syntax::parser;
use kanso_types::message::{AUX_KEYS, ArgKind, CATALOG};

/// 期望的一个参数：变量名，以及（Select 类型时）必须覆盖的全部分支。
#[derive(Clone, Debug, PartialEq, Eq)]
struct ExpParam {
    name: String,
    select_keys: Option<Vec<String>>,
}

/// 期望的消息表：key 到参数表。
type Expected = BTreeMap<String, Vec<ExpParam>>;

/// 由 `CATALOG` 与 `AUX_KEYS` 构造期望表。
///
/// 子 key 的变量是固定约定（平台渲染消息前，先把 Reason / Endpoint 参数用这些子 key
/// 渲染成字符串再代入）：`reason-other` 用 `$detail`，`endpoint-direct` 用 `$host_port`，
/// `endpoint-via` 用 `$host_port` 与 `$via`，其余 `reason-*` 无变量。
fn expected_from_catalog() -> Expected {
    let mut map = Expected::new();
    for spec in CATALOG {
        let params = spec
            .params
            .iter()
            .map(|p| ExpParam {
                name: p.name.to_string(),
                select_keys: match p.kind {
                    ArgKind::Select(keys) => Some(keys.iter().map(|k| k.to_string()).collect()),
                    _ => None,
                },
            })
            .collect();
        map.insert(spec.key.to_string(), params);
    }
    for key in AUX_KEYS {
        let vars: &[&str] = match *key {
            "reason-other" => &["detail"],
            "endpoint-direct" => &["host_port"],
            "endpoint-via" => &["host_port", "via"],
            _ => &[],
        };
        let params = vars
            .iter()
            .map(|v| ExpParam {
                name: v.to_string(),
                select_keys: None,
            })
            .collect();
        map.insert(key.to_string(), params);
    }
    map
}

/// 遍历一个 Pattern，收集引用的变量，以及每个以变量为选择器的 select 表达式的分支名。
#[derive(Default)]
struct Walk {
    vars: BTreeSet<String>,
    /// (选择器变量名, 该表达式的全部分支名)
    selects: Vec<(String, Vec<String>)>,
}

impl Walk {
    fn pattern(&mut self, p: &Pattern<&str>) {
        for el in &p.elements {
            if let PatternElement::Placeable { expression } = el {
                self.expression(expression);
            }
        }
    }

    fn expression(&mut self, e: &Expression<&str>) {
        match e {
            Expression::Inline(i) => self.inline(i),
            Expression::Select { selector, variants } => {
                self.inline(selector);
                if let InlineExpression::VariableReference { id } = selector {
                    let keys = variants
                        .iter()
                        .map(|v| match &v.key {
                            fluent_syntax::ast::VariantKey::Identifier { name } => name.to_string(),
                            fluent_syntax::ast::VariantKey::NumberLiteral { value } => {
                                value.to_string()
                            }
                        })
                        .collect();
                    self.selects.push((id.name.to_string(), keys));
                }
                for v in variants {
                    self.pattern(&v.value);
                }
            }
        }
    }

    fn args(&mut self, a: &CallArguments<&str>) {
        for p in &a.positional {
            self.inline(p);
        }
        for n in &a.named {
            self.inline(&n.value);
        }
    }

    fn inline(&mut self, i: &InlineExpression<&str>) {
        match i {
            InlineExpression::VariableReference { id } => {
                self.vars.insert(id.name.to_string());
            }
            InlineExpression::FunctionReference { arguments, .. } => self.args(arguments),
            InlineExpression::TermReference { arguments, .. } => {
                if let Some(a) = arguments {
                    self.args(a);
                }
            }
            InlineExpression::Placeable { expression } => self.expression(expression),
            InlineExpression::StringLiteral { .. }
            | InlineExpression::NumberLiteral { .. }
            | InlineExpression::MessageReference { .. } => {}
        }
    }
}

/// 把字节偏移换算成 `行:列`（从 1 开始），用于指出语法错误位置。
fn line_col(src: &str, pos: usize) -> (usize, usize) {
    let pos = pos.min(src.len());
    let before = &src[..pos];
    let line = before.bytes().filter(|b| *b == b'\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, col)
}

/// 校验一个 .ftl 文件的内容，返回全部问题（空表示通过）。
fn check_source(file: &str, src: &str, expected: &Expected) -> Vec<String> {
    let mut errors = vec![];
    let resource = match parser::parse(src) {
        Ok(r) => r,
        Err((r, errs)) => {
            for e in errs {
                let (l, c) = line_col(src, e.pos.start);
                errors.push(format!("{file}:{l}:{c}：语法错误（{e}）"));
            }
            r
        }
    };

    let mut seen = BTreeSet::new();
    for entry in &resource.body {
        let Entry::Message(msg) = entry else { continue };
        let key = msg.id.name;
        if !seen.insert(key) {
            errors.push(format!("{file}：key `{key}` 重复定义"));
            continue;
        }
        let Some(params) = expected.get(key) else {
            errors.push(format!(
                "{file}：未使用的 key `{key}`（CATALOG / AUX_KEYS 中不存在）"
            ));
            continue;
        };

        let mut walk = Walk::default();
        if let Some(v) = &msg.value {
            walk.pattern(v);
        }
        for a in &msg.attributes {
            walk.pattern(&a.value);
        }

        let declared: BTreeSet<&str> = params.iter().map(|p| p.name.as_str()).collect();
        for p in &declared {
            if !walk.vars.contains(*p) {
                errors.push(format!("{file}：`{key}` 没有引用已声明的参数 ${p}"));
            }
        }
        for v in &walk.vars {
            if !declared.contains(v.as_str()) {
                errors.push(format!("{file}：`{key}` 引用了未声明的变量 ${v}"));
            }
        }
        for p in params {
            let Some(all) = &p.select_keys else { continue };
            for (var, branches) in &walk.selects {
                if *var != p.name {
                    continue;
                }
                for k in all {
                    if !branches.contains(k) {
                        errors.push(format!(
                            "{file}：`{key}` 对 ${var} 的选择表达式缺少分支 [{k}]"
                        ));
                    }
                }
            }
        }
    }

    for key in expected.keys() {
        if !seen.contains(key.as_str()) {
            errors.push(format!("{file}：缺少 key `{key}`"));
        }
    }
    errors
}

/// `cargo xtask check-locales`。
pub fn check() -> ExitCode {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask 位于仓库根目录之下");
    let dir = root.join("locales");
    let mut files: Vec<_> = match fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "ftl"))
            .collect(),
        Err(e) => {
            eprintln!("check-locales：无法读取 {}（{e}）", dir.display());
            return ExitCode::FAILURE;
        }
    };
    files.sort();
    if files.is_empty() {
        eprintln!("check-locales：{} 下没有 .ftl 文件", dir.display());
        return ExitCode::FAILURE;
    }

    let expected = expected_from_catalog();
    let mut errors = vec![];
    for path in &files {
        let name = format!("locales/{}", path.file_name().unwrap().to_string_lossy());
        match fs::read_to_string(path) {
            Ok(src) => errors.extend(check_source(&name, &src, &expected)),
            Err(e) => errors.push(format!("{name}：无法读取（{e}）")),
        }
    }

    if errors.is_empty() {
        eprintln!(
            "check-locales：通过（{} 种语言，{} 条消息）",
            files.len(),
            expected.len()
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("check-locales：发现 {} 个问题", errors.len());
        for e in &errors {
            eprintln!("  - {e}");
        }
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exp() -> Expected {
        let mut m = Expected::new();
        m.insert("a-plain".into(), vec![]);
        m.insert(
            "a-name".into(),
            vec![ExpParam {
                name: "name".into(),
                select_keys: None,
            }],
        );
        m.insert(
            "a-sel".into(),
            vec![ExpParam {
                name: "kind".into(),
                select_keys: Some(vec!["x".into(), "y".into()]),
            }],
        );
        m
    }

    const GOOD: &str = "a-plain = 文本\na-name = { $name }\na-sel = { $kind ->\n    [x] X\n    [y] Y\n   *[other] O\n}\n";

    #[test]
    fn good_passes() {
        assert_eq!(check_source("f", GOOD, &exp()), Vec::<String>::new());
    }

    #[test]
    fn missing_key() {
        let src = "a-plain = t\na-name = { $name }\n";
        let errs = check_source("f", src, &exp());
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("缺少 key `a-sel`"));
    }

    #[test]
    fn extra_key() {
        let src = format!("{GOOD}zzz = t\n");
        let errs = check_source("f", &src, &exp());
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("未使用的 key `zzz`"));
    }

    #[test]
    fn extra_variable() {
        let src = GOOD.replace("a-name = { $name }", "a-name = { $name } { $other }");
        let errs = check_source("f", &src, &exp());
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("未声明的变量 $other"));
    }

    #[test]
    fn missing_variable() {
        let src = GOOD.replace("a-name = { $name }", "a-name = 没有变量");
        let errs = check_source("f", &src, &exp());
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("没有引用已声明的参数 $name"));
    }

    #[test]
    fn select_missing_branch() {
        let src = GOOD.replace("    [y] Y\n", "");
        let errs = check_source("f", &src, &exp());
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("缺少分支 [y]"));
    }

    #[test]
    fn junk_reported_with_position() {
        let src = format!("{GOOD}broken = {{ $\n");
        let errs = check_source("f", &src, &exp());
        assert!(
            errs.iter()
                .any(|e| e.starts_with("f:") && e.contains("语法错误")),
            "{errs:?}"
        );
    }

    #[test]
    fn real_catalog_expectations() {
        let e = expected_from_catalog();
        assert!(e.contains_key("auth-failed") && e.contains_key("endpoint-via"));
        assert_eq!(e["endpoint-via"].len(), 2);
    }
}
