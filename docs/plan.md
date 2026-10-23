# ymir — plan

> Where all your sessions meet. One task: find a past Claude Code session, read it, and see what
> it changed — with fzf-style search on top.

ymir is levi's sibling: same stack, same habits (search bar always focused, Enter prints and
quits, `--filter` for agents, never writes where it reads). levi reads a repository's history;
ymir reads `~/.claude/projects`.

## Layout

```
┌─ / fuzzy query ─────────────────────────────────────────────────────────────┐
├─ sessions ───────────────┬─ turns ──────────────┬─ transcript ──────────────┤
│ 10-22 levi  Add --focus  │ 1 20:31 Add a --foc… │ ❯ you · 20:31             │
│ 10-21 ymir  Plan ymir    │ 2 20:44 now the tm…  │   Add a --focus flag that │
│ 10-20 site  Fix the nav  │ 3 21:02 commit it    │ ● claude                  │
│ ...                      │                      │   ▸ Bash cargo test       │
│                          │                      │   ▸ Edit src/main.rs +12 -3│
│                          │                      │     @@ -10,6 +10,8 @@     │
└──────────────────────────┴──────────────────────┴───────────────────────────┘
 ↑↓ move  enter pick  /c search text  /h here  tab pane  esc clear/quit
```

Same shape as levi on purpose: sessions are the commits, turns are the files, the transcript is
the diff. Widths ~25% / 20% / 55%, stacking below 120 columns, seams draggable, key hints on the
bottom row: all as levi does it, and for levi's reasons.

- **Sessions** row: last activity (`MM-DD`, or `HH:MM` today), project, title. Newest first.
  The title is the session's latest `ai-title` record, or its first prompt's first line when
  it has none (11 of 70 sessions here). A session with linked commits shows their count.
- **Turns** row: one per prompt you typed: number, time, the prompt's first line. Under a
  search it lists the matching entries instead (a prompt, a reply, a tool call), each under
  the number of its turn, like levi's files column narrows to matching files.
- **Transcript**: the selected session, scrolled to the selected turn or entry. Prompts,
  replies, one line per tool call, the first lines of each result, and file edits as diffs.

## Decisions

| # | Topic | Decision |
|---|---|---|
| 1 | Scope | **Every session on the machine** by default, newest first. `/h` ("here") narrows to sessions whose working directory is inside the current directory (or its git repository). No positional argument: a project is one fuzzy word away. |
| 2 | Source of truth | **The JSONL files, read in place.** No database, no cache on disk (see Rejected). |
| 3 | Order in a session | **File order**, not the `parentUuid` tree. The file is append-only and already in time order; the 1,045 "forks" in my data are all parallel tool calls (an assistant block with both a next block and a tool result as children), not rewinds. |
| 4 | What a session is | One `<session-id>.jsonl` plus its `subagents/agent-*.jsonl`. Subagent transcripts belong to the session that spawned them: searchable with it, shown under their `Agent` call. |
| 5 | What's searched fuzzily | Prompts, replies, tool calls (name + the one-line summary: command, path, pattern, URL), files touched, project path, date, title. **Not** tool results, thinking or injected context: 153 MB of my 545 MB is user records, almost all tool output; fuzzy over that matches everything. `/c` searches it literally. |
| 6 | Git | Shell out to `git`, as levi does, read-only commands only (`--no-optional-locks`). Only for commit links; ymir works without git. |
| 7 | Parsing | `serde_json` into small structs that name only the fields ymir uses; everything else is skipped. Unknown record types, unknown block types, bad lines: skipped and counted, never fatal. |
| 8 | Edits | Diffs come from what Claude Code already recorded (`structuredPatch`, `bashEditDiff`), never from re-reading files on disk: the files have moved on. |
| 9 | Picking | Enter prints the session id and quits, so `claude --resume $(ymir)` works (from the session's project directory). Exit 130 without a pick, like levi and fzf. |

### Rejected

- **A persistent index** (SQLite, tantivy, or a cache file in the temp dir). Indexing all 545 MB
  takes well under a second on every core (see Performance), so a cache buys little, and it
  would put a second copy of every secret in my sessions somewhere else on disk.
- **Rebuilding the `parentUuid` tree.** See decision 3. Rewound branches (Esc Esc) do exist in
  the format; showing them dimmed is in Later.
- **Decoding the project directory name.** `-Users-rodrigo-Documents-FunProjects-ymir` replaces
  both `/` and `.` with `-`, so it can't be reversed. The records carry `cwd`; ymir uses that.
- **A file watcher crate** (`notify`). levi rejected it too. Polling `stat` on ~130 files every
  2 s costs about a millisecond.
- **Fuzzy over tool results.** See decision 5.
- **Showing thinking blocks.** Noise for reading back; in Later as a toggle.

## Data format findings

Measured on this machine, 2026-10-22, Claude Code's current format. Structure only: counts,
keys and types, never content. The format isn't a public contract; everything below is what
ymir expects today and must survive changing.

**Files.** `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`, one per session (70), and
`<session-id>/subagents/agent-<hex>.jsonl` (56), each with an `agent-<hex>.meta.json`
(`agentType`, `description`, `toolUseId`, `spawnDepth`). Also `<session-id>/tool-results/*.txt`
(outputs too large to inline, referenced by `persistedOutputPath`) and `memory/*.md` (not
sessions; ignored). Every file's `sessionId` matches its name; no file holds two sessions. 12
project directories, but 144 distinct `cwd` values: a session's `cwd` follows `cd` (one session
has 41), so the first record's `cwd` names the project and the rest count as "where it worked".
`CLAUDE_CONFIG_DIR`, when set, replaces `~/.claude`; ymir honours it, and tests use it.

