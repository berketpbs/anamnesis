#!/bin/sh
# Install the OpenCode plugin into a scratch project with the binary under
# test, start a server for it, and run check.mjs under Bun.
#
# Usage: run.sh <anamnesis binary> <bun binary>
set -eu

binary=$1
bun=$2
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'kill "$server" 2>/dev/null || true; rm -rf "$work"' EXIT

data="$work/data"
project="$work/project"
port=18080
mkdir -p "$project"
git -C "$project" init -q

"$binary" --data-dir "$data" serve --port "$port" --no-watch >"$work/serve.log" 2>&1 &
server=$!
answered=no
for _ in $(seq 1 50); do
  if "$binary" hook --probe --server "http://127.0.0.1:$port" </dev/null >/dev/null 2>&1; then
    answered=yes
    break
  fi
  sleep 0.2
done
if [ "$answered" != yes ]; then
  cat "$work/serve.log"
  echo "the server did not come up" >&2
  exit 1
fi

# Written by the real installer, not by substituting into the template here:
# the file under test is the one a user gets.
(cd "$project" && "$binary" --data-dir "$data" install-hooks --agent opencode \
  --server "http://127.0.0.1:$port" --write >/dev/null)
plugin="$project/.opencode/plugins/anamnesis.js"
[ -f "$plugin" ] || { echo "install-hooks wrote no plugin at $plugin" >&2; exit 1; }

if ! "$bun" "$here/check.mjs" "$plugin" "$project" "$binary" "$data"; then
  echo "--- server log" >&2
  cat "$work/serve.log" >&2
  exit 1
fi
