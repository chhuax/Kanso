const PRIMARY_BUTTON_MASK = 1;

/**
 * 为终端拖选补偿 WebView 丢失的左键释放事件。
 *
 * 只应在左键按下后调用。补发的事件会到达调用方和 xterm，让 xterm
 * 移除拖选监听器与自动滚动定时器；正常或补偿的 `mouseup` 都会自动
 * 卸载这些临时监听器。
 */
export function startTerminalSelectionReleaseRecovery(
  startEvent: MouseEvent,
  ownerDocument: Document = document,
  ownerWindow: Window = window,
): { dispose: () => void } {
  let lastX = startEvent.clientX;
  let lastY = startEvent.clientY;
  let disposed = false;

  const remember = (event: MouseEvent) => {
    lastX = event.clientX;
    lastY = event.clientY;
  };

  const recover = (event?: MouseEvent) => {
    if (disposed) return;
    if (event) remember(event);
    ownerDocument.dispatchEvent(
      new MouseEvent("mouseup", {
        bubbles: true,
        cancelable: true,
        button: 0,
        buttons: 0,
        clientX: lastX,
        clientY: lastY,
      }),
    );
  };

  const recoverFromButtons = (event: MouseEvent) => {
    remember(event);
    // xterm 不检查 buttons；mouseup 丢失后，它会把普通移动继续当作拖选。
    if (
      typeof event.buttons === "number" &&
      (event.buttons & PRIMARY_BUTTON_MASK) === 0
    ) {
      recover(event);
    }
  };

  const recoverBeforeNextPress = (event: MouseEvent) => {
    remember(event);
    // 新的左键按下意味着上一轮手势已经结束，必须先终止旧拖选。
    if (event.button === 0) recover(event);
  };

  const recoverFromCancel = (event: Event) => {
    recover(event instanceof MouseEvent ? event : undefined);
  };
  const recoverFromBlur = () => recover();

  const dispose = () => {
    if (disposed) return;
    disposed = true;
    ownerDocument.removeEventListener("mousedown", recoverBeforeNextPress, true);
    ownerDocument.removeEventListener("mousemove", recoverFromButtons, true);
    ownerDocument.removeEventListener("wheel", recoverFromButtons, true);
    ownerDocument.removeEventListener("pointercancel", recoverFromCancel, true);
    ownerDocument.removeEventListener("mouseup", dispose);
    ownerWindow.removeEventListener("blur", recoverFromBlur);
  };

  ownerDocument.addEventListener("mousedown", recoverBeforeNextPress, true);
  ownerDocument.addEventListener("mousemove", recoverFromButtons, true);
  ownerDocument.addEventListener("wheel", recoverFromButtons, true);
  ownerDocument.addEventListener("pointercancel", recoverFromCancel, true);
  ownerDocument.addEventListener("mouseup", dispose);
  ownerWindow.addEventListener("blur", recoverFromBlur);

  return { dispose };
}
