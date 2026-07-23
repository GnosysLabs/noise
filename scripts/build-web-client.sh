#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
wasm_output="$repo_root/apps/client/public/wasm"

command -v wasm-bindgen >/dev/null 2>&1 || {
  echo "wasm-bindgen 0.2.126 is required: cargo install wasm-bindgen-cli --version 0.2.126 --locked" >&2
  exit 1
}

cargo build \
  --manifest-path "$repo_root/Cargo.toml" \
  -p noise-web \
  --target wasm32-unknown-unknown \
  --release

rm -rf "$wasm_output"
mkdir -p "$wasm_output"
wasm-bindgen \
  --target web \
  --out-dir "$wasm_output" \
  --out-name noise_web \
  "$repo_root/target/wasm32-unknown-unknown/release/noise_web.wasm"
rm -f "$wasm_output/noise_web.d.ts" "$wasm_output/noise_web_bg.wasm.d.ts"

if command -v shasum >/dev/null 2>&1; then
  wasm_version="$(shasum -a 256 "$wasm_output/noise_web_bg.wasm" | cut -c1-16)"
else
  wasm_version="$(sha256sum "$wasm_output/noise_web_bg.wasm" | cut -c1-16)"
fi
hashed_wasm="noise_web_bg-$wasm_version.wasm"
hashed_wrapper="noise_web-$wasm_version.js"
sed "s/noise_web_bg\\.wasm/$hashed_wasm/g" \
  "$wasm_output/noise_web.js" > "$wasm_output/$hashed_wrapper"
mv "$wasm_output/noise_web_bg.wasm" "$wasm_output/$hashed_wasm"
rm "$wasm_output/noise_web.js"

(
  cd "$repo_root/apps/client"
  pnpm exec tsc
  VITE_NOISE_WASM_VERSION="$wasm_version" pnpm exec vite build
)
