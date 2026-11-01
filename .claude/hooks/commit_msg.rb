#!/usr/bin/env ruby
# frozen_string_literal: true

# Conventional Commits check, shared by the pre-commit hook and CI.
#
#   ruby .claude/hooks/commit_msg.rb BASE..HEAD   # check every commit subject in a range
#   echo "feat: thing" | ruby .claude/hooks/commit_msg.rb   # check one message

module CommitMsg
  TYPES = %w[feat fix docs refactor test ci build perf style chore revert].freeze
  PATTERN = /\A(?:#{TYPES.join('|')})(?:\([a-z0-9._\/-]+\))?!?: \S/

  module_function

  # Returns nil when the message is fine, or a reason when it isn't.
  def problem(message)
    subject = message.to_s.lines.first.to_s.chomp
    return 'the commit message is empty' if subject.strip.empty?
    return nil if PATTERN.match?(subject)

    "#{subject.inspect} isn't a Conventional Commit. Use `type(scope): summary` " \
      "with a type from: #{TYPES.join(', ')}."
  end
end

if $PROGRAM_NAME == __FILE__
  if ARGV.empty?
    reason = CommitMsg.problem($stdin.read)
    abort(reason) if reason
  else
    log = IO.popen(['git', 'log', '--no-merges', '--format=%H %s', ARGV.fetch(0)], &:read)
    abort('git log failed') unless $?.success?

    bad = log.lines.filter_map do |line|
      sha, subject = line.chomp.split(' ', 2)
      reason = CommitMsg.problem(subject)
      "#{sha[0, 7]}: #{reason}" if reason
    end
    abort(bad.join("\n")) unless bad.empty?
  end
end
