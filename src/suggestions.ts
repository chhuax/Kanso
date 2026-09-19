import { COMMON_COMMAND_GROUPS } from "./commonCommands";
import {
  MAX_SUGGESTIONS,
  MIN_GAIN,
  MIN_INPUT,
  commandHistory,
  type CommandSuggestion,
} from "./history";

/**
 * Rows from the commands shipped with the app, matched by the same rule the
 * history is: the typed text has to appear in the command, and completing it
 * has to save at least `MIN_GAIN` characters, so the popup never offers a row
 * that costs more keystrokes than typing the rest.
 */
export function librarySuggestions(input: string): CommandSuggestion[] {
  if (input.trim().length < MIN_INPUT) return [];
  const rows: CommandSuggestion[] = [];
  for (const command of COMMON_COMMAND_GROUPS.flat()) {
    if (command.length < input.length + MIN_GAIN) continue;
    const matchStart = command.indexOf(input);
    if (matchStart === -1) continue;
    rows.push({ command, matchStart });
  }
  return rows;
}

/**
 * What the popup shows: the user's own commands first — they are what this
 * shell has actually been used for — then the shipped ones, under the same
 * row limit. A command that is in both appears once, as the history hit it
 * already is.
 */
export function mergeSuggestions(
  fromHistory: CommandSuggestion[],
  fromLibrary: CommandSuggestion[],
): CommandSuggestion[] {
  const merged = [...fromHistory];
  const seen = new Set(merged.map((row) => row.command));
  for (const row of fromLibrary) {
    if (merged.length >= MAX_SUGGESTIONS) break;
    if (seen.has(row.command)) continue;
    seen.add(row.command);
    merged.push(row);
  }
  return merged;
}

/** The popup's one source: this shell's history, then the shipped commands. */
export function suggestCommands(
  input: string,
  host: string,
): CommandSuggestion[] {
  return mergeSuggestions(
    commandHistory.suggest(input, host),
    librarySuggestions(input),
  );
}
