import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("./platform", () => ({
  IS_MAC: true,
  IS_WINDOWS: false,
}));

import { TerminalController } from "./terminal";

const controllers: TerminalController[] = [];

/** 创建开启本地命令分割线的控制器；`remote` 用于验证远程会话不绘制。 */
function createController(remote = false) {
  const controller = new TerminalController(
    "divider-test",
    {
      onData() {},
      onResize() {},
      onStatus() {},
      onCommand() {},
      onCommandState() {},
      suggest: () => [],
    },
    13,
    200,
    undefined,
    undefined,
    !remote,
  );
  controllers.push(controller);
  return controller;
}

function write(controller: TerminalController, text: string): Promise<void> {
  return new Promise((resolve) => controller.term.write(text, resolve));
}

/** 在当前 prompt 输入命令、回车并写入输出。 */
async function run(
  controller: TerminalController,
  command: string,
  output: string,
) {
  controller.term.input(command, true);
  await write(controller, command);
  controller.term.input("\r", true);
  await write(controller, `\r\n${output}`);
}

/**
 * 按 pane 的方式挂载真实 host 并放置分割线。jsdom 不参与布局，因此固定
 * cell 高度；这里验证分割线是否存在，以及最终落在哪一行。
 */
function attachHost(controller: TerminalController) {
  const host = document.createElement("div");
  document.body.appendChild(host);
  controller.attach(host);
  const internals = controller as unknown as {
    cellHeight: number;
    syncDividers(): void;
  };
  internals.cellHeight = 16;
  return { host, place: () => internals.syncDividers() };
}

/** 返回复用池中当前实际显示的分割线位置。 */
function rules(host: HTMLElement): string[] {
  return [...host.querySelectorAll<HTMLElement>(".term-divider")]
    .filter((rule) => rule.style.display !== "none")
    .map((rule) => rule.style.transform);
}

afterEach(() => {
  for (const controller of controllers.splice(0)) controller.dispose();
});

describe("the rule above each prompt", () => {
  it("puts a rule on the row each command was typed on", async () => {
    const controller = createController();
    const { host, place } = attachHost(controller);
    const prompt = "huaxin ~ % ";

    await write(controller, prompt);
    await run(controller, "ls", "README.md\r\n");
    await write(controller, prompt);
    place();

    // 第 0 行输入 `ls`，第 1 行是输出，第 2 行是下一个 prompt。
    expect(rules(host)).toEqual(["translateY(0px)"]);

    await run(controller, "pwd", "/Users/huaxin\r\n");
    await write(controller, prompt);
    place();

    expect(rules(host)).toEqual(["translateY(0px)", "translateY(32px)"]);
  });

  it("draws an integrated prompt divider before the command is submitted", async () => {
    const controller = createController();
    const { host, place } = attachHost(controller);
    const prompt = "huaxin ~ % ";

    await write(controller, `\x1b]133;A;zenterm-initial\x07${prompt}`);
    await run(controller, "ls", "README.md\r\n");
    await write(
      controller,
      `\x1b]133;A;zenterm-spacer\x07\r\n\r\n${prompt}`,
    );
    place();

    const cursorRow =
      controller.term.buffer.active.baseY +
      controller.term.buffer.active.cursorY;
    expect(cursorRow).toBe(4);
    expect(rules(host)).toEqual(["translateY(48px)"]);
  });

  it("keeps one divider when the shell repeatedly redraws the current prompt", async () => {
    const controller = createController();
    const { host, place } = attachHost(controller);
    const prompt = "huaxin ~ % ";
    const spacedPrompt = `\x1b]133;A;zenterm-spacer\x07\r\n\r\n${prompt}`;

    await write(controller, `\x1b]133;A;zenterm-initial\x07${prompt}`);
    await run(controller, "ls", "README.md\r\n");
    await write(controller, spacedPrompt);
    place();
    expect(rules(host)).toEqual(["translateY(48px)"]);

    // zsh 重绘多行 prompt 会退回留白起点，并再次发送 prompt 内的 OSC 标记。
    for (let redraw = 0; redraw < 12; redraw += 1) {
      await write(controller, `\r\x1b[2A${spacedPrompt}`);
      place();
      expect(rules(host)).toEqual(["translateY(48px)"]);
    }

    await run(controller, "pwd", "/Users/huaxin\r\n");
    await write(controller, spacedPrompt);
    place();
    expect(rules(host)).toEqual(["translateY(48px)", "translateY(112px)"]);
  });

  it("draws none where a remote shell has nothing to divide", async () => {
    const controller = createController(true);
    const { host, place } = attachHost(controller);

    await write(controller, "alice@server:~/work$ ");
    await run(controller, "ls", "README.md\r\n");
    await write(controller, "alice@server:~/work$ ");
    place();

    expect(rules(host)).toEqual([]);
  });

  it("keeps them off a full-screen program's buffer", async () => {
    const controller = createController();
    const { host, place } = attachHost(controller);

    await write(controller, "huaxin ~ % ");
    await run(controller, "vim", "\x1b[?1049h");
    await write(controller, "huaxin ~ % ");
    place();

    expect(rules(host)).toEqual([]);
  });
});
