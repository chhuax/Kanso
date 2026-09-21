import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

vi.mock("../../platform", () => ({ IS_MAC: true, IS_WINDOWS: false }));
vi.mock("../../api", () => ({
  gitFileDiff: vi.fn(),
  gitCommitAll: vi.fn(),
  gitDiscardFile: vi.fn(),
  gitDiscardAll: vi.fn(),
}));
vi.mock("../../actions", () => ({ refreshLocalWhere: vi.fn() }));

import * as api from "../../api";
import { useStore } from "../../store";
import type { GitChanges } from "../../types";
import { GitChangesPanel } from "./GitChangesPanel";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root;
let container: HTMLDivElement;
const initialState = useStore.getState();
const summary: GitChanges = {
  root: "/repo", branch: "main", additions: 1, deletions: 1,
  files: [{ path: "src/file.ts", previousPath: null, status: "modified", additions: 1, deletions: 1 }],
};

beforeEach(() => {
  vi.resetAllMocks();
  useStore.setState({ ...initialState, tabs: [] }, true);
  useStore.getState().addTab({
    id: "local", profileId: null, name: "Local", kind: "local",
    protocol: "shell", address: "test", color: null,
    supportsRemoteFiles: false, legacyAlgorithms: [],
  }, { id: "", name: "Local", kind: "local" }, "connected");
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.restoreAllMocks();
});

async function render(changes: GitChanges) {
  await act(async () => {
    root.render(createElement(GitChangesPanel, {
      tab: { ...useStore.getState().tabs[0], gitChanges: changes },
      onClose: vi.fn(),
    }));
  });
}

async function toggleFile() {
  await act(async () => container.querySelector<HTMLButtonElement>(".git-change-row")!.click());
}

function fill(input: HTMLInputElement, value: string) {
  act(() => {
    // 通过原生 setter 模拟用户输入，让 React 接收真实的 input 事件。
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

it("只在展开文件时加载 diff，刷新后更新已展开的内容", async () => {
  vi.mocked(api.gitFileDiff).mockResolvedValue({
    text: "diff --git a/src/file.ts b/src/file.ts\n--- a/src/file.ts\n+++ b/src/file.ts\n@@ -1 +1 @@\n-old\n+first",
    truncated: false,
  });
  await render(summary);
  expect(api.gitFileDiff).not.toHaveBeenCalled();
  await toggleFile();
  expect(api.gitFileDiff).toHaveBeenCalledWith("/repo", "src/file.ts");
  expect(container.querySelector(".git-diff-view")?.textContent).not.toContain("diff --git");
  expect(container.querySelector(".git-diff-view")?.textContent).not.toContain("@@");
  expect(container.querySelector(".is-added .git-diff-line-number")?.textContent).toBe("1");
  expect(container.querySelector(".is-added .git-diff-line-text")?.textContent).toBe("first");

  vi.mocked(api.gitFileDiff).mockResolvedValue({ text: "-old\n+second", truncated: false });
  // 行数相同的下一次摘要也代表新的读取时刻，不能复用旧 diff。
  await render({ ...summary });
  expect(container.querySelector(".is-added .git-diff-line-text")?.textContent).toBe("second");
  await toggleFile();
  expect(container.querySelector(".git-diff-view")).toBeNull();
});

it("变更清空后显示干净状态", async () => {
  await render({ ...summary, files: [], additions: 0, deletions: 0 });
  expect(container.textContent).toContain("Working tree clean");
});

it("提交全部变更并把后端返回的新状态写回标签", async () => {
  const clean = { ...summary, files: [], additions: 0, deletions: 0 };
  vi.mocked(api.gitCommitAll).mockResolvedValue(clean);
  await render(summary);

  fill(
    container.querySelector<HTMLInputElement>('[aria-label="Commit message"]')!,
    "panel commit",
  );
  await act(async () => {
    container.querySelector<HTMLFormElement>(".git-commit-bar")!.requestSubmit();
  });

  expect(api.gitCommitAll).toHaveBeenCalledWith("/repo", "panel commit");
  expect(useStore.getState().tabs[0].gitChanges).toEqual(clean);
});

it("丢弃单文件前确认，并把文件路径原样交给后端", async () => {
  vi.spyOn(window, "confirm").mockReturnValue(true);
  vi.mocked(api.gitDiscardFile).mockResolvedValue({
    ...summary,
    files: [],
    additions: 0,
    deletions: 0,
  });
  await render(summary);

  await act(async () => {
    container
      .querySelector<HTMLButtonElement>('[aria-label="Discard changes to src/file.ts"]')!
      .click();
  });

  expect(window.confirm).toHaveBeenCalledOnce();
  expect(api.gitDiscardFile).toHaveBeenCalledWith("/repo", "src/file.ts");
});

it("更多菜单中的危险操作确认后丢弃全部变更", async () => {
  vi.spyOn(window, "confirm").mockReturnValue(true);
  vi.mocked(api.gitDiscardAll).mockResolvedValue({
    ...summary,
    files: [],
    additions: 0,
    deletions: 0,
  });
  await render(summary);

  await act(async () => {
    container
      .querySelector<HTMLButtonElement>('[aria-label="More Git actions"]')!
      .click();
  });
  const discardAll = [...container.querySelectorAll<HTMLButtonElement>(".menu-entry")]
    .find((button) => button.textContent?.includes("Discard all changes"));
  await act(async () => discardAll!.click());

  expect(window.confirm).toHaveBeenCalledOnce();
  expect(api.gitDiscardAll).toHaveBeenCalledWith("/repo");
});
