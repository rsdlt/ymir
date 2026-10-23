//! Synthetic Claude Code directories for tests. Every session here is made
//! up; nothing is copied from real ones.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Value, json};
use tempfile::TempDir;

use crate::store::SessionFile;

/// A temporary `CLAUDE_CONFIG_DIR`.
pub struct Claude {
    dir: TempDir,
}

impl Claude {
    pub fn new() -> Claude {
        let claude = Claude {
            dir: TempDir::new().unwrap(),
        };
        fs::create_dir_all(claude.projects()).unwrap();
        claude
    }

    pub fn projects(&self) -> PathBuf {
        self.dir.path().join("projects")
    }

    /// Writes `contents` at `rel` under the projects directory.
    pub fn write(&self, rel: &str, contents: &str) -> PathBuf {
        let path = self.projects().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }

    /// Sets the modification time of `rel`, in seconds since the epoch.
    pub fn touch(&self, rel: &str, secs: u64) {
        let file = File::options()
            .write(true)
            .open(self.projects().join(rel))
            .unwrap();
        file.set_modified(UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    /// A session `id` in project directory `project`, one record per line.
    pub fn session(&self, project: &str, id: &str, lines: &[String]) -> SessionFile {
        let path = self.write(&format!("{project}/{id}.jsonl"), &joined(lines));
        file(&path, id)
    }

    /// A subagent of the session whose directory is `dir`, started by the
    /// tool call `tool_use_id`.
    pub fn subagent(&self, dir: &str, agent: &str, tool_use_id: &str, lines: &[String]) {
        let base = format!("{dir}/subagents/agent-{agent}");
        self.write(&format!("{base}.jsonl"), &joined(lines));
        let meta =
            json!({"agentType": "Explore", "description": "Scout", "toolUseId": tool_use_id});
        self.write(&format!("{base}.meta.json"), &meta.to_string());
    }

    pub fn prompt(&self, text: &str, at: &str) -> String {
        prompt_in(text, at, "/w/paradis")
    }

    pub fn reply(&self, text: &str) -> String {
        reply(text)
    }

    pub fn tool(&self, id: &str, name: &str, input: Value) -> String {
        tool(id, name, input)
    }
}

pub fn file(path: &Path, id: &str) -> SessionFile {
    let meta = fs::metadata(path).unwrap();
    SessionFile {
        id: id.into(),
        path: path.to_owned(),
        modified: meta
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
        size: meta.len(),
    }
}

fn joined(lines: &[String]) -> String {
    lines.iter().map(|l| format!("{l}\n")).collect()
}

pub fn prompt_in(text: &str, at: &str, cwd: &str) -> String {
    json!({"parentUuid": null, "isSidechain": false, "type": "user", "message": {"role": "user", "content": text},
           "uuid": "u", "timestamp": at, "cwd": cwd, "sessionId": "s", "gitBranch": "main", "version": "9.9.9"})
    .to_string()
}

pub fn reply(text: &str) -> String {
    reply_at(text, "2026-01-01T10:00:01.000Z")
}

pub fn thinking(text: &str) -> String {
    json!({"parentUuid": "u", "type": "assistant", "timestamp": "2026-01-01T10:00:01.000Z", "cwd": "/w/paradis",
           "message": {"id": "msg", "role": "assistant", "content": [{"type": "thinking", "thinking": text, "signature": "x"}]}})
    .to_string()
}

pub fn reply_at(text: &str, at: &str) -> String {
    json!({"parentUuid": "u", "type": "assistant", "timestamp": at, "cwd": "/w/paradis",
           "message": {"id": "msg", "role": "assistant", "content": [{"type": "text", "text": text}]}})
    .to_string()
}

pub fn tool(id: &str, name: &str, input: Value) -> String {
    json!({"parentUuid": "u", "type": "assistant", "timestamp": "2026-01-01T10:00:02.000Z", "cwd": "/w/paradis",
           "message": {"id": "msg", "role": "assistant", "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}})
    .to_string()
}

pub fn result(id: &str, text: &str, structured: Value) -> String {
    json!({"parentUuid": "u", "type": "user", "timestamp": "2026-01-01T10:00:03.000Z", "cwd": "/w/paradis",
           "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": text, "is_error": false}]},
           "toolUseResult": structured})
    .to_string()
}

pub fn title(text: &str) -> String {
    json!({"type": "ai-title", "sessionId": "s", "aiTitle": text}).to_string()
}

pub fn compaction(pre: u64, post: u64) -> String {
    json!({"parentUuid": null, "type": "system", "subtype": "compact_boundary", "timestamp": "2026-01-01T11:00:00.000Z",
           "compactMetadata": {"trigger": "auto", "preTokens": pre, "postTokens": post}})
    .to_string()
}
