//! Where sessions live, and reading them. Everything here opens files
//! read-only: ymir never writes to `~/.claude`.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Deserialize;

use crate::record::{self, Detail, Piece};

/// Subagents can start subagents; past this depth they're left out, which
/// also ends any loop a malformed meta file could make.
const MAX_DEPTH: usize = 4;

/// Claude Code's configuration directory: `$CLAUDE_CONFIG_DIR`, or
/// `~/.claude`.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    home().map(|h| h.join(".claude"))
}

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// `path` with the home directory written `~`.
/// Where `/h` looks: the repository `dir` is in, or `dir` itself outside
/// one. Only looks for a `.git`, without running git.
pub fn here(dir: &Path) -> PathBuf {
    dir.ancestors()
        .find(|d| d.join(".git").exists())
        .unwrap_or(dir)
        .to_path_buf()
}

pub fn tilde(path: &str) -> String {
    match home() {
        Some(home) => {
            let home = home.to_string_lossy();
            match path.strip_prefix(home.as_ref()) {
                Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
                _ => path.to_owned(),
            }
        }
        None => path.to_owned(),
    }
}

/// A session's main file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFile {
    pub id: String,
    pub path: PathBuf,
    /// Last modified, in seconds since the epoch.
    pub modified: i64,
    pub size: u64,
}

impl SessionFile {
    /// The directory holding this session's subagents and spilled tool output.
    fn dir(&self) -> PathBuf {
        self.path.with_extension("")
    }
}

/// Every session under `projects`, most recently modified first.
pub fn discover(projects: &Path) -> io::Result<Vec<SessionFile>> {
    let mut found = Vec::new();
    for project in fs::read_dir(projects)? {
        let Ok(project) = project else { continue };
        if !project.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(entries) = fs::read_dir(project.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let modified = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64);
            found.push(SessionFile {
                id: id.to_owned(),
                path: path.clone(),
                modified,
                size: meta.len(),
            });
        }
    }
    // Newest first; the id breaks ties so the order never wobbles.
    found.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.id.cmp(&b.id)));
    Ok(found)
}

/// The subagent transcripts in `dir`, by the id of the tool call that
/// started each (from its `.meta.json`).
fn agents(dir: &Path) -> HashMap<String, PathBuf> {
    #[derive(Deserialize)]
    struct Meta {
        #[serde(rename = "toolUseId")]
        tool_use_id: Option<String>,
    }
    let mut out = HashMap::new();
    let Ok(entries) = fs::read_dir(dir.join("subagents")) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".meta.json") else {
            continue;
        };
        let transcript = path.with_file_name(format!("{stem}.jsonl"));
        let id = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Meta>(&bytes).ok())
            .and_then(|m| m.tool_use_id);
        if let Some(id) = id
            && transcript.is_file()
        {
            out.insert(id, transcript);
        }
    }
    out
}

/// Calls `f` with each complete line of `path`, without its newline, and
/// returns how many bytes that covered. A last line without a newline is
/// still being written: it's left for the next read.
pub fn lines(path: &Path, mut f: impl FnMut(&[u8])) -> io::Result<u64> {
    let mut reader = BufReader::with_capacity(1 << 16, File::open(path)?);
    let mut line = Vec::new();
    let mut done = 0;
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 || line.last() != Some(&b'\n') {
            return Ok(done);
        }
        done += n as u64;
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        f(&line);
    }
}

/// One piece of a session, with where it stands.
pub struct Step<'a> {
    /// 0 for the session itself, 1 inside a subagent it started, and so on.
    pub depth: usize,
    pub time: Option<i64>,
    pub cwd: Option<&'a str>,
    pub branch: Option<&'a str>,
    pub piece: Piece,
}

/// What reading a session found besides its pieces.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// Lines that weren't JSON, or not an object.
    pub bad: usize,
    /// Bytes of the main file read, up to its last complete line.
    pub bytes: u64,
}

/// Reads a session in order, each subagent's transcript right after the
/// call that started it. Unreadable lines are counted and skipped.
pub fn walk(file: &SessionFile, detail: Detail, visit: &mut dyn FnMut(Step)) -> io::Result<Stats> {
    let agents = agents(&file.dir());
    let mut stats = Stats::default();
    let mut seen = HashSet::new();
    stats.bytes = walk_file(&file.path, 0, detail, &agents, &mut seen, &mut stats, visit)?;
    Ok(stats)
}

