import * as api from "./api";
import { fontStack } from "./fonts";
import { commandHistory } from "./history";
import { sessionCompletions } from "./sessionCompletions";
import { IS_WINDOWS } from "./platform";
import { tabTitle, useStore, type HostKeyPrompt, type Tab } from "./store";
import type { TerminalController } from "./terminal";
import {
  disposeController,
  getController,
  setController,
} from "./terminalRegistry";
import { isFileSession, type SessionInfo, type SessionProfile } from "./types";

/** How long a session's asked-for directory answers the popup; see below. */
const CWD_ASK_TTL_MS = 2000;

/** Line written into a terminal when its session ends, however it ended. */
export const SESSION_CLOSED_NOTICE = "\r\n\x1b[33m[session closed]\x1b[0m\r\n";

function newSessionId(): string {
  if (typeof crypto?.randomUUID === "function") return crypto.randomUUID();
  return `s-${Math.random().toString(36).slice(2)}-${Date.now().toString(36)}`;
}

function pendingSessionInfo(
  id: string,
  profile: SessionProfile,
): SessionInfo {
  const name =
    profile.name || profile.host || "session";

  let protocol: string;
  let address: string;
  if (profile.kind === "ssh") {
    protocol = "ssh";
    address = `${profile.host || "localhost"}:${profile.port ?? 22}`;
  } else if (profile.kind === "sftp") {
    protocol = "sftp";
    address = `${profile.host || "localhost"}:${profile.port ?? 22}`;
  } else {
    protocol = "shell";
    address = profile.shell || "default shell";
  }

  return {
    id,
    profileId: profile.id || null,
    name,
    kind: profile.kind,
    protocol,
    address,
    color: profile.color ?? null,
    supportsRemoteFiles: profile.kind === "ssh" || isFileSession(profile.kind),
    // Known once the backend has opened the file.
    // Known once the backend has connected.
    legacyAlgorithms: [],
  };
}

/** Tabs whose open_session call has not replied yet; see `connectSession`. */
const pendingConnects = new Set<string>();

/**
 * The history bucket a session's commands belong to, so suggestions can
 * prefer commands seen on the same host. Resolved per call because the tab's
 * info is refined once the backend connects.
 */
function historyHost(id: string): string {
  const tab = useStore.getState().tabs.find((item) => item.info.id === id);
  return tab ? `${tab.info.protocol}:${tab.info.address}` : "";
}

/**
 * Returns the terminal for a session, creating it if needed. Wiring lives here
 * rather than in the React component so a terminal can exist before anything
 * renders.
 *
 * xterm and its addons are the largest thing the application loads and no
 * session needs them until one is opened, so they arrive with the first
 * terminal instead of with the window.
 */
let localHome: Promise<string> | null = null;

/** The OS home directory, read once; used to write paths the way people do. */
function homeDir(): Promise<string> {
  localHome ??= api.localHome().catch(() => "");
  return localHome;
}

/**
 * Warp's `user_friendly_path`: a path inside the home directory is written
 * with `~`, and nothing else is shortened — the row clips what is left with a
 * trailing ellipsis, which is exactly what Warp does too.
 */
function userFriendlyPath(path: string, home: string): string {
  if (!home) return path;
  if (path === home) return "~";
  if (!path.startsWith(home)) return path;
  const rest = path.slice(home.length);
  // Only a real child of it: `/home/me2` is not under `/home/me`.
  return /^[/\\]/.test(rest) ? `~${rest}` : path;
}

const localWhereRequests = new Map<string, symbol>();

/**
 * 刷新 Local Shell 的当前目录、分支和未提交变更。优先读取进程目录，无法读取时
 * 仍可使用 Shell 的 OSC 7 报告；远程会话不会进入这里，也不会执行 Git 查询。
 */
