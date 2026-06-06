class Codeseek < Formula
  desc "Code intelligence CLI — AST-based call graph + semantic search"
  homepage "https://github.com/CodeBendKit/codeseek"
  version "0.1.11"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/CodeBendKit/codeseek/releases/download/v0.1.11/codeseek-darwin-arm64"
      sha256 "REPLACE_WITH_ACTUAL_SHA256"
    else
      url "https://github.com/CodeBendKit/codeseek/releases/download/v0.1.11/codeseek-darwin-x64"
      sha256 "REPLACE_WITH_ACTUAL_SHA256"
    end
  end

  on_linux do
    url "https://github.com/CodeBendKit/codeseek/releases/download/v0.1.11/codeseek-linux-x64"
    sha256 "REPLACE_WITH_ACTUAL_SHA256"
  end

  def install
    bin.install Dir["*"].first => "codeseek"
  end

  test do
    system "#{bin}/codeseek", "--version"
  end
end
