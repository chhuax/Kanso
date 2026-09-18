/**
 * Recognizes the agentic CLIs — Claude Code, Codex, Gemini CLI and the like —
 * that hold the terminal for a whole session. Their command only returns when
 * the user quits, so tracking it like an ordinary one would leave the tab
 * "command running" for as long as the tool is open, which says nothing. A
 * recognized session switches terminal activity to the assistant's turns
 * instead: busy while it works, finished when it hands the terminal back.
 */

/** An agentic CLI, as the session list names it. */
export interface AiTool {
  /** Stable id; several spellings map to one tool. */
  id: string;
  /** Shown beside the tab, e.g. "Claude". */
  label: string;
  /**
   * The mark's tint, near the tool's own colour. Not an official brand
   * colour and no brand asset is shipped: that needs a licence, which is the
   * same reason Warp falls back to a plain icon until it has one.
   */
  color: string;
}

const CLAUDE: AiTool = { id: "claude", label: "Claude", color: "#d97757" };
const CODEX: AiTool = { id: "codex", label: "Codex", color: "#10a37f" };
const GEMINI: AiTool = { id: "gemini", label: "Gemini", color: "#4285f4" };

/**
 * Binary names, as they are typed. Basenames, so a full path also matches.
 * Several names may map to one tool (a package's spelling beside its binary).
 */
const AI_CLIS: Record<string, AiTool> = {
  claude: CLAUDE,
  codex: CODEX,
  gemini: GEMINI,
  aider: { id: "aider", label: "Aider", color: "#8a5cf6" },
  "cursor-agent": { id: "cursor", label: "Cursor", color: "#8b949e" },
  copilot: { id: "copilot", label: "Copilot", color: "#6e7681" },
  amp: { id: "amp", label: "Amp", color: "#e8794a" },
  opencode: { id: "opencode", label: "OpenCode", color: "#4fb8a8" },
  crush: { id: "crush", label: "Crush", color: "#e05f8a" },
  goose: { id: "goose", label: "Goose", color: "#8aa8ff" },
  qwen: { id: "qwen", label: "Qwen", color: "#7b8cff" },
  droid: { id: "droid", label: "Droid", color: "#8fd14f" },
  openhands: { id: "openhands", label: "OpenHands", color: "#6aa9ff" },
  grok: { id: "grok", label: "Grok", color: "#9aa0a6" },
  // As spelled on npm, for `npx @anthropic-ai/claude-code`.
  "claude-code": CLAUDE,
  "gemini-cli": GEMINI,
};

/** Words that only stand in front of the real command. */
const PREFIXES = new Set([
  "sudo",
  "doas",
  "env",
  "command",
  "exec",
  "time",
  "nohup",
  "npx",
  "pnpx",
  "bunx",
  "uvx",
]);

/** Package runners spelled as two words: `pnpm dlx claude`. */
const RUNNERS: Record<string, string[]> = {
  npm: ["exec"],
  pnpm: ["dlx", "exec"],
  yarn: ["dlx"],
  bun: ["x"],
  uv: ["run"],
  deno: ["run"],
};

/**
 * Flags that turn these tools into a one-shot run with no session to follow:
 * `claude -p "..."`, `aider --message ...`, `codex --version`.
 */
const ONE_SHOT_FLAGS = new Set([
  "-p",
  "--print",
  "--prompt",
  "-m",
  "--message",
  "-h",
  "--help",
  "-v",
  "-V",
  "--version",
]);

/** First-argument subcommands that do not open a session. */
const ONE_SHOT_SUBCOMMANDS = new Set([
  "exec",
  "run",
  "mcp",
  "update",
  "upgrade",
  "install",
  "uninstall",
  "doctor",
  "config",
  "login",
  "logout",
  "auth",
  "help",
  "version",
  "completion",
  "setup-token",
  "migrate-installer",
]);

/** `VAR=value` assignments in front of a command. */
const ASSIGNMENT = /^[A-Za-z_][A-Za-z0-9_]*=/;

/** Last path segment, without a Windows executable suffix. */
function binaryName(token: string): string {
  const base = token.split(/[/\\]/).pop() ?? "";
  return base.replace(/\.(exe|cmd|bat|ps1)$/i, "").toLowerCase();
}

/**
 * The tool an interactive session would start, or null when the command line
 * starts none. `cd repo && claude` counts; a pipeline does not, since a tool
 * reading from a pipe is not driving the terminal.
 */
export function aiToolForCommand(command: string): AiTool | null {
  for (const segment of command.split(/&&|\|\||;/)) {
    if (segment.includes("|")) continue;
    const tool = startsAiSession(segment);
    if (tool) return tool;
  }
  return null;
}

/** True when the command line starts an interactive session with one of them. */
export function isAiSessionCommand(command: string): boolean {
  return aiToolForCommand(command) !== null;
}

function startsAiSession(segment: string): AiTool | null {
  const tokens = segment.trim().split(/\s+/).filter(Boolean);
  let index = 0;
  let prefixed = false;
  while (index < tokens.length) {
    const token = tokens[index];
    // `sudo -E claude`, `npx -y ...`: options belong to the prefix in front.
    if (ASSIGNMENT.test(token) || (prefixed && token.startsWith("-"))) {
      index += 1;
      continue;
    }
    const name = binaryName(token);
    if (PREFIXES.has(name)) {
      index += 1;
      prefixed = true;
      continue;
    }
    if (RUNNERS[name]?.includes(tokens[index + 1] ?? "")) {
      index += 2;
      prefixed = true;
      continue;
    }
    break;
  }

  const tool = AI_CLIS[binaryName(tokens[index] ?? "")];
  if (!tool) return null;

  const args = tokens.slice(index + 1);
  if (args.some((arg) => ONE_SHOT_FLAGS.has(arg))) return null;
  // Only the first positional word is a subcommand; a quoted opening prompt
  // (`codex "run the tests"`) keeps its quote and never looks like one.
  const first = args.find((arg) => !arg.startsWith("-"));
  return first === undefined || !ONE_SHOT_SUBCOMMANDS.has(first) ? tool : null;
}
