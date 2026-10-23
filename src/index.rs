//! What search needs from each session: a header (project, title, date,
//! branch) and its entries (prompts, replies, tool calls), without tool
//! results. The transcript reads the rest when a session is opened.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use crate::record::{Detail, Piece, PromptKind};
use crate::store::{self, SessionFile};
use crate::text::clean;
use crate::time;

/// How much of an entry search looks at. Replies can run for pages, and
/// fuzzy matching over all of it would match nearly anything; `/c` will
/// search the rest.
pub const HAYSTACK_CHARS: usize = 2000;

/// The longest title made from a first prompt.
const TITLE_CHARS: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Prompt(PromptKind),
    Reply,
    Tool { is_path: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub kind: Kind,
    /// The turn it belongs to: 0 before the first prompt, then 1, 2, …
    pub turn: usize,
    /// 0 in the session, 1 inside a subagent, and so on.
    pub depth: usize,
    pub time: Option<i64>,
    /// The tool's name, for tool calls.
    pub tool: Option<String>,
    /// The prompt or reply, or the call's summary, cut to `HAYSTACK_CHARS`.
    pub text: String,
}

impl Entry {
    /// The first line of its text.
    pub fn line(&self) -> &str {
        self.text.lines().next().unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub file: SessionFile,
    /// False until the file has been read; until then only `file` is known.
    pub ready: bool,
    /// Where the session started, `~` for home.
    pub project: String,
    /// The last part of `project`.
    pub name: String,
    /// Every directory it worked in, as recorded.
    pub cwds: Vec<String>,
    pub branch: String,
    pub title: String,
    pub started: Option<i64>,
    pub last: Option<i64>,
    pub entries: Vec<Entry>,
    pub turns: usize,
    /// Lines that couldn't be read at all.
    pub bad: usize,
}

impl Session {
    /// A session known only by its file, until it's read.
    pub fn placeholder(file: SessionFile) -> Session {
        Session {
            file,
            ready: false,
            project: String::new(),
            name: String::new(),
            cwds: Vec::new(),
            branch: String::new(),
            title: String::new(),
            started: None,
            last: None,
            entries: Vec::new(),
            turns: 0,
            bad: 0,
        }
    }

    /// Whether it worked in `dir` or anywhere under it.
    pub fn worked_in(&self, dir: &str) -> bool {
        let dir = dir.trim_end_matches('/');
        self.cwds.iter().any(|c| {
            c.strip_prefix(dir)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
    }

    pub fn load(file: SessionFile) -> io::Result<Session> {
        let mut s = Session::placeholder(file);
        let mut cwd: Option<String> = None;
        // The title Claude Code gave it, or the first thing typed, or else the
        // first command: some sessions are only a `/model` and an `/exit`.
        let (mut title, mut first_prompt, mut first_command) = (None, None, None);
        let stats = store::walk(&s.file.clone(), Detail::Index, &mut |step| {
            if step.depth == 0 {
                if cwd.is_none() {
                    cwd = step.cwd.map(str::to_owned);
                }
                if let Some(c) = step.cwd
                    && !s.cwds.iter().any(|seen| seen == c)
                {
                    s.cwds.push(c.to_owned());
                }
                if s.branch.is_empty()
                    && let Some(branch) = step.branch
                {
                    s.branch = branch.to_owned();
                }
                if let Some(t) = step.time {
                    s.started = Some(s.started.map_or(t, |s| s.min(t)));
                    s.last = Some(s.last.map_or(t, |l| l.max(t)));
                }
            }
            let (kind, tool, text) = match step.piece {
                Piece::Title(t) if step.depth == 0 => {
                    title = Some(t);
                    return;
                }
                Piece::Prompt { kind, text } => {
                    if step.depth == 0 {
                        s.turns += 1;
                        let first = if kind == PromptKind::Typed {
                            &mut first_prompt
                        } else {
                            &mut first_command
                        };
                        if first.is_none() {
                            *first = Some(text.lines().next().unwrap_or_default().to_owned());
                        }
                    }
                    (Kind::Prompt(kind), None, text)
                }
                Piece::Reply(text) => (Kind::Reply, None, text),
                Piece::Tool(call) => {
                    let summary = match step.cwd {
                        Some(cwd) if call.is_path => relative(&call.summary, cwd).to_owned(),
                        _ => call.summary,
                    };
                    let kind = Kind::Tool {
                        is_path: call.is_path,
                    };
                    (kind, Some(call.name), summary)
                }
                _ => return,
            };
            s.entries.push(Entry {
                kind,
                turn: s.turns,
                depth: step.depth,
                time: step.time,
                tool,
                text: cut(clean(&text).into_owned(), HAYSTACK_CHARS),
            });
        })?;
        s.bad = stats.bad;
        s.project = cwd
            .as_deref()
            .map(|c| clean(&store::tilde(c)).into_owned())
            .unwrap_or_default();
        s.name = s
            .project
            .rsplit('/')
            .find(|p| !p.is_empty())
            .unwrap_or(&s.project)
            .to_owned();
        s.title = title
            .or(first_prompt.or(first_command).map(|p| cut(p, TITLE_CHARS)))
            .map_or_else(|| "(no prompt)".into(), |t| clean(&t).into_owned());
        s.branch = clean(&s.branch).into_owned();
        s.ready = true;
        Ok(s)
    }

    /// When the session was last active: its last record, or the file's
    /// modification time before it's read.
    pub fn when(&self) -> i64 {
        self.last.unwrap_or(self.file.modified)
    }

    /// What the header search matches: one haystack, so a negation like
    /// `!levi` drops the session if any field contains it.
    pub fn header(&self) -> String {
        let date = self.when();
        let mut dates = time::local(date).date();
        if let Some(start) = self.started.map(|t| time::local(t).date())
            && start != dates
        {
            dates = format!("{start} {dates}");
        }
        format!(
            "{}\t{}\t{}\t{}",
            self.project, self.title, dates, self.branch
        )
    }

    /// The entries that start a turn, in order.
    pub fn turn_starts(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.depth == 0 && matches!(e.kind, Kind::Prompt(_)))
            .map(|(i, _)| i)
            .collect()
    }
}

/// Reads every file on all cores, calling `each` with its position in
/// `files` as it's done. Files are taken in order, so the newest come first.
pub fn load_all(files: &[SessionFile], each: impl Fn(usize, io::Result<Session>) + Sync) {
    let next = AtomicUsize::new(0);
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    thread::scope(|s| {
        for _ in 0..threads.min(files.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(file) = files.get(i) else { break };
                    each(i, Session::load(file.clone()));
                }
            });
        }
    });
}

