import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("./platform", () => ({
  IS_MAC: true,
  IS_WINDOWS: false,
}));

import { TerminalController } from "./terminal";

const controllers: TerminalController[] = [];

/**
 * A controller with a local shell's block rules on. `remote` turns them off,
 * which is what a session without shell integration gets (`dividers`).
 */
function createController(remote = false) {
  const controller = new TerminalController(
    "block-test",
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

/** Types a command at the prompt, presses Enter, and prints its output. */
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

afterEach(() => {
  for (const controller of controllers.splice(0)) controller.dispose();
});

describe("command blocks", () => {
  it("keeps a command with the output printed under it", async () => {
    const controller = createController();
    const prompt = "huaxin ~ % ";

    await write(controller, prompt);
    await run(controller, "ls", "README.md\r\nsrc\r\n");
    await write(controller, prompt);

    const blocks = controller.blocks();
    expect(blocks).toHaveLength(1);
    expect(blocks[0].command).toBe("ls");
    expect(controller.blockText(blocks[0], "command")).toBe("ls");
    // The blank row the shell leaves before the next prompt is not output.
    expect(controller.blockText(blocks[0], "output")).toBe("README.md\nsrc");
  });

  it("ends a block at the prompt that follows it", async () => {
    const controller = createController();
    const prompt = "huaxin ~ % ";

    await write(controller, prompt);
    await run(controller, "ls", "README.md\r\n");
    await write(controller, prompt);
    await run(controller, "pwd", "/Users/huaxin\r\n");
    await write(controller, prompt);

    const [first, second] = controller.blocks();
    expect(controller.blocks()).toHaveLength(2);
    expect(first.command).toBe("ls");
    expect(second.command).toBe("pwd");
    expect(controller.blockText(first, "output")).toBe("README.md");
    expect(controller.blockText(second, "output")).toBe("/Users/huaxin");
  });

  it("finds the block a row falls in", async () => {
    const controller = createController();
    const prompt = "huaxin ~ % ";

    await write(controller, prompt);
    await run(controller, "ls", "README.md\r\n");
    await write(controller, prompt);
    await run(controller, "pwd", "/Users/huaxin\r\n");
    await write(controller, prompt);

    const [first, second] = controller.blocks();
    expect(controller.blockAt(first.line)?.command).toBe("ls");
    expect(controller.blockAt(first.line + 1)?.command).toBe("ls");
    expect(controller.blockAt(second.line)?.command).toBe("pwd");
    expect(controller.blockAt(second.line + 1)?.command).toBe("pwd");
  });

  it("counts a command with no output at all", async () => {
    const controller = createController();
    const prompt = "huaxin ~ % ";

    await write(controller, prompt);
    await run(controller, "cd /data", "");
    await write(controller, prompt);

    const [only] = controller.blocks();
    expect(only.command).toBe("cd /data");
    expect(controller.blockText(only, "output")).toBe("");
  });

  it("has no blocks without the rules that divide them", async () => {
    const controller = createController(true);
    const prompt = "alice@server:~/work$ ";

    await write(controller, prompt);
    await run(controller, "ls", "README.md\r\n");
    await write(controller, prompt);

    expect(controller.blocks()).toEqual([]);
    expect(controller.blockAt(0)).toBeNull();
  });
});
