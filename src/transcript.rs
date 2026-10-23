//! One session in full, as lines to read: prompts, replies, tool calls with
//! the start of their output, edits as diffs, subagents indented under the
//! call that started them. The TUI wraps these lines; `--show` prints them.

use std::collections::HashMap;
use std::io;
use std::ops::Range;

use crate::index;
use crate::record::{Detail, Edit, Output, Piece, PromptKind};
use crate::store::{self, SessionFile};
use crate::time;
use crate::words;

/// How a line looks. The TUI gives each a style; plain text ignores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// `❯ you · 20:31`
    You,
    Prompt,
    /// `● claude`
    Claude,
    Reply,
    /// `▸ Bash cargo test`
    Tool,
    Output,
    Error,
    /// `… 120 more lines`
    More,
    /// A file an edit changed.
    File,
    Hunk,
    Added,
    Removed,
    Context,
    Notice,
    Divider,
    /// A subagent's prompt.
    Agent,
    Thinking,
}

impl Look {
    /// Lines of a diff, which keep their columns: they don't wrap, and
    /// scroll sideways instead.
    pub fn is_diff(self) -> bool {
        matches!(
            self,
            Look::Hunk | Look::Added | Look::Removed | Look::Context
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub look: Look,
    /// One line, no newline.
    pub text: String,
    pub indent: usize,
    pub turn: usize,
    /// The search entry this line starts, numbered as the index numbers them.
    pub entry: Option<usize>,
    /// Changed words of a diff line, as byte ranges of `text`.
    pub emph: Vec<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    pub id: String,
    pub items: Vec<Item>,
    /// Where each entry starts, by entry number.
    pub entries: Vec<usize>,
    pub turns: usize,
    /// What it was read with.
    pub limits: Limits,
}

/// How much of each output to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Lines of a tool's output.
    pub output: usize,
    /// Lines of one file's diff.
    pub diff: usize,
    /// Whether thinking, multi-line commands and files a call created are
    /// shown whole, or not at all, by their first line, and by their size.
    pub whole: bool,
}

impl Limits {
    pub const SCREEN: Limits = Limits {
        output: 6,
        diff: 400,
        whole: false,
    };
    /// The screen with `C-e`: everything.
    pub const FULL: Limits = Limits {
        output: usize::MAX,
        diff: usize::MAX,
        whole: true,
    };
    pub const PLAIN: Limits = Limits {
        output: 20,
        diff: 2000,
        whole: false,
    };
}

/// Lines grouped so a tool's output can join its call when calls run in
/// parallel (call A, call B, output A, output B).
struct Builder {
    limits: Limits,
    parts: Vec<Vec<Item>>,
    /// The part holding each call, by tool use id, for calls whose output
    /// goes with them (not subagents, whose output follows their transcript).
    calls: HashMap<String, usize>,
    entry: usize,
    turn: usize,
    /// Whether the last thing at the top level was Claude's, so a reply after
    /// a tool call doesn't repeat `● claude`.
    claude_speaking: bool,
    /// Subagent calls, whose output is the subagent's last reply.
    agents: std::collections::HashSet<String>,
    /// The last reply inside a subagent.
    agent_said: Option<String>,
}

impl Builder {
    fn push(&mut self, look: Look, text: &str, indent: usize, entry: Option<usize>) {
        let mut entry = entry;
        let text = crate::text::clean(text);
        let lines: Vec<&str> = if text.is_empty() {
            vec![""]
        } else {
            text.lines().collect()
        };
        let part = self.parts.last_mut().expect("a part");
        for line in lines {
            part.push(Item {
                look,
                text: line.trim_end().to_owned(),
                indent,
                turn: self.turn,
                entry: entry.take(),
                emph: Vec::new(),
            });
        }
    }

    fn new_part(&mut self) {
        if self.parts.last().is_none_or(|p| !p.is_empty()) {
            self.parts.push(Vec::new());
        }
    }