**Lines.** 102,852 lines, all valid JSON, every file ending in a newline. The longest line is
2.0 MB (inline images). Top-level `type`:

| Type | Lines | MB | ymir |
|---|---|---|---|
| `assistant` | 25,013 | 84 | read: replies, tool calls |
| `user` | 13,993 | 153 | read: prompts, tool results |
| `system` | 2,155 | 1 | read `compact_boundary`; skip `turn_duration`, `away_summary`, `informational`, `local_command` |
| `ai-title` | 5,528 | 0.6 | read: latest one wins (re-appended as the session goes) |
| `attachment` | 20,463 | 46 | skip (reminders, environment, listings) |
| `api-request`, `api-request-blob`, `api-request-shape` | 11,688 | 266 | skip |
| `last-prompt`, `mode`, `permission-mode`, `atis-latch`, `file-history-*`, `queue-operation`, `cost-state`, `frame-link`, `artifact-*` | 24,082 | 11 | skip |

Half the bytes are `api-request*` records, and they're the only lines that start with
`{"type":"api-request`; conversation records start with `{"parentUuid"`. So the loader skips a
line by its first bytes before parsing it. That's an optimisation only: if the order of keys
changes, those lines get parsed and then skipped by type, same result, slower.

**Conversation records** (`user`, `assistant`, `system`) always carry `uuid`, `parentUuid`,
`sessionId`, `timestamp` (all 67,549 in the shape `2026-10-22T20:31:05.123Z`, UTC), `cwd`,
`gitBranch`, `isSidechain`, `version`. Many more fields come and go between versions
(`effort`, `slug`, `requestRef`, `wireToolInputs`, …: 70+ distinct key sets for `assistant`
alone); ymir names none of them.

**Assistant records** hold exactly one content block each (`text`, `thinking` or `tool_use`).
One API response is split over several records sharing `message.id` (up to 10 here); ymir
doesn't need to regroup them. `tool_use` has `id`, `name`, `input`. Tool names in my data:
Bash (9,868), MCP tools (658), Write, Read, Edit, ToolSearch, WebFetch, Agent, WebSearch,
Skill, AskUserQuestion and a few others. Summaries per tool: Bash `command`, Read/Edit/Write
`file_path`, Grep/Glob `pattern`, WebFetch `url`, WebSearch `query`, Agent `description`;
anything else, the tool name and its first string input.

**User records** are of four kinds:

