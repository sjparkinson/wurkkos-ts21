#!/usr/bin/env ruby
# frozen_string_literal: true

# PreToolUse hook on Bash. When the command is a `git commit`, it checks the
# message is a Conventional Commit and runs the host tests first. Exit 2 blocks
# the commit and hands stderr back to Claude.

require 'json'
require 'open3'

TYPES = %w[feat fix docs refactor test ci build perf style chore revert].freeze
CONVENTIONAL = /\A(?:#{TYPES.join('|')})(?:\([a-z0-9._\/-]+\))?!?: \S/

# An option's value can't start with `-`, so each token matches only one way and
# a long run of flags can't backtrack exponentially.
COMMIT = /(?:\A|[;&|(]\s*)git\s+(?:-\S+\s+(?:[^\s-]\S*\s+)?)*commit\b/
HEREDOC = /<<-?\s*['"]?(\w+)['"]?\n(.*?)\n\s*\1\b/m
QUOTED = /(?:-m|--message)(?:\s+|=)(?:"((?:[^"\\]|\\.)*)"|'([^']*)')/

def message(command)
  heredoc = command.match(HEREDOC)
  return heredoc[2] if heredoc

  quoted = command.match(QUOTED)
  quoted && (quoted[1] || quoted[2])
end

# Returns nil when the subject line is fine, or a reason when it isn't.
def problem(msg)
  subject = msg.lines.first.to_s.chomp
  return 'the commit message is empty' if subject.strip.empty?
  return nil if CONVENTIONAL.match?(subject)

  "#{subject.inspect} isn't a Conventional Commit. Use `type(scope): summary` " \
    "with a type from: #{TYPES.join(', ')}."
end

def block(reason)
  warn(reason)
  exit 2
end

input = JSON.parse($stdin.read)
command = input.dig('tool_input', 'command').to_s
exit 0 unless COMMIT.match?(command)

# No inline message (-F, --amend --no-edit, the editor): nothing to check.
msg = message(command)
if msg
  reason = problem(msg)
  block(reason) if reason
end

# The checkout the session is in, which may be a worktree rather than CLAUDE_PROJECT_DIR.
top, status = Open3.capture2('git', 'rev-parse', '--show-toplevel')
dir = status.success? ? top.chomp : Dir.pwd
out, status = Open3.capture2e('cargo', 'test', '--workspace', '--locked', '-q', chdir: dir)
block("cargo test --workspace --locked -q failed, so the commit is blocked:\n#{out.lines.last(60).join}") unless status.success?