    fn next_entry(&mut self) -> usize {
        self.entry += 1;
        self.entry - 1
    }

    fn piece(&mut self, depth: usize, time: Option<i64>, cwd: Option<&str>, piece: Piece) {
        let base = depth * 2;
        match piece {
            Piece::Prompt { kind, text } => {
                let entry = self.next_entry();
                self.new_part();
                if depth == 0 {
                    self.turn += 1;
                    self.claude_speaking = false;
                    if self.parts.iter().any(|p| !p.is_empty()) {
                        self.push(Look::Prompt, "", 0, None);
                    }
                    let when = time.map(|t| format!(" · {}", time::local(t).clock()));
                    let head = format!("❯ you{}", when.unwrap_or_default());
                    self.push(Look::You, &head, 0, Some(entry));
                    let text = match kind {
                        PromptKind::Shell => format!("! {text}"),
                        _ => text,
                    };
                    self.push(Look::Prompt, &text, 2, None);
                } else {
                    self.push(Look::Agent, &format!("↳ {text}"), base, Some(entry));
                }
            }
            Piece::Reply(text) => {
                let entry = self.next_entry();
                if depth > 0 {
                    self.agent_said = Some(text.clone());
                }
                self.new_part();
                if depth == 0 && !self.claude_speaking {
                    self.push(Look::Claude, "● claude", 0, Some(entry));
                    self.push(Look::Reply, &text, 2, None);
                } else {
                    self.push(Look::Reply, &text, base + 2, Some(entry));
                }
                if depth == 0 {
                    self.claude_speaking = true;
                }
            }
            Piece::Tool(call) => {
                let entry = self.next_entry();
                self.new_part();
                if depth == 0 && !self.claude_speaking {
                    self.push(Look::Claude, "● claude", 0, None);
                    self.claude_speaking = true;
                }
                let summary = match cwd {
                    Some(cwd) if call.is_path => index::relative(&call.summary, cwd),
                    _ => &call.summary,
                };
                let mut lines = summary.lines();
                let first = lines.next().unwrap_or_default();
                let more = summary.contains('\n') && !self.limits.whole;
                let mut line = format!("▸ {} {first}{}", call.name, if more { " …" } else { "" });
                if let Some(agent) = &call.agent {
                    line.push_str(&format!(" ({agent})"));
                }
                self.push(Look::Tool, line.trim_end(), base + 2, Some(entry));
                if self.limits.whole {
                    // The rest of a multi-line command, under its first line.
                    for rest in lines {
                        self.push(Look::Tool, rest, base + 4, None);
                    }
                }
                if call.agent.is_some() || matches!(call.name.as_str(), "Agent" | "Task") {
                    self.agents.insert(call.id);
                } else {
                    self.calls.insert(call.id, self.parts.len() - 1);
                }
                self.new_part();
            }
            // A subagent's report is its last reply, already shown above.
            Piece::Result(out)
                if self.agents.contains(&out.id)
                    && self.agent_said.as_deref() == Some(out.text.trim()) => {}
            Piece::Result(out) => match self.calls.remove(&out.id) {
                // Into its call's part. Parts are only ever added at the end,
                // so the index is still good.
                Some(at) => {
                    let after = self.parts.split_off(at + 1);
                    self.output(base + 4, &out, cwd);
                    self.parts.extend(after);
                }
                // A subagent's report, or a call ymir never saw: where it is.
                None => {
                    self.new_part();
                    self.output(base + 4, &out, cwd);
                }
            },
            Piece::Thinking(text) if self.limits.whole => {
                self.new_part();
                if depth == 0 && !self.claude_speaking {
                    self.push(Look::Claude, "● claude", 0, None);
                    self.claude_speaking = true;
                }
                self.push(Look::Thinking, &format!("∴ {text}"), base + 2, None);
            }
            Piece::Thinking(_) => {}
            Piece::Notice(text) => {
                self.new_part();
                self.push(Look::Notice, &text, base + 2, None);
            }
            Piece::Compact { pre, post } => {
                self.new_part();
                let tokens = match (pre, post) {
                    (Some(pre), Some(post)) => {
                        format!(" {} → {} tokens", thousands(pre), thousands(post))
                    }
                    _ => String::new(),
                };
                self.push(Look::Divider, &format!("── compacted{tokens} ──"), 0, None);
            }
            Piece::Title(_) => {}
        }
    }

