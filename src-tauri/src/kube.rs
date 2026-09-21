//! What a kubeconfig knows: the contexts it offers and the namespaces they
//! name. The completion popup asks for these so `kubectl -n ` can offer a
//! namespace and `--context ` a context — the part of kubectl's vocabulary
//! that is per-machine rather than per-tool, and that no shipped table can
//! carry.
//!
//! Nothing here runs `kubectl`. The file is read directly: the question is
//! about configuration, not about a cluster, and a terminal that ran a
//! subprocess on every keystroke would be answering slowly and — on a host
//! with no kubeconfig at all — loudly. A namespace that exists in the cluster
//! but is named nowhere in the file is simply not offered.

use std::path::PathBuf;
#[cfg(test)]
use std::path::Path;

use serde::Serialize;

/// The names a kubeconfig offers, each sorted and without duplicates.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct KubeNames {
    /// Every `contexts[].name`, plus `current-context`.
    pub contexts: Vec<String>,
    /// The namespaces the file names: each context's `namespace`, plus any in
    /// a `namespaces:` list. `default` is always there, because that is where
    /// a context without one lands.
    pub namespaces: Vec<String>,
}

/// Reads the names a kubeconfig offers.
///
/// The files are the ones kubectl would read: `KUBECONFIG` when it is set (a
/// list, separated the way the platform separates paths), the user's
/// `~/.kube/config` otherwise. Several merge, which is how kubectl treats a
/// list; a duplicate name is kept once.
pub fn names() -> KubeNames {
    let mut names = base();
    for file in config_files() {
        if let Ok(text) = std::fs::read_to_string(&file) {
            collect(&text, &mut names);
        }
    }
    sort(&mut names);
    names
}

/// The names out of a kubeconfig at a known path: the tests' way in, since
/// they must not read the user's own file to have an answer to check.
#[cfg(test)]
pub fn names_in(path: &Path) -> KubeNames {
    let mut names = base();
    if let Ok(text) = std::fs::read_to_string(path) {
        collect(&text, &mut names);
    }
    sort(&mut names);
    names
}

fn base() -> KubeNames {
    KubeNames {
        contexts: Vec::new(),
        namespaces: vec!["default".to_string()],
    }
}

fn sort(names: &mut KubeNames) {
    names.contexts.sort();
    names.contexts.dedup();
    names.namespaces.sort();
    names.namespaces.dedup();
}

/// The kubeconfig files kubectl would read, in its own order.
fn config_files() -> Vec<PathBuf> {
    let listed = std::env::var_os("KUBECONFIG")
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    let files: Vec<PathBuf> = listed
        .split(if cfg!(windows) { ';' } else { ':' })
        .filter(|part| !part.is_empty())
        .map(PathBuf::from)
        .collect();
    if !files.is_empty() {
        return files;
    }
    dirs::home_dir()
        .map(|home| vec![home.join(".kube").join("config")])
        .unwrap_or_default()
}

/// Which top-level key the reader is inside, if any.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Elsewhere,
    Contexts,
    Namespaces,
}

/// Pulls the names out of one kubeconfig.
///
/// Parsed by lines rather than with a YAML reader: what is wanted is `name:`
/// under `contexts:`, each context's `namespace:`, and the entries of a
/// `namespaces:` list — a fixed, shallow shape. A key this does not understand
/// is skipped, so a file using anchors or flow style still gives up whatever
/// it spells out plainly.
fn collect(text: &str, names: &mut KubeNames) {
    let mut section = Section::Elsewhere;

    for raw in text.lines() {
        let line = strip_comment(raw);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        // A list item belongs to the section that is open even when it is
        // written at the margin, which is how kubectl's own files spell one:
        // only a key — not `- name: x` — can open or close a section.
        if indent == 0 && !trimmed.starts_with("- ") {
            // Only a top-level key opens or closes a section; `current-context`
            // is a scalar, and is read where it stands.
            section = match trimmed {
                "contexts:" => Section::Contexts,
                "namespaces:" => Section::Namespaces,
                _ => {
                    if let Some((key, value)) = split_key(trimmed) {
                        if key == "current-context" {
                            names.contexts.push(unquote(value));
                        }
                    }
                    Section::Elsewhere
                }
            };
            continue;
        }
        // A `- ` at the margin begins an entry of the open section; what
        // follows it is read as that entry's first key, so `- name: x` is a
        // named context rather than a stray list item. A list of bare scalars
        // has no colon at all, which is how `namespaces:` spells them.
        let entry = trimmed.strip_prefix("- ").map(str::trim).unwrap_or(trimmed);
        let Some((key, value)) = split_key(entry) else {
            if !entry.is_empty() && section == Section::Namespaces {
                names.namespaces.push(unquote(entry));
            }
            continue;
        };
        match (section, key) {
            // `contexts[].name`. A `name:` of a user's or a cluster's sits
            // under another top-level key, which closed this section already.
            (Section::Contexts, "name") => names.contexts.push(unquote(value)),
            // `contexts[].context.namespace`, the namespace that context opens
            // in, which is the one `-n` is usually about to name.
            (Section::Contexts, "namespace") => names.namespaces.push(unquote(value)),
            // `namespaces:` written as `name: x` rather than a list.
            (Section::Namespaces, "name") => names.namespaces.push(unquote(value)),
            _ => {}
        }
    }
}

