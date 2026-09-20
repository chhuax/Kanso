import { describe, expect, it, vi } from "vitest";

vi.mock("./platform", () => ({
  IS_MAC: true,
  IS_WINDOWS: false,
}));

import {
  completionsFor,
  lineContext,
  pathCompletions,
  tabAction,
  wantsPath,
  type CompletionBackend,
} from "./completion";
import type { FileEntry } from "./types";

/** A directory entry as the backend reports it; only the fields used here. */
function entry(name: string, isDir = false): FileEntry {
  return {
    name,
    path: `/tmp/${name}`,
    isDir,
    isSymlink: false,
    size: 0,
    modified: null,
    permissions: null,
    owner: null,
    group: null,
  };
}

/** A backend over a fixed tree, recording what it was asked for. */
function backend(tree: Record<string, FileEntry[]>, cwd = "/tmp"): CompletionBackend & {
  asked: string[];
} {
  const asked: string[] = [];
  return {
    local: true,
    asked,
    cwd: () => cwd,
    list: (path: string) => {
      asked.push(path);
      return Promise.resolve(tree[path] ?? []);
    },
  };
}

describe("reading the line", () => {
  it("finds the command and the word under the caret", () => {
    const context = lineContext("cd src/te", 9);
    expect(context.command).toBe("cd");
    expect(context.token).toBe("src/te");
    expect(context.tokenStart).toBe(3);
    expect(context.wordsBefore).toEqual([]);
  });

  it("takes a pipeline's word from the segment it sits in", () => {
    const context = lineContext("ls -la | grep pack", 18);
    expect(context.command).toBe("grep");
    expect(context.token).toBe("pack");
  });

  it("sees past an assignment and a wrapper", () => {
    expect(lineContext("FOO=1 git status", 17).command).toBe("git");
    expect(lineContext("sudo kubectl get ", 18).command).toBe("kubectl");
  });

  it("strips the quotes the shell put around a word", () => {
    const context = lineContext('cat "My Fi', 11);
    expect(context.token).toBe("My Fi");
    // The offset still points at the opening quote, which the replacement keeps.
    expect(context.input.slice(context.tokenStart)).toBe('My Fi');
  });

  it("reports words before the caret's own", () => {
    const context = lineContext("kubectl get po", 14);
    expect(context.wordsBefore).toEqual(["get"]);
    expect(context.token).toBe("po");
  });
});

describe("the words that want a path", () => {
  it("counts a file-taking command", () => {
    expect(wantsPath(lineContext("cd src", 6))).toBe(true);
    expect(wantsPath(lineContext("cat READ", 8))).toBe(true);
    expect(wantsPath(lineContext("vim ", 4))).toBe(true);
  });

  it("leaves a flag alone", () => {
    expect(wantsPath(lineContext("ls -l", 5))).toBe(false);
  });

  it("takes a tool's verb into account", () => {
    expect(wantsPath(lineContext("git add src/", 12))).toBe(true);
    expect(wantsPath(lineContext("git checkout ma", 16))).toBe(true);
  });
});

