#!/bin/sh
# Write the package manager manifests for one release.
#
#   packaging/render.sh v1.1.1 SHA256SUMS [out_dir]
#
# Writes HomebrewFormula/anamnesis.rb and bucket/anamnesis.json under out_dir
# (the repository root when omitted), and with WINGET=1 the three winget
# manifests under out_dir/winget/, which are submitted to microsoft/winget-pkgs
# rather than kept here.
#
# Every hash comes from the release's own SHA256SUMS and nothing is downloaded:
# a manifest is a promise about bytes somebody else will fetch, so the only
# acceptable source for it is the file the release published beside them. An
# archive missing from that file stops the script rather than leaving a
# placeholder a package manager would reject a day later.

set -eu

REPO="berketpbs/anamnesis"
DESCRIPTION="Long-term memory for AI coding agents"

fail() { printf 'render: %s\n' "$*" >&2; exit 1; }

[ $# -ge 2 ] || fail "usage: render.sh <tag> <SHA256SUMS> [out_dir]"
tag="$1"
sums="$2"
out="${3:-$(cd "$(dirname "$0")/.." && pwd)}"

case "$tag" in
    v[0-9]*.[0-9]*.[0-9]*) ;;
    *) fail "$tag is not a release tag like v1.1.1" ;;
esac
[ -f "$sums" ] || fail "no such file: $sums"
version="${tag#v}"
base="https://github.com/$REPO/releases/download/$tag"

# The hash for one archive. sha256sum marks a file hashed in binary mode with a
# leading `*`, and the Windows archive's line carries one.
hash_of() {
    archive="anamnesis-$tag-$1.$2"
    found=$(awk -v name="$archive" '{ file = $2; sub(/^\*/, "", file); if (file == name) print $1 }' "$sums")
    [ -n "$found" ] || fail "$archive is not in $sums"
    printf '%s' "$found"
}

mac_arm=$(hash_of aarch64-apple-darwin tar.gz)
mac_intel=$(hash_of x86_64-apple-darwin tar.gz)
linux=$(hash_of x86_64-unknown-linux-gnu tar.gz)
windows=$(hash_of x86_64-pc-windows-msvc zip)

mkdir -p "$out/HomebrewFormula" "$out/bucket"

cat > "$out/HomebrewFormula/anamnesis.rb" <<EOF
# Written by packaging/render.sh from the $tag release's SHA256SUMS.
# Edit the script, not this file: the next release rewrites it.
class Anamnesis < Formula
  desc "$DESCRIPTION"
  homepage "https://github.com/$REPO"
  version "$version"
  license "MIT"

  on_macos do
    on_arm do
      url "$base/anamnesis-$tag-aarch64-apple-darwin.tar.gz"
      sha256 "$mac_arm"
    end
    on_intel do
      url "$base/anamnesis-$tag-x86_64-apple-darwin.tar.gz"
      sha256 "$mac_intel"
    end
  end

  on_linux do
    on_intel do
      url "$base/anamnesis-$tag-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "$linux"
    end
  end

  def install
    bin.install "anamnesis"
  end

  def caveats
    <<~TEXT
      Wire a repository from inside it with:
        anamnesis setup
      Hooks and the MCP registration name #{HOMEBREW_PREFIX}/bin/anamnesis,
      which brew upgrade keeps pointing at the current version.
    TEXT
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/anamnesis --version")
  end
end
EOF

cat > "$out/bucket/anamnesis.json" <<EOF
{
    "version": "$version",
    "description": "$DESCRIPTION",
    "homepage": "https://github.com/$REPO",
    "license": "MIT",
    "architecture": {
        "64bit": {
            "url": "$base/anamnesis-$tag-x86_64-pc-windows-msvc.zip",
            "hash": "$windows",
            "extract_dir": "anamnesis-$tag-x86_64-pc-windows-msvc"
        }
    },
    "bin": "anamnesis.exe",
    "checkver": "github",
    "autoupdate": {
        "architecture": {
            "64bit": {
                "url": "https://github.com/$REPO/releases/download/v\$version/anamnesis-v\$version-x86_64-pc-windows-msvc.zip",
                "extract_dir": "anamnesis-v\$version-x86_64-pc-windows-msvc"
            }
        },
        "hash": {
            "url": "\$baseurl/SHA256SUMS"
        }
    },
    "notes": "Wire a repository from inside it with: anamnesis setup"
}
EOF

if [ "${WINGET:-}" = "1" ]; then
    id="BerkeTopbas.Anamnesis"
    dir="$out/winget/manifests/b/BerkeTopbas/Anamnesis/$version"
    mkdir -p "$dir"
    # winget wants the hash in capitals.
    windows_upper=$(printf '%s' "$windows" | tr 'a-f' 'A-F')

    cat > "$dir/$id.yaml" <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.version.1.9.0.schema.json
PackageIdentifier: $id
PackageVersion: $version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.9.0
EOF

    cat > "$dir/$id.installer.yaml" <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.installer.1.9.0.schema.json
PackageIdentifier: $id
PackageVersion: $version
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
- RelativeFilePath: anamnesis-$tag-x86_64-pc-windows-msvc\\anamnesis.exe
  PortableCommandAlias: anamnesis
Installers:
- Architecture: x64
  InstallerUrl: $base/anamnesis-$tag-x86_64-pc-windows-msvc.zip
  InstallerSha256: $windows_upper
ManifestType: installer
ManifestVersion: 1.9.0
EOF

    cat > "$dir/$id.locale.en-US.yaml" <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.defaultLocale.1.9.0.schema.json
PackageIdentifier: $id
PackageVersion: $version
PackageLocale: en-US
Publisher: Berke Topbas
PublisherUrl: https://github.com/berketpbs
PackageName: Anamnesis
PackageUrl: https://github.com/$REPO
License: MIT
LicenseUrl: https://github.com/$REPO/blob/main/LICENSE
ShortDescription: $DESCRIPTION
Description: A git-versioned markdown wiki, compiled from what coding agent sessions did, that the next session starts from.
Moniker: anamnesis
Tags:
- ai
- cli
- coding-agent
- mcp
- memory
ReleaseNotesUrl: https://github.com/$REPO/releases/tag/$tag
ManifestType: defaultLocale
ManifestVersion: 1.9.0
EOF
fi

printf 'render: %s manifests written under %s\n' "$tag" "$out"
