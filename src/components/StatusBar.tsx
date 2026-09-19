import { useEffect, useState } from "react";

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
  const status = useStore((s) => s.status);
  const error = useStore((s) => s.error);
  const [clock, setClock] = useState(() => new Date());

  useEffect(() => {
    const timer = window.setInterval(() => setClock(new Date()), 1000);
    return () => window.clearInterval(timer);
  }, []);

  const pad = (n: number) => String(n).padStart(2, "0");
  const stamp = `${clock.getFullYear()}/${clock.getMonth() + 1}/${clock.getDate()} ${pad(
    clock.getHours(),
  )}:${pad(clock.getMinutes())}`;

  const legacy = tab?.state === "connected" ? tab.info.legacyAlgorithms : [];

  return (
    <div className="statusbar">
      <span className={`status-item${error ? " is-error" : ""}`}>
        {error ?? status}
      </span>
      <div className="status-spacer" />
      {tab && (
        <>
          {legacy.length > 0 && (
            <span className="status-item status-legacy" title={legacyTitle(legacy)}>
              <Icon name="warning" />
              Legacy SSH
            </span>
          )}
          {isFileSession(tab.info.kind) ? (
            <span className="status-item">Dual-pane file transfer</span>
          ) : (
            <span className="status-item">
              Window {tab.rows}×{tab.cols}
            </span>
          )}
          <span className="status-item">{tab.info.protocol}</span>
        </>
      )}
      <span className="status-item">{stamp}</span>
    </div>
  );
}
