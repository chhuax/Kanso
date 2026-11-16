//! 渲染器测试：对 `CATALOG` 全量渲染是防止 `.ftl` 与 core 声明漂移的安全网。

use kanso_types::message::{
    AUX_KEYS, ArgKind, CATALOG, Endpoint, Message, MessageArg, Reason, SftpOp,
};

use super::*;

const NAME_STR: &str = "report.txt";

fn reasons() -> Vec<ArgValue> {
    (0..Reason::ALL_KEYS.len())
        .map(|i| ArgValue::Reason {
            key: Reason::ALL_KEYS[i].to_owned(),
            detail: (Reason::ALL_KEYS[i] == "reason-other").then(|| "boom".to_owned()),
        })
        .collect()
}

/// 为一个参数构造第 `variant` 号样例；`variant` 递增时轮流覆盖 Select 的每个取值、
/// 每种 Reason、直连与经由、复数的不同分支。
fn sample(kind: ArgKind, variant: usize) -> ArgValue {
    match kind {
        ArgKind::Str => ArgValue::Str(NAME_STR.to_owned()),
        ArgKind::Int => ArgValue::Int([1, 5, 0][variant % 3]),
        ArgKind::ByteSize => ArgValue::ByteSize([0, 1536, 5_000_000][variant % 3]),
        ArgKind::StrList => ArgValue::StrList(match variant % 3 {
            0 => vec![],
            1 => vec!["a".to_owned()],
            _ => vec!["a".to_owned(), "b".to_owned()],
        }),
        ArgKind::Select(keys) => ArgValue::Select(keys[variant % keys.len()].to_owned()),
        ArgKind::Reason => reasons()[variant % Reason::ALL_KEYS.len()].clone(),
        ArgKind::Endpoint => ArgValue::Endpoint {
            host: "example.com".to_owned(),
            port: 22,
            via: if variant.is_multiple_of(2) {
                vec![]
            } else {
                vec!["jump1".to_owned(), "jump2".to_owned()]
            },
        },
    }
}

fn assert_clean(text: &str, ctx: &str) {
    assert!(!text.trim().is_empty(), "{ctx}: 结果为空");
    for bad in ['{', '}', '$', '\u{2068}', '\u{2069}'] {
        assert!(!text.contains(bad), "{ctx}: 含 {bad:?}：{text}");
    }
}

#[test]
fn bundles_build_without_errors() {
    for lang in Lang::ALL {
        let (_, errors) = build_bundle(lang);
        assert!(errors.is_empty(), "{}：{errors:?}", lang.tag());
    }
}

#[test]
fn every_catalog_message_renders_cleanly_in_every_language() {
    for lang in Lang::ALL {
        for spec in CATALOG {
            // 轮数取 Reason 子 key 个数，足以让所有轮流取值的参数各出现一遍。
            for variant in 0..Reason::ALL_KEYS.len() {
                let args: Vec<(String, ArgValue)> = spec
                    .params
                    .iter()
                    .map(|p| (p.name.to_owned(), sample(p.kind, variant)))
                    .collect();
                let ctx = format!("{} / {} / #{variant}", lang.tag(), spec.key);
                let text = try_render_key(spec.key, &args, lang)
                    .unwrap_or_else(|f| panic!("{ctx}: {:?} {}", f.errors, f.text));
                assert_clean(&text, &ctx);
            }
        }
    }
}

#[test]
fn every_aux_key_exists() {
    for lang in Lang::ALL {
        let bundle = bundle(lang);
        for key in AUX_KEYS {
            assert!(bundle.has_message(key), "{}：缺少 {key}", lang.tag());
        }
    }
}

fn en(m: &Message) -> String {
    render(m, &["en"])
}

#[test]
fn plural_english_one_and_other() {
    let n = |count| en(&Message::ZmodemReceiveSummary { count });
    assert_eq!(n(1), "Received 1 file.");
    assert_eq!(n(5), "Received 5 files.");
    assert_eq!(n(0), "Received 0 files.");
    let hops = |max_hops| en(&Message::JumpTooLong { max_hops });
    assert!(hops(1).contains("1 hop."), "{}", hops(1));
    assert!(hops(8).contains("8 hops."), "{}", hops(8));
}