- *Prompts*: `message.content` is a string (2,169) or an array of `text` and `image` blocks
  (91). Some strings are wrappers rather than typed text: `<bash-input>`/`<bash-stdout>`
  (`!` commands, 371 each), `<command-name>` (slash commands), `<local-command-stdout>`,
  `<task-notification>` (background agents finishing), `<pasted_content>`. ymir shows each
  for what it is: `! cmd`, `/command`, a dim notice, pasted text marked as pasted. 1,170 are
  plain prompts.
- *Tool results* (11,733, one per `tool_use`): a `tool_result` block with `tool_use_id`,
  `content` (string, or an array of `text` blocks) and `is_error` (229 errors). Linked to the
  call by `tool_use_id == tool_use.id`. Beside it, a top-level `toolUseResult` with structured
  data, which differs per tool and is sometimes a bare string or array:
  - Bash: `stdout`, `stderr`, `interrupted`, and sometimes `bashEditDiff` (files a shell
    command changed, with hunks: 2,000 of 9,868 calls), `persistedOutputPath`, `gitOperation`.
  - Edit: `filePath`, `oldString`, `newString`, `structuredPatch`.
  - Write: `type` (`create`/`update`), `filePath`, `content`, `structuredPatch` (empty for a
    create).
  - A hunk is `{oldStart, oldLines, newStart, newLines, lines}`, lines prefixed `+`, `-` or
    space: a unified diff, ready to draw.
- *Meta* (`isMeta: true`, 210): injected skill bodies and the like. Skipped.
- *Compaction summaries* (`isCompactSummary: true`, 29), right after a `system` record with
  `subtype: compact_boundary`, `parentUuid: null`, `logicalParentUuid` and `compactMetadata`
  (`trigger`, `preTokens`, `postTokens`). The records from before the compaction stay in the
  file. ymir draws a divider (`── compacted 180k → 12k tokens ──`) and doesn't search the
  summary, which repeats the conversation.

**Sidechains.** `isSidechain: true` appears only in `subagents/` files (9,716 lines), never in
a main session file. The main file holds the `Agent` call and its result (`agentId`, `status`);
the subagent file holds the conversation, its first record being the prompt. Linked by the
meta file's `toolUseId`.

**Commits.** This is where assuming would have gone wrong. 656 Bash calls ran `git commit`,
and only **one** result contains git's `[branch sha] subject` line: 640 used `git commit -q`,
chained with `git log` or `git push`. The structured `gitOperation` field reported one commit
(and 66 pushes). So the session alone can't name its commits; linking has to ask git (see Git
links).

**Live sessions.** Files are append-only. Watched while this session ran: the first 985,702
bytes were identical (same SHA-1) after it grew to 1,034,865, and the new lines included a
second `ai-title`. A writer may be mid-line when ymir reads, so ymir reads up to the last
newline and keeps the offset; the rest waits for the next look. A file that shrinks or changes
inode is read again from the start.

## Search semantics

