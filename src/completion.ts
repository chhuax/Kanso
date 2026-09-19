import type { FileEntry } from "./types";
import { commandHistory, MAX_SUGGESTIONS } from "./history";
import { librarySuggestions } from "./suggestions";
import {
  GIT_FLAGS,
  GIT_SUBCOMMANDS,
  KUBECTL_FLAGS,
  KUBECTL_RESOURCES,
  KUBECTL_SUBCOMMANDS,
  type CommandFlag,
} from "./commandTables";

/**
 * One row of the completion popup. It carries the *whole* line the row would
 * leave behind rather than just the token it completes, because accepting a
 * suggestion is an edit of the shell's own line: the range between
 * `replaceStart` and `replaceEnd` is what changes, and everything before it
 * (the command, its flags) and after it is left exactly as the user typed it.
 *
 * `label` is what the row reads; it is the whole line for a command and just
 * the token for an argument, so a path completion shows the file name rather
 * than a copy of the line it sits in.
 */
export interface Completion {
  /** What the popup shows. */
  label: string;
  /** The line as it will read once this row is accepted. */
  line: string;
  /** Column in the current line where the replaced range starts. */
  replaceStart: number;
  /** Column where the replaced range ends. */
  replaceEnd: number;
  /** Column in `label` where the typed text starts, for highlighting. */
  matchStart: number;
  /** Characters the typed text covers in `label`. */
  matchLength: number;
  /** Shown at the right edge of the row. */
  hint?: string;
}

/**
 * How a completion provider reaches outside the front end. Injected rather
 * than imported so the engine is a pure function of its inputs and the tests
 * can drive it without a backend.
 */
export interface CompletionBackend {
  /** True for a local shell; false for an SSH session, whose paths are the server's. */
  local: boolean;
  /**
   * Where the session is, or a promise for it. The shell's own report (OSC 7)
   * answers at once; a session that has not reported one is asked instead,
   * which for an SSH host is a round trip — and asking is only worth it for
   * the commands that take a path.
   */
  cwd: () => string | null | Promise<string | null>;
  /** Directory entries of `path`, or [] when it cannot be read. */
  list: (path: string) => Promise<FileEntry[]>;
}

/**
 * The line the caret is on, split into the cursor's word and the command that
 * word belongs to. Quoting and escaping are respected only far enough to know
 * where a word ends: the completion value is re-quoted when it is accepted.
 *
 * A pipeline's word belongs to the segment it follows, so `ls | grep fo`
 * completes as an argument of `grep`, not of `ls`.
 */
export interface LineContext {
  /** Everything up to the caret. */
  input: string;
  /** The word the caret sits in, quotes and backslashes stripped. */
  token: string;
  /** True when the word was written with a quote or a backslash. */
  escaped: boolean;
  /** Offset of `token` in `input`. */
  tokenStart: number;
  /** Offset just past `token`. */
  tokenEnd: number;
  /** The command word of the segment holding the caret; "" at a bare prompt. */
  command: string;
  /** Words of the segment before the caret's word. */
  wordsBefore: string[];
  /** True when the caret's word is the segment's first word. */
  commandWord: boolean;
}

const WORD_BREAK = /[\s|;&()<>]/;

/** Splits a line into words, keeping each word's offset; quotes are kept. */
function splitWords(text: string): { word: string; start: number; end: number }[] {
  const words: { word: string; start: number; end: number }[] = [];
  let start = -1;
  let quote = "";
  for (let i = 0; i < text.length; i += 1) {
    const char = text[i];
    if (quote) {
      if (char === quote) quote = "";
      continue;
    }
    if (char === '"' || char === "'") {
      if (start === -1) start = i;
      quote = char;
      continue;
    }
    // A backslash makes the next character part of the word — `My\ Fi` is one
    // word and the space in it is not a break — while the word itself is kept
    // as written, escapes and all, for the reader to resolve.
    if (char === "\\" && i + 1 < text.length) {
      if (start === -1) start = i;
      i += 1;
      continue;
    }
    if (WORD_BREAK.test(char)) {
      if (start !== -1) {
        words.push({ word: text.slice(start, i), start, end: i });
        start = -1;
      }
      continue;
    }
    if (start === -1) start = i;
  }
  if (start !== -1) words.push({ word: text.slice(start), start, end: text.length });
  return words;
}

