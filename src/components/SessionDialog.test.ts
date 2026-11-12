import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../platform", () => ({ IS_MAC: false, IS_WINDOWS: true }));
vi.mock("../actions", () => ({ openSession: vi.fn() }));
vi.mock("../api", () => ({ listLocalShells: vi.fn(), saveProfile: vi.fn() }));

import * as api from "../api";
import { openSession } from "../actions";
import { useStore } from "../store";
import type { SessionProfile } from "../types";
import { SessionDialog } from "./SessionDialog";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const initialState = useStore.getState();
const local: SessionProfile = { id: "", name: "", kind: "local" };
const pwsh = {
  name: "PowerShell 7 (pwsh)",
  command: '"C:\\Program Files\\PowerShell\\7\\pwsh.exe" -NoLogo',
};
const wsl = {
  name: "WSL (default distribution)",
  command: '"C:\\Windows\\System32\\wsl.exe"',
};

describe("independent session dialogs", () => {
  let root: Root;
  let container: HTMLDivElement;
  const close = vi.fn();

  async function render(initial: SessionProfile | null = local, paneId?: string) {
    await act(async () => root.render(createElement(SessionDialog, {
      initial, paneId, onClose: close,
    })));
  }

  function field<T extends HTMLInputElement | HTMLSelectElement>(label: string): T {
    const span = Array.from(document.querySelectorAll(".session-field-label"))
      .find((item) => item.textContent === label);
    const input = span?.closest(".session-field")?.querySelector("input, select");
    if (!input) throw new Error(`Missing field: ${label}`);
    return input as T;
  }

  async function change(label: string, value: string) {
    await act(async () => {
      const input = field(label);
      const prototype = input instanceof HTMLSelectElement
        ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(prototype, "value")!.set!.call(input, value);
      input.dispatchEvent(new Event(input instanceof HTMLSelectElement ? "change" : "input", { bubbles: true }));
    });
  }

  async function click(name: string) {
    await act(async () => {
      const button = Array.from(document.querySelectorAll("button"))
        .find((item) => item.textContent?.trim() === name);
      if (!button) throw new Error(`Missing button: ${name}`);
      button.click();
    });
  }

  beforeEach(() => {
    vi.resetAllMocks();
    localStorage.clear();
    vi.stubGlobal("ResizeObserver", class {
      observe() {}
      unobserve() {}
      disconnect() {}
    });
    useStore.setState({ ...initialState, profiles: [], groups: [] }, true);
    vi.mocked(api.listLocalShells).mockResolvedValue([pwsh, wsl]);
    vi.mocked(api.saveProfile).mockImplementation(async (profile) => ({
      ...profile, id: profile.id || "saved-shell",
    }));
    vi.mocked(openSession).mockResolvedValue("connected-shell");
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    useStore.setState(initialState, true);
    vi.unstubAllGlobals();
  });

  it("Local Shell uses the shared dialog layout and Save/Connect footer", async () => {
    await render();
    expect(document.querySelector(".session-dialog-title")?.textContent).toBe("New Local Shell");
    expect(document.querySelector('[aria-label="Protocol"]')).toBeNull();
    expect(document.querySelector(".local-shell-picker")).toBeNull();
    expect(field<HTMLSelectElement>("Shell").textContent).toContain(pwsh.name);
    expect(document.querySelector(".dialog-footer")?.textContent).toContain("Save");
    expect(document.querySelector(".dialog-footer")?.textContent).toContain("Connect");
    expect(openSession).not.toHaveBeenCalled();
  });

  it("saves a discovered shell with its full command without opening a terminal", async () => {
    await render();
    await change("Shell", pwsh.command);
    await change("Directory", " D:\\work ");
    await click("Save");
    expect(api.saveProfile).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
      kind: "local", name: pwsh.name, shell: pwsh.command, cwd: "D:\\work",
    }));
    expect(useStore.getState().profiles).toContainEqual(expect.objectContaining({
      id: "saved-shell", shell: pwsh.command,
    }));
    expect(openSession).not.toHaveBeenCalled();
    expect(useStore.getState().defaultLocalProfileId).toBe("saved-shell");
    expect(localStorage.getItem("kanso.defaultLocalProfileId")).toBe("saved-shell");
    expect(close).toHaveBeenCalledOnce();
  });

  it("unchecking the default choice keeps a saved shell from replacing the current default", async () => {
    useStore.setState({ defaultLocalProfileId: "previous" });
    await render();
    await change("Shell", pwsh.command);
    expect(document.querySelector<HTMLInputElement>('.session-check input')?.checked).toBe(false);
    await click("Save");
    expect(useStore.getState().defaultLocalProfileId).toBe("previous");
  });

  it("editing the current default selects the checkbox and can restore the system default", async () => {
    useStore.setState({ defaultLocalProfileId: "existing" });
    await render({ ...local, id: "existing", name: "PowerShell 7", shell: pwsh.command });
    const checkbox = document.querySelector<HTMLInputElement>('.session-check input')!;
    expect(checkbox.checked).toBe(true);
    await act(async () => checkbox.click());
    await click("Save");
    expect(useStore.getState().defaultLocalProfileId).toBeNull();
  });

  it("a failed save does not change the default choice", async () => {
    vi.mocked(api.saveProfile).mockRejectedValue(new Error("Disk full"));
    await render();
    await change("Shell", pwsh.command);
    await click("Save");
    expect(useStore.getState().defaultLocalProfileId).toBeNull();
    expect(document.querySelector('[role="alert"]')?.textContent).toContain("Disk full");
  });

  it("connects a discovered shell through the saved profile in the original pane", async () => {
    const paneId = useStore.getState().splitPane(initialState.activePaneId, "right");
    await render(local, paneId);
    useStore.getState().setActivePane(initialState.activePaneId);
    await change("Shell", wsl.command);
    await click("Connect");
    expect(api.saveProfile).toHaveBeenCalledWith(expect.objectContaining({ shell: wsl.command }));
    expect(openSession).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
      id: "saved-shell", name: wsl.name, kind: "local", shell: wsl.command,
    }));
    expect(useStore.getState().activePaneId).toBe(paneId);
    expect(close).toHaveBeenCalledOnce();
  });

  it("rejects a blank custom command and preserves quoted WSL arguments", async () => {
    await render();
    await change("Shell", "custom");
    await change("Command", "   ");
    await click("Connect");
    expect(document.querySelector('[role="alert"]')?.textContent).toContain("Enter a shell command");
    expect(api.saveProfile).not.toHaveBeenCalled();
    await change("Command", ' wsl.exe -d "Ubuntu 24.04" ');
    await click("Connect");
    expect(openSession).toHaveBeenCalledWith(expect.objectContaining({
      shell: 'wsl.exe -d "Ubuntu 24.04"',
    }));
  });

  it("can switch from a detected shell to a custom command or the default", async () => {
    await render();
    await change("Shell", pwsh.command);
    await change("Shell", "custom");
    expect(field<HTMLSelectElement>("Shell").value).toBe("custom");
    expect(field<HTMLInputElement>("Command").value).toBe(pwsh.command);
    await change("Shell", "");
    await click("Save");
    expect(api.saveProfile).toHaveBeenCalledWith(expect.objectContaining({
      name: "Local Shell", shell: null,
    }));
  });

  it("preserves a saved custom command and session identity when editing", async () => {
    const command = '"D:\\Portable Tools\\pwsh.exe" -NoLogo -NoExit';
    await render({ ...local, id: "existing", name: "Build tools", shell: command });
    expect(document.querySelector(".session-dialog-title")?.textContent).toBe("Edit Local Shell");
    expect(field<HTMLInputElement>("Command").value).toBe(command);
    await click("Save");
    expect(api.saveProfile).toHaveBeenCalledWith(expect.objectContaining({
      id: "existing", name: "Build tools", shell: command,
    }));
  });

  it("keeps default and custom choices usable when discovery fails", async () => {
    vi.mocked(api.listLocalShells).mockRejectedValue(new Error("Unavailable"));
    await render();
    expect(document.querySelector('[role="status"]')?.textContent).toContain("Could not detect shells");
    await click("Connect");
    expect(openSession).toHaveBeenCalledWith(expect.objectContaining({ shell: null }));
  });

  it("keeps New Session as a separate SSH/SFTP page without shell discovery", async () => {
    await render(null);
    expect(document.querySelector(".session-dialog-title")?.textContent).toBe("New Session");
    expect(Array.from(document.querySelectorAll('[aria-label="Protocol"] [role="radio"]'))
      .map((item) => item.textContent?.trim())).toEqual(["SSH", "SFTP"]);
    expect(api.listLocalShells).not.toHaveBeenCalled();
    await click("SFTP");
    await change("Host", "files.example.test");
    await click("Save");
    expect(api.saveProfile).toHaveBeenCalledWith(expect.objectContaining({
      kind: "sftp", host: "files.example.test", port: 22,
    }));
  });

  it("cancel closes the local dialog without saving or connecting", async () => {
    await render();
    await change("Shell", pwsh.command);
    await click("Cancel");
    expect(close).toHaveBeenCalledOnce();
    expect(api.saveProfile).not.toHaveBeenCalled();
    expect(openSession).not.toHaveBeenCalled();
  });
});
