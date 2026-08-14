class Asterline < Formula
  desc "Local-first terminal workspace for coordinating coding agents"
  homepage "https://github.com/song0705/Asterline"
  version "0.2.3"
  license "MIT"

  depends_on :macos

  on_macos do
    on_arm do
      url "https://github.com/song0705/Asterline/releases/download/v#{version}/asterline-#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "c9ec97ea35d1644a72d6fb31c8f639e552972084de33195007f0b951c9ccebce"
    end

    on_intel do
      url "https://github.com/song0705/Asterline/releases/download/v#{version}/asterline-#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "adaff07aa7902a1545aaf8a20411525c5d0f79495aed0cfc0940af7f24dd362a"
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