The search bar works like levi's: nucleo-matcher, fzf syntax (`'exact`, `^prefix`, `suffix$`,
`!not`), smart case, best match first, ties newest first, matched characters highlighted.

- A session stays if the query matches its **header** (project path, title, date, git branch),
  **or any entry** in it (a prompt, a reply, a tool call summary, a file it touched), **or its
  id** or a **commit sha** linked to it (prefix only, the whole query, ≥4 hex for session ids,
  ≥7 for commits: fuzzy hex matches everything, as levi found).
- The turns column shows only the matching entries; if the session matched on its header,
  all its turns.
- Every atom must match within one haystack (the header, or a single entry), as in levi:
  `parser !test` finds an entry about the parser that doesn't mention tests.
- Entries are long (a reply can be pages), and nucleo scores whole haystacks. An entry's
  haystack is its first 2,000 characters; the rest is `/c`'s job. To be tuned with real data.
- Paths use nucleo's path config; text the default one.
- Dates are `YYYY-MM-DD` in local time, so `2026-10` narrows to a month.

### Commands

Whole words in the search bar, as in levi (`src/query.rs`):

- **`/c text`** — literal text search: everything after `/c`, spaces included, smart case,
  over everything in a session: prompts, replies, tool inputs and **results**, thinking, and
  subagents. The fuzzy part before it picks which sessions to search. Runs on every core over
  the files, streams hits, starts 300 ms after typing pauses, is cancelled when the text
  changes. The transcript jumps to the first hit; `S-↑`/`S-↓` step through hits.
- **`/h`** — here: only sessions that worked inside the current directory (or its repository).
- **`/g`** — only sessions with linked commits.

## Git links

Which commits a session produced, so either can lead to the other.

- **Repositories.** For each distinct `cwd` of a session, `git -C <cwd> rev-parse
  --show-toplevel`, cached per directory (one call per distinct `cwd`: 144 here). A directory
  that's gone, or isn't in a repository, has none.
- **Candidates.** Per repository, one `git --no-optional-locks log --all --no-walk=unsorted`
  over the time span of all the sessions that worked there (plus 2 minutes), with
  `%H %ct %s` and the files each commit touched. Matching happens in memory.
- **A commit belongs to a session** when it's in a repository the session worked in, and
  either:
  1. its sha (≥7 hex) appears in the session's tool calls or results, inside the session's
     time span; or
  2. the session ran a Bash call containing `git … commit` whose result is timestamped within
     60 s after the commit's committer time (an amend or a hook can shift it a little).

  Two sessions committing in parallel to one repository is common with agents, which is why
  time alone isn't enough: rule 2 ties a commit to the session that ran the command. If two
  sessions still claim one commit, both keep it, and the transcript says so.
- **Shown** in the session's header in the transcript (`3 commits: d3a93ab Stack the lists …`)
  and as a marker in the turn where it happened (`● commit d3a93ab Stack the lists`).
- **Commit → session**: a sha prefix in the search bar or in `--filter`. With levi:
  `ymir --filter d3a93ab` from a sha copied in levi.
- To be measured before it's built: how many of the 656 commit calls each rule links, and how
  many of my commits with a `Co-Authored-By: Claude` trailer end up with no session.
- Rejected: parsing `[branch sha]` from output (1 of 656), the `gitOperation` field (1 of 656),
  the `Co-Authored-By: Claude` trailer (doesn't say which session, and claude.ai commits have
  it too), and time windows alone (parallel sessions).

## Transcript

- `❯ you · 20:31` and the prompt, in full. `● claude` and the reply text, in full, with code
  fences kept as they are (no Markdown rendering).
- Tool calls: `▸ Bash cargo test --locked`, then the result's first 6 lines, dimmed, and
  `… 120 more lines`. Errors in red. `C-e` expands every result in the session and back.
- Edits: Edit, Write (`update`) and shell edits (`bashEditDiff`) draw their hunks the way
  levi draws a diff: `+`/`-` bars, green and red, changed words marked (levi's `words.rs`).
  A Write that creates a file shows its line count, not its content (`C-e` shows it).
- `Agent` calls show their description and agent type; the subagent's conversation follows,
  indented one level, collapsed to its prompt, its tool calls and its last reply.
- Compaction: a divider. Images: `[image]`.
- Lines wrap (prose, unlike code, is unreadable scrolled sideways); diffs don't, and scroll
  sideways as in levi.
- A session loads in full on a worker when it's selected (the index keeps only what search
  needs) and stays in a small LRU. Stale loads are dropped, as levi drops stale diffs.

## Keys

| Key | Action |
|---|---|
| typing | goes into the search bar (always focused) |
| `↑/↓`, `C-p/C-n` | move in the focused column |
| `Tab` / `S-Tab`, `←/→` | cycle focus: sessions → turns → transcript |
| `PgUp/PgDn`, `C-u/C-d` | scroll the transcript |
| `S-↑/S-↓` | previous/next turn in the transcript; with `/c`, previous/next hit |
| `S-←/S-→` | scroll a diff sideways |
| `Enter` | print the session id and quit |
| `C-y` | copy the session id |
| `C-e` | expand or collapse tool results |
| `C-w` | delete the query's last word |
| `Esc` | clear the query, or quit |
| `C-c` | quit |
| mouse | as levi: click, wheel, drag to copy, drag a seam to resize |

## CLI behaviour

- `ymir`: the TUI. Draws on stderr, so stdout carries only the pick.
- `ymir --filter '<query>'` (`-f`): the same search, headless; `/c`, `/h`, `/g` included. One
  line per session, best first: `<session-id> <YYYY-MM-DD> <project> <title>`. Exit 0 with
  matches, 1 without, 2 on errors.
- `ymir --show <id> [--turn N]`: the transcript as plain text, for agents: prompts, replies,
  tool calls with results cut to 20 lines, edits as unified diffs, linked commits on top. `<id>`
  is a session id or a unique prefix of one. Exit 0, 1 for no such session, 2 for an ambiguous
  prefix or bad arguments.
- `ymir --pick-context`: Enter prints a reference instead of the id:
  `session 50a345b8 ("Plan ymir") in ~/Documents/FunProjects/ymir, turn 3 of 7`.
- `ymir --focus`: prints that reference for the ymir open now, so "look at the session I have
  open in ymir" works: the agent runs `ymir --focus`, then `ymir --show`. The note is
  `$TMPDIR/ymir-focus-<uid>` (one per user: ymir isn't tied to a repository the way levi is),
  mode 0600, written to a temp name and renamed, removed on exit, pid on its first line and
  ignored when that process is gone. Exit 0 with a reference, 1 with no ymir open.
- `-h/--help`, `-V/-v/--version`. Unknown flags and extra arguments exit 2.
- Preflight, one line on stderr, exit 1: `no sessions in ~/.claude/projects` (or the
  `CLAUDE_CONFIG_DIR` one); `needs a terminal` for the TUI without one.
- `NO_COLOR` as in levi.

## Read-only, always

ymir reads `~/.claude` and runs read-only git commands; the only file it writes is the
`--focus` note in the temp directory.

- All session reading goes through one function that opens files read-only.
- Git runs with `--no-optional-locks` and only `rev-parse`, `log`, `cat-file`: none of them
  writes the index or refs.
- **A test holds it to that.** It builds a synthetic `CLAUDE_CONFIG_DIR` and a git repository
  in a temp dir, records every file's path, size, mtime and content hash (`.git/index`
  included), runs the binary through `--filter` (with `/c` and a sha), `--show`, `--focus`, and
  the TUI headless, then checks that nothing changed, appeared or disappeared.
- A second test reads `src/` and fails if `File::create`, `OpenOptions`, `fs::write`,
  `fs::remove` or `fs::rename` appear outside `focus.rs`, so a new write has to be argued for.

## Privacy

Sessions hold secrets, tokens and customer data. ymir makes no network calls: no telemetry,
no update check, nothing. Its dependencies don't do networking either (`cargo deny` bans can
list a few crates to keep it so). Test fixtures are small, synthetic, written by hand or built
in code, never copied from real sessions. The README demo uses a made-up `CLAUDE_CONFIG_DIR`.

## Performance

Measured on this machine (Apple M4 Pro, 12 cores), 2026-10-22, `~/.claude/projects`:

| | |
|---|---|
| session files | 70 main + 56 subagent = 126 `.jsonl` |
| bytes | 545 MB (median file 0.7 MB, p90 4.7 MB, largest 170 MB) |
| lines | 102,852 |
| read every file, warm cache | 0.04 s (`cat`), 0.15 s cold |
| `stat` every file | 0.01 s |
| parse every line with `jq` | 3.7 s, one core |

Targets, to be measured like for like and kept in this file:

- **First screen < 100 ms.** Startup lists the files with `stat` and shows sessions newest
  first at once, with the project from the directory and a placeholder title; titles and
  entries stream in as files are indexed.
- **Full index < 0.5 s** on all cores, newest files first, skipping `api-request*` lines
  before parsing them (half the bytes). One worker per core takes whole files; the 170 MB
  file is split at line boundaries so it doesn't serialise the load.
- **Refilter < 15 ms** per keystroke, as levi (rescan only previous hits when the query
  narrows, every core).
- **Open a session < 100 ms** for the median, < 1 s for the largest.
- **`/c` over everything < 1 s.**
- **Memory**: the index holds prompts, replies and tool summaries, not results. Target
  < 150 MB resident on this corpus; to be measured.

## Crates

Start from levi's: `ratatui`, `nucleo-matcher`, `anyhow`, edition 2024; `libc` on Unix (pid
check for `--focus`, `localtime_r` for local dates); `tempfile` for tests. One addition:

- **`serde` + `serde_json`.** The data is JSON; a hand-written parser for a format that
  changes under us is where bugs would live. `serde_json` is the standard, has no unsafe
  surprises, and with `#[derive(Deserialize)]` on structs naming only the fields ymir needs,
  everything else is skipped without allocating.

