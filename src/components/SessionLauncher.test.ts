import { act, createElement, useCallback, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../platform", () => ({ IS_MAC: true, IS_WINDOWS: false }));
vi.mock("../actions", () => ({
  openSession: vi.fn(),
  openLocalShell: vi.fn(),
}));
vi.mock("../dataTransfer", () => ({ importSshConfig: vi.fn() }));
vi.mock("./TerminalPane", () => ({ useAccelerator: () => "" }));
vi.mock("../api", () => ({
  listSenderCommands: vi.fn(),
  deleteProfile: vi.fn(),
  saveProfile: vi.fn(),
  saveSessionGroup: vi.fn(),
  deleteSessionGroup: vi.fn(),
  listProfiles: vi.fn(),
  listSessionGroups: vi.fn(),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ ask: vi.fn() }));

import * as api from "../api";
import { openLocalShell, openSession } from "../actions";
import { importSshConfig } from "../dataTransfer";
import { useStore } from "../store";
import type { SessionInfo, SessionProfile } from "../types";
import { SessionLauncher } from "./SessionLauncher";
import { TabRail } from "./TabRail";

(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;

const initialState = useStore.getState();
const beijing: SessionProfile = {
  id: "beijing",
  name: "Beijing",
  kind: "ssh",
  host: "bj.example.test",
  username: "deploy",
  groupId: "servers",
};
const tokyo: SessionProfile = {
  id: "tokyo",
  name: "Tokyo",
  kind: "ssh",
  host: "tokyo.example.test",
  username: "ops",
};

const localInfo = (id: string): SessionInfo => ({
  id,
  profileId: null,
  name: id,
  kind: "local",
  protocol: "shell",
  address: "default shell",
  color: null,
  supportsRemoteFiles: false,
  legacyAlgorithms: [],
});

describe("左侧会话管理", () => {
  let root: Root;
  let container: HTMLDivElement;
  const onEdit = vi.fn();
  const onNew = vi.fn();

  function Harness() {
    const [anchor, setAnchor] = useState<{ x: number; y: number } | null>({
      x: 8,
      y: 60,
    });
    const close = useCallback(() => setAnchor(null), []);
    return createElement(SessionLauncher, {
      anchor,
      onClose: close,
      onEditProfile: onEdit,
      onNewSession: onNew,
    });
  }

  const button = (name: string) => {
    const found = Array.from(
      document.querySelectorAll<HTMLButtonElement>("button"),
    ).find(
      (item) =>
        (item.getAttribute("aria-label") ?? item.textContent?.trim()) === name,
    );
    if (!found) throw new Error(`找不到按钮：${name}`);
    return found;
  };

  const click = async (name: string) => {
    await act(async () => {
      const target = button(name);
      target.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
      target.click();
    });
  };

  const fill = (input: HTMLInputElement, value: string) => {
    act(() => {
      // 通过原生 setter 模拟用户输入，让 React 接收真实的 input 事件。
      Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value",
      )!.set!.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
  };

  const search = (value: string) =>
    fill(
      document.querySelector<HTMLInputElement>(
        '[aria-label="Search sessions"]',
      )!,
      value,
    );

  beforeEach(() => {
    vi.resetAllMocks();
    useStore.setState(
      {
        ...initialState,
        profiles: [beijing, tokyo],
        groups: [{ id: "servers", name: "Servers" }],
        activePaneId: "pane-2",
      },
      true,
    );
    vi.mocked(api.listSenderCommands).mockResolvedValue([]);
    vi.mocked(api.saveProfile).mockImplementation(async (profile) => profile);
    vi.mocked(api.saveSessionGroup).mockImplementation(async (group) => ({
      ...group,
      id: group.id || "new-group",
    }));
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
    act(() => root.render(createElement(Harness)));
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    useStore.setState(initialState, true);
  });

  it("搜索主机时展开匹配分组，Enter 连接对应配置", () => {
    search("BJ.EXAMPLE");
    expect(document.querySelector(".panel-body")?.textContent).toContain(
      "Beijing",
    );
    expect(document.querySelector(".panel-body")?.textContent).not.toContain(
      "Tokyo",
    );
    act(() =>
      document.activeElement!.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
      ),
    );
    expect(openSession).toHaveBeenCalledWith(beijing);
    expect(document.querySelector('[aria-label="Sessions"]')).toBeNull();
  });

  it("编辑按钮携带原始配置且不会触发连接", async () => {
    await click("Edit Tokyo");
    expect(onEdit).toHaveBeenCalledWith(tokyo);
    expect(openSession).not.toHaveBeenCalled();
    expect(document.querySelector('[aria-label="Sessions"]')).toBeNull();
  });

  it("移动分组复用持久化操作，重新展开后能看见更新结果", async () => {
    await click("Actions for Tokyo");
    await click("Servers");
    expect(api.saveProfile).toHaveBeenCalledWith({
      ...tokyo,
      groupId: "servers",
    });
    const expand = document.querySelector<HTMLButtonElement>(
      'button[aria-expanded="false"]',
    )!;
    act(() => expand.click());
    expect(document.querySelector(".panel-body")?.textContent).toContain(
      "Tokyo",
    );
    expect(document.querySelector(".panel-body")?.textContent).toContain(
      "Beijing",
    );
  });

  it("删除会话显示专属命令数量，并在确认后才删除持久化配置", async () => {
    vi.mocked(api.listSenderCommands).mockResolvedValue([
      {
        id: "c1",
        name: "检查",
        text: "pwd",
        ending: "lf",
        scope: { type: "profile", id: tokyo.id },
      },
      {
        id: "c2",
        name: "通用",
        text: "pwd",
        ending: "lf",
        scope: { type: "global" },
      },
    ]);
    await click("Actions for Tokyo");
    await click("Delete…");
    expect(
      document.querySelector('[role="alertdialog"]')?.textContent,
    ).toContain("1 saved Sender command");
    expect(api.deleteProfile).not.toHaveBeenCalled();
    await click("Delete");
    expect(api.deleteProfile).toHaveBeenCalledExactlyOnceWith(tokyo.id);
    expect(useStore.getState().profiles).toEqual([beijing]);
  });

  it("取消删除保留已保存会话", async () => {
    await click("Actions for Tokyo");
    await click("Delete…");
    await click("Cancel");
    expect(api.deleteProfile).not.toHaveBeenCalled();
    expect(useStore.getState().profiles).toEqual([beijing, tokyo]);
  });

  it("浮层关闭后仍可新建分组", async () => {
    await click("Session library actions");
    await click("New Group…");
    fill(
      document.querySelector<HTMLInputElement>('[placeholder="Group name"]')!,
      "Work",
    );
    await click("Create");
    expect(api.saveSessionGroup).toHaveBeenCalledWith({ id: "", name: "Work" });
    expect(useStore.getState().groups).toContainEqual({
      id: "new-group",
      name: "Work",
    });
  });

  it("分组重命名保留 id 和成员关系", async () => {
    await click("Actions for group Servers");
    await click("Rename Group…");
    fill(
      document.querySelector<HTMLInputElement>('[placeholder="Group name"]')!,
      "Production",
    );
    await click("Rename");
    expect(api.saveSessionGroup).toHaveBeenCalledWith({
      id: "servers",
      name: "Production",
    });
    expect(useStore.getState().profiles[0].groupId).toBe("servers");
  });

  it("Escape 先关闭操作菜单，再关闭浮层", async () => {
    await click("Actions for Tokyo");
    act(() =>
      window.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      ),
    );
    expect(document.querySelector('[aria-label="Sessions"]')).not.toBeNull();
    expect(document.querySelector('[role="menu"]')).toBeNull();
    act(() =>
      window.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      ),
    );
    expect(document.querySelector('[aria-label="Sessions"]')).toBeNull();
  });

  it("从当前窗格创建本地终端", async () => {
    await click("New Local Shell");
    expect(openLocalShell).toHaveBeenCalledWith("pane-2");
  });

  it("从左侧发起 SSH 配置导入", async () => {
    await click("Session library actions");
    await click("Import OpenSSH Config…");
    expect(importSshConfig).toHaveBeenCalledOnce();
  });

  it("＋与左侧空白处右键都能进入管理，保存后的名称实时更新", async () => {
    act(() =>
      root.render(
        createElement(TabRail, { onNewSession: onNew, onEditProfile: onEdit }),
      ),
    );
    await click("Sessions");
    await click("Edit Tokyo");
    expect(onEdit).toHaveBeenCalledWith(tokyo);
    act(() => {
      useStore.setState({ profiles: [{ ...tokyo, name: "Tokyo Updated" }] });
      document
        .querySelector(".tab-rail-panel")!
        .dispatchEvent(
          new MouseEvent("contextmenu", {
            bubbles: true,
            clientX: 100,
            clientY: 100,
          }),
        );
    });
    expect(document.querySelector(".session-launcher")?.textContent).toContain(
      "Tokyo Updated",
    );
    await click("New Session…");
    expect(onNew).toHaveBeenCalledOnce();
  });

  it("标签栏按当前路径搜索，并回退到配置的初始目录", () => {
    act(() => {
      useStore.setState({ ...initialState }, true);
      HTMLElement.prototype.scrollIntoView = vi.fn();
      useStore.getState().addTab(localInfo("workspace"), {
        id: "",
        name: "Workspace",
        kind: "local",
      });
      useStore.getState().setCwd("workspace", "/data/workspace/ymscloud");
      useStore.getState().addTab(localInfo("documents"), {
        id: "",
        name: "Documents",
        kind: "local",
        cwd: "C:\\Users\\huaxin\\Documents",
      });
      root.render(
        createElement(TabRail, { onNewSession: onNew, onEditProfile: onEdit }),
      );
    });

    const input = document.querySelector<HTMLInputElement>(
      '[aria-label="Search tabs by path"]',
    )!;
    fill(input, "WORKSPACE\\YMSCLOUD");
    expect(
      Array.from(document.querySelectorAll<HTMLElement>("[data-tab-id]")).map(
        (row) => row.dataset.tabId,
      ),
    ).toEqual(["workspace"]);

    fill(input, "documents");
    expect(
      Array.from(document.querySelectorAll<HTMLElement>("[data-tab-id]")).map(
        (row) => row.dataset.tabId,
      ),
    ).toEqual(["documents"]);
  });

  function renderDraggableTabs() {
    act(() => {
      useStore.setState({ ...initialState }, true);
      HTMLElement.prototype.scrollIntoView = vi.fn();
      for (const id of ["a", "b", "c"]) {
        useStore.getState().addTab(localInfo(id), {
          id: "",
          name: id,
          kind: "local",
          cwd: `/workspace/${id}`,
        });
      }
      root.render(
        createElement(TabRail, { onNewSession: onNew, onEditProfile: onEdit }),
      );
    });

    const rail = document.querySelector<HTMLElement>(".tab-rail")!;
    rail.setPointerCapture = vi.fn();
    rail.hasPointerCapture = () => true;
    rail.releasePointerCapture = vi.fn();
    const rows = Array.from(
      rail.querySelectorAll<HTMLElement>("[data-tab-id]"),
    );
    rows.forEach((row, index) => {
      row.getBoundingClientRect = () =>
        ({
          top: index * 68,
          bottom: (index + 1) * 68,
          left: 0,
          right: 280,
          width: 280,
          height: 68,
          x: 0,
          y: index * 68,
          toJSON: () => ({}),
        }) as DOMRect;
    });

    const pointer = (
      type: string,
      target: Element,
      clientY: number,
      buttons = type === "pointerdown" || type === "pointermove" ? 1 : 0,
    ) => {
      const event = new MouseEvent(type, {
        bubbles: true,
        button: 0,
        buttons,
        clientY,
      });
      Object.defineProperty(event, "pointerId", { value: 7 });
      target.dispatchEvent(event);
    };
    return { rail, rows, pointer };
  }

  it("按其他标签的实时中点计算拖拽落点", () => {
    const { rail, rows, pointer } = renderDraggableTabs();
    act(() => pointer("pointerdown", rows[1], 102));
    act(() => pointer("pointermove", rail, 205));
    expect(rows[1].style.getPropertyValue("--tab-drag-y")).toBe("103px");
    act(() => pointer("pointerup", rail, 205));
    expect(useStore.getState().tabs.map((tab) => tab.info.id)).toEqual([
      "a",
      "c",
      "b",
    ]);
  });

  it.each([false, true])("无按键移动会取消残留手势，已开始拖拽：%s", (active) => {
    const { rail, rows, pointer } = renderDraggableTabs();
    act(() => pointer("pointerdown", rows[1], 102));
    if (active) act(() => pointer("pointermove", rail, 205));

    // 模拟 WebView 漏发释放：下次只有移动事件，左键已经不再按住。
    act(() => pointer("pointermove", rail, 220, 0));
    expect(rail.classList.contains("is-reordering")).toBe(false);
    expect(rows[1].style.getPropertyValue("--tab-drag-y")).toBe("");
    expect(rail.querySelector(".is-drop-before, .is-drop-after")).toBeNull();
    expect(useStore.getState().tabs.map((tab) => tab.info.id)).toEqual(["a", "b", "c"]);

    // 清理后仍能重新发起正常拖拽，不能留下永久阻塞的新按压状态。
    act(() => pointer("pointerdown", rows[1], 102));
    act(() => pointer("pointermove", rail, 205));
    act(() => pointer("pointerup", rail, 205));
    expect(useStore.getState().tabs.map((tab) => tab.info.id)).toEqual(["a", "c", "b"]);
  });

  it.each([false, true])("触摸板滚动会取消残留手势，已开始拖拽：%s", (active) => {
    const { rail, rows, pointer } = renderDraggableTabs();
    act(() => pointer("pointerdown", rows[1], 102));
    if (active) act(() => pointer("pointermove", rail, 205));

    act(() => {
      rail.dispatchEvent(new WheelEvent("wheel", { bubbles: true, buttons: 0, deltaY: 40 }));
      rail.scrollTop = 40;
      rail.dispatchEvent(new Event("scroll"));
      pointer("pointermove", rail, 220, 0);
    });
    expect(rail.classList.contains("is-reordering")).toBe(false);
    expect(rows[1].style.getPropertyValue("--tab-drag-y")).toBe("");
    expect(rail.querySelector(".is-drop-before, .is-drop-after")).toBeNull();
    expect(useStore.getState().tabs.map((tab) => tab.info.id)).toEqual(["a", "b", "c"]);
  });

  it.each(["pointercancel", "lostpointercapture", "blur"])("%s 会结束拖拽且不改变顺序", (type) => {
    const { rail, rows, pointer } = renderDraggableTabs();
    act(() => pointer("pointerdown", rows[1], 102));
    act(() => pointer("pointermove", rail, 205));
    expect(rail.classList.contains("is-reordering")).toBe(true);
    act(() => {
      if (type === "blur") window.dispatchEvent(new Event("blur"));
      else pointer(type, rail, 205);
    });
    expect(rail.classList.contains("is-reordering")).toBe(false);
    expect(rows[1].style.getPropertyValue("--tab-drag-y")).toBe("");
    expect(useStore.getState().tabs.map((tab) => tab.info.id)).toEqual(["a", "b", "c"]);
    expect(rail.releasePointerCapture).toHaveBeenCalledWith(7);
  });
});
