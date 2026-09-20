import { describe, expect, it } from "vitest";

import { COMMON_COMMAND_GROUPS } from "./commonCommands";
import { MAX_SUGGESTIONS } from "./history";
import { librarySuggestions, mergeSuggestions } from "./suggestions";

describe("the commands shipped with the app", () => {
  it("completes a tool's everyday invocations", () => {
    const rows = librarySuggestions("kubectl get po").map((row) => row.command);
    expect(rows).toContain("kubectl get pods -A");
    expect(rows.every((command) => command.includes("kubectl get po"))).toBe(
      true,
    );
    expect(librarySuggestions("docker logs").map((r) => r.command)).toContain(
      "docker logs -f ",
    );
  });

  it("says nothing before there is enough to go on", () => {
    expect(librarySuggestions("k")).toEqual([]);
    expect(librarySuggestions(" ")).toEqual([]);
  });

  it("leaves out a row that would not save keystrokes", () => {
    // `git stat` -> `git status` saves one character: not worth a row.
    expect(librarySuggestions("git stat").map((r) => r.command)).not.toContain(
      "git status",
    );
  });

  it("points at where the typed text sits, for highlighting", () => {
    const [row] = librarySuggestions("docker ps");
    expect(row.command.startsWith("docker ps")).toBe(true);
    expect(row.matchStart).toBe(0);
    const [later] = librarySuggestions("journalctl -u");
    expect(later.command.slice(later.matchStart)).toContain("journalctl -u");
  });

  it("ships nothing that destroys by accident", () => {
    // The rule the list is kept to: a stray Enter costs a look, not a cluster.
    const destructive = [
      "delete",
      "drain",
      "system prune",
      "volume rm",
      "rm -rf",
      "clean -f",
      "reset --hard",
      "push --force ",
      "kill",
    ];
    for (const command of COMMON_COMMAND_GROUPS.flat()) {
      for (const word of destructive) {
        expect(command).not.toContain(word);
      }
    }
  });
});

describe("merging the library into the history", () => {
  it("keeps the user's own commands first and repeats none", () => {
    const merged = mergeSuggestions(
      [{ command: "kubectl get pods -A", matchStart: 0 }],
      [
        { command: "kubectl get pods -A", matchStart: 0 },
        { command: "kubectl get svc", matchStart: 0 },
      ],
    );
    expect(merged.map((row) => row.command)).toEqual([
      "kubectl get pods -A",
      "kubectl get svc",
    ]);
  });

  it("stops at the popup's own row limit", () => {
    const mine = Array.from({ length: MAX_SUGGESTIONS }, (_, index) => ({
      command: `mine ${index}`,
      matchStart: 0,
    }));
    const merged = mergeSuggestions(mine, [
      { command: "kubectl get svc", matchStart: 0 },
      { command: "docker ps", matchStart: 0 },
    ]);
    expect(merged).toHaveLength(MAX_SUGGESTIONS);
    expect(merged.every((row) => row.command.startsWith("mine"))).toBe(true);
  });
});
