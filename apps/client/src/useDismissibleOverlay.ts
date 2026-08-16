import { useEffect, useRef } from "react";

export function overlayDismissesOnMouseDown(button: number) {
  // macOS trackpads fire contextmenu and then a button-2 mousedown.
  // Closing on that follow-up click makes the menu vanish before it is seen.
  return button !== 2;
}

export function isSecondaryPointer(event: { button: number }) {
  return event.button === 2;
}

export function useContextMenuTarget<T extends HTMLElement>(
  onOpen?: (position: { x: number; y: number }) => void,
  ignore?: string,
) {
  const ref = useRef<T | null>(null);
  useEffect(() => {
    const node = ref.current;
    if (!node || !onOpen) return;
    const ignored = (event: Event) => {
      const target = event.target;
      return Boolean(
        ignore
        && target instanceof Element
        && target.closest(ignore),
      );
    };
    const clearSelection = () => {
      window.getSelection()?.removeAllRanges();
    };
    const suppressSelection = () => {
      node.classList.add("suppress-select");
      clearSelection();
      requestAnimationFrame(clearSelection);
    };
    const restoreSelection = () => {
      node.classList.remove("suppress-select");
    };
    const open = (event: MouseEvent) => {
      if (ignored(event)) return;
      event.preventDefault();
      event.stopPropagation();
      suppressSelection();
      onOpen({ x: event.clientX, y: event.clientY });
    };
    const onMouseDown = (event: MouseEvent) => {
      if (!isSecondaryPointer(event)) return;
      event.preventDefault();
      suppressSelection();
      if (ignored(event)) return;
      event.stopPropagation();
      onOpen({ x: event.clientX, y: event.clientY });
    };
    const onSelectStart = (event: Event) => {
      if (!node.classList.contains("suppress-select")) return;
      event.preventDefault();
      clearSelection();
    };
    node.addEventListener("mousedown", onMouseDown, true);
    node.addEventListener("selectstart", onSelectStart, true);
    node.addEventListener("contextmenu", open, true);
    window.addEventListener("mouseup", restoreSelection);
    return () => {
      restoreSelection();
      node.removeEventListener("mousedown", onMouseDown, true);
      node.removeEventListener("selectstart", onSelectStart, true);
      node.removeEventListener("contextmenu", open, true);
      window.removeEventListener("mouseup", restoreSelection);
    };
  }, [ignore, onOpen]);
  return ref;
}

export function useDismissibleOverlay(onClose: () => void) {
  useEffect(() => {
    const onMouseDown = (event: MouseEvent) => {
      if (!overlayDismissesOnMouseDown(event.button)) return;
      onClose();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("mousedown", onMouseDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("mousedown", onMouseDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [onClose]);
}