/** The words after the last separator, i.e. the segment the caret is in. */
function segmentWords(text: string): { word: string; start: number; end: number }[] {
  const cut = Math.max(
    text.lastIndexOf("|"),
    text.lastIndexOf(";"),
    text.lastIndexOf("&"),
    text.lastIndexOf("("),
  );
  const base = cut === -1 ? 0 : cut + 1;
  return splitWords(text.slice(base)).map((word) => ({
    ...word,
    start: word.start + base,
    end: word.end + base,
  }));
}

/** The command word of a segment, after assignments and wrapper commands. */
function commandOf(words: { word: string }[]): string {
  for (const { word } of words) {
    if (/^[A-Za-z_][A-Za-z0-9_]*=/.test(word)) continue;
    if (word === "sudo" || word === "command" || word === "builtin" || word === "nohup") {
      continue;
    }
    return word;
  }
  return "";
}

/**
 * The word as the shell will read it, and the range of the raw word it came
 * from: `My\ Fi` reads as `My Fi` covering all six characters, `"My Fi` as
 * `My Fi` starting one in. Everything the word costs — the opening quote and
 * each backslash — is what a completion has to replace.
 */
function wordHead(raw: string): {
  text: string;
  startOffset: number;
  endOffset: number;
} {
  const quote = raw[0];
  const quoted = quote === '"' || quote === "'";
  const body = quoted ? raw.slice(1) : raw;
  const closed = quoted && body.endsWith(quote);
  const trimmed = closed ? body.slice(0, -1) : body;
  const chars: string[] = [];
  const starts: number[] = [];
  const ends: number[] = [];
  for (let i = 0; i < trimmed.length; i += 1) {
    if (trimmed[i] === "\\" && i + 1 < trimmed.length) {
      starts.push(i + 1);
      ends.push(i + 2);
      i += 1;
      chars.push(trimmed[i]);
      continue;
    }
    starts.push(i);
    ends.push(i + 1);
    chars.push(trimmed[i]);
  }
  return {
    text: chars.join(""),
    startOffset: (quoted ? 1 : 0) + (starts[0] ?? 0),
    endOffset: quoted && closed ? trimmed.length + 2 : (ends[ends.length - 1] ?? 0),
  };
}

/**
 * Where the caret sits in the line, and which command owns it. The caret's
 * word comes from the segment's words rather than from scanning backwards, so
 * a caret inside a word still reports the whole word — which is what the
 * completion replaces.
 */
