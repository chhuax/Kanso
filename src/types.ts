export type SessionKind = "local" | "ssh" | "sftp";
export type AuthKind = "password" | "publicKey" | "agent";

/**
 * File-transfer sessions with no interactive terminal: they open the dual-pane
 * file manager instead of an xterm, and are skipped by the Sender, search, and
 * terminal-only wiring. SFTP rides an SSH transport but is browse-and-transfer
 * only.
 */
export function isFileSession(kind: SessionKind): boolean {
  return kind === "sftp";
}

/** Sessions that ride an SSH transport, and so can be or use a jump host. */
export function isSshTransport(kind: SessionKind): boolean {
  return kind === "ssh" || kind === "sftp";
}

export type ThemeMode = "dark" | "light";

export interface SessionProfile {
  id: string;
  name: string;
  kind: SessionKind;
  color?: string | null;
  /** 会话所属分组；null 或 undefined 表示在会话浮层顶层展示。 */
  groupId?: string | null;

  // terminal text (local / ssh)
  /**
   * Character encoding of the terminal byte stream, a WHATWG label such as
   * "gbk"; null / unknown means UTF-8. See encodings.ts.
   */
  encoding?: string | null;
  /**
   * Locale handed to the shell as LANG — set on a local shell, requested
   * from an SSH server. Null means automatic (see session/locale.rs).
   */
  locale?: string | null;
  // local
  shell?: string | null;
  cwd?: string | null;

  // ssh / sftp
  host?: string | null;
  port?: number | null;
  username?: string | null;
  auth?: AuthKind | null;
  password?: string | null;
  privateKeyPath?: string | null;
  passphrase?: string | null;
  /**
   * Saved SSH / SFTP profile to tunnel through (ProxyJump), for a host that
   * is only reachable from that session's network. The jump profile may
   * itself name a jump host, giving a chain.
   */
  jumpProfileId?: string | null;
}

/** 会话库的单层用户分组，可混合保存不同协议的会话，不支持嵌套。 */
export interface SessionGroup {
  id: string;
  name: string;
}

export interface SessionInfo {
  id: string;
  profileId: string | null;
  name: string;
  kind: SessionKind;
  protocol: string;
  address: string;
  color: string | null;
  supportsRemoteFiles: boolean;
  /**
   * The SSH servers of this session (jump hosts first) that only connected
   * on algorithms kept for old devices; empty when none did.
   */
  legacyAlgorithms: LegacyAlgorithms[];
}

/** The legacy algorithms one SSH server was connected with (issue #60). */
export interface LegacyAlgorithms {
  /** host:port of the server. */
  address: string;
  /** SSH algorithm names, e.g. "diffie-hellman-group14-sha1". */
  algorithms: string[];
}

/** An SSH host whose key no longer matches the one recorded in known_hosts. */
export interface HostKeyChange {
  host: string;
  port: number;
  /** Algorithm of the presented key, e.g. "ssh-ed25519". */
  keyType: string;
  /** SHA256 fingerprint of the presented key, for the user to verify. */
  fingerprint: string;
  /** The presented key as an OpenSSH public key line; this exact key is recorded on accept. */
  publicKey: string;
  /** known_hosts file and line holding the conflicting entry. */
  knownHosts: string;
  line: number;
  /** One-line explanation for the terminal and status bar. */
  message: string;
}

/** One question in a server's keyboard-interactive challenge. */
export interface AuthPromptField {
  /** The server's wording, e.g. "Verification code:". */
  prompt: string;
  /** Whether what the user types may be shown; false for secrets. */
  echo: boolean;
}

/**
 * A round of questions an SSH server asked while authenticating (RFC 4256's
 * keyboard-interactive, the usual carrier for MFA). The answers go back to
 * `answerAuthPrompt` under the same id and are never saved.
 */
export interface AuthPrompt {
  id: string;
  /** The session being opened, so the dialog can name its tab. */
  sessionId: string;
  /** host:port of the hop asking; a jump host has challenges of its own. */
  address: string;
  username: string;
  /** Server-supplied title for the round; usually empty. */
  name: string;
  /** Server-supplied text shown above the questions; usually empty. */
  instructions: string;
  prompts: AuthPromptField[];
}

/** What open_session produced: a live session, or a decision for the user. */
export type OpenSessionOutcome =
  | { status: "connected"; info: SessionInfo }
  | { status: "hostKeyChanged"; change: HostKeyChange };

export interface FileEntry {
  name: string;
  path: string;
  isDir: boolean;
  isSymlink: boolean;
  size: number;
  modified: number | null;
  permissions: number | null;
  owner: string | null;
  group: string | null;
}

