import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("./platform", () => ({
  IS_MAC: true,
  IS_WINDOWS: false,
}));

import { TerminalController } from "./terminal";

const controllers: TerminalController[] = [];

/** A controller with a local shell's rules on; `remote` turns them off. */
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

/**
 * Attaches to a real host, the way a pane does, and places the rules. jsdom
 * lays nothing out, so the cell height they are placed by is stubbed: what is
 * under test is that a rule exists and which row it lands on.
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

/** The rules actually being drawn, out of the layer's pooled elements. */
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

    // Rows: 0 the prompt `ls` was typed on, 1 its output, 2 the next prompt.
    expect(rules(host)).toEqual(["translateY(0px)"]);

    await run(controller, "pwd", "/Users/huaxin\r\n");
    await write(controller, prompt);
    place();

    expect(rules(host)).toEqual(["translateY(0px)", "translateY(32px)"]);
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
