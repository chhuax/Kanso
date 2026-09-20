import { useEffect, useMemo, useRef, useState } from "react";

import { commandHistory } from "../history";
import { useStore } from "../store";
import type { CommandHistoryEntry } from "../api";

interface Props {
  /** The session the box was opened from, so its host's commands lead. */
  sessionId: string | null;
  onClose: () => void;
  /** Puts the chosen command on that session's line, ready to edit or run. */
  onAccept: (command: string) => void;
}

/** How many rows the list holds; the browser is for finding, not for reading. */
const LIMIT = 100;

/** "just now", "5m", "3h", "2d" — how long ago a command was last run. */
function ago(at: number): string {
  const seconds = Math.max(0, Math.round((Date.now() - at) / 1000));
  if (seconds < 60) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.round(hours / 24)}d`;
}

/**
 * The commands this app remembers, searched the way a shell's own reverse
 * search is: type to narrow, walk with the arrows, Enter to take one. It is
 * deliberately not the shell's history — that one belongs to a single shell
 * and dies with it — but the record the completion popup already keeps, which
 * spans every tab and both machines.
 *
 * Nothing runs on Enter: the command lands on the prompt, where it can be read
 * and edited before it is submitted.
 */
export function HistoryOverlay({ sessionId, onClose, onAccept }: Props) {
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLDivElement>(null);

  // The same key the completion popup ranks by (see `historyHost`), so the
  // commands of the session the box was opened from lead the list.
  const host = useStore((state) => {
    const tab = state.tabs.find((item) => item.info.id === sessionId);
    return tab ? `${tab.info.protocol}:${tab.info.address}` : "";
  });

  const rows = useMemo(
    () => commandHistory.browse(query, host, LIMIT),
    [query, host],
  );

  useEffect(() => {
    input.current?.focus();
  }, []);

  // The query changed, so the old selection means nothing.
  useEffect(() => {
    setIndex(0);
  }, [query]);

  // Keep the selected row on screen as the arrows walk past the fold.
  useEffect(() => {
    list.current
      ?.querySelector<HTMLElement>(".history-item.is-selected")
      ?.scrollIntoView({ block: "nearest" });
  }, [index]);

  const take = (entry: CommandHistoryEntry | undefined) => {
    if (!entry) return;
    onAccept(entry.command);
    onClose();
  };

  return (
    <div className="history-overlay" role="dialog" aria-label="Command history">
      <div className="history-box">
        <div className="history-query">
          <input
            ref={input}
            value={query}
            spellCheck={false}
            placeholder="Search command history"
            aria-label="Search command history"
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                onClose();
                return;
              }
              if (event.key === "ArrowDown") {
                event.preventDefault();
                setIndex((current) => Math.min(current + 1, rows.length - 1));
                return;
              }
              if (event.key === "ArrowUp") {
                event.preventDefault();
                setIndex((current) => Math.max(current - 1, 0));
                return;
              }
              if (event.key === "Enter") {
                event.preventDefault();
                take(rows[index]);
              }
            }}
          />
          <span className="history-count">
            {rows.length === 0
              ? "No commands"
              : rows.length >= LIMIT
                ? `${LIMIT}+`
                : String(rows.length)}
          </span>
        </div>
        <div className="history-list" ref={list} role="listbox">
          {rows.map((entry, at) => (
            <div
              key={`${entry.host}\u0000${entry.command}`}
              className={`history-item${at === index ? " is-selected" : ""}`}
              role="option"
              aria-selected={at === index}
              // mousedown rather than click: the box must not lose focus to
              // the row before the press is handled.
              onMouseDown={(event) => {
                event.preventDefault();
                take(entry);
              }}
              onMouseEnter={() => setIndex(at)}
            >
              <span className="history-command">{entry.command}</span>
              <span className="history-meta">
                <span className="history-count-badge" title="Times run">
                  {entry.count > 1 ? `×${entry.count}` : ""}
                </span>
                <span className="history-ago">{ago(entry.lastUsed)}</span>
              </span>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