export async function refreshLocalWhere(id: string): Promise<void> {
  const tab = useStore.getState().tabs.find((item) => item.info.id === id);
  if (!tab || tab.info.kind !== "local" || tab.state !== "connected") return;
  const request = Symbol();
  localWhereRequests.set(id, request);
  const isCurrent = () => {
    const current = useStore.getState().tabs.find((item) => item.info.id === id);
    return (
      localWhereRequests.get(id) === request &&
      current?.info.kind === "local" &&
      current.state === "connected"
    );
  };
  try {
    const [cwd, home] = await Promise.all([shellCwd(tab), homeDir()]);
    if (!isCurrent()) return;
    if (!cwd) throw new Error("the shell reported no directory");
    const friendly = userFriendlyPath(cwd, home);
    useStore.getState().setCwd(id, friendly);
    const changes = await api.gitChanges(cwd);
    // 快速执行命令或切换目录时，先发出的查询可能后返回；只接受最后一次请求。
    if (isCurrent()) {
      useStore.getState().setGitChanges(id, changes);
    }
  } catch (error) {
    if (isCurrent()) {
      useStore.getState().setGitChanges(id, null);
      useStore.getState().setStatus(`Working directory / Git: ${String(error)}`);
    }
  } finally {
    if (localWhereRequests.get(id) === request) localWhereRequests.delete(id);
  }
}

export async function ensureController(
  id: string,
  /**
   * True for a local shell: it draws a rule above each prompt (see
   * TerminalController) and is the only kind whose path the rail shows.
   */
  local = false,
): Promise<TerminalController> {
  const existing = getController(id);
  if (existing) return existing;

  const { TerminalController } = await import("./terminal");
  // A second caller may have finished while this one waited for the module.
  const raced = getController(id);
  if (raced) return raced;

  const controller = new TerminalController(
    id,
    {
      onData: (data) => void api.writeSession(id, data).catch(() => undefined),
      onCommand: (command) => commandHistory.record(command, historyHost(id)),
      onCommandState: (state, kind) => {
        const store = useStore.getState();
        if (state === "running") store.markCommandStarted(id, kind);
        else if (state === "complete") {
          store.markCommandCompleted(id, kind);
          // A shell only moves while a command runs, and asking the OS where a
          // local one is costs no round trip.
          if (local) void refreshLocalWhere(id);
        } else store.clearCommandActivity(id);
      },
      // Completions know what the line is asking for: a path lists this
      // session's own filesystem, a tool's subcommands and flags come from
      // its table, and the shell's history answers the rest.
      suggest: sessionCompletions(
        {
          id,
          local,
          cwd: () => whereTheShellIs(id),
        },
        historyHost(id),
      ),
      onAiTool: (tool) => useStore.getState().setAiTool(id, tool),
      onResize: (cols, rows) => {
        useStore.getState().setSize(id, cols, rows);
        void api.resizeSession(id, cols, rows).catch(() => undefined);
      },
      onStatus: (message, error = false) => {
        const store = useStore.getState();
        store.setStatus(message);
        if (error) store.setError(message, id);
        else if (store.errorSessionId === id) store.setError(null);
      },
    },
    useStore.getState().bufferFontSize,
    useStore.getState().terminalScrollback,
    useStore.getState().theme,
    fontStack(
      "mono",
      useStore.getState().bufferFontFamily,
      useStore.getState().symbolFontFamilies,
    ),
    local,
  );
  controller.setSuggestions(useStore.getState().suggestionsEnabled);
  controller.setRightClickAction(useStore.getState().rightClickAction);
  setController(id, controller);
  return controller;
}

/**
 * Opens a session. The id is minted here and the terminal is created *before*
 * the backend connects, so output emitted during login (an SSH banner, a
 * shell's first prompt) always has somewhere to land.
 */
export async function openSession(
  profile: SessionProfile,
  transportSessionId?: string,
): Promise<string | null> {
  const id = newSessionId();
  useStore
    .getState()
    .addTab(pendingSessionInfo(id, profile), profile, "connecting");
  if (!isFileSession(profile.kind)) {
    await ensureController(id, profile.kind === "local");
  }
  return connectSession(id, profile, transportSessionId);
}

/** 所有“直接打开终端”入口共用的临时本地 Shell 配置。 */
export const LOCAL_SHELL_PROFILE: SessionProfile = {
  id: "",
  name: "Local Shell",
  kind: "local",
  color: "#3fb950",
};

