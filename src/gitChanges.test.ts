import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./platform", () => ({ IS_MAC: true, IS_WINDOWS: false }));
vi.mock("./api", () => ({
  sessionCwd: vi.fn(),
  localHome: vi.fn().mockResolvedValue("/home/test"),
  gitChanges: vi.fn(),
}));

import * as api from "./api";
import { refreshLocalWhere } from "./actions";
import { useStore, type Tab } from "./store";
import type { GitChanges, SessionKind } from "./types";

const initialState = useStore.getState();
const changes = (root = "/repo", additions = 1): GitChanges => ({
  root, branch: "main", additions, deletions: 0,
  files: [{ path: "file.txt", previousPath: null, status: "modified", additions, deletions: 0 }],
});

function addTab(kind: SessionKind = "local") {
  useStore.getState().addTab({
    id: "session", profileId: null, name: "Session", kind,
    protocol: kind, address: "test", color: null,
    supportsRemoteFiles: kind !== "local", legacyAlgorithms: [],
  }, { id: "", name: "Session", kind }, "connected");
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

beforeEach(() => {
  useStore.setState({ ...initialState, tabs: [] }, true);
  vi.mocked(api.sessionCwd).mockReset().mockResolvedValue("/repo");
  vi.mocked(api.gitChanges).mockReset().mockResolvedValue(changes());
});

describe("Local Shell Git 刷新", () => {
  it.each(["ssh", "sftp"] as const)("%s 会话不读取目录或 Git 状态", async (kind) => {
    addTab(kind);
    await refreshLocalWhere("session");
    expect(api.sessionCwd).not.toHaveBeenCalled();
    expect(api.gitChanges).not.toHaveBeenCalled();
  });

  it("本地目录变化时清空旧摘要并读取新仓库", async () => {
    addTab();
    const query = deferred<GitChanges | null>();
    useStore.getState().setCwd("session", "/old");
    useStore.getState().setGitChanges("session", changes("/old"));
    vi.mocked(api.sessionCwd).mockResolvedValue("/home/test/project");
    vi.mocked(api.gitChanges).mockReturnValue(query.promise);
    const refreshing = refreshLocalWhere("session");
    await vi.waitFor(() => expect(api.gitChanges).toHaveBeenCalledWith("/home/test/project"));
    expect(useStore.getState().tabs[0]).toMatchObject({ cwd: "~/project", gitChanges: null });
    query.resolve(changes("/home/test/project"));
    await refreshing;
    expect(useStore.getState().tabs[0].branch).toBe("main");
  });

  it("同一目录的新查询不会被较晚返回的旧结果覆盖", async () => {
    addTab();
    const old = deferred<GitChanges | null>();
    vi.mocked(api.gitChanges).mockReturnValueOnce(old.promise);
    const first = refreshLocalWhere("session");
    await vi.waitFor(() => expect(api.gitChanges).toHaveBeenCalledTimes(1));
    vi.mocked(api.gitChanges).mockResolvedValue(changes("/repo", 9));
    await refreshLocalWhere("session");
    old.resolve(changes("/repo", 2));
    await first;
    expect(useStore.getState().tabs[0].gitChanges?.additions).toBe(9);
  });

  it("会话关闭后丢弃正在返回的摘要", async () => {
    addTab();
    const query = deferred<GitChanges | null>();
    vi.mocked(api.gitChanges).mockReturnValue(query.promise);
    const refreshing = refreshLocalWhere("session");
    await vi.waitFor(() => expect(api.gitChanges).toHaveBeenCalledTimes(1));
    useStore.setState({ tabs: useStore.getState().tabs.map((tab): Tab => ({ ...tab, state: "closed" })) });
    query.resolve(changes());
    await refreshing;
    expect(useStore.getState().tabs[0].gitChanges).toBeNull();
  });

  it("离开 Git 仓库时清空分支和变更", async () => {
    addTab();
    await refreshLocalWhere("session");
    vi.mocked(api.gitChanges).mockResolvedValue(null);
    await refreshLocalWhere("session");
    expect(useStore.getState().tabs[0]).toMatchObject({ branch: null, gitChanges: null });
  });
});
