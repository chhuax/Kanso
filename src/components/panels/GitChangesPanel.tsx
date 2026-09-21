import { useEffect, useRef, useState, type FormEvent } from "react";

import * as api from "../../api";
import { refreshLocalWhere } from "../../actions";
import { useStore, type Tab } from "../../store";
import type {
  GitChanges,
  GitFileChange,
  GitFileDiff,
  GitFileStatus,
} from "../../types";
import { FileIcon } from "../FileIcon";
import { ContextMenu } from "../ContextMenu";
import { Icon } from "../icons";

interface Props {
  tab: Tab | null;
  onClose: () => void;
}

type DiffState =
  | { state: "loading" }
  | { state: "ready"; diff: GitFileDiff }
  | { state: "error"; message: string };

type DiffLine = {
  kind: "added" | "removed" | "context" | "hunk" | "meta";
  lineNumber: number | null;
  marker: string;
  text: string;
};

const STATUS_MARK: Record<GitFileStatus, string> = {
  modified: "M",
  added: "A",
  deleted: "D",
  renamed: "R",
  copied: "C",
  untracked: "U",
  conflicted: "!",
  typeChanged: "T",
};

const repoName = (root: string): string =>
  root.split(/[/\\]/).filter(Boolean).pop() ?? root;

const DIFF_META = /^(?:diff |index |--- |\+\+\+|new file|deleted file|similarity index|rename (?:from|to)|Binary files|\\ No newline)/;

function parseDiffLines(text: string): DiffLine[] {
  let oldLine: number | null = null;
  let newLine: number | null = null;
  let fallbackOld = 1;
  let fallbackNew = 1;
  const source = text.endsWith("\n") ? text.slice(0, -1) : text;

  return source.split("\n").map((raw): DiffLine => {
    const hunk = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(raw);
    if (hunk) {
      oldLine = Number(hunk[1]);
      newLine = Number(hunk[2]);
      fallbackOld = oldLine;
      fallbackNew = newLine;
      return { kind: "hunk", lineNumber: null, marker: "", text: raw };
    }
    if (DIFF_META.test(raw)) {
      return { kind: "meta", lineNumber: null, marker: "", text: raw };
    }
    if (raw.startsWith("+")) {
      const lineNumber = newLine ?? fallbackNew;
      if (newLine !== null) newLine += 1;
      else fallbackNew += 1;
      return { kind: "added", lineNumber, marker: "+", text: raw.slice(1) };
    }
    if (raw.startsWith("-")) {
      const lineNumber = oldLine ?? fallbackOld;
      if (oldLine !== null) oldLine += 1;
      else fallbackOld += 1;
      return { kind: "removed", lineNumber, marker: "-", text: raw.slice(1) };
    }
    if (raw.startsWith(" ")) {
      const lineNumber = newLine ?? fallbackNew;
      if (oldLine !== null) oldLine += 1;
      else fallbackOld += 1;
      if (newLine !== null) newLine += 1;
      else fallbackNew += 1;
      return { kind: "context", lineNumber, marker: "", text: raw.slice(1) };
    }
    return { kind: "meta", lineNumber: null, marker: "", text: raw };
  });
}

function ChangeCounts({ file }: { file: GitFileChange }) {
  if (file.additions === null && file.deletions === null) {
    return <span className="git-change-binary">—</span>;
  }
  return (
    <span className="git-change-counts">
      <span className="git-added">+{file.additions ?? 0}</span>
      <span className="git-removed">-{file.deletions ?? 0}</span>
    </span>
  );
}

function DiffView({ value }: { value: DiffState | undefined }) {
  if (!value || value.state === "loading") {
    return <div className="git-diff-message">Loading diff...</div>;
  }
  if (value.state === "error") {
    return <div className="git-diff-message is-error">{value.message}</div>;
  }
  if (!value.diff.text) {
    return (
      <div className="git-diff-message">No net content changes relative to HEAD</div>
    );
  }
  const allLines = parseDiffLines(value.diff.text).filter(
    (line) => line.kind !== "meta" && line.kind !== "hunk",
  );
  const lines = allLines.slice(0, 3000);
  const truncated = value.diff.truncated || lines.length < allLines.length;
  return (
    <div className="git-diff-view">
      <pre>
        {lines.map((line, index) => (
          <span className={`git-diff-line is-${line.kind}`} key={index}>
            <span className="git-diff-line-number">
              {line.lineNumber ?? ""}
            </span>
            <span className="git-diff-line-marker">{line.marker}</span>
            <span className="git-diff-line-text">{line.text || " "}</span>
          </span>
        ))}
      </pre>
      {truncated && <div className="git-diff-truncated">Diff truncated</div>}
    </div>
  );
}

