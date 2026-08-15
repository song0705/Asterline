class Asterline < Formula
  desc "Local-first terminal workspace for coordinating coding agents"
  homepage "https://github.com/song0705/Asterline"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/song0705/Asterline/releases/download/v0.2.8/asterline-0.2.8-aarch64-apple-darwin.tar.gz"
      sha256 "fe93fedc7fa561e6cfa9399cab9a09fa7fecd30df60773a25d0c2dad02cb024c"
    end

    on_intel do
      url "https://github.com/song0705/Asterline/releases/download/v0.2.8/asterline-0.2.8-x86_64-apple-darwin.tar.gz"
      sha256 "be17f0575a98a8814ccdba9730b1067613d62b2fc68c27bea3b34b7ee0adc0e4"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/song0705/Asterline/releases/download/v0.2.8/asterline-v0.2.8-Linux-arm64.tar.gz"
      sha256 "18cf85bcb1000040aa6231891cc169f47f19da913e033bb28a8f564f26bc33ae"
    end

    on_intel do
      url "https://github.com/song0705/Asterline/releases/download/v0.2.8/asterline-v0.2.8-Linux-x86_64.tar.gz"
      sha256 "f8a35ede4a74b3e8158708369f06ee1c44708204652e23399acd76b319c2e164"
    end
  end

  def install
    bin.install "asterline", "ast"
    doc.install "LICENSE"
  end

  test do
    assert_match "Usage: asterline", shell_output("#{bin}/asterline --help")
    assert_match "Usage: asterline", shell_output("#{bin}/ast --help")
  end
end
