/**
 * Vocabulary for the completion popup: the subcommands, resources and flags of
 * the two CLIs people type all day. The popup matches these against whatever
 * prefix the cursor is sitting on, so this file is deliberately inert — plain
 * data, no logic, no imports — and the caller decides what to do with it.
 *
 * kubectl and git are described apart rather than as one list of strings
 * because their completions are not alike: kubectl wants a resource before its
 * verbs make sense, while git wants its flags positioned by hand. Keeping the
 * shape here lets the caller place a hint beside each entry without parsing it
 * back out of a sentence.
 */

/** kubectl subcommands a person actually types, in the order they reach for them. */
export const KUBECTL_SUBCOMMANDS: string[] = [
  "get",
  "describe",
  "apply",
  "delete",
  "logs",
  "exec",
  "port-forward",
  "edit",
  "create",
  "replace",
  "rollout",
  "scale",
  "top",
  "explain",
  "api-resources",
  "config",
  "auth",
  "cp",
  "drain",
  "cordon",
  "uncordon",
  "label",
  "annotate",
  "patch",
  "expose",
  "set",
  "wait",
  "diff",
  "kustomize",
  "version",
];

/**
 * Resources, with the spellings and short names kubectl accepts. `names` is
 * every alias that should complete: "pods", "pod", "po".
 */
export interface KubectlResource {
  /** Canonical singular, e.g. "pod". */
  name: string;
  /** Every alias kubectl accepts, canonical plural and short forms first. */
  names: string[];
  /** True for namespaced resources, so `-n` and `-A` make sense for them. */
  namespaced: boolean;
}
export const KUBECTL_RESOURCES: KubectlResource[] = [
  // Workloads first: these are what a person is usually asking about.
  { name: "pod", names: ["pods", "pod", "po"], namespaced: true },
  { name: "service", names: ["services", "service", "svc"], namespaced: true },
  { name: "deployment", names: ["deployments", "deployment", "deploy"], namespaced: true },
  { name: "statefulset", names: ["statefulsets", "statefulset", "sts"], namespaced: true },
  { name: "daemonset", names: ["daemonsets", "daemonset", "ds"], namespaced: true },
  { name: "replicaset", names: ["replicasets", "replicaset", "rs"], namespaced: true },
  { name: "job", names: ["jobs", "job"], namespaced: true },
  { name: "cronjob", names: ["cronjobs", "cronjob", "cj"], namespaced: true },

  // Configuration and storage that live beside a workload.
  { name: "configmap", names: ["configmaps", "configmap", "cm"], namespaced: true },
  { name: "secret", names: ["secrets", "secret"], namespaced: true },
  { name: "persistentvolumeclaim", names: ["persistentvolumeclaims", "persistentvolumeclaim", "pvc"], namespaced: true },

  // Cluster-scoped, so the caller must not offer `-n` for these.
  { name: "node", names: ["nodes", "node", "no"], namespaced: false },
  { name: "namespace", names: ["namespaces", "namespace", "ns"], namespaced: false },
  { name: "persistentvolume", names: ["persistentvolumes", "persistentvolume", "pv"], namespaced: false },
  { name: "storageclass", names: ["storageclasses", "storageclass", "sc"], namespaced: false },

  // Networking, events and identity.
  { name: "ingress", names: ["ingresses", "ingress", "ing"], namespaced: true },
  { name: "endpoint", names: ["endpoints", "endpoint", "ep"], namespaced: true },
  { name: "event", names: ["events", "event", "ev"], namespaced: true },
  { name: "serviceaccount", names: ["serviceaccounts", "serviceaccount", "sa"], namespaced: true },

  // RBAC, split by whether the binding crosses namespaces.
  { name: "role", names: ["roles", "role"], namespaced: true },
  { name: "rolebinding", names: ["rolebindings", "rolebinding"], namespaced: true },
  { name: "clusterrole", names: ["clusterroles", "clusterrole"], namespaced: false },
  { name: "clusterrolebinding", names: ["clusterrolebindings", "clusterrolebinding"], namespaced: false },
];

