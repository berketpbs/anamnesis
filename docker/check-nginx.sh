#!/bin/sh
# Run docker/nginx.conf.example in front of an anamnesis image and send it
# what a hook, a search and a browser send.
#
# Usage: docker/check-nginx.sh <image>
set -eu

image=$1
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
net="anamnesis-nginx-check-$$"
cleanup() {
  docker rm -f "$net-server" "$net-proxy" >/dev/null 2>&1 || true
  docker network rm "$net" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

# The paths docker is handed are the host's. Under Git Bash on Windows that
# means the Windows spelling, and no rewriting of the arguments by MSYS.
hostpath() { if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else echo "$1"; fi; }
export MSYS_NO_PATHCONV=1

# As the caller, so the key is the caller's to make readable and to delete:
# written as the container's root, a Linux runner could do neither.
docker run --rm --user "$(id -u):$(id -g)" -v "$(hostpath "$work"):/out" \
  --entrypoint openssl alpine/openssl \
  req -x509 -newkey rsa:2048 -nodes -days 1 -subj "/CN=anamnesis.local" \
  -keyout /out/key.pem -out /out/cert.pem >/dev/null 2>&1
# Readable by nginx, which does not run as the caller.
chmod 644 "$work/key.pem"

docker network create "$net" >/dev/null
# Named `anamnesis` on this network, which is the upstream the template names.
docker run -d --name "$net-server" --network "$net" --network-alias anamnesis "$image" >/dev/null
docker run -d --name "$net-proxy" --network "$net" -p 127.0.0.1::443 \
  -v "$(hostpath "$here")/nginx.conf.example:/etc/nginx/nginx.conf:ro" \
  -v "$(hostpath "$work"):/etc/nginx/ssl:ro" nginx:alpine >/dev/null
port=$(docker port "$net-proxy" 443/tcp | head -n 1 | sed 's/.*://')
base="https://127.0.0.1:$port"

failures=0
check() {
  if [ "$1" = "$2" ]; then echo "ok   $3"; else echo "FAIL $3 (got $1, wanted $2)"; failures=$((failures + 1)); fi
}

up=no
for _ in $(seq 1 60); do
  if curl -fsk "$base/health" >/dev/null 2>&1; then up=yes; break; fi
  sleep 1
done
check "$up" yes "the proxy answers /health through to the server"

if docker logs "$net-proxy" 2>&1 | grep -qE '\[(warn|emerg)\]'; then
  docker logs "$net-proxy" 2>&1 | grep -E '\[(warn|emerg)\]'
  check warnings none "nginx starts without a warning"
else
  check none none "nginx starts without a warning"
fi

# `|| true` on each measurement: the status code is the assertion, and curl's
# own exit status (a write to /dev/null that Git Bash cannot open, say) is not.
#
# A hook event past nginx's default 1 MB, which the server reads up to 16 MB.
python=$(command -v python3 || command -v python)
"$python" - "$(hostpath "$work")/big.json" <<'PY'
import json, sys
event = {
    "session_id": "nginx-check",
    "hook_event_name": "PostToolUse",
    "cwd": "/workspace",
    "tool_name": "Read",
    "tool_input": {"file_path": "/workspace/big.log"},
    "tool_response": "x" * 2_000_000,
}
open(sys.argv[1], "w").write(json.dumps(event))
PY
code=$(curl -sk -o /dev/null -w '%{http_code}' -X POST \
  -H 'content-type: application/json' --data-binary "@$(hostpath "$work")/big.json" \
  "$base/hook?agent=claude-code&probe=1" || true)
check "$code" 200 "a 2 MB hook event reaches the server"

code=$(curl -sk -o /dev/null -w '%{http_code}' "$base/api/v1/scopes" || true)
check "$code" 200 "the API answers through the proxy"

frames=$(curl -sk -D - -o /dev/null "$base/ui" | grep -ci '^x-frame-options:' || true)
check "$frames" 1 "the browser gets one X-Frame-Options, the server's"

[ "$failures" -eq 0 ] || { docker logs "$net-proxy" 2>&1 | tail -20; exit 1; }
