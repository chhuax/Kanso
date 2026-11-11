import { beforeEach, describe, expect, it, vi } from "vitest";

// The store reaches platform.ts through the shortcut table, and IS_MAC is a
// build-time constant vitest does not define.
vi.mock("./platform", () => ({
  IS_MAC: true,
  IS_WINDOWS: false,
}));
vi.mock("./api", () => ({
  listProfiles: vi.fn(),
  listSessionGroups: vi.fn(),
  saveProfile: vi.fn(),
  deleteProfile: vi.fn(),
  deleteSessionGroup: vi.fn(),
}));

import * as api from "./api";
import { loadDefaultLocalProfileId, useStore } from "./store";
import type { SessionProfile } from "./types";

const initialState = useStore.getState();

beforeEach(() => {
  localStorage.clear();
  vi.resetAllMocks();
  useStore.setState({ ...initialState }, true);
});

describe("default local shell preference", () => {
  const pwsh: SessionProfile = { id: "pwsh", name: "PowerShell 7", kind: "local", shell: "pwsh -NoLogo" };

  it("persists across reload, export, reset and import", async () => {
    useStore.getState().setDefaultLocalProfileId("pwsh");
    expect(loadDefaultLocalProfileId()).toBe("pwsh");
    const settings = useStore.getState().exportSettings();
    expect(settings.defaultLocalProfileId).toBe("pwsh");
    useStore.getState().resetSettings();
    expect(loadDefaultLocalProfileId()).toBeNull();
    useStore.getState().applySettings(settings);
    // Import applies preferences before asynchronously loading saved profiles.
    expect(useStore.getState().defaultLocalProfileId).toBe("pwsh");
    vi.mocked(api.listProfiles).mockResolvedValue([pwsh]);
    vi.mocked(api.listSessionGroups).mockResolvedValue([]);
    await useStore.getState().loadProfiles();
    expect(useStore.getState().defaultLocalProfileId).toBe("pwsh");
  });

  it("ignores malformed imports and accepts an explicit system-default choice", () => {
    useStore.getState().setDefaultLocalProfileId("pwsh");
    useStore.getState().applySettings({ defaultLocalProfileId: 42 });
    expect(useStore.getState().defaultLocalProfileId).toBe("pwsh");
    useStore.getState().applySettings({ defaultLocalProfileId: null });
    expect(loadDefaultLocalProfileId()).toBeNull();
  });

  it("clears a deleted default profile only after deletion succeeds", async () => {
    useStore.setState({ profiles: [pwsh], defaultLocalProfileId: "pwsh" });
    vi.mocked(api.deleteProfile).mockRejectedValueOnce(new Error("Failed"));
    await expect(useStore.getState().removeProfile("pwsh")).rejects.toThrow("Failed");
    expect(useStore.getState().defaultLocalProfileId).toBe("pwsh");
    vi.mocked(api.deleteProfile).mockResolvedValueOnce(undefined);
    await useStore.getState().removeProfile("pwsh");
    expect(useStore.getState().defaultLocalProfileId).toBeNull();
  });

  it.each([{ profiles: [] }, { profiles: [{ ...pwsh, kind: "ssh" as const }] }])("drops stale or nonlocal references after loading profiles", async ({ profiles }) => {
    useStore.getState().setDefaultLocalProfileId("pwsh");
    vi.mocked(api.listProfiles).mockResolvedValue(profiles);
    vi.mocked(api.listSessionGroups).mockResolvedValue([]);
    await useStore.getState().loadProfiles();
    expect(useStore.getState().defaultLocalProfileId).toBeNull();
    expect(loadDefaultLocalProfileId()).toBeNull();
  });

  it("clears the default when its group is deleted and profiles reload", async () => {
    useStore.setState({ profiles: [pwsh], defaultLocalProfileId: pwsh.id });
    vi.mocked(api.deleteSessionGroup).mockResolvedValue(undefined);
    vi.mocked(api.listProfiles).mockResolvedValue([]);
    vi.mocked(api.listSessionGroups).mockResolvedValue([]);
    await useStore.getState().removeGroup("local-tools");
    expect(useStore.getState().defaultLocalProfileId).toBeNull();
  });

  it("does not let an older initial load overwrite a newly saved default", async () => {
    let finishOldRead!: (profiles: SessionProfile[]) => void;
    vi.mocked(api.listProfiles)
      .mockReturnValueOnce(new Promise((resolve) => { finishOldRead = resolve; }))
      .mockResolvedValueOnce([pwsh]);
    vi.mocked(api.listSessionGroups).mockResolvedValue([]);
    vi.mocked(api.saveProfile).mockResolvedValue(pwsh);
    const loading = useStore.getState().loadProfiles();
    const saved = await useStore.getState().upsertProfile(pwsh);
    useStore.getState().setDefaultLocalProfileId(saved.id);
    finishOldRead([]);
    await loading;
    expect(api.listProfiles).toHaveBeenCalledTimes(2);
    expect(useStore.getState().profiles).toEqual([pwsh]);
    expect(useStore.getState().defaultLocalProfileId).toBe(pwsh.id);
    expect(loadDefaultLocalProfileId()).toBe(pwsh.id);
  });

  it("refreshes an in-flight read when data import changes the saved profiles", async () => {
    let finishOldRead!: (profiles: SessionProfile[]) => void;
    vi.mocked(api.listProfiles)
      .mockReturnValueOnce(new Promise((resolve) => { finishOldRead = resolve; }))
      .mockResolvedValueOnce([pwsh]);
    vi.mocked(api.listSessionGroups).mockResolvedValue([]);
    const initialLoad = useStore.getState().loadProfiles();
    useStore.getState().applySettings({ defaultLocalProfileId: pwsh.id });
    const refresh = useStore.getState().loadProfiles(true);
    finishOldRead([]);
    await Promise.all([initialLoad, refresh]);
    expect(useStore.getState().profiles).toEqual([pwsh]);
    expect(useStore.getState().defaultLocalProfileId).toBe(pwsh.id);
  });
});

