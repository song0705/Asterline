class Asterline < Formula
  desc "Local-first terminal workspace for coordinating coding agents"
  homepage "https://github.com/song0705/Asterline"
  license "MIT"

  depends_on :macos

  on_macos do
    on_arm do
      url "https://github.com/song0705/Asterline/releases/download/v0.2.5/asterline-0.2.5-aarch64-apple-darwin.tar.gz"
      sha256 "9fe038680d0e3a380b4bbb190865c46b5b6dc4348314f067f8c0fdd732d957a6"
    end

    on_intel do
      url "https://github.com/song0705/Asterline/releases/download/v0.2.5/asterline-0.2.5-x86_64-apple-darwin.tar.gz"
      sha256 "1970b9ea724481ee54b49ea306aabb3cf7e81bcb09d6e5a6aee1cd80516d412f"
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