#[test]
fn select_every_value_renders_distinctly_or_default() {
    let ep = Endpoint {
        host: "h".into(),
        port: 22,
        via: vec![],
    };
    for method in [
        kanso_types::profile::AuthMethod::Password,
        kanso_types::profile::AuthMethod::PrivateKey,
        kanso_types::profile::AuthMethod::Agent,
    ] {
        let text = en(&Message::AuthFailed {
            endpoint: ep.clone(),
            user: "u".into(),
            method,
        });
        assert_clean(&text, "auth-failed");
    }
    let pw = en(&Message::AuthFailed {
        endpoint: ep.clone(),
        user: "u".into(),
        method: kanso_types::profile::AuthMethod::Password,
    });
    assert!(pw.starts_with("Password authentication failed"), "{pw}");
    // 未知取值走 `*` 默认分支，不报错。
    let args = vec![
        ("endpoint".to_owned(), sample(ArgKind::Endpoint, 0)),
        ("user".to_owned(), ArgValue::Str("u".into())),
        (
            "method".to_owned(),
            ArgValue::Select("future-method".into()),
        ),
    ];
    let text = try_render_key("auth-failed", &args, Lang::En).unwrap();
    assert!(text.starts_with("Authentication failed"), "{text}");
}

#[test]
fn reason_subkeys_render_in_both_languages() {
    for (lang, reason, expect) in [
        (Lang::En, Reason::TimedOut, "timed out"),
        (Lang::En, Reason::NotFound, "not found"),
        (Lang::ZhHans, Reason::PermissionDenied, "权限"),
    ] {
        let text = render(
            &Message::SftpOpFailed {
                op: SftpOp::List,
                path: "/x".into(),
                reason,
            },
            &[lang.tag()],
        );
        assert!(text.contains(expect), "{text}");
    }
    // `reason-other` 把 detail 原样代入。
    let text = en(&Message::SftpOpFailed {
        op: SftpOp::List,
        path: "/x".into(),
        reason: Reason::Other {
            detail: "weird failure".into(),
        },
    });
    assert!(text.contains("weird failure"), "{text}");
    // 每个 reason 子 key 本身可渲染。
    for r in reasons() {
        let args = vec![
            ("op".to_owned(), ArgValue::Select("list".into())),
            ("path".to_owned(), ArgValue::Str("/x".into())),
            ("reason".to_owned(), r),
        ];
        for lang in Lang::ALL {
            try_render_key("sftp-op-failed", &args, lang).unwrap();
        }
    }
}

#[test]
fn endpoint_direct_and_via() {
    let direct = en(&Message::SshConnectionLost {
        endpoint: Endpoint {
            host: "h".into(),
            port: 22,
            via: vec![],
        },
    });
    assert!(
        direct.contains("h:22") && !direct.contains("via"),
        "{direct}"
    );
    let via = en(&Message::SshConnectionLost {
        endpoint: Endpoint {
            host: "h".into(),
            port: 22,
            via: vec!["j1".into(), "j2".into()],
        },
    });
    assert!(via.contains("h:22 via j1, j2"), "{via}");
    // IPv6 字面量带方括号。
    let v6 = en(&Message::SshConnectionLost {
        endpoint: Endpoint {
            host: "::1".into(),
            port: 22,
            via: vec![],
        },
    });
    assert!(v6.contains("[::1]:22"), "{v6}");
}

#[test]
fn zh_hans_renders_chinese() {
    let text = render(&Message::ProfileHostEmpty {}, &["zh-Hans"]);
    assert!(
        text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "{text}"
    );
    assert_ne!(text, render(&Message::ProfileHostEmpty {}, &["en"]));
}

#[test]
fn language_negotiation_table() {
    let cases: &[(&[&str], Lang)] = &[
        (&["zh-Hans"], Lang::ZhHans),
        (&["zh-Hans-CN"], Lang::ZhHans),
        (&["zh-CN"], Lang::ZhHans),
        (&["zh_CN"], Lang::ZhHans),
        (&["zh"], Lang::ZhHans),
        (&["ZH-hans"], Lang::ZhHans),
        (&["zh-SG"], Lang::ZhHans),
        (&["zh_CN.UTF-8"], Lang::ZhHans),
        (&["zh-Hant"], Lang::En),
        (&["zh-TW"], Lang::En),
        (&["zh-HK"], Lang::En),
        (&["zh-Hant-TW"], Lang::En),
        (&["en"], Lang::En),
        (&["en-US"], Lang::En),
        (&["en_GB"], Lang::En),
        (&["fr"], Lang::En),
        (&["fr", "zh-Hans"], Lang::ZhHans),
        (&["zh-Hant", "zh-Hans"], Lang::ZhHans),
        (&["en", "zh-Hans"], Lang::En),
        (&["zh-Hans", "en"], Lang::ZhHans),
        (&[], Lang::En),
        (&[""], Lang::En),
        (&["zhx"], Lang::En),
    ];
    for (input, want) in cases {
        assert_eq!(Lang::negotiate(input), *want, "{input:?}");
    }
}

