class Asterline < Formula
  desc "Local-first terminal workspace for coordinating coding agents"
  homepage "https://github.com/song0705/Asterline"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/song0705/Asterline/releases/download/v0.2.7/asterline-0.2.7-aarch64-apple-darwin.tar.gz"
      sha256 "d187fe949a93169eb68605d0000326f1046db4de97ad600c9c8729a282167605"
    end

    on_intel do
      url "https://github.com/song0705/Asterline/releases/download/v0.2.7/asterline-0.2.7-x86_64-apple-darwin.tar.gz"
      sha256 "8880b6beef6515399b0bf11b64804e3ed6e10a14dfc783f97fde3598e81ec6d0"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/song0705/Asterline/releases/download/v0.2.7/asterline-v0.2.7-Linux-arm64.tar.gz"
      sha256 "63426c8a145c878d899d3581dac2b75dceee356cd006223fefdb8c72e8b50d26"
    end

    on_intel do
      url "https://github.com/song0705/Asterline/releases/download/v0.2.7/asterline-v0.2.7-Linux-x86_64.tar.gz"
      sha256 "a847b4875ca727f92755737ef55b93787f9945c6e83ba5f71a79c62ce7b694d4"
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
