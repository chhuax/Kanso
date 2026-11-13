import type { DragDropEvent } from "@tauri-apps/api/webview";
import { PhysicalPosition } from "@tauri-apps/api/dpi";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { onDragDropEvent, platform } = vi.hoisted(() => ({
  onDragDropEvent: vi.fn(),
  platform: { IS_MAC: true, IS_WINDOWS: false },
}));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent }),
}));
vi.mock("../platform", () => platform);
vi.mock("../actions", () => ({ ensureController: vi.fn() }));

import { ensureController } from "../actions";
import { useStore } from "../store";
import type { TerminalController } from "../terminal";
import { disposeController, setController } from "../terminalRegistry";
import { SessionPane } from "./TerminalPane";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
const initialState = useStore.getState();
type DropHandler = (event: { payload: DragDropEvent }) => void;
let root: Root;
let container: HTMLDivElement;
let handlers: Set<DropHandler>;
let elementFromPoint: ReturnType<typeof vi.fn>;

function controller() {
  return {
    attach: vi.fn(), dispose: vi.fn(), focus: vi.fn(), fit: vi.fn(),
    pastePaths: vi.fn(), setGutterMode: vi.fn(), setFontSize: vi.fn(),
    setFontFamily: vi.fn(), setScrollback: vi.fn(), setCursor: vi.fn(),
    setSuggestions: vi.fn(), setRightClickAction: vi.fn(), setVisible: vi.fn(),
  };
}
const first = controller();
const second = controller();

async function render(firstVisible = true) {
  await act(async () => {
    root.render(createElement("div", {}, ...useStore.getState().tabs.map((tab, index) =>
      createElement(SessionPane, {
        key: tab.info.id, tab, style: {},
        visible: index === 0 ? firstVisible : true, focused: false,
      }),
    )));
  });
}

function drop(paths = ["/Users/example/文件.txt"], x = 800, y = 300) {
  act(() => {
    for (const handler of handlers) {
      handler({ payload: { type: "drop", paths, position: new PhysicalPosition(x, y) } });
    }
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  platform.IS_MAC = true;
  platform.IS_WINDOWS = false;
  handlers = new Set();
  onDragDropEvent.mockImplementation(async (handler: DropHandler) => {
    handlers.add(handler);
    return () => handlers.delete(handler);
  });
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  vi.stubGlobal("devicePixelRatio", 2);
  elementFromPoint = vi.fn();
  Object.defineProperty(document, "elementFromPoint", { configurable: true, value: elementFromPoint });
  useStore.setState({ ...initialState, tabs: [] }, true);
  for (const [index, mock] of [first, second].entries()) {
    const id = `drop-${index}`;
    useStore.getState().addTab({
      id, profileId: null, name: id, kind: "local", protocol: "shell",
      address: "test", color: null, supportsRemoteFiles: false, legacyAlgorithms: [],
    }, { id: "", name: id, kind: "local" }, "connected");
    setController(id, mock as unknown as TerminalController);
  }
  vi.mocked(ensureController).mockImplementation(async (id) =>
    (id === "drop-0" ? first : second) as unknown as TerminalController,
  );
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  delete (document as unknown as { elementFromPoint?: unknown }).elementFromPoint;
  disposeController("drop-0");
  disposeController("drop-1");
  useStore.setState(initialState, true);
  vi.unstubAllGlobals();
});

describe("终端原生文件拖放", () => {
  it("Retina Mac 按松手位置向对应终端粘贴文件及文件夹路径并切换焦点", async () => {
    await render();
    const pane = container.querySelectorAll(".term-pane")[0];
    const child = pane.appendChild(document.createElement("span"));
    elementFromPoint.mockReturnValue(child);
    const paths = ["/Users/example/文件.txt", "/Users/example/项目 目录"];

    drop(paths);

    expect(elementFromPoint).toHaveBeenCalledWith(800, 300);
    expect(first.pastePaths).toHaveBeenCalledExactlyOnceWith(paths);
    expect(first.focus).toHaveBeenCalledOnce();
    expect(second.pastePaths).not.toHaveBeenCalled();
    expect(useStore.getState().activeId).toBe("drop-0");
  });

  it("拖到另一分屏时只粘贴到松手所在终端", async () => {
    await render();
    elementFromPoint.mockReturnValue(container.querySelectorAll(".term-pane")[1]);
    drop();
    expect(second.pastePaths).toHaveBeenCalledOnce();
    expect(first.pastePaths).not.toHaveBeenCalled();
  });

  it("拖到文件面板或覆盖终端的对话框时不向终端发送路径", async () => {
    await render();
    elementFromPoint.mockReturnValue(container);
    drop();
    expect(first.pastePaths).not.toHaveBeenCalled();
    expect(second.pastePaths).not.toHaveBeenCalled();
  });

  it("隐藏标签释放监听并且不接收路径", async () => {
    await render();
    await render(false);
    elementFromPoint.mockReturnValue(container.querySelector(".term-pane"));
    drop();
    expect(handlers.size).toBe(1);
    expect(first.pastePaths).not.toHaveBeenCalled();
    expect(second.pastePaths).not.toHaveBeenCalled();
  });

  it("拖动经过、取消或没有路径时不输入", async () => {
    await render();
    elementFromPoint.mockReturnValue(container.querySelector(".term-pane"));
    act(() => {
      for (const handler of handlers) {
        handler({ payload: { type: "enter", paths: ["/tmp/file"], position: new PhysicalPosition(800, 300) } });
        handler({ payload: { type: "over", position: new PhysicalPosition(800, 300) } });
        handler({ payload: { type: "leave" } });
      }
    });
    drop([]);
    expect(first.pastePaths).not.toHaveBeenCalled();
    expect(second.pastePaths).not.toHaveBeenCalled();
  });

  it("Windows 的物理坐标转换成 CSS 坐标后命中终端", async () => {
    platform.IS_MAC = false;
    platform.IS_WINDOWS = true;
    await render();
    elementFromPoint.mockReturnValue(container.querySelector(".term-pane"));
    drop();
    expect(elementFromPoint).toHaveBeenCalledWith(400, 150);
    expect(first.pastePaths).toHaveBeenCalledOnce();
  });

  it("卸载后取消所有原生拖放监听", async () => {
    await render();
    await act(async () => root.render(null));
    expect(handlers.size).toBe(0);
  });
});
