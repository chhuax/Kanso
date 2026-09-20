import { beforeEach, describe, expect, it, vi } from "vitest";

// The history mirrors itself in memory and persists through the backend; the
// backend is not what this is about.
vi.mock("./api", () => ({
  listCommandHistory: () => Promise.resolve([]),
  recordCommand: () => Promise.resolve(),
  clearCommandHistory: () => Promise.resolve(),
}));

import { commandHistory } from "./history";

const HOST = "ssh:macpro";

beforeEach(async () => {
  await commandHistory.clear();
});

describe("browsing the remembered commands", () => {
  it("answers with the most recent first", async () => {
    commandHistory.record("git status", HOST);
    await new Promise((resolve) => setTimeout(resolve, 2));
    commandHistory.record("git stash", HOST);
    await new Promise((resolve) => setTimeout(resolve, 2));
    commandHistory.record("ls -la", HOST);
    expect(commandHistory.browse("", HOST).map((row) => row.command)).toEqual([
      "ls -la",
      "git stash",
      "git status",
    ]);
  });

  it("narrows by what was typed, anywhere in the command", () => {
    commandHistory.record("git status", HOST);
    commandHistory.record("kubectl get pods", HOST);
    commandHistory.record("docker ps", HOST);
    expect(commandHistory.browse("pods", HOST).map((row) => row.command)).toEqual([
      "kubectl get pods",
    ]);
    // Case does not decide whether a remembered command is found.
    expect(commandHistory.browse("KUBECTL", HOST)).toHaveLength(1);
    expect(commandHistory.browse("git", HOST).map((row) => row.command)).toEqual([
      "git status",
    ]);
  });

  it("puts this shell's own commands first", async () => {
    // The other host's command is the more recent one, and still sorts under
    // the one that ran here.
    commandHistory.record("tail -f app.log", "ssh:macmini");
    await new Promise((resolve) => setTimeout(resolve, 2));
    commandHistory.record("git status", HOST);
    expect(commandHistory.browse("", HOST)[0].command).toBe("git status");
    expect(commandHistory.browse("", HOST).map((row) => row.host)).toEqual([
      HOST,
      "ssh:macmini",
    ]);
  });

  it("keeps the count and the time a command was last run", async () => {
    commandHistory.record("make test", HOST);
    commandHistory.record("make test", HOST);
    const [row] = commandHistory.browse("make", HOST);
    expect(row.count).toBe(2);
    expect(Date.now() - row.lastUsed).toBeLessThan(1000);
  });

  it("holds a screenful, not the whole record", () => {
    for (let i = 0; i < 140; i += 1) commandHistory.record(`echo ${i}`, HOST);
    expect(commandHistory.browse("", HOST, 100)).toHaveLength(100);
  });
});
