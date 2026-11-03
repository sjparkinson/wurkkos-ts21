#!/usr/bin/env ruby
# PostToolUse hook on Edit and Write. Lints the file that changed and, on a
# problem, exits 2 so stderr goes back to Claude to fix.
#
#   .rs  rustfmt --check, then clippy (the AVR target's clippy for hw.rs and rt.rs, which
#        the host never compiles)
#   .rb  ruby -wc
#   .github/workflows/*.yml  gh actions-lock --verify-local (lockfile coverage)

require "json"
require "open3"

FIRMWARE_ONLY = %w[src/hw.rs src/rt.rs].freeze

def run(*cmd, chdir:)
  out, status = Open3.capture2e(*cmd, chdir: chdir)
  status.success? ? nil : "$ #{cmd.join(' ')}\n#{out.lines.last(80).join}"
end

def rust(path, dir)
  relative = path.delete_prefix("#{dir}/")
  fmt = run("rustfmt", "--edition", "2024", "--check", "--color", "never", path, chdir: dir)
  return fmt if fmt

  if FIRMWARE_ONLY.include?(relative)
    run("cargo", "clippy", "-q", "--package", "ts21", "--release", "--target", "avr-none",
        "-Zbuild-std=core", "--", "-D", "warnings", chdir: dir)
  else
    run("cargo", "clippy", "--workspace", "--all-targets", "--locked", "-q", "--", "-D", "warnings", chdir: dir)
  end
end

input = JSON.parse($stdin.read)
path = input.dig("tool_input", "file_path").to_s
exit 0 if path.empty? || !File.file?(path)

# The checkout the file lives in, so edits in a worktree lint that worktree.
top, status = Open3.capture2("git", "-C", File.dirname(path), "rev-parse", "--show-toplevel")
dir = status.success? ? top.chomp : Dir.pwd
problem =
  if path.start_with?("#{dir}/.github/workflows/") && path.end_with?(".yml", ".yaml")
    run("gh", "actions-lock", "--verify-local", chdir: dir)
  else
    case File.extname(path)
    when ".rs" then rust(path, dir)
    when ".rb" then run("ruby", "-wc", path, chdir: dir)
    end
  end

exit 0 unless problem

warn("Lint problems after editing #{path}:\n#{problem}")
exit 2
