import { useEffect, useState } from "react";

import * as api from "../api";
import { IS_WINDOWS } from "../platform";
import type { LocalShell } from "../types";

const CUSTOM = "custom";

/** Shell discovery supplies choices; the session dialog owns saving/connecting. */
export function LocalShellFields({
  value,
  onChange,
}: {
  value: string | null | undefined;
  onChange: (command: string | null, name: string) => void;
}) {
  const [shells, setShells] = useState<LocalShell[]>([]);
  const [custom, setCustom] = useState(false);
  const [loading, setLoading] = useState(IS_WINDOWS);
  const [error, setError] = useState("");

  useEffect(() => {
    if (!IS_WINDOWS) return;
    let disposed = false;
    api.listLocalShells()
      .then(
        (items) => {
          if (!disposed) setShells(items);
        },
        (reason) => {
          if (!disposed) setError(`Could not detect shells: ${String(reason)}`);
        },
      )
      .finally(() => {
        if (!disposed) setLoading(false);
      });
    return () => {
      disposed = true;
    };
  }, []);

  const selected = custom
    ? CUSTOM
    : value
      ? shells.find((shell) => shell.command === value)?.command ?? CUSTOM
      : "";

  return (
    <>
      <label className="session-field is-wide">
        <span className="session-field-label">Shell</span>
        <select
          value={selected}
          onChange={(event) => {
            const choice = event.target.value;
            setCustom(choice === CUSTOM);
            if (choice === CUSTOM) onChange(value ?? "", "Local Shell");
            else {
              const shell = shells.find((item) => item.command === choice);
              onChange(shell?.command ?? null, shell?.name ?? "Local Shell");
            }
          }}
        >
          <option value="">
            Default shell ({IS_WINDOWS ? "%COMSPEC%" : "$SHELL"})
          </option>
          {shells.map((shell) => (
            <option key={shell.command} value={shell.command}>
              {shell.name}
            </option>
          ))}
          <option value={CUSTOM}>Custom command…</option>
        </select>
        {loading && (
          <span className="session-field-hint" role="status">
            Detecting installed shells…
          </span>
        )}
        {error && (
          <span className="session-field-hint" role="status">{error}</span>
        )}
      </label>
      {selected !== "" && (
        <label className="session-field is-wide">
          <span className="session-field-label">Command</span>
          <input
            autoCapitalize="none"
            autoCorrect="off"
            spellCheck={false}
            value={value ?? ""}
            placeholder={IS_WINDOWS ? "wsl.exe -d Ubuntu" : "/bin/bash"}
            onChange={(event) => {
              setCustom(true);
              onChange(event.target.value, "Local Shell");
            }}
          />
        </label>
      )}
    </>
  );
}
