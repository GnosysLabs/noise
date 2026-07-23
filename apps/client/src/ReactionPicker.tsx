import data from "@emoji-mart/data";
import { Picker } from "emoji-mart";
import { useEffect, useRef } from "react";

type EmojiSelection = {
  native?: string;
};

export function ReactionPicker({
  x,
  y,
  onPick,
  onClose,
}: {
  x: number;
  y: number;
  onPick: (emoji: string) => void;
  onClose: () => void;
}) {
  const host = useRef<HTMLDivElement | null>(null);
  const onPickRef = useRef(onPick);
  onPickRef.current = onPick;

  useEffect(() => {
    const container = host.current;
    if (!container) return;
    const picker = new (Picker as any)({
      data,
      theme: "dark",
      set: "native",
      previewPosition: "none",
      skinTonePosition: "search",
      navPosition: "top",
      perLine: 9,
      maxFrequentRows: 2,
      emojiButtonRadius: "6px",
      autoFocus: true,
      dynamicWidth: false,
      onEmojiSelect: (selection: EmojiSelection) => {
        if (selection.native) onPickRef.current(selection.native);
      },
    });
    container.appendChild(picker as unknown as Node);
    return () => {
      container.innerHTML = "";
    };
  }, []);

  useEffect(() => {
    const close = () => onClose();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [onClose]);

  const pickerWidth = Math.min(352, window.innerWidth - 24);
  const pickerHeight = Math.min(435, window.innerHeight - 24);
  const left = Math.max(12, Math.min(x, window.innerWidth - pickerWidth - 12));
  const top = Math.max(12, Math.min(y, window.innerHeight - pickerHeight - 12));

  return (
    <div
      className="reaction-picker"
      style={{ left, top, width: pickerWidth, height: pickerHeight }}
      onMouseDown={(event) => event.stopPropagation()}
      onClick={(event) => event.stopPropagation()}
    >
      <div ref={host} className="emoji-mart-host" />
    </div>
  );
}
