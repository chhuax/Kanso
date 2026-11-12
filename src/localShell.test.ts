import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./platform", () => ({ IS_MAC: false, IS_WINDOWS: true }));
vi.mock("./api", () => ({
  openSession: vi.fn(),
  listProfiles: vi.fn(),
  listSessionGroups: vi.fn().mockResolvedValue([]),
  closeSession: vi.fn().mockResolvedValue(undefined),
  resizeSession: vi.fn().mockResolvedValue(undefined),
  sessionCwd: vi.fn().mockRejectedValue(new Error("No cwd")),
  localHome: vi.fn().mockResolvedValue("C:\\Users\\test"),
}));
const terminal = vi.hoisted(() => ({
    setEncoding: vi.fn(),
    writeText: vi.fn(),
    setLocked: vi.fn(),
}));
vi.mock("./terminalRegistry", () => ({
  getController: () => terminal,
}));

import * as api from "./api";
import { LOCAL_SHELL_PROFILE, openLocalShell, splitSession } from "./actions";
import { useStore } from "./store";
import type { LocalShell, SessionProfile } from "./types";

const initialState = useStore.getState();

beforeEach(() => {
  vi.clearAllMocks();
  useStore.setState(initialState, true);
  localStorage.clear();
  vi.mocked(api.openSession).mockImplementation(async (profile, id) => ({
    status: "connected",
    info: {
      id: id!,
      profileId: null,
      name: profile.name,
      kind: "local",
      protocol: "shell",
      address: profile.shell ?? "default shell",
      color: profile.color ?? null,
      supportsRemoteFiles: false,
      legacyAlgorithms: [],
    },
  }));
});

describe("local shell launch", () => {
  const saved: SessionProfile = {
    id: "pwsh", name: "PowerShell 7", kind: "local",
    shell: '"C:\\Program Files\\PowerShell\\7\\pwsh.exe" -NoLogo',
    cwd: "D:\\work", encoding: "gbk", locale: "zh_CN.UTF-8",
  };

  it("opens the complete saved default configuration", async () => {
    useStore.setState({ profiles: [saved], profilesLoaded: true, defaultLocalProfileId: saved.id });
    await openLocalShell();
    expect(api.openSession).toHaveBeenCalledWith(saved, expect.any(String), undefined);
  });

  it("waits for startup profile loading and shares the in-flight request", async () => {
    let resolveProfiles!: (profiles: SessionProfile[]) => void;
    vi.mocked(api.listProfiles).mockReturnValue(new Promise((resolve) => { resolveProfiles = resolve; }));
    useStore.setState({ defaultLocalProfileId: saved.id });
    const loading = useStore.getState().loadProfiles();
    const opening = openLocalShell();
    expect(api.openSession).not.toHaveBeenCalled();
    expect(api.listProfiles).toHaveBeenCalledOnce();
    resolveProfiles([saved]);
    await Promise.all([loading, opening]);
    expect(api.openSession).toHaveBeenCalledWith(saved, expect.any(String), undefined);
  });

  it("falls back to the system shell for a deleted default", async () => {
    useStore.setState({ defaultLocalProfileId: "missing", profilesLoaded: true });
    await openLocalShell();
    expect(api.openSession).toHaveBeenCalledWith(LOCAL_SHELL_PROFILE, expect.any(String), undefined);
    expect(useStore.getState().defaultLocalProfileId).toBeNull();
    expect(terminal.writeText).toHaveBeenCalledWith(expect.stringContaining("Using the system shell"));
  });

  it("retries a failed default spawn with the system shell in the same tab", async () => {
    useStore.setState({ profiles: [saved], profilesLoaded: true, defaultLocalProfileId: saved.id });
    vi.mocked(api.openSession).mockRejectedValueOnce(new Error("Executable not found"));
    const id = await openLocalShell();
    expect(api.openSession).toHaveBeenNthCalledWith(1, saved, id, undefined);
    expect(api.openSession).toHaveBeenNthCalledWith(2, LOCAL_SHELL_PROFILE, id, undefined);
    expect(useStore.getState().tabs).toHaveLength(1);
    expect(useStore.getState().tabs[0].profile).toEqual(LOCAL_SHELL_PROFILE);
    expect(terminal.writeText).toHaveBeenCalledWith(expect.stringContaining("Executable not found"));
    expect(useStore.getState().defaultLocalProfileId).toBe(saved.id);
  });

  it("an explicit shell overrides the default without replacing the preference", async () => {
    useStore.setState({ profiles: [saved], profilesLoaded: true, defaultLocalProfileId: saved.id });
    await openLocalShell(undefined, { name: "Ubuntu", command: "wsl.exe -d Ubuntu" });
    expect(api.openSession).toHaveBeenCalledWith(expect.objectContaining({ shell: "wsl.exe -d Ubuntu" }), expect.any(String), undefined);
    expect(useStore.getState().defaultLocalProfileId).toBe(saved.id);
  });

  it("does not reopen a default-shell tab the user closed while connecting", async () => {
    useStore.setState({ profiles: [saved], profilesLoaded: true, defaultLocalProfileId: saved.id });
    let finish!: () => void;
    const connected = vi.mocked(api.openSession).getMockImplementation()!;
    vi.mocked(api.openSession).mockImplementationOnce((...args) => new Promise((resolve) => {
      finish = () => { void connected(...args).then(resolve); };
    }));
    const opening = openLocalShell();
    await Promise.resolve();
    expect(api.openSession).toHaveBeenCalledOnce();
    useStore.setState({ tabs: [] });
    finish();
    expect(await opening).toBeNull();
    expect(api.openSession).toHaveBeenCalledOnce();
    expect(useStore.getState().tabs).toHaveLength(0);
  });

  it.each<LocalShell>([
    { name: "PowerShell 7 (pwsh)", command: '"C:\\Program Files\\PowerShell\\7\\pwsh.exe" -NoLogo' },
    { name: "Ubuntu", command: 'wsl.exe -d "Ubuntu 24.04"' },
  ])("passes $name and its arguments to the backend in the requested pane", async (shell) => {
    const paneId = useStore.getState().splitPane(initialState.activePaneId, "right");
    const id = await openLocalShell(paneId, shell);
    expect(api.openSession).toHaveBeenCalledWith(
      { ...LOCAL_SHELL_PROFILE, name: shell.name, shell: shell.command },
      id,
      undefined,
    );
    const tab = useStore.getState().tabs.find((tab) => tab.info.id === id)!;
    expect(tab.paneId).toBe(paneId);
    expect(tab.profile.shell).toBe(shell.command);

    await splitSession(id!, "down");
    const calls = vi.mocked(api.openSession).mock.calls;
    expect((calls[1][0] as SessionProfile).shell).toBe(shell.command);

    // A later shortcut/default launch must not inherit the earlier selection.
    await openLocalShell();
    expect(api.openSession).toHaveBeenLastCalledWith(
      LOCAL_SHELL_PROFILE, expect.any(String), undefined,
    );
    expect(LOCAL_SHELL_PROFILE.shell).toBeUndefined();
  });
});
