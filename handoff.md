# Noise marketing handoff

Updated October 12, 2026. Canonical local checkout: `/Users/christopher/Dev/noise-marketing`. Remote: `https://github.com/GnosysLabs/noise.git`. This independent checkout was cloned from the existing repository and its origin restored to GitHub; the old checkout at `/Users/christopher/Dev/old/noise` and its untracked files were left intact.

The current web app, Electron desktop and native iOS code are in **a different repository**: `/Users/christopher/Dev/noise`, remote `https://github.com/GnosysLabs/make-noise.git`. Read that checkout's `handoff.md` for the complete cross-platform handoff. This repository's root README, Rust crates, Tauri client and old native code describe an earlier product. Do not use them to rebuild or deploy the current app.

## Working files

- `apps/marketing/index.html`: homepage, navigation, feature copy, download widgets, FAQ and footer.
- `apps/marketing/style.css`: layout, responsive styling and typography.
- `apps/marketing/src.js`: browser interactions.
- `apps/marketing/privacy/index.html`, `terms/index.html`: public legal pages.
- `apps/marketing/public/`: original Noise branding, robots and social images.
- `apps/marketing/vite.config.js`: three-page build plus static-worker export.
- `deploy/marketing/README.md`, `release.sh`: guarded production publication.

## Local commands

```sh
cd /Users/christopher/Dev/noise-marketing
pnpm --dir apps/marketing install --frozen-lockfile --ignore-scripts
pnpm --dir apps/marketing dev --port 4175
pnpm --dir apps/marketing build
```

Default marketing dev port is 4173, also used by the app API; use 4175 when both run. Output is `apps/marketing/dist/`, ignored by Git. No production secrets or database are needed for marketing development.

Dependencies were installed and the three-page production build succeeded in this checkout during handoff preparation. No marketing publication was performed.

## Production

Live site: `https://makenoise.chat`; app: `https://app.makenoise.chat`; privacy: `/privacy/`; terms: `/terms/`; download storefront: `https://gnosyslabs.itch.io/noise`.

Read `deploy/marketing/README.md`, commit reviewed marketing changes, then from repository root:

```sh
bash deploy/marketing/release.sh
```

Use existing SSH access to `root@cyphers-vps`, with host verification intact. Static webroot: `/var/www/makenoise.chat`. Backups: `/var/backups/noise-marketing/<release>/site.tar.gz`. The script builds, backs up, publishes hashed assets before HTML, compares the three public pages against the build and restores on failure. It verifies nginx stayed unchanged. It does not deploy/restart the web app, API, SFU or bot workers. Do not use old relay/Tauri bootstrap/deployment scripts for this work.

The itch.io widget needs the existing CSP `frame-src 'self' https://itch.io/embed/` allowance. Preserve current nginx directives/includes if a separately requested configuration change is necessary. A static release does not change CSP.

Read-only checkpoint October 12: live release `20261012T052547Z-8329b3e`, source `8329b3e712bd201e709e01dbb378466ea9c97416`. Source was current with GitHub at checkout preparation. This handoff advances documentation only, not live website assets.

## Preferences and claims

Christopher expects the agent to manage Git, preserve unrelated edits and use tests sparingly. Stay within the requested task; another agent may own app work. Preserve the site's established artwork and styling. Current copy recommends desktop and offers the browser app; the native iOS app is being prepared for App Store submission. Do not revive obsolete claims about frequency codes, E2EE or independently hosted relays. Live calls use transport encryption; do not market that as E2EE.

Privacy/terms must agree with actual account/session/social/content/upload/voice processing and the current App Store disclosures in the app repository's `ios/APP_STORE.md`. Do not promise features merely because an old roadmap mentions them. Review responsive rendering for meaningful UI changes, use a focused build, and report source/build/live evidence separately.
