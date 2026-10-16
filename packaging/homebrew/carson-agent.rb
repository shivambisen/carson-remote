# Homebrew formula template for a tap repository named `homebrew-carson`
# (users then run: brew install <you>/carson/carson-agent).
# The release workflow prints the URLs and SHA-256 values to paste below.
class CarsonAgent < Formula
  desc "Free remote desktop: view and control your own computers from your browser"
  homepage "https://github.com/OWNER/carson"
  version "0.1.0"
  license "UNLICENSED" # update when the project picks a license

  on_arm do
    url "https://github.com/OWNER/carson/releases/download/v#{version}/carson-agent-#{version}-macos-arm64.tar.gz"
    sha256 "REPLACE_WITH_ARM64_SHA256"
  end
  on_intel do
    url "https://github.com/OWNER/carson/releases/download/v#{version}/carson-agent-#{version}-macos-x86_64.tar.gz"
    sha256 "REPLACE_WITH_X86_64_SHA256"
  end

  depends_on :macos
  depends_on macos: :sonoma # ScreenCaptureKit APIs used by carson need macOS 14

  def install
    bin.install "carson-agent"
  end

  def caveats
    <<~EOS
      Finish setup (pairs this Mac, asks for permissions, starts at login):
        carson-agent setup --server https://YOUR-CARSON-SERVER

      After `brew upgrade`, macOS may ask again for Screen Recording / Accessibility.
    EOS
  end

  test do
    assert_match "carson-agent #{version}", shell_output("#{bin}/carson-agent --version")
  end
end
