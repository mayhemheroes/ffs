class Ffs < Formula
  desc "the file filesystem: mount semi-structured data (like JSON) as a Unix filesystem"
  homepage "https://mgree.github.io/ffs/"
  url "https://github.com/mgree/ffs/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "f585ae96a85210edf152106351fc276238de5f98af278571f4f688a3a133cd28"
  license "GPL-3.0"
  head "https://github.com/mgree/ffs.git"

  depends_on "rust" => :build
  depends_on "pkg-config" => :build
  # breaking... it's a cask not a formula. what does that mean?
  depends_on "macfuse"

  def install
    system "cargo", "build", "--release"
    system "cp", "target/release/ffs", prefix/bin/"ffs"
  end

  test do
    system bin/"ffs", "--completions", "bash"
    # how do you run something in the background?
  end
end