    /// A tool's output: its edits as diffs, or the start of its text.
    fn output(&mut self, indent: usize, out: &Output, cwd: Option<&str>) {
        if !out.edits.is_empty() && !out.error {
            for edit in &out.edits {
                self.edit(indent, edit, cwd);
            }
            return;
        }
        let look = if out.error { Look::Error } else { Look::Output };
        let text = out.text.trim_end_matches('\n');
        if text.trim().is_empty() {
            return;
        }
        let lines: Vec<&str> = text.lines().collect();
        let shown = lines.len().min(self.limits.output);
        for line in &lines[..shown] {
            self.push(look, line, indent, None);
        }
        if lines.len() > shown {
            let more = lines.len() - shown;
            self.push(
                Look::More,
                &format!("… {more} more {}", plural(more, "line")),
                indent,
                None,
            );
        }
    }

    fn edit(&mut self, indent: usize, edit: &Edit, cwd: Option<&str>) {
        let path = match cwd {
            Some(cwd) => index::relative(&edit.path, cwd),
            None => &edit.path,
        };
        let (added, removed) =
            edit.hunks
                .iter()
                .flat_map(|h| &h.lines)
                .fold((0, 0), |(a, r), l| match l.as_bytes().first() {
                    Some(b'+') => (a + 1, r),
                    Some(b'-') => (a, r + 1),
                    _ => (a, r),
                });
        let head = match edit.created {
            Some(lines) => format!("{path} · created, {lines} {}", plural(lines, "line")),
            None => format!("{path} · +{added} -{removed}"),
        };
        self.push(Look::File, &head, indent, None);
        if edit.created.is_some() && !self.limits.whole {
            // A new file's content is the whole file; its size says enough.
            return;
        }
        if let Some(content) = &edit.content {
            // As a diff would show it: every line added.
            for line in content.lines() {
                self.push(Look::Added, &format!("+{line}"), indent, None);
            }
            return;
        }
        let start = self.parts.last().map_or(0, Vec::len);
        let mut budget = self.limits.diff;
        let total: usize = edit.hunks.iter().map(|h| h.lines.len() + 1).sum();
        for hunk in &edit.hunks {
            if budget == 0 {
                break;
            }
            let head = format!(
                "@@ -{},{} +{},{} @@",
                hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
            );
            self.push(Look::Hunk, &head, indent, None);
            budget -= 1;
            for line in &hunk.lines {
                if budget == 0 {
                    break;
                }
                let look = match line.as_bytes().first() {
                    Some(b'+') => Look::Added,
                    Some(b'-') => Look::Removed,
                    _ => Look::Context,
                };
                // A line of a diff is one line, even if it holds a `\r`.
                self.push(look, &line.replace(['\n', '\r'], " "), indent, None);
                budget -= 1;
            }
        }
        if let Some(part) = self.parts.last_mut() {
            words::mark(&mut part[start..]);
        }
        let shown = self.limits.diff.min(total);
        if total > shown {
            let more = total - shown;
            self.push(
                Look::More,
                &format!("… {more} more {}", plural(more, "line")),
                indent,
                None,
            );
        }
    }
}

/// Reads a session in full.
pub fn load(file: &SessionFile, limits: Limits) -> io::Result<Transcript> {
    let mut b = Builder {
        limits,
        parts: vec![Vec::new()],
        calls: HashMap::new(),
        entry: 0,
        turn: 0,
        claude_speaking: false,
        agents: Default::default(),
        agent_said: None,
    };
    store::walk(file, Detail::Full, &mut |step| {
        b.piece(step.depth, step.time, step.cwd, step.piece);
    })?;
    let items: Vec<Item> = b.parts.into_iter().flatten().collect();
    let mut entries = vec![0; b.entry];
    for (i, item) in items.iter().enumerate() {
        if let Some(e) = item.entry {
            entries[e] = i;
        }
    }
    Ok(Transcript {
        id: file.id.clone(),
        items,
        entries,
        turns: b.turn,
        limits,
    })
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        word.into()
    } else {
        format!("{word}s")
    }
}

