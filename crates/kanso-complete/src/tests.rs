use super::*;

fn candidates(input: &str, history: &[History]) -> Vec<Candidate> {
    parse(input).map_or_else(Vec::new, |context| {
        complete(&context, history, None, &[], &KubeNames::default())
    })
}

fn history(command: &str, same_host: bool, last_used_at: i64) -> History {
    History {
        command: command.into(),
        same_host,
        last_used_at,
    }
}

#[test]
fn d4_shell_segments_quotes_escapes_and_simple_prefixes() {
    for input in [
        "git st",
        "sudo git st",
        "env A=x git st",
        "A=x time nohup git st",
        "echo 'a; b' && git st",
        "echo \"a|b\" || git st",
        "echo a\\;b; git st",
        "cat x | git st",
    ] {
        let context = parse(input).unwrap();
        assert_eq!(context.command(), "git", "{input}");
        assert!(
            candidates(input, &[])
                .iter()
                .any(|candidate| candidate.replacement == "status"),
            "{input}"
        );
        assert_eq!(candidates(input, &[])[0].delete_codepoints, 2);
    }
    assert_eq!(parse("cat 'a b'").unwrap().word(), "a b");
    assert_eq!(parse("cat a\\ b").unwrap().word(), "a b");
    assert_eq!(parse("echo ';' ; ").unwrap().command(), "");
    for input in ["'A=x' git st", "A\\=x git st"] {
        assert_eq!(parse(input).unwrap().command(), "A=x");
        assert!(candidates(input, &[]).is_empty());
    }
    assert!(parse("echo # ignored; git st").is_none());
    assert!(parse("echo '# literal' | git st").is_some());
}

#[test]
fn d4_ambiguous_input_has_no_candidates() {
    for input in [
        "cat 'abc",
        "cat \"abc",
        "cat a\\",
        "cat <<EOF",
        "cat > file",
        "cat < file",
        "echo $(pwd)",
        "echo `pwd`",
        "echo \"$(pwd)\"",
        "sudo -u root git st",
        "env -i git st",
        "time -p git st",
        "echo x & git st",
        "cat C:\\tmp\\f",
        "cat C:/tmp/f",
        "git st\n",
    ] {
        assert!(parse(input).is_none(), "{input}");
    }
    assert!(parse("echo '$(literal)' | git st").is_some());
}

#[test]
fn d2_full_commands_only_at_command_position_and_replace_fragment() {
    let history = [history("docker ps --all", true, 1)];
    assert!(candidates("echo do", &history).is_empty());
    for fragment in ["do", "sudo do", "env A=x do", "A=x time nohup do"] {
        let input = format!("echo x && {fragment}");
        let candidate = &candidates(&input, &history)[0];
        assert_eq!(candidate.replacement, "docker ps --all");
        assert_eq!(
            candidate.delete_codepoints as usize,
            fragment.chars().count()
        );
        let context = parse(&input).unwrap();
        assert_eq!(&context.input[..context.command_start], "echo x && ");
    }
}

#[test]
fn history_threshold_case_order_and_deduplication() {
    let records = [
        history("other git status", true, 100),
        history("git log", false, 90),
        history("git status", true, 1),
        history("git status", false, 200),
        history("Git checkout", true, 300),
    ];
    let results = candidates("git", &records);
    assert_eq!(results[0].replacement, "git status");
    assert_eq!(results[1].replacement, "git log");
    assert_eq!(results[2].replacement, "other git status");
    assert_eq!(
        results
            .iter()
            .filter(|candidate| candidate.replacement == "git status")
            .count(),
        1
    );
    assert!(
        !results
            .iter()
            .any(|candidate| candidate.replacement.starts_with("Git"))
    );
    assert!(candidates("g", &records).is_empty());
    assert!(candidates("ab", &[history("abcd", true, 0)]).is_empty());
    assert!(
        candidates("中文", &[history("中文命令行", true, 0)])
            .iter()
            .any(|candidate| candidate.replacement == "中文命令行")
    );
}

