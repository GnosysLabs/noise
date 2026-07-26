# Web client

## Build

The production client is built from the shared repository:

```sh
pnpm --dir apps/client install --frozen-lockfile
pnpm --dir apps/client build:web
```

`scripts/build-web-client.sh` builds `noise-web` for `wasm32-unknown-unknown`, runs `wasm-bindgen` 0.2.126, creates content-hashed WASM/JS names, type-checks, and runs Vite. Output is `apps/client/dist/`.

Inspect `dist/index.html`, the hashed JS asset, `dist/media-sw.js` when present, and the hashed files under `dist/wasm/`. Do not deploy a plain `pnpm build` as the production web client because it omits the canonical WASM packaging flow.

## Deploy

Production is served at `https://app.makenoise.chat` from `/var/www/app.makenoise.chat` on `cyphers-vps`. Use the `cyphers-vps-ssh` skill for live access.

After explicit production authorization:

1. Create an explicit timestamped backup of `/var/www/app.makenoise.chat`.
2. Sync only `apps/client/dist/` to `/var/www/app.makenoise.chat/`.
3. Use `rsync --delete` only after validating both exact paths.
4. Do not overwrite nginx or certificates during a normal client deployment.

The nginx source is `deploy/nginx/app.makenoise.chat.conf`. It serves `index.html` without long caching and content-hashed `/assets/` and `/wasm/` files with long immutable caching.

## Verify

- Fetch production `index.html` with cache bypass and identify its current hashed JS.
- Fetch that exact JS, the referenced hashed WASM wrapper/background, `media-sw.js`, and the manifest; require status 200.
- Confirm the served hashes match the just-built `dist/`, not merely that the domain responds.
- Perform the requested interaction in the deployed web app.
- Check the browser console and service-worker registration for media-streaming changes.

A successful `rsync` is not deployment proof; the served asset graph and user-visible behavior are.