#[test]
fn unknown_key_returns_key() {
    assert_eq!(render_key_quiet("no-such-key"), "no-such-key");
}

fn render_key_quiet(key: &str) -> String {
    // 直接走内部函数：公开入口在 debug 下会 `debug_assert!`。
    match try_render_key(key, &[], Lang::En) {
        Ok(t) => t,
        Err(f) => f.text,
    }
}

#[test]
fn missing_variable_returns_partial_text_without_panic() {
    let err = try_render_key("ssh-connection-lost", &[], Lang::En).unwrap_err();
    assert!(!err.errors.is_empty());
    assert!(err.text.contains("The connection to"), "{}", err.text);
}

#[test]
fn fallback_formats() {
    assert_eq!(fallback_byte_size(0), "0 B");
    assert_eq!(fallback_byte_size(999), "999 B");
    assert_eq!(fallback_byte_size(1000), "1 KB");
    assert_eq!(fallback_byte_size(1_200_000), "1.2 MB");
    assert_eq!(fallback_byte_size(u64::MAX), "18446.7 PB");
}

#[test]
fn render_args_cover_all_message_arg_kinds() {
    // MessageArg → ArgValue 的转换不丢信息。
    assert_eq!(
        ArgValue::from(&MessageArg::Select("kex")),
        ArgValue::Select("kex".into())
    );
    assert_eq!(
        ArgValue::from(&MessageArg::Reason(Reason::Other { detail: "d".into() })),
        ArgValue::Reason {
            key: "reason-other".into(),
            detail: Some("d".into())
        }
    );
}

#[test]
fn concurrent_rendering_is_safe() {
    let handles: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(|| {
                for _ in 0..50 {
                    assert_clean(&render(&Message::ProfileHostEmpty {}, &["zh-Hans"]), "t");
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}

/// 两份 `.ftl` 的 key 集合与每条消息引用的占位符必须一致，防止某一语言漏改。
#[test]
fn en_and_zh_hans_ftl_keys_and_placeholders_match() {
    use std::collections::{BTreeMap, BTreeSet};

    fn placeholders(src: &str) -> BTreeMap<String, BTreeSet<String>> {
        let mut out = BTreeMap::new();
        let mut current: Option<(String, BTreeSet<String>)> = None;
        for line in src.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            // 顶层消息：`key = ...`（不以空白开头）
            if !line.starts_with(char::is_whitespace)
                && let Some((key, rest)) = trimmed.split_once('=')
            {
                let key = key.trim();
                if key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && !key.is_empty() {
                    if let Some((prev_key, vars)) = current.take() {
                        out.insert(prev_key, vars);
                    }
                    let mut vars = BTreeSet::new();
                    for cap in rest.split('$').skip(1) {
                        let name: String = cap
                            .chars()
                            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                            .collect();
                        if !name.is_empty() {
                            vars.insert(name);
                        }
                    }
                    current = Some((key.to_owned(), vars));
                    continue;
                }
            }
            if let Some((_, vars)) = current.as_mut() {
                for cap in line.split('$').skip(1) {
                    let name: String = cap
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    if !name.is_empty() {
                        vars.insert(name);
                    }
                }
            }
        }
        if let Some((key, vars)) = current {
            out.insert(key, vars);
        }
        out
    }

    let en = placeholders(EN_FTL);
    let zh = placeholders(ZH_HANS_FTL);
    assert_eq!(
        en.keys().collect::<Vec<_>>(),
        zh.keys().collect::<Vec<_>>(),
        "en.ftl 与 zh-Hans.ftl 的 key 集合不一致"
    );
    for key in en.keys() {
        assert_eq!(
            en[key], zh[key],
            "key `{key}` 的占位符不一致：en={:?} zh-Hans={:?}",
            en[key], zh[key]
        );
    }
}
