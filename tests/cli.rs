//! Runs the real binary: flags, exit codes, output, and the promise that it
//! never writes where it reads.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tempfile::TempDir;

const NORTH_GATE: &str = "0b5e55ed-0000-4000-8000-000000000001";
const FOREST_MAP: &str = "0b5e3a90-0000-4000-8000-000000000002";

/// A copy of `tests/fixtures/claude` to run against, with the north gate
/// session modified last.
fn claude() -> TempDir {
    let dir = TempDir::new().unwrap();
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude"),
        dir.path(),
    );
    let projects = dir.path().join("projects");
    touch(
        &projects.join(format!("-home-eren-paradis/{NORTH_GATE}.jsonl")),
        2_000_000_000,
    );
    touch(
        &projects.join(format!("-home-eren-map/{FOREST_MAP}.jsonl")),
        1_900_000_000,
    );
    dir
}

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn touch(path: &Path, secs: u64) {
    let file = File::options().write(true).open(path).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
}

fn ymir(claude: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ymir"))
        .args(args)
        .env("CLAUDE_CONFIG_DIR", claude)
        // Dates print in local time; pin it so expectations hold anywhere.
        .env("TZ", "UTC")
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn help_and_version() {
    let dir = claude();
    let out = ymir(dir.path(), &["--help"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("--filter"));
    for flag in ["-V", "-v", "--version"] {
        let out = ymir(dir.path(), &[flag]);
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(
            stdout(&out),
            format!("ymir {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
}

#[test]
fn bad_arguments_exit_2() {
    let dir = claude();
    for args in [
        &["stray"][..],
        &["--filter"],
        &["--show"],
        &["--turn", "1"],
        &["--show", NORTH_GATE, "--turn", "zero"],
        &["--nope"],
    ] {
        let out = ymir(dir.path(), args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(stderr(&out).starts_with("ymir: "), "{args:?}");
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn no_sessions_at_all() {
    let empty = TempDir::new().unwrap();
    let out = ymir(empty.path(), &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).starts_with("ymir: no sessions in "),
        "{}",
        stderr(&out)
    );
    let out = ymir(empty.path(), &["--filter", ""]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
}

#[test]
fn the_tui_needs_a_terminal() {
    let dir = claude();
    // Run by cargo, stderr is a pipe.
    let out = ymir(dir.path(), &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("needs a terminal"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn filter_lists_sessions_best_first() {
    let dir = claude();
    let out = ymir(dir.path(), &["--filter", ""]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stdout(&out),
        format!(
            "{NORTH_GATE} 2026-10-20 /home/eren/paradis Fix the north gate\n\
             {FOREST_MAP} 2026-10-18 /home/eren/map Draw a map of the forest beyond the wall.\n"
        )
    );

    // In a reply, a tool call, a subagent, an id.
    for (query, id) in [
        ("hinge pin", NORTH_GATE),
        ("cargo test", NORTH_GATE),
        // A subagent's prompt. (Tool output isn't searched fuzzily.)
        ("every gate in src", NORTH_GATE),
        ("castle", FOREST_MAP),
        ("/home/eren/map", FOREST_MAP),
        (FOREST_MAP, FOREST_MAP),
    ] {
        let out = ymir(dir.path(), &["--filter", query]);
        assert_eq!(out.status.code(), Some(0), "{query}");
        assert!(stdout(&out).starts_with(id), "{query}: {}", stdout(&out));
    }
}

#[test]
fn filter_exits_1_when_nothing_matches() {
    let dir = claude();
    // Nonsense; a line still being written; a compaction summary; a skipped
    // record; tool output.
    for query in [
        "zzzzqqq",
        "half wri",
        "summary: gates",
        "api-request",
        "east.rs",
    ] {
        let out = ymir(dir.path(), &["--filter", query]);
        assert_eq!(out.status.code(), Some(1), "{query}: {}", stdout(&out));
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn filter_searches_text_with_c() {
    let dir = claude();
    // Tool output inside a subagent, which fuzzy search leaves out.
    let out = ymir(dir.path(), &["--filter", "/c east.rs"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).starts_with(NORTH_GATE), "{}", stdout(&out));
    assert_eq!(stdout(&out).lines().count(), 1);
    // The fuzzy part picks the sessions to search.
    let out = ymir(dir.path(), &["--filter", "forest /c east.rs"]);
    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
    // Smart case.
    let out = ymir(dir.path(), &["--filter", "/c EAST.rs"]);
    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
}

#[test]
fn filter_here_keeps_sessions_from_this_directory() {
    let dir = claude();
    // Run from a directory none of the sessions worked in.
    let out = Command::new(env!("CARGO_BIN_EXE_ymir"))
        .args(["--filter", "/h"])
        .env("CLAUDE_CONFIG_DIR", dir.path())
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
}

#[test]
fn show_prints_a_transcript() {
    let dir = claude();
    let out = ymir(dir.path(), &["--show", NORTH_GATE]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.starts_with(&format!(
            "session {NORTH_GATE} \"Fix the north gate\"\n\
         in /home/eren/paradis, on main, 2026-10-20 09:00 to 09:32, 4 turns\n\n\
         ❯ you · 09:00\n  The north gate sticks when it rains. Find out why.\n"
        )),
        "{text}"
    );
    for part in [
        "  ▸ Edit src/gate.rs\n    src/gate.rs · +1 -1\n    @@ -1,3 +1,3 @@\n     pub fn open() {\n    -    pin(3);\n    +    pin(5);\n",
        "    running 3 tests\n",
        "  ▸ Agent Scout the wall (Explore)\n  ↳ List every gate",
        "── compacted 150k → 9k tokens ──",
        "  ! git status --short\n",
        "  Committed as 4f2c9a1.\n",
    ] {
        assert!(text.contains(part), "missing {part:?} in\n{text}");
    }
    // No terminal escapes from the tool output, no thinking, no summary.
    assert!(!text.contains('\x1b'));
    assert!(!text.contains("continued from a previous"));
}

#[test]
fn show_one_turn_by_id_prefix() {
    let dir = claude();
    let out = ymir(dir.path(), &["--show", "0b5e", "--turn", "4"]);
    assert_eq!(out.status.code(), Some(2), "ambiguous: {}", stdout(&out));
    assert!(stderr(&out).contains("could be 2 sessions"));

    let out = ymir(dir.path(), &["--show", "0B5E55", "--turn", "4"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("turn 4 of 4\n\n❯ you · 09:32\n  Commit it.\n"),
        "{text}"
    );
    assert!(!text.contains("north gate sticks"), "{text}");

    let out = ymir(dir.path(), &["--show", "feedface"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).starts_with("ymir: no session 'feedface'"));
}

/// Every file under `dir`: its size, modification time and a hash of its
/// contents.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (u64, SystemTime, u64)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).unwrap() {
            let entry = entry.unwrap();
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                stack.push(entry.path());
                out.insert(entry.path(), (0, meta.modified().unwrap(), 0));
            } else {
                let mut h = DefaultHasher::new();
                fs::read(entry.path()).unwrap().hash(&mut h);
                out.insert(
                    entry.path(),
                    (meta.len(), meta.modified().unwrap(), h.finish()),
                );
            }
        }
    }
    out
}

/// ymir reads `~/.claude` and writes nothing there: no file changed,
/// appeared or disappeared, whatever it was asked.
#[test]
fn never_writes_where_it_reads() {
    let dir = claude();
    let before = snapshot(dir.path());
    for args in [
        &[][..],
        &["--filter", ""],
        &["--filter", "gate"],
        &["--filter", NORTH_GATE],
        &["--filter", "/c gate"],
        &["--filter", "/h"],
        &["--show", NORTH_GATE],
        &["--show", FOREST_MAP, "--turn", "1"],
        &["--show", "nope"],
        &["--help"],
    ] {
        ymir(dir.path(), args);
    }
    assert_eq!(snapshot(dir.path()), before);
}