#[test]
fn tool_words_precede_history_and_do_not_require_two_characters() {
    assert_eq!(candidates("git s", &[])[0].replacement, "show");
    assert_eq!(candidates("git ", &[]).len(), MAX_CANDIDATES);
    assert_eq!(candidates("kubectl get po", &[])[0].replacement, "pods");
    assert_eq!(
        candidates("kubectl get --na", &[])[0].replacement,
        "--namespace"
    );
    assert_eq!(candidates("git commit --am", &[])[0].replacement, "--amend");
    assert!(candidates("git checkout feature", &[]).is_empty());
    assert!(candidates("kubectl logs instance", &[]).is_empty());
    assert!(!candidates("kubectl -n dev g", &[]).is_empty());
}

#[test]
fn d1_no_relative_paths_without_cwd_and_no_home_fallback() {
    assert!(
        parse("cat a")
            .unwrap()
            .path_request(None, Some("/home/me"))
            .is_none()
    );
    assert_eq!(
        parse("cat /tmp/a")
            .unwrap()
            .path_request(None, None)
            .unwrap()
            .directory,
        "/tmp/"
    );
    assert_eq!(
        parse("cat ~/a")
            .unwrap()
            .path_request(None, Some("/home/me"))
            .unwrap()
            .directory,
        "/home/me/"
    );
    assert!(parse("cat ~/a").unwrap().path_request(None, None).is_none());
}

#[test]
fn d3_path_applicability() {
    for command in [
        "cd", "pushd", "ls", "cat", "less", "tail", "head", "vim", "vi", "nano", "code", "cp",
        "mv", "rm", "mkdir", "touch", "chmod", "chown", "source", "tar", "scp",
    ] {
        assert!(
            parse(&format!("{command} a"))
                .unwrap()
                .path_request(Some("/cwd"), None)
                .is_some(),
            "{command}"
        );
    }
    for input in [
        "unknown a",
        "git checkout a",
        "git switch a",
        "git branch a",
        "kubectl get a",
        "scp host:/a",
        "cat --opt",
    ] {
        assert!(
            parse(input)
                .unwrap()
                .path_request(Some("/cwd"), None)
                .is_none(),
            "{input}"
        );
    }
    for input in [
        "unknown ./a",
        "unknown ../a",
        "unknown ~/a",
        "./bin/a",
        "git add a",
        "git checkout -- a",
        "git restore a",
        "git rm a",
        "git diff a",
    ] {
        assert!(
            parse(input)
                .unwrap()
                .path_request(Some("/cwd"), Some("/home/me"))
                .is_some(),
            "{input}"
        );
    }
}

#[test]
fn paths_sort_directories_filter_hidden_and_quote_special_characters() {
    let entries: Vec<_> = [
        ("z", true),
        ("a", false),
        (".hidden", true),
        ("a b'$;", true),
        ("a\nb", false),
    ]
    .into_iter()
    .map(|(name, directory)| DirectoryEntry {
        name: name.into(),
        directory,
    })
    .collect();
    let context = parse("cat ").unwrap();
    let path = context.path_request(Some("/cwd"), None).unwrap();
    let result = complete(&context, &[], Some(&path), &entries, &KubeNames::default());
    assert_eq!(
        result
            .iter()
            .map(|candidate| candidate.replacement.as_str())
            .collect::<Vec<_>>(),
        ["a\\ b\\'\\$\\;/", "z/", "a"]
    );
    for command in ["cd ", "pushd ", "mkdir "] {
        let context = parse(command).unwrap();
        let path = context.path_request(Some("/cwd"), None).unwrap();
        assert!(
            complete(&context, &[], Some(&path), &entries, &KubeNames::default())
                .iter()
                .all(|candidate| candidate.replacement.ends_with('/'))
        );
    }
    let context = parse("cat .h").unwrap();
    let path = context.path_request(Some("/cwd"), None).unwrap();
    assert_eq!(
        complete(&context, &[], Some(&path), &entries, &KubeNames::default())[0].replacement,
        ".hidden/"
    );
    for word in [".", ".."] {
        let context = parse(&format!("cat {word}")).unwrap();
        assert_eq!(context.path_request(Some("/cwd"), None).unwrap().prefix, "");
    }
}