describe("path rows", () => {
  const tree = {
    "/tmp": [entry("src", true), entry("src2", true), entry("setup.sh"), entry(".hidden")],
    "/tmp/src": [entry("terminal.ts"), entry("terminal.test.ts")],
  };

  it("offers the entries of the typed directory", async () => {
    const rows = await pathCompletions(lineContext("cd sr", 5), backend(tree));
    expect(rows.map((row) => row.label)).toEqual(["src/", "src2/"]);
    // The line keeps the command and swaps only the word.
    expect(rows[0].line).toBe("cd src/");
    expect(rows[0].replaceStart).toBe(3);
  });

  it("completes inside a word that already names a path", async () => {
    const rows = await pathCompletions(lineContext("vim src/term", 12), backend(tree));
    expect(rows.map((row) => row.label)).toEqual(["terminal.test.ts", "terminal.ts"]);
    expect(rows[0].line).toBe("vim src/terminal.test.ts");
    // Only the name is replaced, so `src/` is not typed again.
    expect(rows[0].replaceStart).toBe(8);
    expect(rows[0].replaceEnd).toBe(12);
  });

  it("sorts the file itself above a file about it", async () => {
    // `src.` is where the two part ways: one is the file, the other is a file
    // whose name starts with its name and a dot.
    const both = await pathCompletions(
      lineContext("vim src.", 8),
      backend({ "/tmp": [entry("src.test.ts"), entry("src.ts")] }),
    );
    expect(both.map((row) => row.label)).toEqual(["src.test.ts", "src.ts"]);
  });

  it("resolves the directory against where the shell is", async () => {
    const stub = backend({ "/home/me/work": [entry("api", true)] }, "/home/me/work");
    const rows = await pathCompletions(lineContext("ls .", 4), stub);
    expect(stub.asked).toEqual(["/home/me/work"]);
    expect(rows[0].line).toBe("ls api/");
  });

  it("leaves a dotfile out until it is asked for", async () => {
    const bare = await pathCompletions(lineContext("cd ", 3), backend(tree));
    expect(bare.map((row) => row.label)).not.toContain(".hidden");
    const dotted = await pathCompletions(lineContext("cd .h", 5), backend(tree));
    expect(dotted.map((row) => row.label)).toEqual([".hidden"]);
  });

  it("quotes a name that needs it, keeping the shell's own quote", async () => {
    const rows = await pathCompletions(
      lineContext("cat My\\ Fi", 10),
      backend({ "/tmp": [entry("My File.txt")] }),
    );
    expect(rows[0].label).toBe("My File.txt");
    expect(rows[0].line).toBe('cat "My File.txt"');
  });
});

describe("what a tool says next", () => {
  const backendFor = backend({ "/tmp": [] });

  it("offers kubectl's subcommands, resources and flags", async () => {
    const subcommands = await completionsFor("kubectl ge", 10, backendFor, "host");
    expect(subcommands.map((row) => row.label)).toContain("get");

    const resources = await completionsFor("kubectl get po", 14, backendFor, "host");
    const labels = resources.map((row) => row.label);
    expect(labels).toContain("pods");
    expect(labels).toContain("po");
    // A resource row rewrites the word, not the whole line.
    expect(resources.find((row) => row.label === "pods")?.line).toBe("kubectl get pods");

    const flags = await completionsFor("kubectl get pods -", 19, backendFor, "host");
    expect(flags.map((row) => row.label)).toContain("--namespace");
    expect(flags[0].hint).toBeTruthy();
  });

  it("offers git's subcommands and flags", async () => {
    const rows = await completionsFor("git che", 7, backendFor, "host");
    expect(rows.map((row) => row.label)).toContain("checkout");
    const flags = await completionsFor("git commit --am", 16, backendFor, "host");
    expect(flags.map((row) => row.label)).toContain("--amend");
  });

  it("leaves a tool alone once its own words are done", async () => {
    // A ref name is not something a table can guess at.
    const rows = await completionsFor("git checkout ma", 16, backendFor, "host");
    expect(rows.map((row) => row.label)).not.toContain("main");
  });

  it("keeps the typed words and replaces only the one under the caret", async () => {
    const rows = await completionsFor("kubectl get po", 14, backendFor, "host");
    const pods = rows.find((row) => row.label === "pods");
    expect(pods?.line).toBe("kubectl get pods");
    expect(pods?.replaceStart).toBe(12);
    expect(pods?.replaceEnd).toBe(14);
  });
});

describe("what Tab does", () => {
  it("takes the first row, then walks, then accepts", () => {
    expect(tabAction(3, -1, false)).toBe("first");
    expect(tabAction(3, 0, false)).toBe("next");
    expect(tabAction(3, 1, false)).toBe("next");
    // Past the last row the press accepts what is selected rather than
    // wrapping around a list the user has already read.
    expect(tabAction(3, 2, false)).toBe("accept");
  });

  it("steps backwards with shift", () => {
    expect(tabAction(3, -1, false, true)).toBe("last");
    expect(tabAction(3, 2, false, true)).toBe("previous");
    expect(tabAction(3, 0, false, true)).toBe("accept");
  });

  it("holds the key while an answer is on its way", () => {
    // A path in an SSH session is a round trip; the shell must not complete
    // behind the popup's back.
    expect(tabAction(0, -1, true)).toBe("hold");
    expect(tabAction(0, -1, false)).toBe("shell");
  });
});

