#!/usr/bin/env ruby
# frozen_string_literal: true

# Updates Asterline's four prebuilt-archive references in its Homebrew Formula.
# The workflow obtains each checksum from the matching published GitHub Release.

abort "usage: #{$PROGRAM_NAME} <formula> <version> <mac-arm-sha> <mac-intel-sha> <linux-arm-sha> <linux-intel-sha>" unless ARGV.length == 6

formula_path, version, mac_arm_sha, mac_intel_sha, linux_arm_sha, linux_intel_sha = ARGV
abort "version must be a stable X.Y.Z release" unless version.match?(/\A\d+\.\d+\.\d+\z/)

checksums = [mac_arm_sha, mac_intel_sha, linux_arm_sha, linux_intel_sha]
abort "every checksum must be a lowercase SHA-256" unless checksums.all? { |checksum| checksum.match?(/\A[0-9a-f]{64}\z/) }

formula = File.read(formula_path)
release_url = "https://github.com/song0705/Asterline/releases/download/v#{version}"
updates = [
  ["on_macos do", "on_arm", "asterline-#{version}-aarch64-apple-darwin.tar.gz", mac_arm_sha],
  ["on_macos do", "on_intel", "asterline-#{version}-x86_64-apple-darwin.tar.gz", mac_intel_sha],
  ["on_linux do", "on_arm", "asterline-v#{version}-Linux-arm64.tar.gz", linux_arm_sha],
  ["on_linux do", "on_intel", "asterline-v#{version}-Linux-x86_64.tar.gz", linux_intel_sha]
]

updates.each do |platform, architecture, archive, checksum|
  pattern = Regexp.new(
    "(#{Regexp.escape(platform)}.*?#{Regexp.escape(architecture)} do\\s+url \")[^\"]+(\"\\s+sha256 \")[0-9a-f]{64}(\")",
    Regexp::MULTILINE
  )
  abort "expected exactly one #{platform} #{architecture} archive block" unless formula.scan(pattern).length == 1

  formula.sub!(pattern, "\\1#{release_url}/#{archive}\\2#{checksum}\\3")
end

File.write(formula_path, formula)