#[test]
fn paths_only_replace_current_word_and_preserve_tilde() {
    let context = parse("echo x && cat ~/中文\\ 文").unwrap();
    let path = context.path_request(None, Some("/home/me")).unwrap();
    let result = complete(
        &context,
        &[],
        Some(&path),
        &[DirectoryEntry {
            name: "中文 文件".into(),
            directory: false,
        }],
        &KubeNames::default(),
    );
    assert_eq!(
        result[0].delete_codepoints,
        "~/中文\\ 文".chars().count() as u32
    );
    assert_eq!(result[0].replacement, "~/中文\\ 文件");
}

#[test]
fn quoted_and_escaped_paths_preserve_literal_shell_characters() {
    for input in ["cat '~/x'", "cat \\~/x", "cat \"~/x\""] {
        let context = parse(input).unwrap();
        let path = context
            .path_request(Some("/cwd"), Some("/home/me"))
            .unwrap();
        assert_eq!(path.directory, "/cwd/~/");
        let result = complete(
            &context,
            &[],
            Some(&path),
            &[DirectoryEntry {
                name: "xyz".into(),
                directory: false,
            }],
            &KubeNames::default(),
        );
        assert_eq!(result[0].replacement, "\\~/xyz");
    }
    for input in ["cat '$file'", "cat \\$file"] {
        assert!(
            parse(input)
                .unwrap()
                .path_request(Some("/cwd"), None)
                .is_some()
        );
    }
    for input in [
        "cat $file",
        "cat \"$file\"",
        "cat *.txt",
        "git $command -",
        "git * -",
    ] {
        assert!(parse(input).is_none());
    }
    assert_eq!(parse("VAR=$HOME git st").unwrap().command(), "git");
    assert_eq!(parse("cat \"a\\ b\"").unwrap().word(), "a\\ b");
}

#[test]
fn d5_directory_scan_limit_applies_before_matching() {
    let mut entries: Vec<_> = (0..MAX_DIRECTORY_ENTRIES)
        .map(|index| DirectoryEntry {
            name: format!("a{index}"),
            directory: false,
        })
        .collect();
    entries.push(DirectoryEntry {
        name: "z-hidden-by-limit".into(),
        directory: false,
    });
    let context = parse("cat z").unwrap();
    let path = context.path_request(Some("/cwd"), None).unwrap();
    assert!(complete(&context, &[], Some(&path), &entries, &KubeNames::default()).is_empty());
    assert!(
        candidates(
            "git",
            &(0..100)
                .map(|index| history(&format!("git command-{index}"), true, index))
                .collect::<Vec<_>>()
        )
        .len()
            <= 8
    );
}

#[test]
fn d6_builtin_categories_are_small_and_make_no_safety_claim() {
    assert!(vocabulary::BUILTINS.len() <= 20);
    for &(command, category) in vocabulary::BUILTINS {
        let expected = match command.split_whitespace().next().unwrap() {
            "kubectl" => Category::Kubernetes,
            "docker" => Category::Docker,
            "git" => Category::Git,
            "systemctl" | "journalctl" => Category::Systemd,
            other => panic!("不支持的内置命令：{other}"),
        };
        assert_eq!(category, expected);
    }
}