/** 打开本地 Shell；`paneId` 指定新标签所属 pane。 */
export async function openLocalShell(paneId?: string): Promise<string | null> {
  if (paneId) useStore.getState().setActivePane(paneId);
  return openSession(LOCAL_SHELL_PROFILE);
}

/**
 * 复制已打开的标签。Local Shell 使用当前真实目录，而不是最初配置的目录；
 * SSH 复用原标签已认证的 transport 并新开独立 PTY channel；SFTP 仍按原配置新建连接。
 */
export async function duplicateSession(
  id: string,
  paneId?: string,
): Promise<string | null> {
  const tab = useStore.getState().tabs.find((item) => item.info.id === id);
  if (!tab) return null;

  let profile = tab.profile;
  if (tab.info.kind === "local" && tab.state === "connected") {
    try {
      profile = { ...profile, cwd: await shellCwd(tab) };
    } catch {
      // 当前目录暂时不可读时仍复制 Shell，并沿用原配置的启动目录。
    }
  }

  useStore.getState().setActivePane(paneId ?? tab.paneId);
  return openSession(
    profile,
    tab.info.kind === "ssh" && tab.state === "connected"
      ? tab.info.id
      : undefined,
  );
}

/**
 * Split Right / Split Down: opens another session of the tab's profile in
 * a new pane beside the tab's own — what a terminal's split means (iTerm2,
 * Windows Terminal, VS Code's terminal), since one session cannot show in
 * two places. The profile is the tab's own copy, secrets included, so a
 * saved password or passphrase is not asked for again.
 */
export async function splitSession(
  id: string,
  side: "right" | "down",
): Promise<string | null> {
  const store = useStore.getState();
  const tab = store.tabs.find((item) => item.info.id === id);
  if (!tab) return null;
  // The new pane becomes the active one, which is where openSession opens.
  store.splitPane(tab.paneId, side);
  return openSession(tab.profile);
}

/**
 * Disconnects a tab's session but keeps the tab, its terminal and its
 * scrollback, so `reconnectSession` can bring it back in place. The backend
 * does not echo a "closed" state for a close it was asked for (see
 * `emit_state` in session/mod.rs), so the tab is marked here.
 */
export async function disconnectSession(id: string): Promise<void> {
  const tab = useStore.getState().tabs.find((item) => item.info.id === id);
  // The backend reports "connected" as soon as the session task starts,
  // which can land before open_session's own reply. Closing in that window
  // would let the late reply mark the tab connected again over a session
  // that is already gone, so wait for the connect to settle first.
  if (!tab || tab.state !== "connected" || pendingConnects.has(id)) return;

  let failure: string | null = null;
  await api.closeSession(id).catch((e) => {
    failure = String(e);
  });

  const store = useStore.getState();
  const current = store.tabs.find((item) => item.info.id === id);
  // The tab may have been closed meanwhile, or the session may have ended on
  // its own while the close was in flight; either way there is nothing left
  // to mark.
  if (!current || current.state !== "connected") return;

  store.applyState(id, "closed", failure ?? "Disconnected");
  // The backend stays silent for a close it was asked for, so echo the same
  // notice into the terminal that a peer-initiated close gets (see App.tsx).
  getController(id)?.writeText(SESSION_CLOSED_NOTICE);
  if (failure) store.setError(failure, id);
  else store.setStatus(`Disconnected from ${tabTitle(current)}`);
}

/**
 * Reconnects a tab whose session ended — by `disconnectSession`, a failed
 * connection, or the peer going away — reusing its terminal so the earlier
 * output stays in the scrollback. Resolves like `openSession`.
 */
export function reconnectSession(id: string): Promise<string | null> {
  const tab = useStore.getState().tabs.find((item) => item.info.id === id);
  if (!tab || (tab.state !== "closed" && tab.state !== "error")) {
    return Promise.resolve(null);
  }
  return connectSession(id, tab.profile);
}

/**
 * The one-button behaviour of a tab's power toggle: a live session is
 * disconnected, an ended one is reconnected, and a session still connecting
 * is left alone.
 */
export function toggleSessionConnection(id: string): void {
  const tab = useStore.getState().tabs.find((item) => item.info.id === id);
  if (!tab) return;
  if (tab.state === "connected") void disconnectSession(id);
  else if (tab.state !== "connecting") void reconnectSession(id);
}

