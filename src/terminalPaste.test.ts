import { afterEach, describe, expect, it, vi } from "vitest";

const { readClipboardContent } = vi.hoisted(() => ({
  readClipboardContent: vi.fn(),
}));

vi.mock("./api", () => ({ readClipboardContent }));
vi.mock("./platform", () => ({
  IS_MAC: true,
  IS_WINDOWS: false,
}));

import type { AiTool } from "./aiTools";
import { TerminalController } from "./terminal";

const controllers: TerminalController[] = [];

function createController(output: string[]) {
  const controller = new TerminalController(
    "paste-test",
    {
      onData: (data) => output.push(data),
      onResize() {},
      onStatus() {},
      onCommand() {},
      onCommandState() {},
      suggest: () => [],
    },
    13,
    100,
  );
  controllers.push(controller);
  return controller;
}

function enterAiSession(controller: TerminalController, tool: AiTool) {
  const state = controller as unknown as {
    aiSession: boolean;
    aiTool: AiTool | null;
  };
  state.aiSession = true;
  state.aiTool = tool;
}

afterEach(() => {
  readClipboardContent.mockReset();
  for (const controller of controllers.splice(0)) controller.dispose();
});

describe("终端剪贴板图片粘贴", () => {
  it("在 Claude Code 会话中把图片粘贴转发为 Ctrl+V", async () => {
    const output: string[] = [];
    const controller = createController(output);
    enterAiSession(controller, {
      id: "claude",
      label: "Claude",
      color: "#d97757",
    });
    readClipboardContent.mockResolvedValue({
      text: "",
      paths: [],
      hasImage: true,
    });

    controller.pasteFromClipboard();

    await vi.waitFor(() => expect(output).toEqual(["\x16"]));
  });

  it("在 AI CLI 会话中仍把普通文本交给 xterm 粘贴", async () => {
    const controller = createController([]);
    enterAiSession(controller, {
      id: "claude",
      label: "Claude",
      color: "#d97757",
    });
    const paste = vi.spyOn(controller.term, "paste").mockImplementation(() => {});
    readClipboardContent.mockResolvedValue({
      text: "plain text",
      paths: [],
      hasImage: false,
    });

    controller.pasteFromClipboard();

    await vi.waitFor(() => expect(paste).toHaveBeenCalledWith("plain text"));
  });

  it("普通 shell 不转发图片粘贴键", async () => {
    const output: string[] = [];
    const controller = createController(output);
    const paste = vi.spyOn(controller.term, "paste").mockImplementation(() => {});
    readClipboardContent.mockResolvedValue({
      text: "",
      paths: [],
      hasImage: true,
    });

    controller.pasteFromClipboard();

    await vi.waitFor(() => expect(readClipboardContent).toHaveBeenCalled());
    expect(output).toEqual([]);
    expect(paste).not.toHaveBeenCalled();
  });

  it("普通 shell 优先粘贴已转义的完整文件路径", async () => {
    const controller = createController([]);
    const paste = vi.spyOn(controller.term, "paste").mockImplementation(() => {});
    readClipboardContent.mockResolvedValue({
      text: "YMSCloud-流程设计.html",
      paths: [
        "/Users/example/Documents/2027 新产品设计/YMSCloud-流程设计.html",
      ],
      hasImage: false,
    });

    controller.pasteFromClipboard();

    await vi.waitFor(() =>
      expect(paste).toHaveBeenCalledWith(
        "/Users/example/Documents/2027\\ 新产品设计/YMSCloud-流程设计.html",
      ),
    );
  });
});
