# Changelog

All notable changes to ymir are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and ymir uses
[Semantic Versioning](https://semver.org/). Its public surface is the keys, the flags,
the search syntax, what `--filter` and `--show` print, and the exit codes.

## [Unreleased]

## [0.2.0] - 2026-10-23

### Added

- Changed words in diffs are marked, as levi marks them.
- `C-e` shows all of every tool's output, whole diffs, and the content of files a call
  created; `C-e` again goes back to the first lines. It stays on from session to session.
- Drag over a pane to copy: the transcript copies the text under the drag, with wrapped
  lines joined back up; sessions copy as `--filter` prints them; turns copy their lines.

### Changed

- Diff lines no longer wrap. They keep their columns and scroll sideways with `S-←`/`S-→`
  or a sideways wheel; the transcript's title says how far.

## [0.1.0] - 2026-10-22

First release.

### Added

- Three panes: your Claude Code sessions, newest first; the prompts of the selected one,
  turn by turn; and its transcript, with replies, tool calls, the first lines of their
  output, edits as diffs, subagents under the call that started them, and where the context
  was compacted.
- fzf-style search over every session at once: project, title, date and branch, and the
  prompts, replies, tool calls and file paths in them. The middle column narrows to the
  matching lines and the transcript opens on them. In prose, a word's letters must stay
  close together, so a search finds the replies that say it; paths stay fully fuzzy.
- `Enter` prints the session id (`claude --resume $(ymir)`), `C-y` copies it. Quitting
  without picking exits with 130.
- `ymir --filter '<query>'` runs the same search without the TUI and prints
  `<id> <date> <project> <title>` per session, best first. It exits 1 when nothing matches.
- `ymir --show <id> [--turn N]` prints a session, or one turn of it, as plain text. The id
  can be any unique prefix.
- Mouse support: click, wheel, and drag a border to resize the panes.
- Sessions are read on every core and indexed without their tool output: 545 MB in 0.06 s.
  Sessions come from `~/.claude/projects`, or `$CLAUDE_CONFIG_DIR/projects`.
- ymir never writes to `~/.claude`, makes no network calls, and strips terminal escapes
  from session text. Unknown record types and unreadable lines are skipped.
- CI: tests on Linux and macOS, rustfmt, clippy, the minimum Rust version (1.88), and
  `cargo deny`.

[Unreleased]: https://github.com/rsdlt/ymir/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/rsdlt/ymir/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/rsdlt/ymir/releases/tag/v0.1.0