/**
 * Connects the backend session behind an existing tab. Resolves to the tab id
 * once the tab has something to show — a live session, or a changed host key
 * waiting on the user — and to null when the connection failed or the tab
 * was closed meanwhile.
 */
async function connectSession(
  id: string,
  profile: SessionProfile,
  transportSessionId?: string,
): Promise<string | null> {
  const store = useStore.getState();
  const pending = store.tabs.find((item) => item.info.id === id);
  const label = pending
    ? tabTitle(pending)
    : profile.name || profile.host || "session";

  store.setStatus(`Connecting to ${label}…`);
  store.setError(null);
  store.applyState(id, "connecting");

  // The decoder for the profile's encoding must be in place before the
  // first byte of the banner arrives; a reconnect may carry a re-edited
  // profile, so it is set on every connect.
  getController(id)?.setEncoding(profile.encoding);

  pendingConnects.add(id);
  try {
    const outcome = await api.openSession(profile, id, transportSessionId);

    // The user may close the optimistic tab while SSH is still negotiating.
    // In that case close the newly-created backend session immediately.
    const connectedStore = useStore.getState();
    const tab = connectedStore.tabs.find((item) => item.info.id === id);
    if (!tab) {
      if (outcome.status === "connected") {
        await api.closeSession(id).catch(() => undefined);
      }
      return null;
    }

    if (outcome.status === "hostKeyChanged") {
      // Nothing was opened. Park the tab on the refusal and let the user
      // decide in the host key dialog, which reconnects on accept.
      const { change } = outcome;
      failSession(id, change.message);
      connectedStore.setHostKeyPrompt({ sessionId: id, profile, change });
      return id;
    }

    const { info } = outcome;
    connectedStore.updateTabInfo(id, info);
    connectedStore.applyState(id, "connected");
    if (info.kind === "local") void refreshLocalWhere(id);
    connectedStore.setStatus(
      `Connected to ${tabTitle({ info, ordinal: tab.ordinal })}`,
    );

    // The pane was fitted while the backend was still connecting, so its
    // first resize command could not be delivered. Re-send the current size.
    if (!isFileSession(info.kind)) {
      void api.resizeSession(id, tab.cols, tab.rows).catch(() => undefined);
    }
    return id;
  } catch (e) {
    const message = String(e);
    if (useStore.getState().tabs.some((tab) => tab.info.id === id)) {
      failSession(id, message);
    } else {
      disposeController(id);
    }
    return null;
  } finally {
    pendingConnects.delete(id);
  }
}

/** Marks a tab's connection as failed and echoes why into its terminal. */
function failSession(id: string, message: string): void {
  const store = useStore.getState();
  store.applyState(id, "error", message);
  store.setError(message, id);
  store.setStatus(`Failed: ${message}`);
  const terminalMessage = message.replace(/[\x00-\x1f\x7f]/g, " ");
  getController(id)?.writeText(
    `\r\n\x1b[31m[connection failed: ${terminalMessage}]\x1b[0m\r\n`,
  );
}

/**
 * Accepts the key a host now presents — replacing every known_hosts entry for
 * it — and reconnects the tab that was refused. Rejects when known_hosts could
 * not be updated; a failure of the reconnect itself shows on the tab.
 */
export async function acceptHostKey(prompt: HostKeyPrompt): Promise<void> {
  await api.acceptHostKey(prompt.change);
  const store = useStore.getState();
  store.setHostKeyPrompt(null);
  if (!store.tabs.some((tab) => tab.info.id === prompt.sessionId)) return;
  getController(prompt.sessionId)?.writeText(
    `\r\n[accepted new host key ${prompt.change.fingerprint}; reconnecting]\r\n`,
  );
  void connectSession(prompt.sessionId, prompt.profile);
}

// --- Filer: reveal the shell's working directory ----------------------------

let localHostname: Promise<string> | null = null;

function firstLabel(host: string): string {
  return host.toLowerCase().split(".")[0];
}

/**
 * Whether an OSC 7 host names this machine. A remote shell the user ssh'd
 * into by hand from a local tab reports its own host, and that directory
 * must not be looked for in the local Filer.
 */
