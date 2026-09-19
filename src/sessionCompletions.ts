import * as api from "./api";
import {
  completionsFor,
  type Completion,
  type CompletionBackend,
} from "./completion";
import type { FileEntry } from "./types";

/**
 * The directory listing a completion needs, remembered for a moment. The
 * popup asks again on every keystroke while a path is being typed, and for an
 * SSH session each ask is a round trip; a short life is enough to cover the
 * typing of one word without ever showing a stale directory for long.
 */
const LIST_TTL_MS = 1500;
const listings = new Map<string, { at: number; entries: FileEntry[] }>();

async function listCached(
  key: string,
  read: () => Promise<FileEntry[]>,
): Promise<FileEntry[]> {
  const hit = listings.get(key);
  if (hit && Date.now() - hit.at < LIST_TTL_MS) return hit.entries;
  try {
    const entries = await read();
    // A cache that only grows would hold every directory ever visited.
    if (listings.size > 64) listings.clear();
    listings.set(key, { at: Date.now(), entries });
    return entries;
  } catch {
    return [];
  }
}

/** Where a provider is asked about paths: the shell's own report, or home. */
export interface CompletionEnvironment {
  /** The session a path belongs to (local shell or SSH). */
  id: string;
  /** True for a local shell; an SSH session's paths are the server's. */
  local: boolean;
  /** Where the session is right now, as the shell last reported it. */
  cwd: () => string | null;
}

/**
 * The completion source for one session: rows from the line's context, then
 * this shell's own history. Paths come from the session's own filesystem —
 * `local_list` for a shell here, `sftp_list` for one on a server — so the
 * popup offers the files the command would actually see.
 */
export function sessionCompletions(
  environment: CompletionEnvironment,
  host: string,
): (input: string) => Promise<Completion[]> {
  const backend: CompletionBackend = {
    local: environment.local,
    cwd: environment.cwd,
    list: (path) =>
      listCached(`${environment.id}\u0000${path}`, () =>
        environment.local
          ? api.localList(path).then((listing) => listing.entries)
          : api.sftpList(environment.id, path).then((listing) => listing.entries),
      ),
  };
  return (input: string) => completionsFor(input, input.length, backend, host);
}