/// A line without its comment. `#` only starts one where it is not inside a
/// value, which for the names here means at the start or after whitespace.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'#' && (index == 0 || bytes[index - 1].is_ascii_whitespace()) {
            return &line[..index];
        }
    }
    line
}

/// `key: value` from a trimmed line, with the value's own quotes still on.
fn split_key(trimmed: &str) -> Option<(&str, &str)> {
    let (key, value) = trimmed.split_once(':')?;
    if key.contains(char::is_whitespace) {
        return None;
    }
    Some((key, value.trim()))
}

/// A scalar without the quotes YAML allows around it.
fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    let quoted = bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''));
    if quoted {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What kubectl writes, near enough: two contexts, a user and a cluster
    /// list after them, and a namespaces list of its own.
    const CONFIG: &str = r#"
apiVersion: v1
kind: Config
current-context: prod-eu
preferences: {}
contexts:
- context:
    cluster: eu
    namespace: payments
    user: deploy
  name: prod-eu
- context:
    cluster: local
    namespace: kube-system
    user: minikube
  name: minikube
- context:
    cluster: plain
    user: nobody
  name: no-namespace
users:
- name: deploy
  user:
    token: hunter2
clusters:
- name: eu
  cluster:
    server: https://eu.example
namespaces:
- kube-public
- argocd
"#;

    /// Writes a kubeconfig to a scratch directory and reads it back.
    fn read(text: &str) -> (KubeNames, PathBuf) {
        let dir = std::env::temp_dir().join(format!("kanso-kube-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create the directory");
        let file = dir.join("config");
        std::fs::write(&file, text).expect("write the kubeconfig");
        (names_in(&file), dir)
    }

    #[test]
    fn reads_contexts_and_their_namespaces() {
        let (names, dir) = read(CONFIG);
        assert_eq!(
            names.contexts,
            vec!["minikube", "no-namespace", "prod-eu"],
            "a user's or a cluster's `name:` leaked into the contexts"
        );
        assert_eq!(
            names.namespaces,
            vec!["argocd", "default", "kube-public", "kube-system", "payments"]
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_section_ends_at_the_next_top_level_key() {
        let (names, dir) = read(CONFIG);
        // `users:` and `clusters:` follow the contexts; nothing under them is
        // a context or a namespace.
        assert!(!names.contexts.contains(&"deploy".to_string()));
        assert!(!names.contexts.contains(&"eu".to_string()));
        assert!(!names.namespaces.contains(&"minikube".to_string()));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn comments_and_quotes_do_not_leak_into_a_name() {
        let (names, dir) = read(
            r#"
contexts:
- name: "quoted-name"
  context:
    namespace: 'team-a'   # the team's own
namespaces:
- "-weird"
"#,
        );
        assert_eq!(names.contexts, vec!["quoted-name"]);
        assert_eq!(names.namespaces, vec!["-weird", "default", "team-a"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_missing_file_is_an_empty_answer() {
        let names = names_in(Path::new("/nonexistent/kube/config"));
        assert_eq!(names.contexts, Vec::<String>::new());
        // `default` is where a context without a namespace lands, so it is
        // always offerable.
        assert_eq!(names.namespaces, vec!["default"]);
    }

    #[test]
    fn a_duplicate_name_is_kept_once() {
        let (names, dir) = read(
            r#"
contexts:
- name: same
- name: same
  context:
    namespace: default
namespaces:
- default
"#,
        );
        assert_eq!(names.contexts, vec!["same"]);
        assert_eq!(names.namespaces, vec!["default"]);
        std::fs::remove_dir_all(dir).ok();
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Reads the developer's own kubeconfig when there is one, so the parser
    /// meets a file kubectl wrote rather than only the fixture above. Skipped
    /// where there is none, which is every CI machine.
    #[test]
    fn the_users_own_kubeconfig_parses() {
        let Some(home) = dirs::home_dir() else { return };
        let path = home.join(".kube").join("config");
        if !path.is_file() {
            eprintln!("no kubeconfig here; the parser is tested by the fixture only");
            return;
        }
        let names = names_in(&path);
        eprintln!(
            "contexts: {:?}\nnamespaces: {:?}",
            names.contexts, names.namespaces
        );
        // A real kubeconfig names at least one context, and every name is a
        // usable word rather than a fragment of YAML.
        assert!(!names.contexts.is_empty(), "no context found in {path:?}");
        assert!(
            names.contexts.iter().all(|name| !name.contains(':') && !name.is_empty()),
            "a context name kept part of the file: {:?}",
            names.contexts
        );
        assert!(names.namespaces.contains(&"default".to_string()));
    }
}