export function lineContext(input: string, cursor: number): LineContext {
  const upToCaret = input.slice(0, cursor);
  const words = segmentWords(upToCaret);
  const last = words[words.length - 1];
  const inWord = last !== undefined && last.end === upToCaret.length;
  const tokenWord = inWord ? last : undefined;

  const commandWords = tokenWord ? words.slice(0, -1) : words;
  const command = commandOf(commandWords);
  const commandIndex = commandWords.findIndex((word) => word.word === command);
  const wordsBefore =
    commandIndex === -1
      ? []
      : commandWords.slice(commandIndex + 1).map((word) => word.word);

  // The word as written includes its opening quote and the backslashes before
  // what it escaped; the token is what the shell will read, so the offsets
  // move by exactly what those cost.
  const head = wordHead(tokenWord?.word ?? "");
  const token = head.text;
  // Where the token's first character sits in the line: past the word's own
  // opening quote. Its end follows from the text's length, so a word written
  // with escapes (`My\ Fi`) reports the name the shell will see, and the
  // replacement covers exactly that.
  const tokenStart = tokenWord
    ? tokenWord.start + head.startOffset
    : upToCaret.length;
  // A word written with escapes (`My\ Fi`) covers more characters than the
  // name the shell will see, and the completion has to replace all of them.
  const tokenEnd = tokenWord
    ? tokenWord.start + head.endOffset
    : upToCaret.length;

  return {
    input: upToCaret,
    token,
    escaped: /[\\'"]/.test(tokenWord?.word ?? ""),
    tokenStart,
    tokenEnd,
    command: inWord ? command : commandOf(words),
    wordsBefore,
    commandWord: !inWord || commandWords.length === 0,
  };
}

/** True when every character of `prefix` starts `value`, case-sensitively. */
function startsWith(value: string, prefix: string): boolean {
  return prefix.length <= value.length && value.startsWith(prefix);
}

/**
 * Replacing the caret's word with `value` in the line, along with the range
 * that changes. The word's own quote is kept — the shell put it there — and
 * the leftover it closed with is taken with it. `quoted` is false for a value
 * that is a whole command rather than a word inside one.
 */
function wordEdit(
  context: LineContext,
  value: string,
  quoted = true,
): { line: string; replaceStart: number; replaceEnd: number } {
  const open = context.input[context.tokenStart];
  const hasQuote = quoted && (open === '"' || open === "'");
  const closing = hasQuote ? context.input[context.tokenEnd] : undefined;
  const closed = closing !== undefined && closing === open;
  const replaceStart = context.tokenStart;
  const replaceEnd = context.tokenEnd + (closed ? 1 : 0);
  return {
    line: context.input.slice(0, replaceStart) + value + context.input.slice(replaceEnd),
    replaceStart,
    replaceEnd,
  };
}

/** A row for a word that is completed by a plain prefix match. */
function wordRow(context: LineContext, value: string, hint?: string): Completion | null {
  if (!startsWith(value, context.token)) return null;
  return {
    label: value,
    ...wordEdit(context, value),
    matchStart: 0,
    matchLength: context.token.length,
    hint,
  };
}

/** Rows from the flags whose names start with what has been typed. */
function flagCompletions(context: LineContext, flags: CommandFlag[]): Completion[] {
  return flags
    .filter((flag) => startsWith(flag.name, context.token))
    .map((flag) => ({
      label: flag.name,
      ...wordEdit(context, flag.name),
      matchStart: 0,
      matchLength: context.token.length,
      hint: flag.hint,
    }));
}

/**
 * Commands whose next word is a file even when it does not look like a path:
 * `cat READ` and `vim src/te` both mean a file, where a bare word after `ls`
 * might equally be something else. The directory-listing commands stay out.
 */
const FILE_FIRST_COMMANDS = new Set([
  "cat", "bat", "less", "more", "head", "tail", "vim", "vi", "nvim", "nano",
  "emacs", "code", "open", "source", "python", "python3", "node", "deno", "bun",
  "sh", "bash", "zsh", "file", "stat", "wc", "grep", "rg", "jq", "yq",
]);

/**
 * Commands whose next word is a path. Used only to decide *whether* to offer
 * paths, never to complete the command itself.
 */
const PATH_COMMANDS = new Set([
  "cd", "pushd", "popd", "ls", "ll", "la", "cp", "mv", "rm", "rmdir", "mkdir",
  "touch", "chmod", "chown", "ln", "find", "sed", "awk", "tar", "zip", "unzip",
  "gzip", "gunzip", "diff", "du", "df", "tree", "sort", "uniq", "make",
  "cargo", "go", "java", "javac", "git", "kubectl", "docker", "helm",
  "terraform", "aws", "gcloud", "az", "rsync", "scp", "ssh", "sftp", "psql",
  "mysql", "redis-cli", ...FILE_FIRST_COMMANDS,
]);

/** The verbs of each tool that take a path (or a file) as their next word. */
const TOOL_PATH_VERBS: Record<string, Set<string>> = {
  git: new Set(["add", "checkout", "restore", "diff", "show", "apply", "rm", "mv", "log", "commit", "stash"]),
  kubectl: new Set(["apply", "create", "delete", "edit", "logs", "exec", "cp", "describe", "get", "patch", "replace", "scale"]),
  docker: new Set(["build", "run", "exec", "cp", "save", "load", "logs"]),
  helm: new Set(["install", "upgrade", "template", "lint", "package", "show"]),
  terraform: new Set(["apply", "plan", "init", "fmt", "validate", "destroy", "import"]),
};

/**
 * Whether the word the caret is in should be completed from the filesystem.
 * A word that already looks like a path always is; a plain word is one only
 * where the command has nothing else to say, so `cd ` and `cat READ` get files
 * while `git checkout ma` gets none — a ref is not a file name.
 */
export function wantsPath(context: LineContext): boolean {
  const { command, token } = context;
  if (!command || token.startsWith("-")) return false;
  const looksLikePath =
    token === "" ||
    token.startsWith(".") ||
    token.startsWith("/") ||
    token.startsWith("~");
  const verbs = TOOL_PATH_VERBS[command];
  if (verbs) {
    const verb = context.wordsBefore[0];
    return verb !== undefined && verbs.has(verb);
  }
  if (!PATH_COMMANDS.has(command)) return false;
  // `cd` takes nothing but a directory, so every word of it is one.
  if (command === "cd" || command === "pushd") return true;
  if (FILE_FIRST_COMMANDS.has(command)) return true;
  return looksLikePath;
}

/** Directory entries of the word's directory, or [] when it cannot be read. */
async function listFor(
  backend: CompletionBackend,
  token: string,
): Promise<{ prefix: string; entries: FileEntry[] }> {
  const cwd = (await backend.cwd()) ?? "";
  const slash = token.lastIndexOf("/");
  const typedDir = slash === -1 ? "" : token.slice(0, slash + 1);
  const prefix = slash === -1 ? token : token.slice(slash + 1);

  let dir: string;
  if (typedDir === "") dir = cwd || ".";
  else if (typedDir.startsWith("~")) dir = typedDir;
  else if (typedDir.startsWith("/")) dir = typedDir;
  // The directory the word names, without the slash that separated it from the
  // name being typed: `/tmp/src/` is not a path any backend knows.
  else dir = `${cwd.replace(/\/+$/, "")}/${typedDir}`.replace(/\/+$/, "") || "/";

  if (!dir) return { prefix, entries: [] };
  const entries = await backend.list(dir).catch(() => [] as FileEntry[]);
  return { prefix, entries };
}

/**
 * Path rows for the word under the caret. Entries are matched on the text
 * after the last slash, and only their tail is offered: the directory already
 * typed stays where it is, so `cd src/te` becomes `cd src/terminal.ts`.
 */
export async function pathCompletions(
  context: LineContext,
  backend: CompletionBackend,
): Promise<Completion[]> {
  const { prefix, entries } = await listFor(backend, context.token);
  const opening = context.input[context.tokenStart];
  const quote = opening === '"' || opening === "'" ? opening : "";
  const closed = quote !== "" && context.input[context.tokenEnd] === quote;
  const where = context.token.slice(0, Math.max(0, context.token.length - prefix.length));
  // A name written with a backslash (`My\ Fi`) is replaced by the plain name:
  // the escape is the shell's quoting, and the name is now whole.
  const escapedName = where === "" && context.escaped;
  // Everything before the name being completed is the user's and stays: the
  // replacement covers the name alone.
  const start = escapedName ? context.tokenStart : context.tokenStart + where.length;
  const replaceEnd = context.tokenEnd + (closed ? 1 : 0);
  // `.` and `..` are directory names in their own right, not prefixes of the
  // entries inside them.
  const tail = prefix === "." || prefix === ".." ? "" : prefix;
  const dotfiles = tail.startsWith(".");
  const value = escapedName ? prefix.replace(/\\/g, "") : tail;

  const scored = entries
    // A dotfile is noise unless the user asked for one, the way a shell's own
    // completion leaves them be.
    .filter(
      (entry) =>
        (value === "" || startsWith(entry.name, value)) &&
        (dotfiles || !entry.name.startsWith(".")),
    )
    .map((entry) => ({
      entry,
      // A directory is what the next word usually goes into, completing a
      // whole name says more than completing a prefix of one, and a name that
      // carries on past the prefix (`terminal.ts` for `term`) is the file
      // itself where one that breaks at a dot (`terminal.test.ts`) is a file
      // about it.
      score: (entry.isDir ? 2 : 0) + (entry.name[value.length] === "." ? 0 : 1),
    }))
    .sort((a, b) => b.score - a.score || a.entry.name.localeCompare(b.entry.name))
    .slice(0, MAX_SUGGESTIONS);

  return scored.map(({ entry }) => {
    const slash = entry.isDir ? "/" : "";
    // A quoted or escaped word stays quoted — the shell's own quoting is not
    // ours to drop — and an unquoted name that needs quotes gets them.
    const quoted = quote !== "" || escapedName || /[ "'\\$`]/.test(entry.name);
    const name = quoted ? `${entry.name}${slash}${quote || '"'}` : `${entry.name}${slash}`;
    const replace = quote !== "" ? `${quote}${name}` : quoted ? `"${name}` : name;
    return {
      label: `${entry.name}${slash}`,
      line: context.input.slice(0, start) + replace + context.input.slice(replaceEnd),
      replaceStart: start,
      replaceEnd,
      matchStart: 0,
      matchLength: value.length,
      hint: entry.isDir ? "directory" : undefined,
    };
  });
}

/** Rows for the tool the caret's command names: subcommands, then flags. */
function toolCompletions(command: string, context: LineContext): Completion[] {
  const words = context.wordsBefore;
  const subcommands = (names: string[]) =>
    names
      .map((name) => wordRow(context, name))
      .filter((row): row is Completion => row !== null);

  if (command === "kubectl") {
    if (context.token.startsWith("-")) return flagCompletions(context, KUBECTL_FLAGS);
    if (words.length === 0) return subcommands(KUBECTL_SUBCOMMANDS);
    const verb = words[0];
    const resourceVerbs = new Set([
      "get", "describe", "edit", "delete", "apply", "patch", "label", "annotate",
      "expose", "scale", "rollout", "logs", "exec", "port-forward", "top", "wait",
    ]);
    // `kubectl get po` completes the resource; `kubectl get pods ` opens the
    // field where a name or a selector goes and nothing static helps.
    if (!resourceVerbs.has(verb) || words.length !== 1) return [];
    const rows: Completion[] = [];
    for (const resource of KUBECTL_RESOURCES) {
      // Every spelling the token starts counts: `po` is a prefix of the short
      // name as well as of `pods`, and kubectl accepts both.
      for (const name of resource.names) {
        if (!startsWith(name, context.token)) continue;
        rows.push({
          label: name,
          ...wordEdit(context, name),
          matchStart: 0,
          matchLength: context.token.length,
          hint: name === resource.name ? undefined : resource.name,
        });
      }
    }
    return rows;
  }
  if (command === "git") {
    if (context.token.startsWith("-")) return flagCompletions(context, GIT_FLAGS);
    if (words.length === 0) return subcommands(GIT_SUBCOMMANDS);
    return [];
  }
  return [];
}

/**
 * A history (or library) row: the command replaces the line, because a command
 * is completed as a whole rather than as a word inside one.
 */
function historyCompletion(
  command: string,
  matchStart: number,
  context: LineContext,
): Completion {
  return {
    label: command,
    ...wordEdit(context, command, false),
    matchStart,
    matchLength: context.token.length,
  };
}

/**
 * Everything the popup can offer for one keystroke: what the line's context
 * says comes first (a subcommand, a flag, a path), then the user's own
 * commands, then the ones shipped with the app. The context rows are the only
 * ones that know what the shell is being asked to do, so they lead whenever
 * the command is known; history leads at a bare prompt.
 */
export async function completionsFor(
  line: string,
  cursor: number,
  backend: CompletionBackend,
  host: string,
): Promise<Completion[]> {
  const context = lineContext(line, cursor);

  const rows: Completion[] = [];
  if (context.command) rows.push(...toolCompletions(context.command, context));
  // A path is offered after the tool's own words: a subcommand or a flag the
  // tool actually has is a stronger guess than a file name in the directory.
  if (rows.length < MAX_SUGGESTIONS && wantsPath(context)) {
    rows.push(...(await pathCompletions(context, backend)));
  }

  const history = commandHistory
    .suggest(context.token, host)
    .map((row) => historyCompletion(row.command, row.matchStart, context));
  const library = context.commandWord
    ? librarySuggestions(context.token).map((row) =>
        historyCompletion(row.command, row.matchStart, context),
      )
    : [];

  const seen = new Set<string>();
  const merged: Completion[] = [];
  for (const row of [...rows, ...history, ...library]) {
    if (merged.length >= MAX_SUGGESTIONS) break;
    if (seen.has(row.line)) continue;
    seen.add(row.line);
    merged.push(row);
  }
  return merged;
}

/** What a key does to the completion popup. */
export type TabAction = "accept" | "next" | "previous" | "first" | "last" | "hold" | "shell";

/**
 * What Tab (or Shift+Tab) should do, given the popup's state. The rule is the
 * IDE's: the first press takes the first row — or the last, with Shift — the
 * next presses walk the list, and the press that walks past the end accepts
 * what is selected. With no rows and an answer still on the way the key is
 * held back rather than handed to the shell, whose own completion would race
 * the list that is about to appear; with neither, the shell owns Tab.
 */
export function tabAction(
  count: number,
  index: number,
  pending: boolean,
  shift = false,
): TabAction {
  if (count > 0) {
    if (index < 0) return shift ? "last" : "first";
    const next = index + (shift ? -1 : 1);
    return next < 0 || next >= count ? "accept" : shift ? "previous" : "next";
  }
  return pending ? "hold" : "shell";
}
