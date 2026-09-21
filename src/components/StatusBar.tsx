import { useEffect, useState } from "react";

import { toggleFilerForSession } from "../actions";
import { useActiveTab, useStore } from "../store";
import { isFileSession, type LegacyAlgorithms } from "../types";
import { Icon } from "./icons";

/** The tooltip naming each server's legacy algorithms, one server a line. */
const legacyTitle = (servers: LegacyAlgorithms[]): string =>
  [
    "Connected with legacy SSH algorithms, because the server offers nothing newer:",
    ...servers.map(({ address, algorithms }) => `${address}: ${algorithms.join(", ")}`),
  ].join("\n");

export function StatusBar() {
  const tab = useActiveTab();
  const error = useStore((s) => s.error);
  const filerOpen = useStore((s) => s.panels.filer);
  const gutterMode = useStore((s) => s.gutterMode);
  const setGutterMode = useStore((s) => s.setGutterMode);
  const [clock, setClock] = useState(() => new Date());

  useEffect(() => {
    const timer = window.setInterval(() => setClock(new Date()), 1000);
    return () => window.clearInterval(timer);
  }, []);

  const pad = (value: number) => String(value).padStart(2, "0");
  const stamp = `${clock.getFullYear()}/${clock.getMonth() + 1}/${clock.getDate()} ${pad(
    clock.getHours(),
  )}:${pad(clock.getMinutes())}`;
  const legacy = tab?.state === "connected" ? tab.info.legacyAlgorithms : [];
  const fileSession = tab ? isFileSession(tab.info.kind) : false;
  const activeFiler = filerOpen && !fileSession;

  return (
    <div className="statusbar">
      {tab && (
        <>
          <button
            type="button"
            className={`status-action${activeFiler ? " is-active" : ""}`}
            onClick={() => tab && void toggleFilerForSession(tab.info.id)}
            disabled={fileSession}
            aria-pressed={activeFiler}
            title={
              fileSession
                ? "File explorer is already open in this file session"
                : activeFiler
                  ? "Hide file explorer"
                  : "Show file explorer"
            }
          >
            <Icon name={activeFiler ? "folder-opened" : "folder"} />
            <span>File explorer</span>
          </button>
          {!fileSession && (
            <button
              type="button"
              className={`status-action${gutterMode !== "off" ? " is-active" : ""}`}
              onClick={() => setGutterMode(gutterMode === "off" ? "both" : "off")}
              aria-pressed={gutterMode !== "off"}
              title={gutterMode === "off" ? "Show timeline" : "Hide timeline"}
            >
              <Icon name="watch" />
              <span>Timeline</span>
            </button>
          )}
          {tab.cwd && (
            <span className="status-context status-directory" title={tab.cwd}>
              <Icon name="folder" />
              <span>{tab.cwd}</span>
            </span>
          )}
          {tab.branch && (
            <span className="status-context status-branch" title={tab.branch}>
              <Icon name="source-control" />
              <span>{tab.branch}</span>
            </span>
          )}
        </>
      )}
      <div className="status-spacer" />
      {error && <span className="status-item is-error">{error}</span>}
      {legacy.length > 0 && (
        <span className="status-item status-legacy" title={legacyTitle(legacy)}>
          <Icon name="warning" />
          Legacy SSH
        </span>
      )}
      <span className="status-item">{stamp}</span>
    </div>
  );
}
