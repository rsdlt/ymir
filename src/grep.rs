//! `/c`: literal search through everything in a session, as `C-e` shows it:
//! prompts, replies, thinking, whole commands, all their output, edits and
//! subagents. Searching what the transcript shows means every hit has a line
//! to jump to.

use std::io;
use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use crate::store::SessionFile;
use crate::transcript::{self, Limits, Transcript};

/// What `/c` found in one session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    /// Occurrences of the text.
    pub hits: usize,
    /// The entries they're in, as the index numbers them: a hit in a tool's
    /// output is in its call's entry.
    pub entries: Vec<usize>,
}

/// Where `pattern` occurs in `line`, as byte ranges: literally, ignoring case
/// unless the pattern has an uppercase letter. (Case folding is ASCII-only,
/// as in levi.)
pub fn find(line: &str, pattern: &str) -> Vec<Range<usize>> {
    let (hay, needle) = (line.as_bytes(), pattern.as_bytes());
    if needle.is_empty() || needle.len() > hay.len() {
        return Vec::new();
    }
    let ignore_case = !pattern.chars().any(char::is_uppercase);
    let mut found = Vec::new();
    let mut at = 0;
    while at + needle.len() <= hay.len() {
        let window = &hay[at..at + needle.len()];
        let hit = if ignore_case {
            window.eq_ignore_ascii_case(needle)
        } else {
            window == needle
        };
        if hit {
            found.push(at..at + needle.len());
            at += needle.len();
        } else {
            at += 1;
        }
    }
    found
}

/// What `pattern` finds in a transcript read with `Limits::FULL`.
pub fn in_transcript(t: &Transcript, pattern: &str) -> Found {
    let mut found = Found::default();
    let mut entry = None;
    for item in &t.items {
        entry = item.entry.or(entry);
        let n = find(&item.text, pattern).len();
        if n == 0 {
            continue;
        }
        found.hits += n;
        if let Some(e) = entry
            && found.entries.last() != Some(&e)
        {
            found.entries.push(e);
        }
    }
    found
}

/// Reads one session in full and searches it.
pub fn session(file: &SessionFile, pattern: &str) -> io::Result<Found> {
    let t = transcript::load(file, Limits::FULL)?;
    Ok(in_transcript(&t, pattern))
}

/// Searches `files` on every core, reporting each by its place in `files` as
/// it's done. Stops taking new files once `live` says the search is stale.
pub fn search(
    files: &[SessionFile],
    pattern: &str,
    live: impl Fn() -> bool + Sync,
    each: impl Fn(usize, io::Result<Found>) + Sync,
) {
    let next = AtomicUsize::new(0);
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    thread::scope(|s| {
        for _ in 0..threads.min(files.len()) {
            s.spawn(|| {
                while live() {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(file) = files.get(i) else { break };
                    each(i, session(file, pattern));
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{self, Claude};
    use serde_json::json;

    #[test]
    fn find_is_literal_with_smart_case() {
        assert_eq!(find("let x = Foo(foo)", "foo"), vec![8..11, 12..15]);
        assert_eq!(find("let x = Foo(foo)", "Foo"), vec![8..11]);
        assert_eq!(find("a.b axb", "a.b"), vec![0..3]);
        assert_eq!(find("ñandú ñ", "ñ"), vec![0..2, 8..10]);
        assert!(find("short", "longer text").is_empty());
    }

    #[test]
    fn finds_what_the_index_leaves_out() {
        let claude = Claude::new();
        let file = claude.session(
            "-w",
            "aaaa0001",
            &[
                claude.prompt("Why does the gate stick?", "2026-01-01T10:00:00Z"),
                testutil::thinking("Maybe the hinge pin."),
                claude.tool("t1", "Bash", json!({"command": "cargo test"})),
                testutil::result("t1", "a\nb\nc\nd\ne\nf\ng\nhinge pin too short", json!({})),
                claude.reply("The hinge pin was too short."),
            ],
        );
        let found = session(&file, "hinge pin").unwrap();
        // Thinking, the 8th line of output (past what the screen shows), and the
        // reply. Thinking has no entry of its own: it's in the prompt's.
        assert_eq!(found.hits, 3);
        assert_eq!(found.entries, [0, 1, 2]);
        assert_eq!(session(&file, "Hinge").unwrap().hits, 0, "smart case");
    }

    #[test]
    fn searches_every_file_until_stale() {
        let claude = Claude::new();
        let files: Vec<_> = (0..20)
            .map(|n| {
                claude.session(
                    "-w",
                    &format!("aaaa{n:04}"),
                    &[claude.reply(if n % 2 == 0 { "wall" } else { "sea" })],
                )
            })
            .collect();
        let hits = std::sync::Mutex::new(vec![0; files.len()]);
        search(
            &files,
            "wall",
            || true,
            |i, f| hits.lock().unwrap()[i] = f.unwrap().hits,
        );
        let hits = hits.into_inner().unwrap();
        assert_eq!(hits.iter().sum::<usize>(), 10);
        assert!(hits.iter().step_by(2).all(|&h| h == 1));

        let done = AtomicUsize::new(0);
        search(
            &files,
            "wall",
            || false,
            |_, _| {
                done.fetch_add(1, Ordering::Relaxed);
            },
        );
        assert_eq!(done.into_inner(), 0);
    }
}

#[cfg(test)]
mod real {
    use super::*;
    use crate::store;
    use std::sync::Mutex;
    use std::time::Instant;

    /// `/c` over every real session. Prints counts and timings, never
    /// content: `cargo test --release -- --ignored --nocapture grep_speed`.
    #[test]
    #[ignore]
    fn grep_speed() {
        let projects = store::config_dir().unwrap().join("projects");
        let files = store::discover(&projects).unwrap();
        let bytes: u64 = files.iter().map(|f| f.size).sum();
        for pattern in ["the", "focus", "cargo test --locked", "Zzzzqqq"] {
            let start = Instant::now();
            let found = Mutex::new((0, 0));
            search(
                &files,
                pattern,
                || true,
                |_, f| {
                    let f = f.unwrap();
                    let mut found = found.lock().unwrap();
                    found.0 += usize::from(f.hits > 0);
                    found.1 += f.hits;
                },
            );
            let (sessions, hits) = found.into_inner().unwrap();
            println!(
                "{pattern:>20}: {:?} over {} sessions ({:.0} MB), {sessions} with hits, {hits} hits",
                start.elapsed(),
                files.len(),
                bytes as f64 / 1e6
            );
        }
    }
}