Considered and not added: `chrono`/`jiff` (timestamps have one fixed UTC shape; local time
comes from `localtime_r`, UTC on Windows), `simd-json` (faster, but the target is met
without it, and it mutates its input), `notify` (see Rejected), `syntect` (later, as in levi:
it came in at 0.15).

## Code layout

```
src/
  main.rs       – args, preflight, terminal setup/restore, panic hook, event loop
  store.rs      – find session files, stat them, follow growing files by offset
  record.rs     – serde structs for the records ymir reads; line → Record or skip
  index.rs      – records → sessions and entries (haystacks, turns, files touched)
  transcript.rs – one session in full: entries, results, hunks, subagents
  filter.rs     – nucleo matching over sessions and entries
  query.rs      – slash commands
  grep.rs       – /c: literal search over files on every core
  git.rs        – repository roots, candidate commits, linking
  worker.rs     – index / session / grep / git threads, one Msg channel
  app.rs        – state, thread-free, as levi's
  layout.rs     – panes and seams
  ui.rs         – drawing
  words.rs      – changed-word marks (from levi)
  focus.rs      – the --focus note, the only writer
  plain.rs      – --show and --filter output
  testutil.rs   – builds synthetic sessions and repositories
tests/
  cli.rs        – runs the binary: flags, exit codes, output, read-only check
  fixtures/     – hand-written synthetic .jsonl: every record kind, unknown types,
                  a cut last line, a compaction, a subagent
```

