class Asterline < Formula
  desc "Local-first terminal workspace for coordinating coding agents"
  homepage "https://github.com/song0705/Asterline"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/song0705/Asterline/releases/download/v0.2.9/asterline-0.2.9-aarch64-apple-darwin.tar.gz"
      sha256 "322604253de110254c119300d562c2646992465026ba85173acca82b08bcb6a4"
    end

    on_intel do
      url "https://github.com/song0705/Asterline/releases/download/v0.2.9/asterline-0.2.9-x86_64-apple-darwin.tar.gz"
      sha256 "ed33fbc31705663e5a00cc03a53ebb522e0e3d0db6bb5286a3bc5e3b62162842"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/song0705/Asterline/releases/download/v0.2.9/asterline-v0.2.9-Linux-arm64.tar.gz"
      sha256 "fb2afd79405f25ed697c596860a169599d6bb258e232f34fc5df6069a71de766"
    end

    on_intel do
      url "https://github.com/song0705/Asterline/releases/download/v0.2.9/asterline-v0.2.9-Linux-x86_64.tar.gz"
      sha256 "eedde381531acfbad4a451cbdd8c5de008b4f54d6f0f42567742ae90451b802d"
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
