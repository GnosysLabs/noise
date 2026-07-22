import { Avatar, Style } from "@dicebear/core";
import glass from "@dicebear/styles/glass.json" with { type: "json" };

const glassStyle = new Style(glass);
const avatarSize = 256;

export async function generateGroupAvatar(seed: string): Promise<string> {
  const svg = new Avatar(glassStyle, {
    seed,
    size: avatarSize,
  }).toString();
  const source = URL.createObjectURL(
    new Blob([svg], { type: "image/svg+xml;charset=utf-8" }),
  );
  try {
    const image = await loadImage(source);
    const canvas = document.createElement("canvas");
    canvas.width = avatarSize;
    canvas.height = avatarSize;
    const context = canvas.getContext("2d");
    if (!context) throw new Error("this browser cannot prepare group icons");
    context.drawImage(image, 0, 0, avatarSize, avatarSize);
    const blob = await new Promise<Blob>((resolve, reject) =>
      canvas.toBlob(
        (value) =>
          value ? resolve(value) : reject(new Error("group icon encoding failed")),
        "image/png",
      ),
    );
    return encodeBase64(await blob.arrayBuffer());
  } finally {
    URL.revokeObjectURL(source);
  }
}

function loadImage(source: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.onload = () => resolve(image);
    image.onerror = () => reject(new Error("DiceBear could not render the group icon"));
    image.src = source;
  });
}

function encodeBase64(value: ArrayBuffer): string {
  const bytes = new Uint8Array(value);
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000));
  }
  return btoa(binary);
}
