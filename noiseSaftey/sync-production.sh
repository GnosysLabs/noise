#!/bin/zsh

set -euo pipefail

production_root="/Users/christopher/Library/Application Support/noise/safety-production"
ssh_key="$production_root/sync_ed25519"
inbox_dir="$production_root/inbox"
state_dir="$production_root/review-state"
outbox_dir="$state_dir/directive-outbox"
uploaded_dir="$state_dir/uploaded-directives"
sync_host="noise-safety-sync@cyphers-vps"

if [[ ! -f "$ssh_key" ]]; then
  print -u2 "The production safety sync key is missing."
  exit 1
fi

mkdir -p "$inbox_dir" "$outbox_dir" "$uploaded_dir"
chmod 700 "$production_root" "$inbox_dir" "$state_dir" "$outbox_dir" "$uploaded_dir"
chmod 600 "$ssh_key"

ssh_options=(
  -i "$ssh_key"
  -o BatchMode=yes
  -o ConnectTimeout=10
  -o IdentitiesOnly=yes
  -o LogLevel=ERROR
  -o ServerAliveCountMax=2
  -o ServerAliveInterval=5
  -o StrictHostKeyChecking=yes
)

report_list="$(ssh "${ssh_options[@]}" "$sync_host" list)"
pulled=0
while IFS= read -r receipt_id; do
  [[ -z "$receipt_id" ]] && continue
  if [[ ! "$receipt_id" =~ '^[0-9a-fA-F]{64}$' ]]; then
    print -u2 "The production safety server returned an invalid report identifier."
    exit 1
  fi
  report_path="$inbox_dir/$receipt_id.json"
  [[ -f "$report_path" ]] && continue
  temporary_path="$report_path.part.$$"
  if ! ssh "${ssh_options[@]}" "$sync_host" "read $receipt_id" > "$temporary_path"; then
    rm -f "$temporary_path"
    print -u2 "Could not pull an encrypted production safety report."
    exit 1
  fi
  chmod 600 "$temporary_path"
  mv "$temporary_path" "$report_path"
  ((pulled += 1))
done <<< "$report_list"

uploaded=0
for directive_path in "$outbox_dir"/*.json(N); do
  directive_id="${directive_path:t:r}"
  if [[ ! "$directive_id" =~ '^[0-9a-fA-F]{64}$' ]]; then
    print -u2 "The local safety outbox contains an invalid directive filename."
    exit 1
  fi
  uploaded_marker="$uploaded_dir/$directive_id"
  [[ -f "$uploaded_marker" ]] && continue
  ssh "${ssh_options[@]}" "$sync_host" "install $directive_id" < "$directive_path" >/dev/null
  : > "$uploaded_marker"
  chmod 600 "$uploaded_marker"
  ((uploaded += 1))
done

if (( pulled > 0 || uploaded > 0 )); then
  print "production safety sync: $pulled report(s) pulled, $uploaded directive(s) uploaded"
fi
