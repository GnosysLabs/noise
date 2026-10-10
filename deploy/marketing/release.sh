#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
if ! git diff --quiet -- apps/marketing deploy/marketing || ! git diff --cached --quiet -- apps/marketing deploy/marketing; then
  printf 'Commit marketing source before deploying.\n' >&2
  exit 1
fi
release="$(date -u +%Y%m%dT%H%M%SZ)-$(git rev-parse --short HEAD)"
source_commit="$(git rev-parse HEAD)"
pnpm --dir apps/marketing build
mkdir -p .noise/marketing-releases
node --input-type=module - "$release" "$source_commit" <<'JS' > apps/marketing/dist/marketing-release.json
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
const [, , release, sourceCommit] = process.argv;
const files = ['index.html','privacy/index.html','terms/index.html'];
const sha256 = Object.fromEntries(files.map(path => [path, createHash('sha256').update(readFileSync('apps/marketing/dist/'+path)).digest('hex')]));
console.log(JSON.stringify({ release, sourceCommit, builtAt: new Date().toISOString(), sha256 }, null, 2));
JS
COPYFILE_DISABLE=1 tar --no-xattrs -czf ".noise/marketing-releases/$release.tar.gz" --exclude='./server' --exclude='._*' -C apps/marketing/dist .
scp -q ".noise/marketing-releases/$release.tar.gz" "root@cyphers-vps:/tmp/noise-marketing-$release.tar.gz"
ssh -o BatchMode=yes root@cyphers-vps bash -s -- "$release" <<'REMOTE'
set -euo pipefail
release="$1"
webroot=/var/www/makenoise.chat
staging="/var/www/.noise-marketing-$release"
backup="/var/backups/noise-marketing/$release"
install -d -m 0755 "$staging"
install -d -m 0700 "$backup"
test -f "$webroot/index.html"
tar -czf "$backup/site.tar.gz" -C "$webroot" .
cp -a /etc/nginx/sites-available/makenoise.chat "$backup/nginx.conf"
tar -xzf "/tmp/noise-marketing-$release.tar.gz" -C "$staging"
rollback() {
  tar -xzf "$backup/site.tar.gz" -C "$webroot"
  printf 'Marketing release failed; previous pages restored from %s\n' "$backup" >&2
}
trap rollback ERR
# Publish hashed assets first, then replace each HTML page atomically.
rsync -a --exclude='index.html' "$staging/" "$webroot/"
for page in privacy/index.html terms/index.html index.html; do
  target="$webroot/$page"
  install -m 0644 "$staging/$page" "$target.next"
  mv -f "$target.next" "$target"
done
for page in index.html privacy/index.html terms/index.html; do
  url="https://makenoise.chat/${page%index.html}"
  curl -fsS --connect-timeout 5 --max-time 20 "$url" -o "$staging/verified.html"
  cmp "$staging/$page" "$staging/verified.html"
done
cmp /etc/nginx/sites-available/makenoise.chat "$backup/nginx.conf"
trap - ERR
cat "$webroot/marketing-release.json"
printf '\nMarketing backup: %s\n' "$backup"
rm -rf -- "$staging"
rm -f -- "/tmp/noise-marketing-$release.tar.gz"
REMOTE
