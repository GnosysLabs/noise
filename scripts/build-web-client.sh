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

mkdir -p "$wasm_output"
wasm-bindgen \
  --target web \
  --out-dir "$wasm_output" \
  --out-name noise_web \
  "$repo_root/target/wasm32-unknown-unknown/release/noise_web.wasm"
rm -f "$wasm_output/noise_web.d.ts" "$wasm_output/noise_web_bg.wasm.d.ts"

(
  cd "$repo_root/apps/client"
  pnpm exec tsc
  pnpm exec vite build
)