#[test]
fn d7_kubeconfig_names_merge_without_inventing_instances() {
    let config = "contexts:\n- name: prod\n  context:\n    namespace: apps\n- name: dev\n  context:\n    namespace: dev\n";
    let names = KubeNames::from_configs(&[config.into(), config.into(), "invalid: [".into()]);
    assert_eq!(names.contexts, ["dev", "prod"]);
    assert_eq!(names.namespaces, ["apps", "dev"]);
    for (input, expected) in [
        ("kubectl --context p", "prod"),
        ("kubectl get -n a", "apps"),
        ("kubectl --namespace=d", "--namespace=dev"),
        ("kubectl config use-context p", "prod"),
        ("kubectl config set-context p", "prod"),
        ("kubectl config delete-context p", "prod"),
        ("kubectl -n apps config use-context p", "prod"),
        ("kubectl config --context dev set-context p", "prod"),
        ("kubectl config set-context --cluster cluster p", "prod"),
        ("sudo kubectx p", "prod"),
        ("kubectx -d p", "prod"),
        ("kubens a", "apps"),
    ] {
        let context = parse(input).unwrap();
        assert_eq!(
            complete(&context, &[], None, &[], &names)[0].replacement,
            expected,
            "{input}"
        );
    }
    for input in [
        "other --context p",
        "kubectl --kubeconfig other --context p",
        "kubectl --kubeconfig=other -n a",
        "kubectl --kubeconfig other get po; kubectl --context p",
        "kubectl --context missing",
        "kubectl config use-context missing",
        "kubectl config set-context prod missing",
        "kubectl config set-context --cluster p",
        "kubectl config rename-context prod",
        "kubectl --kubeconfig other config use-context prod",
        "kubectl config --kubeconfig=other set-context prod",
        "kubectx --kubeconfig=other p",
        "kubens --kubeconfig other a",
        "kubectx missing",
        "kubens missing",
    ] {
        let context = parse(input).unwrap();
        assert!(
            context.name_kind().is_none() || !input.contains("--kubeconfig"),
            "{input}"
        );
        assert!(
            complete(&context, &[], None, &[], &names).is_empty(),
            "{input}"
        );
    }
    assert!(KubeNames::from_configs(&[]).contexts.is_empty());
}

#[test]
fn d7_names_never_fall_back_to_paths() {
    for command in [
        "kubectl config use-context",
        "kubectl config set-context",
        "kubectl config delete-context",
        "kubectx",
        "kubens",
    ] {
        assert!(parse(command).unwrap().name_kind().is_none(), "{command}");
        for word in ["", "missing", "./missing", "missing=value"] {
            let input = format!("{command} {word}");
            let context = parse(&input).unwrap();
            let expected = if command == "kubens" {
                NameKind::Namespace
            } else {
                NameKind::Context
            };
            assert_eq!(context.name_kind(), Some(expected), "{input}");
            assert_eq!(context.name_prefix(), ("", word), "{input}");
            assert!(
                context
                    .path_request(Some("/cwd"), Some("/home/me"))
                    .is_none(),
                "{input}"
            );
            assert!(
                complete(&context, &[], None, &[], &KubeNames::default()).is_empty(),
                "{input}"
            );
        }
    }
}

#[test]
fn d7_kubectl_vocabulary_available_at_any_argument_position() {
    for prefix in [
        "kubectl",
        "kubectl get",
        "kubectl logs instance",
        "kubectl -n apps get pods",
        "kubectl --kubeconfig other get",
    ] {
        for (word, expected) in [("ge", "get"), ("po", "pods"), ("--na", "--namespace")] {
            let input = format!("{prefix} {word}");
            let result = candidates(&input, &[]);
            assert!(
                result
                    .iter()
                    .any(|candidate| candidate.replacement == expected),
                "{input}"
            );
            assert!(
                result
                    .iter()
                    .all(|candidate| candidate.category == Category::Kubernetes)
            );
        }
    }
}

#[test]
fn candidates_carry_categories_without_display_strings() {
    assert_eq!(
        candidates("gi", &[history("git status", true, 1)])[0].category,
        Category::History
    );
    let context = parse("cat ").unwrap();
    let path = context.path_request(Some("/cwd"), None).unwrap();
    let entries = [
        DirectoryEntry {
            name: "folder".into(),
            directory: true,
        },
        DirectoryEntry {
            name: "file".into(),
            directory: false,
        },
    ];
    let result = complete(&context, &[], Some(&path), &entries, &KubeNames::default());
    assert_eq!(
        result
            .iter()
            .map(|candidate| candidate.category)
            .collect::<Vec<_>>(),
        [Category::Directory, Category::File]
    );
}

#[test]
fn d8_replacement_counts_unicode_codepoints() {
    let result = candidates("中e\u{301}", &[history("中e\u{301}命令测试", true, 0)]);
    assert_eq!(result[0].delete_codepoints, 3);
}