/// `180000` as `180k`, `1500` as `1.5k`.
fn thousands(n: u64) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..10_000 if !n.is_multiple_of(1000) => format!("{:.1}k", n as f64 / 1000.0),
        _ => format!("{}k", (n + 500) / 1000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Session;
    use crate::testutil::{self, Claude};
    use serde_json::json;

    fn text(t: &Transcript) -> Vec<String> {
        t.items
            .iter()
            .map(|i| format!("{}{}", " ".repeat(i.indent), i.text))
            .collect()
    }

    fn hunk() -> serde_json::Value {
        json!({"oldStart": 3, "oldLines": 1, "newStart": 3, "newLines": 2, "lines": [" keep", "-old", "+new", "+more"]})
    }

    #[test]
    fn a_session_reads_like_a_conversation() {
        let claude = Claude::new();
        let file = claude.session(
            "-w",
            "aaaa0001",
            &[
                claude.prompt("Fix the gate", "2026-01-01T10:00:00Z"),
                claude.reply("Looking."),
                claude.tool("t1", "Bash", json!({"command": "ls\nwc -l"})),
                claude.tool("t2", "Edit", json!({"file_path": "/w/paradis/src/gate.rs"})),
                testutil::result("t1", "a\nb\nc\nd\ne\nf\ng\nh", json!({"stdout": "…"})),
                testutil::result(
                    "t2",
                    "updated",
                    json!({"filePath": "/w/paradis/src/gate.rs", "structuredPatch": [hunk()]}),
                ),
                claude.reply("Done."),
                testutil::compaction(180_000, 12_000),
                claude.prompt(
                    "<bash-input>cargo test</bash-input>",
                    "2026-01-01T10:05:00Z",
                ),
            ],
        );
        let t = load(&file, Limits::SCREEN).unwrap();
        let clock = |s| time::local(time::parse(s).unwrap()).clock();
        assert_eq!(
            text(&t),
            [
                format!("❯ you · {}", clock("2026-01-01T10:00:00Z")),
                "  Fix the gate".into(),
                "● claude".into(),
                "  Looking.".into(),
                "  ▸ Bash ls …".into(),
                "    a".into(),
                "    b".into(),
                "    c".into(),
                "    d".into(),
                "    e".into(),
                "    f".into(),
                "    … 2 more lines".into(),
                "  ▸ Edit src/gate.rs".into(),
                "    src/gate.rs · +2 -1".into(),
                "    @@ -3,1 +3,2 @@".into(),
                "     keep".into(),
                "    -old".into(),
                "    +new".into(),
                "    +more".into(),
                "  Done.".into(),
                "── compacted 180k → 12k tokens ──".into(),
                "".into(),
                format!("❯ you · {}", clock("2026-01-01T10:05:00Z")),
                "  ! cargo test".into(),
            ]
        );
        assert_eq!(t.turns, 2);
    }

    #[test]
    fn entries_are_numbered_as_the_index_numbers_them() {
        let claude = Claude::new();
        let file = claude.session(
            "-w",
            "aaaa0002",
            &[
                claude.prompt("go", "2026-01-01T10:00:00Z"),
                claude.tool("t1", "Read", json!({"file_path": "/w/paradis/a"})),
                claude.tool(
                    "t2",
                    "Agent",
                    json!({"description": "Scout", "subagent_type": "Explore"}),
                ),
                testutil::result("t1", "contents", json!({})),
                testutil::result("t2", "Found trees.", json!({})),
                claude.reply("The scout found trees."),
            ],
        );
        claude.subagent(
            "-w/aaaa0002",
            "s",
            "t2",
            &[
                claude.prompt("look around", "2026-01-01T10:00:01Z"),
                claude.reply("Trees."),
            ],
        );
        let t = load(&file, Limits::SCREEN).unwrap();
        let s = Session::load(file).unwrap();
        assert_eq!(t.entries.len(), s.entries.len());
        for (n, entry) in s.entries.iter().enumerate() {
            let item = &t.items[t.entries[n]];
            assert!(
                item.text.contains(entry.line()) || matches!(item.look, Look::You | Look::Claude),
                "entry {n}: {:?} at {:?}",
                entry.line(),
                item.text
            );
        }
        assert_eq!(
            text(&t)[3..],
            [
                "  ▸ Read a",
                "    contents",
                "  ▸ Agent Scout (Explore)",
                "  ↳ look around",
                "    Trees.",
                // Not the subagent's last reply, so it's shown.
                "    Found trees.",
                "  The scout found trees.",
            ]
        );
    }

    #[test]
    fn created_files_and_errors() {
        let claude = Claude::new();
        let file = claude.session(
            "-w",
            "aaaa0003",
            &[
                claude.prompt("go", "2026-01-01T10:00:00Z"),
                claude.tool("t1", "Write", json!({"file_path": "/w/paradis/new.rs"})),
                testutil::result("t1", "created", json!({"type": "create", "filePath": "/w/paradis/new.rs", "content": "a\nb\nc", "structuredPatch": []})),
                claude.tool("t2", "Bash", json!({"command": "false"})),
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"Exit code 1","is_error":true}]}}"#.into(),
            ],
        );
        let t = load(&file, Limits::SCREEN).unwrap();
        let tail: Vec<_> = t.items[3..]
            .iter()
            .map(|i| (i.look, i.text.as_str()))
            .collect();
        assert_eq!(
            tail,
            [
                (Look::Tool, "▸ Write new.rs"),
                (Look::File, "new.rs · created, 3 lines"),
                (Look::Tool, "▸ Bash false"),
                (Look::Error, "Exit code 1"),
            ]
        );
        // With everything shown, the new file is all there.
        let t = load(&file, Limits::FULL).unwrap();
        let file: Vec<_> = t.items[4..8].iter().map(|i| i.text.as_str()).collect();
        assert_eq!(file, ["new.rs · created, 3 lines", "+a", "+b", "+c"]);
        assert_eq!(t.items[5].look, Look::Added);
    }

    #[test]
    fn full_limits_show_thinking_and_whole_commands() {
        let claude = Claude::new();
        let file = claude.session(
            "-w",
            "aaaa0006",
            &[
                claude.prompt("go", "2026-01-01T10:00:00Z"),
                testutil::thinking("The pin is short."),
                claude.tool("t1", "Bash", json!({"command": "cd gate\ncargo test"})),
            ],
        );
        let lines = |limits| text(&load(&file, limits).unwrap())[2..].to_vec();
        assert_eq!(lines(Limits::SCREEN), ["● claude", "  ▸ Bash cd gate …"]);
        assert_eq!(
            lines(Limits::FULL),
            [
                "● claude",
                "  ∴ The pin is short.",
                "  ▸ Bash cd gate",
                "    cargo test"
            ]
        );
    }

    #[test]
    fn full_limits_show_all_output() {
        let claude = Claude::new();
        let long: Vec<String> = (0..100).map(|n| format!("line {n}")).collect();
        let file = claude.session(
            "-w",
            "aaaa0005",
            &[
                claude.tool("t1", "Bash", json!({"command": "seq"})),
                testutil::result("t1", &long.join("\n"), json!({})),
            ],
        );
        let output = |limits| {
            let t = load(&file, limits).unwrap();
            t.items.iter().filter(|i| i.look == Look::Output).count()
        };
        assert_eq!(output(Limits::SCREEN), 6);
        assert_eq!(output(Limits::FULL), 100);
    }

    #[test]
    fn long_diffs_are_cut() {
        let claude = Claude::new();
        let lines: Vec<String> = (0..50).map(|n| format!("+line {n}")).collect();
        let file = claude.session(
            "-w",
            "aaaa0004",
            &[
                claude.tool("t1", "Edit", json!({"file_path": "/w/paradis/a"})),
                testutil::result("t1", "ok", json!({"filePath": "/w/paradis/a", "structuredPatch": [
                    {"oldStart": 1, "oldLines": 0, "newStart": 1, "newLines": 50, "lines": lines}]})),
            ],
        );
        let limits = Limits {
            diff: 10,
            ..Limits::SCREEN
        };
        let t = load(&file, limits).unwrap();
        let last = t.items.last().unwrap();
        assert_eq!(last.text, "… 41 more lines");
        assert_eq!(t.items.iter().filter(|i| i.look == Look::Added).count(), 9);
    }

    #[test]
    fn thousands_reads_well() {
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1500), "1.5k");
        assert_eq!(thousands(2000), "2k");
        assert_eq!(thousands(180_000), "180k");
        assert_eq!(thousands(12_499), "12k");
    }
}