fn walk_file(
    path: &Path,
    depth: usize,
    detail: Detail,
    agents: &HashMap<String, PathBuf>,
    seen: &mut HashSet<PathBuf>,
    stats: &mut Stats,
    visit: &mut dyn FnMut(Step),
) -> io::Result<u64> {
    lines(path, |bytes| {
        let Some(line) = record::read(bytes, detail) else {
            if !record::skipped(bytes) {
                stats.bad += 1;
            }
            return;
        };
        for piece in line.pieces {
            let agent = match &piece {
                Piece::Tool(call) if depth < MAX_DEPTH => agents.get(&call.id).cloned(),
                _ => None,
            };
            visit(Step {
                depth,
                time: line.time,
                cwd: line.cwd.as_deref(),
                branch: line.branch.as_deref(),
                piece,
            });
            if let Some(agent) = agent
                && seen.insert(agent.clone())
            {
                // A subagent file that can't be read just isn't shown.
                let _ = walk_file(&agent, depth + 1, detail, agents, seen, stats, visit);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Claude;
    use serde_json::json;

    #[test]
    fn discovers_sessions_newest_first() {
        let claude = Claude::new();
        claude.session(
            "-w-old",
            "aaaa1111",
            &[claude.prompt("first", "2026-01-01T10:00:00Z")],
        );
        claude.session(
            "-w-new",
            "bbbb2222",
            &[claude.prompt("second", "2026-01-02T10:00:00Z")],
        );
        claude.touch("-w-old/aaaa1111.jsonl", 1_000);
        claude.touch("-w-new/bbbb2222.jsonl", 2_000);
        // Not sessions: a memory file, a stray directory, a subagent.
        claude.write("-w-new/memory/MEMORY.md", "notes");
        claude.write("-w-new/bbbb2222/subagents/agent-1.jsonl", "{}\n");
        let found = discover(&claude.projects()).unwrap();
        let ids: Vec<_> = found.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["bbbb2222", "aaaa1111"]);
        assert_eq!(found[0].modified, 2_000);
    }

    #[test]
    fn a_line_still_being_written_waits() {
        let claude = Claude::new();
        claude.write("p/s.jsonl", "{\"a\":1}\n{\"b\":2}\r\n{\"c\":");
        let mut seen = Vec::new();
        let read = lines(&claude.projects().join("p/s.jsonl"), |l| {
            seen.push(String::from_utf8_lossy(l).into_owned())
        })
        .unwrap();
        assert_eq!(seen, ["{\"a\":1}", "{\"b\":2}"]);
        assert_eq!(read, 17);
    }

    #[test]
    fn subagents_are_walked_after_their_call() {
        let claude = Claude::new();
        let file = claude.session(
            "-w",
            "cccc3333",
            &[
                claude.prompt("send a scout", "2026-01-01T10:00:00Z"),
                claude.tool(
                    "toolu_9",
                    "Agent",
                    json!({"description": "Scout", "subagent_type": "Explore"}),
                ),
                "not json at all".into(),
                claude.reply("The scout is back."),
            ],
        );
        claude.subagent(
            "-w/cccc3333",
            "a1",
            "toolu_9",
            &[
                claude.prompt("look around", "2026-01-01T10:00:05Z"),
                claude.reply("Trees."),
            ],
        );
        let mut order = Vec::new();
        let stats = walk(&file, Detail::Index, &mut |step| {
            order.push((step.depth, format!("{:?}", step.piece)))
        })
        .unwrap();
        let depths: Vec<_> = order.iter().map(|(d, _)| *d).collect();
        assert_eq!(depths, [0, 0, 1, 1, 0]);
        assert!(order[2].1.contains("look around"));
        assert!(order[4].1.contains("The scout is back."));
        assert_eq!(stats.bad, 1);
    }

    #[test]
    fn home_is_written_as_a_tilde() {
        let Some(home) = home() else { return };
        let home = home.to_string_lossy().into_owned();
        assert_eq!(tilde(&format!("{home}/code/levi")), "~/code/levi");
        assert_eq!(tilde(&home), "~");
        assert_eq!(tilde(&format!("{home}x/y")), format!("{home}x/y"));
        assert_eq!(tilde("/elsewhere"), "/elsewhere");
    }
}
