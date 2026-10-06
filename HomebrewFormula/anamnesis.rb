# Written by packaging/render.sh from the v1.2.1 release's SHA256SUMS.
# Edit the script, not this file: the next release rewrites it.
class Anamnesis < Formula
  desc "Long-term memory for AI coding agents"
  homepage "https://github.com/berketpbs/anamnesis"
  version "1.2.1"
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
      url "https://github.com/berketpbs/anamnesis/releases/download/v1.2.1/anamnesis-v1.2.1-aarch64-apple-darwin.tar.gz"
      sha256 "a7500856f8851993811b64826324bcbd3337c3cf589b33f5c8ca3239cbb65983"
    end
    on_intel do
      url "https://github.com/berketpbs/anamnesis/releases/download/v1.2.1/anamnesis-v1.2.1-x86_64-apple-darwin.tar.gz"
      sha256 "7f0a10ee3df8d314ae83dd843a000845436d81a8a01e1e608e510c4cf11bcd37"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/berketpbs/anamnesis/releases/download/v1.2.1/anamnesis-v1.2.1-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "57c2a05b18d4d9e027015c5911e8a27aa56bb0a4d48d79ba959dc2f9906cc616"
    end
    on_intel do
      url "https://github.com/berketpbs/anamnesis/releases/download/v1.2.1/anamnesis-v1.2.1-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "123ec4299b6ee915ba6850b14afded6ce4dd82f374aa0d73a9b782a682de93fe"
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
