# Marketing checkout instructions

Read `handoff.md` before working. The active marketing website is `apps/marketing`; the current web/desktop/iOS application is in the separate `GnosysLabs/make-noise` repository at `/Users/christopher/Dev/noise` on this Mac. Root legacy Rust/Tauri instructions do not describe that application.

1. Use tests sparingly; prefer fast iteration and checks relevant to the change.
2. Manage Git for Christopher. Preserve unrelated changes and handle commits, branches and synchronization.
3. For marketing releases, read `deploy/marketing/README.md` and use `bash deploy/marketing/release.sh`. Do not restart or modify the application/API/SFU/bot services for static website work.
