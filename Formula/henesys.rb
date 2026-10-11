# Rendered by .github/workflows/release.yml into Formula/henesys.rb on the master branch.
class Henesys < Formula
  desc "Makes heavy commands on a shared machine wait their turn, and says what they are waiting behind."
  homepage "https://github.com/Vusys/henesys"
  version "1.0.0-rc.1"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Vusys/henesys/releases/download/v1.0.0-rc.1/henesys-1.0.0-rc.1-aarch64-apple-darwin.tar.gz"
      sha256 "931740315d86e7c8d1988daef6b29ab13bae22535f7974806419fcd87f369083"
    end
    on_intel do
      url "https://github.com/Vusys/henesys/releases/download/v1.0.0-rc.1/henesys-1.0.0-rc.1-x86_64-apple-darwin.tar.gz"
      sha256 "58a95f922e7c85cd0ee2439f84820a1418ee83d2f38206d9fca43da7404111ed"
    end
  end

  def install
    bin.install "henesys"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/henesys --version")
  end
end