describe("where the shell is", () => {
  it("waits for a directory that has to be asked for", async () => {
    const asked: string[] = [];
    const backend: CompletionBackend = {
      local: false,
      // A session that has not printed an OSC report yet: the answer arrives
      // from the server, so the popup has to await it.
      cwd: () => Promise.resolve("/srv/app"),
      list: (path) => {
        asked.push(path);
        return Promise.resolve([entry("logs", true)]);
      },
    };
    const rows = await completionsFor("cd log", 6, backend, "host");
    expect(asked).toEqual(["/srv/app"]);
    expect(rows.map((row) => row.label)).toEqual(["logs/"]);
    expect(rows[0].line).toBe("cd logs/");
  });

  it("asks for nothing when no directory is known", async () => {
    const asked: string[] = [];
    const backend: CompletionBackend = {
      local: true,
      cwd: () => null,
      list: (path) => {
        asked.push(path);
        return Promise.resolve([]);
      },
    };
    // With no directory there is nothing to list, and a relative word would
    // otherwise be resolved against the process's own.
    await completionsFor("cd sr", 5, backend, "host");
    expect(asked).toEqual(["."]);
  });
});

describe("what a flag's value may be", () => {
  const names = () =>
    Promise.resolve({
      contexts: ["minikube", "prod-eu"],
      namespaces: ["argocd", "default", "payments"],
    });

  it("offers a namespace after -n and --namespace", async () => {
    for (const line of ["kubectl -n ", "kubectl --namespace ", "kubectl get pods -n pay"]) {
      const backend: CompletionBackend = {
        local: true,
        cwd: () => "/tmp",
        list: () => Promise.resolve([]),
        names,
      };
      const rows = await completionsFor(line, line.length, backend, "host");
      const labels = rows.map((row) => row.label);
      if (line.endsWith("pay")) expect(labels).toEqual(["payments"]);
      else expect(labels).toContain("default");
      expect(rows[0].hint).toBe("namespace");
    }
  });

  it("offers a context after --context", async () => {
    const backend: CompletionBackend = {
      local: true,
      cwd: () => "/tmp",
      list: () => Promise.resolve([]),
      names,
    };
    const rows = await completionsFor("kubectl --context prod", 21, backend, "host");
    expect(rows.map((row) => row.label)).toEqual(["prod-eu"]);
    expect(rows[0].hint).toBe("context");
    expect(rows[0].line).toBe("kubectl --context prod-eu");
    // The flag and the command stay where the user put them.
    expect(rows[0].replaceStart).toBe(18);
  });

  it("prefers the name a flag declares over a file in the directory", async () => {
    let listed = false;
    const backend: CompletionBackend = {
      local: true,
      cwd: () => "/tmp",
      list: () => {
        listed = true;
        return Promise.resolve([entry("payments.yaml")]);
      },
      names,
    };
    const rows = await completionsFor("kubectl -n pay", 14, backend, "host");
    expect(rows.map((row) => row.label)).toEqual(["payments"]);
    // A file called `payments.yaml` is sitting in the directory the shell is
    // in, and it is not offered: this word is a namespace, not a path.
    expect(listed).toBe(false);
  });

  it("offers nothing where the file cannot be read", async () => {
    const backend: CompletionBackend = {
      local: true,
      cwd: () => "/tmp",
      list: () => Promise.resolve([]),
      // No `names`: an SSH session, whose kubeconfig is the server's.
    };
    const rows = await completionsFor("kubectl -n ", 11, backend, "host");
    expect(rows).toEqual([]);
  });
});
