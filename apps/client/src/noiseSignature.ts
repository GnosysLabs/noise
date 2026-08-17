const ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
export const NOISE_SIGNATURE_PATTERN = "[0-9A-HJKMNP-TV-Z]{6}-?[0-9A-HJKMNP-TV-Z]{6}";

export function noiseSignature(publicKey: string) {
  try {
    const padded = publicKey.padEnd(Math.ceil(publicKey.length / 4) * 4, "=");
    const bytes = Uint8Array.from(atob(padded), (character) => character.charCodeAt(0));
    if (bytes.length < 8) return "UNAVAILABLE";
    let signature = "";
    for (let characterIndex = 0; characterIndex < 12; characterIndex += 1) {
      let value = 0;
      for (let bitIndex = 0; bitIndex < 5; bitIndex += 1) {
        const sourceBit = characterIndex * 5 + bitIndex;
        value = (value << 1) | ((bytes[Math.floor(sourceBit / 8)] >> (7 - (sourceBit % 8))) & 1);
      }
      signature += ALPHABET[value];
    }
    return `${signature.slice(0, 6)}-${signature.slice(6)}`;
  } catch {
    return "UNAVAILABLE";
  }
}

export function compactNoiseSignature(publicKey: string) {
  const signature = noiseSignature(publicKey);
  return signature === "UNAVAILABLE" ? "" : signature.replace("-", "");
}

export function normalizeNoiseSignature(value: string) {
  return value.replace(/-/g, "").toUpperCase();
}