async function isThisHost(host: string): Promise<boolean> {
  if (host === "" || host === "localhost") return true;
  localHostname ??= api.localHostname().catch(() => "");
  const mine = await localHostname;
  return mine !== "" && firstLabel(host) === firstLabel(mine);
}

/** `/C:/Users/me` from a Windows shell's OSC 7 URL → `C:\Users\me`. */
function localPathFromUrlPath(path: string): string {
  if (!IS_WINDOWS) return path;
  const drive = /^\/([A-Za-z]:)(\/.*)?$/.exec(path);
  if (!drive) return path;
  return `${drive[1]}${(drive[2] ?? "/").replace(/\//g, "\\")}`;
}

/**
 * Where a session is, for the completion popup to list a directory against.
 * The shell's own report is free and already exact, so it answers whenever it
 * is there; a session that has not printed one yet is asked the way the
 * "reveal working directory" command asks — the server for an SSH host, the
 * OS for a local shell — and the answer is kept, because a popup asks on
 * every keystroke of a path and a round trip per character is not a popup.
 */
const askedCwd = new Map<string, { at: number; path: Promise<string | null> }>();

function whereTheShellIs(id: string): string | null | Promise<string | null> {
  const reported = getController(id)?.reportedCwd;
  if (reported) {
    askedCwd.delete(id);
    return reported.path;
  }
  const held = askedCwd.get(id);
  if (held && Date.now() - held.at < CWD_ASK_TTL_MS) return held.path;
  const tab = useStore.getState().tabs.find((item) => item.info.id === id);
  if (!tab) return null;
  const path = shellCwd(tab).catch(() => null);
  askedCwd.set(id, { at: Date.now(), path });
  // A session that has gone away leaves nothing to answer with.
  void path.then((answer) => {
    if (answer === null) askedCwd.delete(id);
  });
  return path;
}

/**
 * Where the tab's shell is right now. An SSH shell's own OSC report wins
 * (exact even inside sudo or a nested shell), then the server is asked
 * (`session_cwd`; Linux hosts). A local shell is asked through the OS
 * first — exact, and needing no shell setup — with its OSC report as the
 * fallback where the OS cannot be asked (Windows).
 */
async function shellCwd(tab: Tab): Promise<string> {
  const id = tab.info.id;
  const reported = getController(id)?.reportedCwd ?? null;
  if (tab.info.kind === "ssh") {
    if (reported) return reported.path;
    return api.sessionCwd(id);
  }
  try {
    return await api.sessionCwd(id);
  } catch (error) {
    if (reported && (await isThisHost(reported.host))) {
      return localPathFromUrlPath(reported.path);
    }
    throw error;
  }
}

/**
 * Points the Filer at the directory the session's shell is in — ⌘J /
 * Ctrl+Shift+J, the terminal's context menu and the Filer's locate button
 * all land here. One query per request; nothing follows the shell around.
 */
export async function revealCwdInFiler(id: string): Promise<void> {
  const store = useStore.getState();
  const tab = store.tabs.find((item) => item.info.id === id);
  if (!tab) return;
  if (tab.info.kind !== "local" && tab.info.kind !== "ssh") {
    store.setStatus(
      "Filer: only shell and SSH sessions have a working directory",
    );
    return;
  }
  if (tab.state !== "connected") {
    store.setStatus("Filer: the session is not connected");
    return;
  }
  try {
    const path = await shellCwd(tab);
    useStore.getState().revealInFiler(id, path);
    useStore.getState().setStatus(`Filer: ${path}`);
  } catch (error) {
    useStore.getState().setError(`Filer: ${String(error)}`, id);
  }
}

/**
 * 切换文件浏览器。关闭时立即收起；打开时先读取当前 Shell 的真实目录，避免
 * 面板挂载后退回 Home。调用方必须传入当前高亮的 Local 或 SSH 标签。
 */
export async function toggleFilerForSession(id: string): Promise<void> {
  const store = useStore.getState();
  if (store.panels.filer) {
    store.togglePanel("filer");
    return;
  }
  await revealCwdInFiler(id);
}
