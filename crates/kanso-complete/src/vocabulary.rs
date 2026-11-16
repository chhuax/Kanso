use crate::{Category, Context};

pub(crate) const BUILTINS: &[(&str, Category)] = &[
    ("kubectl get pods", Category::Kubernetes),
    ("kubectl get services", Category::Kubernetes),
    ("kubectl describe pod ", Category::Kubernetes),
    ("kubectl logs -f ", Category::Kubernetes),
    ("docker ps -a", Category::Docker),
    ("docker images", Category::Docker),
    ("docker logs -f ", Category::Docker),
    ("docker compose up -d", Category::Docker),
    ("git status", Category::Git),
    ("git diff", Category::Git),
    ("git log --oneline", Category::Git),
    ("git add ", Category::Git),
    ("systemctl status ", Category::Systemd),
    ("systemctl restart ", Category::Systemd),
    ("journalctl -u ", Category::Systemd),
    ("journalctl -f", Category::Systemd),
];

const GIT: &[&str] = &[
    "add", "branch", "checkout", "clone", "commit", "diff", "fetch", "init", "log", "merge",
    "pull", "push", "rebase", "remote", "reset", "restore", "rm", "show", "stash", "status",
    "switch", "tag",
];
const KUBECTL: &[&str] = &[
    "apply",
    "config",
    "create",
    "delete",
    "describe",
    "edit",
    "exec",
    "explain",
    "get",
    "logs",
    "port-forward",
    "rollout",
    "scale",
    "top",
    "version",
];
const RESOURCES: &[&str] = &[
    "pods",
    "pod",
    "po",
    "services",
    "service",
    "svc",
    "deployments",
    "deployment",
    "deploy",
    "namespaces",
    "namespace",
    "ns",
    "nodes",
    "node",
    "no",
    "configmaps",
    "secrets",
    "ingresses",
    "statefulsets",
    "daemonsets",
    "jobs",
    "cronjobs",
];

pub(crate) fn words(context: &Context) -> Vec<(&'static str, Category)> {
    if context.command_position() || context.name_kind().is_some() {
        return Vec::new();
    }
    let arguments = context.arguments();
    let command = context.command();
    if context.word().starts_with('-') {
        let options: &[&str] = match command {
            "git" => match arguments.first().copied() {
                Some("diff" | "log" | "show") => {
                    &["--stat", "--name-only", "--oneline", "--color", "--help"]
                }
                Some("commit") => &["--message", "--amend", "--all", "--help"],
                Some("checkout" | "restore") => &["--", "--staged", "--source", "--help"],
                _ => &["--help", "--version", "--no-pager"],
            },
            "kubectl" => &[
                "--context",
                "--namespace",
                "-n",
                "--kubeconfig",
                "--all-namespaces",
                "-A",
                "--output",
                "-o",
                "--selector",
                "-l",
                "--watch",
                "--help",
            ],
            _ => &[],
        };
        return options
            .iter()
            .map(|option| (*option, command_category(command)))
            .collect();
    }
    let mut positional = Vec::new();
    let mut skip = false;
    for argument in &arguments[..arguments.len().saturating_sub(1)] {
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
                | "-o"
                | "--output"
                | "-l"
                | "--selector"
        ) {
            skip = true;
        } else if !argument.starts_with('-') {
            positional.push(*argument);
        }
    }
    if skip {
        return Vec::new();
    }
    if command == "kubectl" {
        let (first, second) = if positional.is_empty() {
            (KUBECTL, RESOURCES)
        } else {
            (RESOURCES, KUBECTL)
        };
        return first
            .iter()
            .chain(second)
            .map(|word| (*word, Category::Kubernetes))
            .collect();
    }
    if positional.is_empty() {
        return match command {
            "git" => GIT,
            _ => &[],
        }
        .iter()
        .map(|word| (*word, command_category(command)))
        .collect();
    }
    Vec::new()
}

fn command_category(command: &str) -> Category {
    match command {
        "git" => Category::Git,
        "kubectl" => Category::Kubernetes,
        _ => unreachable!("仅 Git 和 kubectl 提供工具词汇"),
    }
}