function FileDiff({ changes, path }: { changes: GitChanges; path: string }) {
  const [value, setValue] = useState<DiffState>({ state: "loading" });
  useEffect(() => {
    let cancelled = false;
    setValue({ state: "loading" });
    // 同一文件行数不变时内容仍可能变化，任何新摘要都要使已展开的 diff 失效。
    void api.gitFileDiff(changes.root, path).then(
      (diff) => {
        if (cancelled) return;
        setValue(
          diff
            ? { state: "ready", diff }
            : { state: "error", message: "This change is no longer available" },
        );
      },
      (error) => {
        if (!cancelled) setValue({ state: "error", message: String(error) });
      },
    );
    return () => {
      cancelled = true;
    };
  }, [changes, path]);
  return <DiffView value={value} />;
}

/** 展示当前 Local Shell 仓库的未提交文件；没有仓库时展示可关闭的空状态。 */
export function GitChangesPanel({ tab, onClose }: Props) {
  const theme = useStore((state) => state.theme);
  const changes = tab?.gitChanges ?? null;
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const [refreshing, setRefreshing] = useState(false);
  const [commitMessage, setCommitMessage] = useState("");
  const [operation, setOperation] = useState<string | null>(null);
  const [operationError, setOperationError] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const menuButton = useRef<HTMLButtonElement>(null);

  const root = changes?.root ?? "";
  useEffect(() => {
    setExpanded(new Set());
    setOperationError(null);
    setMenu(null);
  }, [root]);

  const toggleFile = (file: GitFileChange) => {
    if (expanded.has(file.path)) {
      setExpanded((current) => {
        const next = new Set(current);
        next.delete(file.path);
        return next;
      });
      return;
    }
    setExpanded((current) => new Set(current).add(file.path));
  };

  const refresh = () => {
    if (!tab || refreshing || operation) return;
    setRefreshing(true);
    void refreshLocalWhere(tab.info.id).finally(() => setRefreshing(false));
  };

  const runOperation = async (
    key: string,
    action: () => Promise<GitChanges>,
  ) => {
    if (!tab || operation) return;
    setOperation(key);
    setOperationError(null);
    try {
      const next = await action();
      useStore.getState().setGitChanges(tab.info.id, next);
      const remaining = new Set(next.files.map((file) => file.path));
      setExpanded((current) =>
        new Set([...current].filter((path) => remaining.has(path))),
      );
      if (key === "commit") setCommitMessage("");
    } catch (error) {
      setOperationError(String(error));
    } finally {
      setOperation(null);
    }
  };

  const commit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!changes || !commitMessage.trim()) return;
    void runOperation("commit", () =>
      api.gitCommitAll(changes.root, commitMessage),
    );
  };

  const discardFile = (file: GitFileChange) => {
    if (!changes || operation) return;
    const permanent = ["added", "untracked", "copied"].includes(file.status);
    const detail = permanent
      ? "The file content will be permanently deleted."
      : "All staged and unstaged edits to this file will be lost.";
    if (!window.confirm(`Discard changes to “${file.path}”?\n\n${detail}`)) return;
    void runOperation(`discard:${file.path}`, () =>
      api.gitDiscardFile(changes.root, file.path),
    );
  };

  const discardAll = () => {
    setMenu(null);
    if (!changes || operation) return;
    if (
      !window.confirm(
        `Discard all changes in ${repoName(changes.root)}?\n\nAll staged, unstaged, and untracked changes will be permanently lost.`,
      )
    ) {
      return;
    }
    void runOperation("discard-all", () => api.gitDiscardAll(changes.root));
  };

  const openMenu = () => {
    const rect = menuButton.current?.getBoundingClientRect();
    if (!rect) return;
    setMenu((current) =>
      current ? null : { x: rect.right, y: rect.bottom + 4 },
    );
  };

  return (
    <section className="panel git-changes-panel" aria-label="Git changes">
      <div className="panel-header">
        <div className="panel-title is-changes">
          <Icon name="source-control" />
          Changes
          {changes && <span className="panel-badge">{changes.files.length}</span>}
        </div>
        {changes && (
          <button
            className={`panel-action${refreshing ? " is-spinning" : ""}`}
            title="Refresh Changes"
            aria-label="Refresh Changes"
            onClick={refresh}
            disabled={refreshing || operation !== null}
          >
            <Icon name="refresh" />
          </button>
        )}
        <button
          className="panel-action"
          title="Close Changes"
          aria-label="Close Changes"
          onClick={onClose}
        >
          <Icon name="close" />
        </button>
      </div>

      {changes && (
        <div className="git-repo-summary" title={changes.root}>
          <span className="git-repo-name">{repoName(changes.root)}</span>
          {changes.branch && <span className="git-repo-branch">{changes.branch}</span>}
          <span className="git-change-total">
            <span className="git-added">+{changes.additions}</span>
            <span className="git-removed">-{changes.deletions}</span>
          </span>
        </div>
      )}

      {changes && changes.files.length > 0 && (
        <form className="git-commit-bar" onSubmit={commit}>
          <input
            className="git-commit-input"
            aria-label="Commit message"
            placeholder="Commit message"
            value={commitMessage}
            maxLength={16 * 1024}
            disabled={operation !== null}
            onChange={(event) => setCommitMessage(event.target.value)}
          />
          <button
            className="git-commit-button"
            type="submit"
            disabled={!commitMessage.trim() || operation !== null}
            title="Commit all changes"
          >
            <Icon name="git-commit" />
            <span>Commit</span>
          </button>
          <button
            ref={menuButton}
            className="git-more-button"
            type="button"
            aria-label="More Git actions"
            aria-haspopup="menu"
            aria-expanded={menu !== null}
            disabled={operation !== null}
            onClick={openMenu}
          >
            <Icon name="kebab-vertical" />
          </button>
          {menu && (
            <ContextMenu
              x={menu.x}
              y={menu.y}
              align="right"
              items={[
                {
                  label: "Discard all changes",
                  icon: "discard",
                  danger: true,
                  action: discardAll,
                },
              ]}
              onClose={() => setMenu(null)}
            />
          )}
        </form>
      )}

      {operationError && (
        <div className="git-operation-error" role="alert">
          <Icon name="error" />
          <span>{operationError}</span>
          <button
            type="button"
            aria-label="Dismiss Git error"
            onClick={() => setOperationError(null)}
          >
            <Icon name="close" />
          </button>
        </div>
      )}

      <div
        className={`panel-body git-change-list${!changes || changes.files.length === 0 ? " is-empty" : ""}`}
      >
        {!changes ? (
          <div className="git-changes-empty">
            <Icon name="folder" />
            <strong>Cannot detect changes for this folder</strong>
            <span>Changes are only available for Git repositories.</span>
          </div>
        ) : changes.files.length === 0 ? (
          <div className="panel-empty">Working tree clean</div>
        ) : (
          changes.files.map((file) => {
            const open = expanded.has(file.path);
            return (
              <div className="git-change" key={file.path}>
                <div className="git-change-header">
                  <button
                    className="git-change-row"
                    onClick={() => toggleFile(file)}
                    aria-expanded={open}
                    title={file.path}
                    disabled={operation !== null}
                  >
                    <span className={`row-caret${open ? " is-open" : ""}`}>
                      <Icon name="chevron-right" />
                    </span>
                    <span className="filer-icon">
                      <FileIcon name={file.path} isDir={false} theme={theme} />
                    </span>
                    <span className="git-change-path">
                      <span>{file.path}</span>
                      {file.previousPath && <small>from {file.previousPath}</small>}
                    </span>
                    <ChangeCounts file={file} />
                    <span className={`git-change-status is-${file.status}`}>
                      {STATUS_MARK[file.status]}
                    </span>
                  </button>
                  <button
                    className="git-change-action"
                    type="button"
                    aria-label={`Discard changes to ${file.path}`}
                    title="Discard file changes"
                    disabled={operation !== null}
                    onClick={() => discardFile(file)}
                  >
                    <Icon name="discard" />
                  </button>
                </div>
                {open && <FileDiff changes={changes} path={file.path} />}
              </div>
            );
          })
        )}
      </div>
    </section>
  );
}