export interface DirListing {
  path: string;
  entries: FileEntry[];
}

/** 未提交文件的主要 Git 状态，供本地变更列表展示。 */
export type GitFileStatus =
  | "modified"
  | "added"
  | "deleted"
  | "renamed"
  | "copied"
  | "untracked"
  | "conflicted"
  | "typeChanged";

/** Git 工作区中的一个未提交文件，由 Local Shell 的当前目录解析。 */
export interface GitFileChange {
  path: string;
  previousPath: string | null;
  status: GitFileStatus;
  /** 二进制或超出读取上限的文件没有可靠行数，返回 null。 */
  additions: number | null;
  /** 二进制或超出读取上限的文件没有可靠行数，返回 null。 */
  deletions: number | null;
}

/** 当前 Local Shell 所在仓库的只读变更摘要。 */
export interface GitChanges {
  root: string;
  branch: string | null;
  files: GitFileChange[];
  additions: number;
  deletions: number;
}

/** 单文件展开后显示的统一 diff；过大的内容会由后端截断。 */
export interface GitFileDiff {
  text: string;
  truncated: boolean;
}

export type SessionState = "connecting" | "connected" | "closed" | "error";

export type LineEnding = "none" | "lf" | "crlf";

/**
 * Sender 命令的可见范围：全局、某种会话类型、某个会话分组，或某个已保存
 * 会话。标签页如何解析作用域链见 `senderScope.ts`。
 */
export type CommandScope =
  | { type: "global" }
  | { type: "kind"; kind: SessionKind }
  | { type: "group"; id: string }
  | { type: "profile"; id: string };

export interface SavedCommand {
  id: string;
  name: string;
  text: string;
  ending: LineEnding;
  scope: CommandScope;
}

/**
 * One export / import file (Session → Export / Import Data…). `settings` is
 * the frontend's own `AppSettings`, carried through the backend untouched;
 * passwords and passphrases are never part of a data file.
 */
export interface AppData {
  app: string;
  format: number;
  exportedAt?: string | null;
  settings?: unknown;
  profiles: SessionProfile[];
  groups: SessionGroup[];
  senderCommands: SavedCommand[];
}

/** How many entries an export wrote or an import merged. */
export interface DataSummary {
  profiles: number;
  groups: number;
  senderCommands: number;
  /** Sender tags an import left out because the library was already full. */
  skippedSenderCommands: number;
}

/**
 * One `Host` alias of an OpenSSH client configuration, resolved the way
 * `ssh alias` resolves it (Session → Import OpenSSH Config…).
 */
export interface SshConfigEntry {
  alias: string;
  host: string;
  port: number;
  username: string;
  auth: AuthKind;
  privateKeyPath?: string | null;
  /** ProxyJump hops, first hop first: aliases of the file or `[user@]host[:port]`. */
  jumps: string[];
  /**
   * A saved SSH session that already stands for the alias; importing it
   * updates that session in place instead of adding a twin.
   */
  existingId?: string | null;
  existingName?: string | null;
}

/** What an import of the file would bring in, for the dialog to choose from. */
export interface SshConfigPreview {
  path: string;
  entries: SshConfigEntry[];
}

export interface SshImportSummary {
  added: number;
  updated: number;
  /** Profiles created beyond the selection because a selected host tunnels through them. */
  jumpHosts: number;
  /** Hosts whose multi-hop ProxyJump was left unimported (only single-hop jumps are imported). */
  jumpsIgnored: number;
  /** Links that could not be made; the sessions themselves were saved. */
  warnings: string[];
}

/** Colours used for session dots, mirroring WindTerm's per-session markers. */
export const SESSION_COLORS = [
  "#4ea1f3",
  "#3fb950",
  "#e3b341",
  "#f85149",
  "#bc8cff",
  "#39c5cf",
  "#ff7b72",
  "#a5d6ff",
  "#f0883e",
  "#db61a2",
  "#7ee787",
  "#6e8cff",
  "#2db89a",
  "#f0c36e",
  "#e56bce",
  "#d2a8ff",
];

export function randomSessionColor(): string {
  return SESSION_COLORS[Math.floor(Math.random() * SESSION_COLORS.length)];
}

export function colorForSession(seed: string): string {
  let hash = 0;
  for (let i = 0; i < seed.length; i++) {
    hash = (hash * 31 + seed.charCodeAt(i)) >>> 0;
  }
  return SESSION_COLORS[hash % SESSION_COLORS.length];
}