## Build order

Each step ends in a release with a CHANGELOG entry; commit messages carry the numbers.

1. **0.1.0** — `record.rs`, `index.rs`, `store.rs` with fixture tests (unknown types skipped,
   cut last line, wrappers, compaction). Then the three panes, fuzzy search, Enter prints the
   id, `--filter`, `--show`, the read-only test, CI (fmt, clippy `-D warnings`, tests on Linux
   and macOS, MSRV, `cargo deny`), `deny.toml`, README, CHANGELOG. Measure first screen, full
   index, refilter, memory.
2. **0.2.0** — diffs of edits in the transcript, changed words, `C-e`.
3. **0.3.0** — `/c` literal search with hit jumps; `/h`.
4. **0.4.0** — follow live sessions (stat poll, offsets), keeping the selection.
5. **0.5.0** — git links, measured first against my 656 commit calls; `/g`, sha search.
6. **0.6.0** — `--pick-context`, `--focus`.
7. Release workflow (binaries, binstall), demo GIF over made-up sessions.

## Name

The crate is **`ymir-sessions`** (`ymir` is taken on crates.io); the binary is `ymir`, as
`levi-git` ships `levi`. Free as of 2026-10-22, as are `ymir-paths`, `ymir-cc`, `ymir-tui`.

- Rejected: `ymir-paths` (thematic, but on crates.io it reads like a filesystem-path crate),
  `ymir-cc` (cryptic), `ymir-tui` (says how, not what).

## Later (not now)

- Rewound branches (Esc Esc) shown dimmed beside the path that was kept.
- Thinking blocks, as a toggle.
- Syntax colors in diffs (levi's `syntax.rs`, with `syntect`).
- Open the edited file at the edited line with `C-o`, as levi does.
- Jump from a linked commit into levi (`levi` at that sha), and from levi to ymir.
- herdr and tmux plugins, as levi's, typing a `--pick-context` reference into the agent.
- Token and cost totals per session (`cost-state`, `usage`): data's there, not the job.
- Other agents' session formats (Codex, Gemini CLI).
