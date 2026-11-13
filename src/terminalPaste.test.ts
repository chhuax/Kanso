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


describe("终端文件路径粘贴", () => {
  it("将含空格、中文和 shell 特殊字符的文件与文件夹转义后一次粘贴", () => {
    const controller = createController([]);
    const paste = vi.spyOn(controller.term, "paste").mockImplementation(() => {});

    controller.pastePaths([
      "/Users/example/项目 目录/截图.png",
      "/tmp/a'$文件;[1].txt",
    ]);

    expect(paste).toHaveBeenCalledExactlyOnceWith(
      "/Users/example/项目\\ 目录/截图.png /tmp/a\\'\\$文件\\;\\[1\\].txt",
    );
    expect(readClipboardContent).not.toHaveBeenCalled();
  });

  it("AI CLI 保留原始路径，与从 Finder 复制路径的语义一致", () => {
    const controller = createController([]);
    enterAiSession(controller, { id: "claude", label: "Claude", color: "#d97757" });
    const paste = vi.spyOn(controller.term, "paste").mockImplementation(() => {});

    controller.pastePaths(["/Users/example/项目 目录/图片.png"]);

    expect(paste).toHaveBeenCalledExactlyOnceWith("/Users/example/项目 目录/图片.png");
  });

  it("通过 xterm 发送 bracketed paste，且不追加回车执行命令", async () => {
    const output: string[] = [];
    const controller = createController(output);
    controller.term.open(document.createElement("div"));
    await new Promise<void>((resolve) => controller.term.write("\x1b[?2004h", resolve));

    controller.pastePaths(["/tmp/项目 目录"]);

    expect(output).toEqual(["\x1b[200~/tmp/项目\\ 目录\x1b[201~"]);
  });

  it("没有文件路径时不发送输入", () => {
    const controller = createController([]);
    const paste = vi.spyOn(controller.term, "paste").mockImplementation(() => {});
    controller.pastePaths([]);
    expect(paste).not.toHaveBeenCalled();
  });
});
