<p align="center">
  <em>Where all your Claude Code sessions meet. Fuzzy search for you, plain text for your agents.</em>
</p>

# ymir

[![CI](https://github.com/rsdlt/ymir/actions/workflows/ci.yml/badge.svg)](https://github.com/rsdlt/ymir/actions/workflows/ci.yml)

> *Everything you and your agents ever said is still here. Take your time.*

ymir browses your past Claude Code sessions: every prompt you typed, every reply, every tool
call and the files it changed. Type to search all of them at once, fzf style, and read any
session back as a clean transcript.

## Why ymir

Claude Code keeps every session on disk, as JSON lines under `~/.claude/projects`. Three
weeks later you remember the conversation, not where it was: which project, which day, what
you called the thing. `grep` over half a gigabyte of JSON finds the words, buried in API
requests and escaped newlines.

ymir reads those files and shows you the conversations. In *Attack on Titan*, every Eldian's
memories flow through the Paths, and Ymir sits where they all meet. ymir is where all your
sessions meet. It only ever reads.

## Speed

On my machine, an Apple M4 Pro (12 cores), over 126 session files and 545 MB, the largest
170 MB:

| Job | ymir |
|---|---|
| Read and index every session | **0.06 s** (0.27 s from a cold disk cache) |
| First screen | 15–45 ms |
| Refilter as you type | 9–14 ms per key, over all 17,700 entries |
| Open the largest session | 46 ms |
| Memory, everything indexed | 38 MB |

ymir skips what isn't conversation before parsing it (the API requests Claude Code logs are
half the bytes), reads on every core, and keeps tool output out of the index until you open
a session. Time `ymir --filter ''` on your own sessions to compare.

## Install

```sh
cargo install ymir-code
```

Someone else has the name `ymir` on crates.io; the command is still `ymir`.

## Browse

Run `ymir`. Sessions are listed newest first, with their project and title. The middle
column lists your prompts in the selected session, one per turn, and the transcript on the
right follows the one you pick.

Tool calls show the first lines of their output; `C-e` shows all of it, in every session,
until you press it again. Edits show as diffs, with the words that changed marked. Prose
wraps; diffs keep their columns and scroll sideways.

`Enter` prints the selected session's id and quits, so ymir works as a picker. From the
session's project directory:

```sh
claude --resume $(ymir)
```

| Key | Action |
|---|---|
| type | search |
| `↑` `↓` | move |
| `Tab` | switch pane: sessions, turns, transcript |
| `PgUp` `PgDn` | scroll the transcript |
| `S-↑` `S-↓` | previous or next prompt in the transcript |
| `S-←` `S-→` | scroll diffs sideways |
| `C-e` | all tool output, or the first lines again |
| `Enter` | print the session id and quit |
| `C-y` | copy the session id |
| `Esc` | clear the search, or quit |

The mouse works too: click to select, scroll with the wheel, drag a border to resize the
panes. Drag over text to copy it: the transcript copies what you selected, the sessions
column copies sessions as `--filter` prints them. `ymir --help` lists every key. Set `NO_COLOR` for plain black and white.

## Search

The search matches a session's project path, title, date and git branch, and everything in
it: your prompts, Claude's replies, the tool calls (the commands, the file paths, the
patterns) and the subagents it ran. When it matches inside a session, the middle column
lists just the matching lines, and the transcript opens on them.

fzf syntax works: `'exact`, `^prefix`, `suffix$`, `!not`, smart case, and every word must
match within one entry: the header, a prompt, a reply or a tool call. In prose, a word's letters have to stay close
together, so `focus` finds the replies that say it rather than every paragraph with an f,
an o and a c in it; typos still match. Paths are fully fuzzy: `scbtn` finds
`src/components/Button.tsx`. Four or more characters of a session id find that session.

Tool output isn't searched yet; that comes with `/c`, a literal search over everything.

## For your agents

Agents can't work a TUI, so ymir tells them in plain text. `ymir --filter` runs the same
search and prints one line per session, best first. (The sessions here are made up: they're
the ones ymir's tests use, in `tests/fixtures`.)

```sh
$ ymir --filter 'north gate'
0b5e55ed-0000-4000-8000-000000000001 2026-10-20 /home/eren/paradis Fix the north gate
```

`ymir --show <id>` prints that session as a transcript: prompts, replies, tool calls with the
first lines of their output, and edits as diffs. `--turn N` prints one prompt and what
followed it, and the id can be any unique prefix:

```sh
$ ymir --show 0b5e55 --turn 1
session 0b5e55ed-0000-4000-8000-000000000001 "Fix the north gate"
in /home/eren/paradis, on main, 2026-10-20 09:00 to 09:32, 4 turns
turn 1 of 4

❯ you · 09:00
  The north gate sticks when it rains. Find out why.
● claude
  I'll look at the gate first.
  ▸ Read src/gate.rs
    1    pub fn open() {
    2        pin(3);
    3    }
  ▸ Edit src/gate.rs
    src/gate.rs · +1 -1
    @@ -1,3 +1,3 @@
     pub fn open() {
    -    pin(3);
    +    pin(5);
     }
  ▸ Bash cargo test
    running 3 tests
    test gate::opens ... ok
    test gate::closes ... ok
    test gate::rain ... ok

    test result: ok. 3 passed
  Fixed: the hinge pin was too short, so the gate dropped when the wood swelled.
```

`--filter` exits 1 when nothing matches, `--show` when there's no such session, and both
exit 2 on errors, so a script can tell "nothing" from "broken". Brief your agent once, in
`CLAUDE.md` or `AGENTS.md`:

```markdown
To find a past Claude Code session, run `ymir --filter '<query>'`. The query is fuzzy over
each session's project, title, date and branch, and over its prompts, replies and tool calls
(commands, file paths). It prints `<session-id> <date> <project> <title>`, best match first.
Read one with `ymir --show <id>`, or `ymir --show <id> --turn N` for one prompt and what
followed.
```

## Quiet by design

- **Read-only.** ymir never writes to `~/.claude` or anywhere else. A test runs the binary
  over a copy of a sessions directory and checks that no file changed, appeared or
  disappeared, and another fails the build if code that writes files shows up.
- **Private.** Sessions hold secrets. ymir makes no network calls at all, and terminal
  escapes in old tool output are stripped before anything reaches your screen.
- **Forgiving.** Claude Code's session format isn't a public contract and changes between
  versions. ymir reads only the fields it needs, and skips what it doesn't know rather than
  failing: a new record type, a renamed field, a line still being written.
- **Small.** About 3,300 lines of Rust and a 1.8 MB binary.

Sessions are read from `~/.claude/projects`, or from `$CLAUDE_CONFIG_DIR/projects` when
that's set.

## License

[MIT](LICENSE).
