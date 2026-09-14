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
    latest=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") ||
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
curl -fsSL -o "$work/$archive" "$base/$archive" || fail "could not download $base/$archive"
curl -fsSL -o "$work/SHA256SUMS" "$base/SHA256SUMS" || fail "could not download $base/SHA256SUMS"

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