/** Flags: `--flag` long forms and `-x` short forms, with a short description used as the popup's hint. */
export interface CommandFlag {
  name: string;
  /** One short line, shown beside the flag in the popup. Sentence case, no trailing period. */
  hint: string;
}
export const KUBECTL_FLAGS: CommandFlag[] = [
  // Scope: the flags that decide what the command acts on.
  { name: "-n", hint: "Namespace to act in" },
  { name: "--namespace", hint: "Namespace to act in" },
  { name: "-A", hint: "Query every namespace" },
  { name: "--all-namespaces", hint: "Query every namespace" },
  { name: "-f", hint: "File or directory to read manifests from" },
  { name: "--filename", hint: "File or directory to read manifests from" },
  { name: "-l", hint: "Label selector" },
  { name: "--selector", hint: "Label selector" },
  { name: "--field-selector", hint: "Server-side field selector" },
  { name: "--all", hint: "Every resource of the kind" },
  { name: "--context", hint: "Kubeconfig context to use" },
  { name: "--kubeconfig", hint: "Path to the kubeconfig file" },
  { name: "--cluster", hint: "Cluster in the kubeconfig to use" },
  { name: "--user", hint: "User in the kubeconfig to use" },
  { name: "--server", hint: "Address of the API server" },
  { name: "--token", hint: "Bearer token for authentication" },
  { name: "--as", hint: "Impersonate this user" },
  { name: "--as-group", hint: "Impersonate this group" },
  { name: "--insecure-skip-tls-verify", hint: "Skip TLS certificate verification" },

  // Output: how much to show and in what shape.
  { name: "-o", hint: "Output format" },
  { name: "--output", hint: "Output format" },
  { name: "--show-labels", hint: "Print labels as a column" },
  { name: "--no-headers", hint: "Omit column headers" },
  { name: "--sort-by", hint: "Sort by a field, such as .metadata.name" },
  { name: "-w", hint: "Watch for changes" },
  { name: "--watch", hint: "Watch for changes" },
  { name: "--tail", hint: "Lines of log to show from the end" },
  { name: "--follow", hint: "Stream logs as they are written" },
  { name: "-v", hint: "Log verbosity level" },
  { name: "--request-timeout", hint: "Give up on a request after this long" },

  // Containers and files: picking the right one inside a pod.
  { name: "-c", hint: "Container to target" },
  { name: "--container", hint: "Container to target" },
  { name: "-p", hint: "Use the previous container instance" },
  { name: "--previous", hint: "Use the previous container instance" },
  { name: "-it", hint: "Attach stdin and allocate a TTY" },
  { name: "-i", hint: "Attach stdin" },
  { name: "--stdin", hint: "Attach stdin" },
  { name: "-t", hint: "Allocate a TTY" },
  { name: "--tty", hint: "Allocate a TTY" },
  { name: "--recursive", hint: "Recurse into directories" },
  { name: "-R", hint: "Recurse into directories" },

  // Mutation: the flags that decide what a write does.
  { name: "--dry-run", hint: "Preview the change without sending it" },
  { name: "--force", hint: "Delete without waiting for graceful shutdown" },
  { name: "--grace-period", hint: "Seconds to wait before killing" },
  { name: "--now", hint: "Trigger immediately instead of on schedule" },
  { name: "--record", hint: "Record the command in the resource annotation" },
  { name: "--validate", hint: "Validate the manifest against the schema" },
  { name: "--prune", hint: "Remove resources missing from the source" },
  { name: "--ignore-not-found", hint: "Succeed when nothing matched" },
  { name: "--restart", hint: "Restart policy for generated pods" },
  { name: "--type", hint: "Type of resource to create" },
  { name: "--replicas", hint: "Desired replica count" },
  { name: "--image", hint: "Container image to run" },
  { name: "--port", hint: "Port the service exposes" },
  { name: "--target-port", hint: "Container port the service forwards to" },
  { name: "--address", hint: "Addresses the service listens on" },
  { name: "--limits", hint: "Resource limits, as name=value" },
  { name: "--requests", hint: "Resource requests, as name=value" },
];

/** git subcommands, the ones that show up in a normal working day. */
export const GIT_SUBCOMMANDS: string[] = [
  "status",
  "add",
  "commit",
  "push",
  "pull",
  "fetch",
  "checkout",
  "switch",
  "restore",
  "branch",
  "merge",
  "rebase",
  "reset",
  "revert",
  "stash",
  "log",
  "diff",
  "show",
  "blame",
  "tag",
  "remote",
  "clone",
  "init",
  "cherry-pick",
  "clean",
  "describe",
  "reflog",
  "config",
  "worktree",
  "submodule",
];

/** git flags, same shape as the kubectl ones. */
export const GIT_FLAGS: CommandFlag[] = [
  // Staging and committing.
  { name: "-a", hint: "Stage tracked files before committing" },
  { name: "--all", hint: "Stage tracked files before committing" },
  { name: "-m", hint: "Commit message" },
  { name: "--message", hint: "Commit message" },
  { name: "--amend", hint: "Replace the previous commit" },
  { name: "--no-edit", hint: "Keep the existing commit message" },
  { name: "--no-verify", hint: "Skip the pre-commit and commit-msg hooks" },
  { name: "--staged", hint: "Compare against the index" },
  { name: "--cached", hint: "Compare against the index" },
  { name: "-p", hint: "Choose hunks interactively" },
  { name: "--patch", hint: "Choose hunks interactively" },
  { name: "--author", hint: "Override the commit author" },
  { name: "--date", hint: "Override the commit date" },

  // Remote and branch movement.
  { name: "-u", hint: "Set the upstream branch" },
  { name: "--set-upstream", hint: "Set the upstream branch" },
  { name: "--force", hint: "Overwrite the remote branch" },
  { name: "--force-with-lease", hint: "Force push only if the remote is unchanged" },
  { name: "--tags", hint: "Include tags" },
  { name: "--prune", hint: "Drop refs deleted on the remote" },
  { name: "--depth", hint: "Limit history to this many commits" },
  { name: "--rebase", hint: "Rebase instead of merging" },
  { name: "--no-rebase", hint: "Merge instead of rebasing" },
  { name: "--ff-only", hint: "Fast-forward only, never create a merge" },

  // Resetting and conflict resolution.
  { name: "--hard", hint: "Discard working tree and index changes" },
  { name: "--soft", hint: "Move HEAD but keep index and working tree" },
  { name: "--mixed", hint: "Reset the index but keep the working tree" },
  { name: "--continue", hint: "Resume the operation in progress" },
  { name: "--abort", hint: "Abandon the operation in progress" },
  { name: "--skip", hint: "Skip the current step and continue" },
  { name: "--dry-run", hint: "Show what would happen without doing it" },

  // Reading history.
  { name: "--stat", hint: "Show a diffstat" },
  { name: "--oneline", hint: "One commit per line" },
  { name: "--graph", hint: "Draw the branch graph" },
  { name: "--decorate", hint: "Show ref names on commits" },
  { name: "--pretty", hint: "Format the commit output" },
  { name: "--name-only", hint: "List changed paths without the diff" },
];
