#!/usr/bin/env ruby
# frozen_string_literal: true

# PreToolUse hook on Bash. When the command is a `git commit`, it checks the
# message is a Conventional Commit and runs the host tests first. Exit 2 blocks
# the commit and hands stderr back to Claude.

require 'json'
require 'open3'
require_relative 'commit_msg'

COMMIT = /(?:\A|[;&|(]\s*)git\s+(?:-\S+\s+(?:\S+\s+)?)*commit\b/
HEREDOC = /<<-?\s*['"]?(\w+)['"]?\n(.*?)\n\s*\1\b/m
QUOTED = /(?:-m|--message)(?:\s+|=)(?:"((?:[^"\\]|\\.)*)"|'([^']*)')/

def message(command)
  heredoc = command.match(HEREDOC)
  return heredoc[2] if heredoc

  quoted = command.match(QUOTED)
  quoted && (quoted[1] || quoted[2])
end

def block(reason)
  warn(reason)
  exit 2
end

input = JSON.parse($stdin.read)
command = input.dig('tool_input', 'command').to_s
exit 0 unless COMMIT.match?(command)

# No inline message (-F, --amend --no-edit, the editor): nothing to check
# here, and CI checks every commit on the PR anyway.
msg = message(command)
if msg
  reason = CommitMsg.problem(msg)
  block(reason) if reason
end

dir = ENV.fetch('CLAUDE_PROJECT_DIR', Dir.pwd)
out, status = Open3.capture2e('cargo', 'test', '--workspace', '--locked', '-q', chdir: dir)
block("cargo test --workspace --locked -q failed, so the commit is blocked:\n#{out.lines.last(60).join}") unless status.success?
