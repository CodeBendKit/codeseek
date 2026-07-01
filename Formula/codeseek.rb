class Codeseek < Formula
  desc "Code intelligence CLI — AST-based call graph + semantic search"
  homepage "https://github.com/CodeBendKit/codeseek"
  version "0.1.26"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/CodeBendKit/codeseek/releases/download/vv0.1.22/codeseek-darwin-arm64"
      sha256 "d69412ba56a1d1e5a986ba39c8d1cd060594321cee2dd22344935e2c736aa62d"
    else
      url "https://github.com/CodeBendKit/codeseek/releases/download/vv0.1.22/codeseek-darwin-x64"
      sha256 "48af60a85e047ba5e6fe338941485e2a18c52a5a28a1e9ea3482ea1affd3b147"
    end
  end

  on_linux do
    url "https://github.com/CodeBendKit/codeseek/releases/download/vv0.1.22/codeseek-linux-x64"
    sha256 "6fd5e4177bf3a30ff43d9cbc5cb4246e36e9b18b2bfc5dd2608bbed667fd31cf"
  end

  def install
    bin.install Dir["*"].first => "codeseek"
  end

  test do
    system "#{bin}/codeseek", "--version"
  end
end
