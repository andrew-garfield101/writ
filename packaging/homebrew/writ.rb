class Writ < Formula
  desc "AI-native version control for agentic systems"
  homepage "https://github.com/agarfield/writ"
  version "0.1.0"
  license "AGPL-3.0-only"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/agarfield/writ/releases/download/v#{version}/writ-aarch64-apple-darwin.tar.gz"
      # sha256 "PLACEHOLDER" # Update after first release
    else
      url "https://github.com/agarfield/writ/releases/download/v#{version}/writ-x86_64-apple-darwin.tar.gz"
      # sha256 "PLACEHOLDER" # Update after first release
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/agarfield/writ/releases/download/v#{version}/writ-aarch64-unknown-linux-gnu.tar.gz"
      # sha256 "PLACEHOLDER" # Update after first release
    else
      url "https://github.com/agarfield/writ/releases/download/v#{version}/writ-x86_64-unknown-linux-gnu.tar.gz"
      # sha256 "PLACEHOLDER" # Update after first release
    end
  end

  def install
    bin.install "writ"
  end

  test do
    system "#{bin}/writ", "--version"
  end
end
