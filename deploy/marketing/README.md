# Marketing site releases

The website source is `apps/marketing`. This is separate from the current Noise app checkout and its service deployment.

Install its pinned dependencies with `pnpm --dir apps/marketing install --frozen-lockfile --ignore-scripts`. Commit website changes, then run `bash deploy/marketing/release.sh` from this repository. The script builds the three static pages once, keeps a complete site backup, publishes hashed assets before atomically replacing the HTML, and verifies the public pages against the build. It does not change nginx, the Noise app, its database, or its API.

Backups are at `/var/backups/noise-marketing/<release>/site.tar.gz`. A failed verification restores the previous pages automatically. To restore a release later, extract that backup into `/var/www/makenoise.chat` on `root@cyphers-vps`. Extra hashed assets may remain and are harmless.

The October 9, 2026 update describes the new browser app. Earlier native downloads and the old application's identity, encryption, and frequency-code claims are no longer promoted as current features. The site's original logo and artwork are retained.