describe("cursor settings", () => {
  it("default to a blinking block", () => {
    const state = useStore.getState();
    expect(state.cursorStyle).toBe("block");
    expect(state.cursorBlink).toBe(true);
  });

  it("persist the shape and the blink switch", () => {
    const store = useStore.getState();
    store.setCursorStyle("bar");
    store.setCursorBlink(false);

    expect(useStore.getState().cursorStyle).toBe("bar");
    expect(useStore.getState().cursorBlink).toBe(false);
    expect(localStorage.getItem("kanso.cursorStyle")).toBe("bar");
    expect(localStorage.getItem("kanso.cursorBlink")).toBe("off");
  });

  it("travel with a data export and survive a malformed import", () => {
    const store = useStore.getState();
    store.setCursorStyle("underline");
    store.setCursorBlink(false);
    const exported = useStore.getState().exportSettings();
    expect(exported.cursorStyle).toBe("underline");
    expect(exported.cursorBlink).toBe(false);

    store.resetSettings();
    expect(useStore.getState().cursorStyle).toBe("block");
    expect(useStore.getState().cursorBlink).toBe(true);
    expect(localStorage.getItem("kanso.cursorStyle")).toBeNull();
    expect(localStorage.getItem("kanso.cursorBlink")).toBeNull();

    useStore.getState().applySettings(exported);
    expect(useStore.getState().cursorStyle).toBe("underline");
    expect(useStore.getState().cursorBlink).toBe(false);

    // A shape the terminal does not draw, or a blink flag that is not a
    // boolean, leaves the current values alone.
    useStore.getState().applySettings({ cursorStyle: "beam", cursorBlink: "yes" });
    expect(useStore.getState().cursorStyle).toBe("underline");
    expect(useStore.getState().cursorBlink).toBe(false);
  });
});
