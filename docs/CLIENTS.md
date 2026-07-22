# Noise clients

Noise now has one shared React interface in `apps/client` and a deliberately
small runtime boundary in `src/api.ts`.

## Desktop today

The Tauri shell exposes the existing `noise-ffi` JSON request API as one command.
The UI therefore uses the same Rust identity, signing, encryption, reducer, and
relay transport as the CLI and native macOS prototype. macOS and Windows share
the interface; only the thin platform shell differs.

Private identity state is owned by Rust and stored outside the webview. The
webview receives only the view models needed to render the current screen.

## Browser next

The browser build already renders the same interface, but intentionally refuses
protocol operations until these pieces are implemented:

1. Compile the portable parts of `noise-core` and `noise-client` to WASM.
2. Add an IndexedDB-backed identity store with export and recovery UX.
3. Replace the native `reqwest` transport with browser `fetch`.
4. Expose the same typed request/response contract implemented by `src/api.ts`.
5. Add browser-specific lifecycle, offline, and multi-tab coordination.

Relays now opt into CORS. They remain untrusted stores of signed encrypted
objects and do not use cookies or hold a browser user's credentials.

Browser clients are clients, not dependable relays. They may opportunistically
exchange data later, but durable availability still comes from user-selected,
replaceable relays.
