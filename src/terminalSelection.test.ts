import { describe, expect, it, vi } from "vitest";

import { startTerminalSelectionReleaseRecovery } from "./terminalSelection";

describe("terminal selection release recovery", () => {
  it("keeps a live left-button drag and ends it after a lost release", () => {
    const releases: MouseEvent[] = [];
    let dragMoves = 0;
    const onDragMove = () => {
      dragMoves += 1;
    };
    const endDrag = () => {
      document.removeEventListener("mousemove", onDragMove);
    };
    const onRelease = (event: MouseEvent) => {
      releases.push(event);
    };
    // 模拟 xterm 在按下后注册、在抬起时移除的拖选监听器。
    document.addEventListener("mousemove", onDragMove);
    document.addEventListener("mouseup", endDrag);
    document.addEventListener("mouseup", onRelease);
    const recovery = startTerminalSelectionReleaseRecovery(
      new MouseEvent("mousedown", { button: 0, clientX: 10, clientY: 15 }),
    );

    document.dispatchEvent(
      new MouseEvent("mousemove", { buttons: 1, clientX: 20, clientY: 30 }),
    );
    expect(releases).toHaveLength(0);
    expect(dragMoves).toBe(1);

    document.dispatchEvent(
      new MouseEvent("mousemove", { buttons: 0, clientX: 40, clientY: 50 }),
    );
    expect(releases).toHaveLength(1);
    expect(releases[0]).toMatchObject({
      button: 0,
      buttons: 0,
      clientX: 40,
      clientY: 50,
    });
    expect(dragMoves).toBe(1);

    recovery.dispose();
    document.removeEventListener("mousemove", onDragMove);
    document.removeEventListener("mouseup", endDrag);
    document.removeEventListener("mouseup", onRelease);
  });

  it("recovers on wheel, pointer cancellation, blur, and the next press", () => {
    const onRelease = vi.fn();
    document.addEventListener("mouseup", onRelease);

    startTerminalSelectionReleaseRecovery(new MouseEvent("mousedown"));
    document.dispatchEvent(new WheelEvent("wheel", { buttons: 0 }));

    startTerminalSelectionReleaseRecovery(new MouseEvent("mousedown"));
    document.dispatchEvent(new MouseEvent("pointercancel"));

    startTerminalSelectionReleaseRecovery(new MouseEvent("mousedown"));
    window.dispatchEvent(new Event("blur"));

    startTerminalSelectionReleaseRecovery(new MouseEvent("mousedown"));
    document.dispatchEvent(new MouseEvent("mousedown", { button: 0 }));

    expect(onRelease).toHaveBeenCalledTimes(4);

    document.removeEventListener("mouseup", onRelease);
  });

  it("removes every recovery listener on dispose", () => {
    const onRelease = vi.fn();
    document.addEventListener("mouseup", onRelease);
    const recovery = startTerminalSelectionReleaseRecovery(
      new MouseEvent("mousedown"),
    );
    recovery.dispose();

    document.dispatchEvent(new MouseEvent("mousemove", { buttons: 0 }));
    document.dispatchEvent(new WheelEvent("wheel", { buttons: 0 }));
    document.dispatchEvent(new MouseEvent("pointercancel"));
    window.dispatchEvent(new Event("blur"));
    document.dispatchEvent(new MouseEvent("mousedown", { button: 0 }));

    expect(onRelease).not.toHaveBeenCalled();
    document.removeEventListener("mouseup", onRelease);
  });
});
