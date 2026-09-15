# Written by packaging/render.sh from the v1.1.1 release's SHA256SUMS.
# Edit the script, not this file: the next release rewrites it.
class Anamnesis < Formula
  desc "Long-term memory for AI coding agents"
  homepage "https://github.com/berketpbs/anamnesis"
  version "1.1.1"
  license "MIT"

  # The counterpart of the bucket's checkver: what brew livecheck and
  # brew bump-formula-pr read to see that a release has been tagged whose
  # manifests are not merged yet.
  livecheck do
    url :homepage
    strategy :github_latest
  end

  on_macos do
    on_arm do
      url "https://github.com/berketpbs/anamnesis/releases/download/v1.1.1/anamnesis-v1.1.1-aarch64-apple-darwin.tar.gz"
      sha256 "9cf0277aa74bdd97bd6e86e42eb61276ef9102c60b85e87dcd028889ceb579aa"
    end
    on_intel do
      url "https://github.com/berketpbs/anamnesis/releases/download/v1.1.1/anamnesis-v1.1.1-x86_64-apple-darwin.tar.gz"
      sha256 "ec559580bd38c00e5d117422ff5cc31fe3096693988e23bdca88d48698833c0a"
    end
  end

  on_linux do
    on_arm do
      # No Linux arm64 build is published. Without this the formula simply has
      # no url on such a machine, and brew fails on the missing url rather than
      # on the reason for it.
      depends_on arch: :x86_64
    end
    on_intel do
      url "https://github.com/berketpbs/anamnesis/releases/download/v1.1.1/anamnesis-v1.1.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "bb4d24cad2a8aef656bd929ed9366e40bba1a23aa03dea754d17fd030028349d"
    end
  end

  def install
    bin.install "anamnesis"
  end

  def caveats
    <<~TEXT
      Wire a repository from inside it with:
        anamnesis setup
      Run it again after every brew upgrade. Hooks and the MCP registration
      keep the path they were written with, an upgrade moves the binary, and
      setup rewrites an entry that no longer leads to the binary running it.
    TEXT
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/anamnesis --version")
  end
end