/// `path` relative to `dir`, when it's inside it.
pub fn relative<'a>(path: &'a str, dir: &str) -> &'a str {
    path.strip_prefix(dir)
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(path)
}

/// `text` cut to at most `max` chars.
fn cut(mut text: String, max: usize) -> String {
    if let Some((at, _)) = text.char_indices().nth(max) {
        text.truncate(at);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{self, Claude};
    use serde_json::json;

    #[test]
    fn a_session_in_brief() {
        let claude = Claude::new();
        let file = claude.session(
            "-w-paradis",
            "dddd4444",
            &[
                testutil::compaction(1, 1),
                claude.prompt("Fix the wall gate\nIt sticks.", "2026-01-01T10:00:00Z"),
                claude.tool("t1", "Edit", json!({"file_path": "/w/paradis/src/gate.rs"})),
                testutil::result("t1", "ok", json!({})),
                claude.tool("t2", "Read", json!({"file_path": "/etc/hosts"})),
                claude.reply("Fixed."),
                claude.prompt(
                    "<bash-input>cargo test</bash-input>",
                    "2026-01-01T12:00:00Z",
                ),
                testutil::reply_at("All green.", "2026-01-01T12:00:09Z"),
            ],
        );
        let s = Session::load(file).unwrap();
        assert!(s.ready);
        assert_eq!(
            (s.project.as_str(), s.name.as_str()),
            ("/w/paradis", "paradis")
        );
        assert_eq!(s.branch, "main");
        assert_eq!(
            s.title, "Fix the wall gate",
            "first line of the first prompt"
        );
        assert_eq!(s.turns, 2);
        assert_eq!(s.started, time::parse("2026-01-01T10:00:00Z"));
        assert_eq!(s.last, time::parse("2026-01-01T12:00:09Z"));
        let entries: Vec<_> = s
            .entries
            .iter()
            .map(|e| (e.turn, e.tool.as_deref(), e.text.as_str()))
            .collect();
        assert_eq!(
            entries,
            [
                (1, None, "Fix the wall gate\nIt sticks."),
                (1, Some("Edit"), "src/gate.rs"),
                (1, Some("Read"), "/etc/hosts"),
                (1, None, "Fixed."),
                (2, None, "cargo test"),
                (2, None, "All green."),
            ]
        );
        assert_eq!(s.turn_starts(), [0, 4]);
        assert_eq!(s.entries[0].line(), "Fix the wall gate");
    }

    #[test]
    fn titles_and_subagents() {
        let claude = Claude::new();
        let file = claude.session(
            "-w",
            "eeee5555",
            &[
                claude.prompt("go", "2026-01-01T10:00:00Z"),
                testutil::title("Early name"),
                claude.tool("toolu_a", "Agent", json!({"description": "Scout"})),
                testutil::title("Scouting the forest"),
            ],
        );
        claude.subagent(
            "-w/eeee5555",
            "x",
            "toolu_a",
            &[
                claude.prompt("look", "2026-01-01T10:00:01Z"),
                claude.reply("Trees."),
            ],
        );
        let s = Session::load(file).unwrap();
        assert_eq!(s.title, "Scouting the forest", "the latest title wins");
        assert_eq!(s.turns, 1, "a subagent's prompt isn't a turn of yours");
        let depths: Vec<_> = s.entries.iter().map(|e| (e.depth, e.turn)).collect();
        assert_eq!(depths, [(0, 1), (0, 1), (1, 1), (1, 1)]);
        assert_eq!(s.turn_starts(), [0]);
    }

    #[test]
    fn a_session_of_commands_is_named_by_its_first() {
        let claude = Claude::new();
        let file = claude.session(
            "-w",
            "abab1212",
            &[
                claude.prompt(
                    "<command-name>/model</command-name><command-args>opus</command-args>",
                    "2026-01-01T10:00:00Z",
                ),
                claude.prompt("<command-name>/exit</command-name>", "2026-01-01T10:00:05Z"),
            ],
        );
        assert_eq!(Session::load(file).unwrap().title, "/model opus");
    }

    #[test]
    fn control_characters_never_reach_the_screen() {
        let claude = Claude::new();
        let file = claude.session(
            "-w",
            "abab3434",
            &[
                testutil::title("\x1b]0;pwned\x07Title"),
                claude.prompt("\x1b[2Jclear the \x1b[31mwall", "2026-01-01T10:00:00Z"),
            ],
        );
        let s = Session::load(file).unwrap();
        assert_eq!(s.title, "Title");
        assert_eq!(s.entries[0].text, "clear the wall");
    }

    #[test]
    fn long_text_is_cut_for_search() {
        let claude = Claude::new();
        let long = "titan ".repeat(1000);
        let file = claude.session(
            "-w",
            "ffff6666",
            &[claude.prompt(&long, "2026-01-01T10:00:00Z")],
        );
        let s = Session::load(file).unwrap();
        assert_eq!(s.entries[0].text.chars().count(), HAYSTACK_CHARS);
        assert_eq!(s.title.chars().count(), TITLE_CHARS);
    }

    #[test]
    fn an_empty_session_still_loads() {
        let claude = Claude::new();
        let file = claude.session("-w", "0000aaaa", &[r#"{"type":"mode","mode":"x"}"#.into()]);
        let s = Session::load(file).unwrap();
        assert_eq!(
            (s.title.as_str(), s.turns, s.project.as_str()),
            ("(no prompt)", 0, "")
        );
    }

    #[test]
    fn paths_are_relative_only_inside() {
        assert_eq!(relative("/w/p/src/a.rs", "/w/p"), "src/a.rs");
        assert_eq!(relative("/w/pp/a.rs", "/w/p"), "/w/pp/a.rs");
        assert_eq!(relative("/w/p", "/w/p"), "/w/p");
        assert_eq!(relative("/w/p/", "/w/p"), "/w/p/");
    }

    /// Indexes the real `~/.claude/projects` and prints sizes and timings,
    /// never content: `cargo test --release -- --ignored --nocapture real_index`.
    #[test]
    #[ignore]
    fn real_index_speed() {
        use std::sync::Mutex;
        use std::time::Instant;
        let projects = store::config_dir().unwrap().join("projects");
        let start = Instant::now();
        let files = store::discover(&projects).unwrap();
        let found = start.elapsed();
        let bytes: u64 = files.iter().map(|f| f.size).sum();
        let sessions = Mutex::new(Vec::new());
        let start = Instant::now();
        load_all(&files, |_, s| sessions.lock().unwrap().push(s.unwrap()));
        let took = start.elapsed();
        let sessions = sessions.into_inner().unwrap();
        let entries: usize = sessions.iter().map(|s| s.entries.len()).sum();
        let kept: usize = sessions
            .iter()
            .flat_map(|s| &s.entries)
            .map(|e| e.text.len() + e.tool.as_ref().map_or(0, String::len))
            .sum();
        let bad: usize = sessions.iter().map(|s| s.bad).sum();
        let turns: usize = sessions.iter().map(|s| s.turns).sum();
        let titled = sessions.iter().filter(|s| s.title != "(no prompt)").count();
        println!(
            "{} sessions, {:.1} MB main files; discover {:?}, index {:?}",
            files.len(),
            bytes as f64 / 1e6,
            found,
            took
        );
        println!(
            "{entries} entries ({:.1} MB of text), {turns} turns, {titled} titled, {bad} unreadable lines",
            kept as f64 / 1e6
        );
    }

    #[test]
    fn cut_respects_chars() {
        assert_eq!(cut("ñandú".into(), 3), "ñan");
        assert_eq!(cut("ab".into(), 3), "ab");
    }
}
