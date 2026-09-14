#!/bin/sh
# Install anamnesis from a GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/berketpbs/anamnesis/main/install.sh | sh
#
# Settings, all optional:
#   ANAMNESIS_VERSION      a tag such as v1.1.0; the latest release when unset
#   ANAMNESIS_INSTALL_DIR  where the binary goes (see below for the default)
#
# The archive is checked against the release's SHA256SUMS before anything is
# installed: a release nobody verifies is a release nobody should run.
#
# Where it goes matters more than it looks. Hooks, the MCP registration and the
# service all name the binary by its path, so an anamnesis that is already
# installed is replaced where it is, and a new one goes to ~/.local/bin rather
# than wherever the archive happened to be unpacked.

set -eu

REPO="berketpbs/anamnesis"

say() { printf '%s\n' "$*"; }
fail() { printf 'anamnesis install: %s\n' "$*" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || fail "this needs $1, and it is not on PATH"
}

# GitHub answers a release download with a 504 now and then: on 2026-09-14 both
# CI runs of the install job failed that way on three systems while brew, which
# retries, fetched the same files in the same runs. So a request that got no
# answer, a 408, a 429 or a 5xx is made again, up to five times, waiting 1, 2, 4
# and 8 seconds; anything else, such as a 404 for a release that does not exist,
# fails at once. The status is read here rather than left to `curl --retry`:
# macOS's curl reported that 504 as a receive error (exit 56), which `--retry`
# does not count as transient, and gave up on the first one.
#
# fetch <curl arguments>: prints the final URL, the body goes where -o says.
fetch() {
    attempt=1
    while :; do
        if answer=$(curl -sSL -w '%{http_code} %{url_effective}' "$@"); then
            status=${answer%% *}
        else
            status=000
        fi
        case "$status" in
            2??) printf '%s\n' "${answer#* }"; return 0 ;;
            000 | 408 | 429 | 5??) ;;
            *) say "  HTTP $status from ${answer#* }" >&2; return 1 ;;
        esac
        [ "$attempt" -lt 5 ] || return 1
        sleep $((1 << (attempt - 1)))
        attempt=$((attempt + 1))
    done
}

need curl
need tar
need uname

case "$(uname -s)" in
    Linux) os="unknown-linux-gnu" ;;
    Darwin) os="apple-darwin" ;;
    *) fail "no release is built for $(uname -s); on Windows use install.ps1" ;;
esac

case "$(uname -m)" in
    x86_64 | amd64) arch="x86_64" ;;
    arm64 | aarch64) arch="aarch64" ;;
    *) fail "no release is built for $(uname -m)" ;;
esac

target="$arch-$os"
case "$target" in
    x86_64-unknown-linux-gnu | aarch64-apple-darwin | x86_64-apple-darwin) ;;
    *) fail "no release is built for $target; build from source with cargo build --release" ;;
esac

# The latest tag, read from where /releases/latest redirects rather than from
# the API, which limits unauthenticated callers to sixty requests an hour.
version="${ANAMNESIS_VERSION:-}"
if [ -z "$version" ]; then
    latest=$(fetch -I -o /dev/null "https://github.com/$REPO/releases/latest") ||
        fail "could not reach github.com to find the latest release"
    version="${latest##*/}"
    case "$version" in
        v*) ;;
        *) fail "could not tell the latest release from $latest" ;;
    esac
fi

name="anamnesis-$version-$target"
archive="$name.tar.gz"
base="https://github.com/$REPO/releases/download/$version"

if [ -n "${ANAMNESIS_INSTALL_DIR:-}" ]; then
    dir="$ANAMNESIS_INSTALL_DIR"
elif existing=$(command -v anamnesis 2>/dev/null) && [ -n "$existing" ]; then
    dir=$(dirname "$existing")
else
    dir="$HOME/.local/bin"
fi

work=$(mktemp -d)
# A trapped signal runs its handler and then carries on, so the signals exit
# explicitly and leave the cleanup to the EXIT trap.
trap 'rm -rf "$work"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

say "anamnesis $version for $target"
fetch -o "$work/$archive" "$base/$archive" >/dev/null || fail "could not download $base/$archive"
fetch -o "$work/SHA256SUMS" "$base/SHA256SUMS" >/dev/null || fail "could not download $base/SHA256SUMS"

expected=$(awk -v file="$archive" '$2 == file || $2 == "*" file { print $1 }' "$work/SHA256SUMS")
[ -n "$expected" ] || fail "SHA256SUMS has no line for $archive"
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$work/$archive" | awk '{ print $1 }')
else
    need shasum
    actual=$(shasum -a 256 "$work/$archive" | awk '{ print $1 }')
fi
[ "$expected" = "$actual" ] || fail "$archive does not match SHA256SUMS (expected $expected, got $actual); nothing was installed"
say "  checksum   matches SHA256SUMS"

tar -xzf "$work/$archive" -C "$work"
[ -f "$work/$name/anamnesis" ] || fail "$archive does not hold $name/anamnesis"

# Started once where it was unpacked, before it replaces anything: a binary this
# machine cannot run (a glibc older than the one it was built against, say)
# must fail here, with the loader's own words, and leave a working install as
# it was. Asked afterwards, the failure hides inside a command substitution that
# set -e does not see, and the script reports success over a broken binary.
chmod 755 "$work/$name/anamnesis"
version_line=$("$work/$name/anamnesis" --version 2>&1) ||
    fail "the $target binary does not run on this machine; nothing was installed:
$version_line"

mkdir -p "$dir"
# Copied beside the destination and then renamed over it, so a hook that runs
# in the middle finds either the old binary or the new one, never half of one.
cp "$work/$name/anamnesis" "$dir/.anamnesis.new"
chmod 755 "$dir/.anamnesis.new"
mv -f "$dir/.anamnesis.new" "$dir/anamnesis"
say "  installed  $dir/anamnesis"
say "  version    $version_line"

case ":$PATH:" in
    *":$dir:"*) ;;
    *)
        say ""
        say "  $dir is not on PATH. Add it to your shell profile:"
        say "    export PATH=\"$dir:\$PATH\""
        ;;
esac

say ""
say "  A server that is already running keeps the old binary until it restarts."
say "  Next, inside a repository you want remembered:"
say "    anamnesis setup"