#[cfg(test)]
mod real {
    use super::*;
    use crate::index::Session;
    use std::time::Instant;

    /// Opens every real session: entry numbers must agree with the index.
    /// Prints sizes and timings, never content:
    /// `cargo test --release -- --ignored --nocapture real_transcripts`.
    #[test]
    #[ignore]
    fn real_transcripts() {
        let projects = store::config_dir().unwrap().join("projects");
        let mut files = store::discover(&projects).unwrap();
        files.sort_by_key(|f| f.size);
        let mut times = Vec::new();
        let (mut lines, mut diff, mut marked, mut widest) = (0, 0, 0, 0);
        for file in &files {
            let s = Session::load(file.clone()).unwrap();
            let start = Instant::now();
            let t = load(file, Limits::SCREEN).unwrap();
            times.push((start.elapsed(), file.size));
            lines += t.items.len();
            for item in t.items.iter().filter(|i| i.look.is_diff()) {
                diff += 1;
                marked += usize::from(!item.emph.is_empty());
                widest = widest.max(crate::text::columns(&item.text));
            }
            assert_eq!(t.entries.len(), s.entries.len(), "{}", file.id);
            assert_eq!(t.turns, s.turns, "{}", file.id);
        }
        let median = times[times.len() / 2];
        let largest = times.last().unwrap();
        let slowest = times.iter().max().unwrap();
        println!(
            "{} sessions, {lines} lines; median {:?} ({:.1} MB), largest {:?} ({:.1} MB), slowest {:?}",
            times.len(),
            median.0,
            median.1 as f64 / 1e6,
            largest.0,
            largest.1 as f64 / 1e6,
            slowest.0
        );
        println!("{diff} diff lines, {marked} with changed words; widest {widest} columns");
        // With C-e: everything, for the largest and the slowest sessions.
        let slow = times.iter().position(|t| t == slowest).unwrap();
        for file in [&files[files.len() - 1], &files[slow]] {
            let start = Instant::now();
            let t = load(file, Limits::FULL).unwrap();
            let bytes: usize = t.items.iter().map(|i| i.text.len()).sum();
            println!(
                "all output, {:.1} MB session: {:?}, {} lines, {:.1} MB of text",
                file.size as f64 / 1e6,
                start.elapsed(),
                t.items.len(),
                bytes as f64 / 1e6
            );
        }
    }
}
