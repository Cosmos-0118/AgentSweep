# Homebrew formula for AgentSweep.
# Host this tap at <you>/homebrew-agentsweep as Formula/agentsweep.rb.
# version + sha256 below are updated automatically by .github/workflows/release.yml
# (scripts/update_homebrew_sha.py) whenever a v* tag is released.
class Agentsweep < Formula
  desc "Understand and control what your coding agents store locally"
  homepage "https://github.com/Cosmos-0118/AgentSweep"
  version "0.1.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Cosmos-0118/AgentSweep/releases/download/v0.1.0/agentsweep-aarch64-apple-darwin"
      sha256 "2f4cbddde8e360a1fe5642d01557bfdaf729d454dff477cee19e0823003f25c8"
    else
      url "https://github.com/Cosmos-0118/AgentSweep/releases/download/v0.1.0/agentsweep-x86_64-apple-darwin"
      sha256 "REPLACE_WITH_SHA256"
    end
  end

  on_linux do
    url "https://github.com/Cosmos-0118/AgentSweep/releases/download/v0.1.0/agentsweep-x86_64-unknown-linux-gnu"
    sha256 "REPLACE_WITH_SHA256"
  end

  def install
    bin.install "agentsweep-aarch64-apple-darwin" => "agentsweep" if OS.mac? && Hardware::CPU.arm?
    bin.install "agentsweep-x86_64-apple-darwin" => "agentsweep" if OS.mac? && Hardware::CPU.intel?
    bin.install "agentsweep-x86_64-unknown-linux-gnu" => "agentsweep" if OS.linux?
  end

  test do
    assert_match "agentsweep", shell_output("#{bin}/agentsweep --version")
  end
end
