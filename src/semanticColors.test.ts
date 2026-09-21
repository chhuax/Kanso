import { describe, expect, it } from "vitest";

import {
  allowsTableHeaderColors,
  semanticLine,
  SEMANTIC_BANDS,
} from "./semanticColors";

const LS_ROW = "AweSun  Code  Documents  go  Movies  OrbStack  Public";

describe("表头语义着色", () => {
  it("保留普通命令的表头识别", () => {
    expect(semanticLine(LS_ROW).band).toBe(SEMANTIC_BANDS.dark.header);
    expect(semanticLine(LS_ROW, allowsTableHeaderColors("ps aux")).band).toBe(
      SEMANTIC_BANDS.dark.header,
    );
  });

  it.each([
    "ls",
    "ls -la",
    "/bin/ls -la",
    "sudo -E ls",
    "FOO=1 command ls",
    "cd /tmp && ls",
    "ls | head",
  ])("对 %s 的输出禁用表头着色", (command) => {
    expect(allowsTableHeaderColors(command)).toBe(false);
    expect(semanticLine(LS_ROW, allowsTableHeaderColors(command)).band).toBeUndefined();
  });
});
