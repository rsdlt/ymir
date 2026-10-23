//! One line of a session file, read as little as possible.
//!
//! The format isn't a public contract, so everything here is defensive: a
//! line ymir can't read, a record type it doesn't know, a block it hasn't
//! seen, a field of an unexpected type, all come out as nothing, never as an
//! error. See "Data format findings" in docs/plan.md for what's expected.

use std::borrow::Cow;

use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::value::RawValue;

use crate::time;

/// Record types skipped before parsing. Claude Code logs every API request
/// in the session file; those lines are half the bytes and start with their
/// type. If the key order ever changes they get parsed and skipped by type
/// instead: same result, slower.
const SKIP_PREFIX: &[u8] = br#"{"type":"api-request"#;

/// The fields ymir reads from a record, each borrowed as raw JSON and read
/// on its own (see `string` and `flag`), so a field that changes type loses
/// only itself, not the line. Everything else is skipped without being kept.
#[derive(Deserialize)]
struct Raw<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<&'a RawValue>,
    #[serde(borrow)]
    timestamp: Option<&'a RawValue>,
    #[serde(borrow)]
    cwd: Option<&'a RawValue>,
    #[serde(rename = "gitBranch", borrow)]
    branch: Option<&'a RawValue>,
    #[serde(rename = "isMeta", borrow)]
    meta: Option<&'a RawValue>,
    #[serde(rename = "isCompactSummary", borrow)]
    compact_summary: Option<&'a RawValue>,
    #[serde(borrow)]
    subtype: Option<&'a RawValue>,
    #[serde(rename = "compactMetadata", borrow)]
    compact: Option<&'a RawValue>,
    #[serde(rename = "aiTitle", borrow)]
    title: Option<&'a RawValue>,
    #[serde(borrow)]
    message: Option<&'a RawValue>,
    #[serde(rename = "toolUseResult", borrow)]
    result: Option<&'a RawValue>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CompactMeta {
    #[serde(rename = "preTokens")]
    pre: Option<u64>,
    #[serde(rename = "postTokens")]
    post: Option<u64>,
}

#[derive(Deserialize)]
struct Message<'a> {
    #[serde(borrow)]
    content: Option<&'a RawValue>,
}

/// A content block. Which fields are set depends on `kind`.
#[derive(Deserialize)]
struct Block<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<&'a RawValue>,
    #[serde(borrow)]
    text: Option<&'a RawValue>,
    // tool_use
    #[serde(borrow)]
    id: Option<&'a RawValue>,
    #[serde(borrow)]
    name: Option<&'a RawValue>,
    #[serde(borrow)]
    input: Option<&'a RawValue>,
    // tool_result
    #[serde(borrow)]
    tool_use_id: Option<&'a RawValue>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
    #[serde(borrow)]
    is_error: Option<&'a RawValue>,
    // Images carry their bytes here; named so they're skipped, not kept.
    #[allow(dead_code)]
    source: Option<IgnoredAny>,
}

impl Block<'_> {
    fn kind(&self) -> Option<Cow<'_, str>> {
        string(self.kind)
    }
}

/// A JSON string, borrowed when it has no escapes. Anything else is `None`.
fn string(value: Option<&RawValue>) -> Option<Cow<'_, str>> {
    let raw = value?.get();
    let inner = raw.strip_prefix('"')?.strip_suffix('"')?;
    if inner.contains('\\') {
        serde_json::from_str::<String>(raw).ok().map(Cow::Owned)
    } else {
        Some(Cow::Borrowed(inner))
    }
}

/// A JSON `true`; anything else, including a missing field, is false.
fn flag(value: Option<&RawValue>) -> bool {
    value.is_some_and(|v| v.get() == "true")
}

/// A line of a session file, as far as ymir cares.
#[derive(Debug, Default, PartialEq)]
pub struct Line {
    pub time: Option<i64>,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub pieces: Vec<Piece>,
}

/// What a session is made of, in file order.
#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    /// Something typed: a prompt, a `!` shell command or a slash command.
    /// Each starts a turn.
    Prompt {
        kind: PromptKind,
        text: String,
    },
    /// Claude's text.
    Reply(String),
    Tool(Call),
    /// A tool's output. Only read when asked for (`Detail::Full`).
    Result(Output),
    /// Something shown but not typed: a `!` command's output, a slash
    /// command's, a background agent finishing, an interruption.
    Notice(String),
    /// The context was compacted here, from `pre` tokens down to `post`.
    Compact {
        pre: Option<u64>,
        post: Option<u64>,
    },
    /// The session's title, as Claude Code named it. The latest one wins.
    Title(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    Typed,
    Shell,
    Command,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub id: String,
    pub name: String,
    /// One line naming what the call is about: the command, the path, the
    /// pattern, the URL.
    pub summary: String,
    /// Whether `summary` is a file path.
    pub is_path: bool,
    /// For `Agent` calls, the kind of agent.
    pub agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub id: String,
    pub text: String,
    pub error: bool,
    /// The files the call changed, as Claude Code recorded them.
    pub edits: Vec<Edit>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Edit {
    pub path: String,
    pub hunks: Vec<Hunk>,
    /// For a file the call created: how many lines it has.
    pub created: Option<usize>,
    /// A file Write created, whole: it has no hunks.
    pub content: Option<String>,
}

/// A unified-diff hunk: `lines` start with `+`, `-` or a space.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Hunk {
    #[serde(rename = "oldStart")]
    pub old_start: u64,
    #[serde(rename = "oldLines")]
    pub old_lines: u64,
    #[serde(rename = "newStart")]
    pub new_start: u64,
    #[serde(rename = "newLines")]
    pub new_lines: u64,
    pub lines: Vec<String>,
}

/// How much of a line to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// What search needs: no tool results.
    Index,
    /// Everything shown in a transcript.
    Full,
}

/// Whether `line` is a record ymir skips unread, by its first bytes.
pub fn skipped(line: &[u8]) -> bool {
    line.starts_with(SKIP_PREFIX)
}

/// Reads one line (without its newline). Anything unreadable is `None`.
pub fn read(line: &[u8], detail: Detail) -> Option<Line> {
    if skipped(line) {
        return None;
    }
    let raw: Raw = serde_json::from_slice(line).ok()?;
    let mut out = Line {
        time: string(raw.timestamp).as_deref().and_then(time::parse),
        cwd: string(raw.cwd).map(Cow::into_owned),
        branch: string(raw.branch).map(Cow::into_owned),
        pieces: Vec::new(),
    };
    match string(raw.kind)?.as_ref() {
        "user" if flag(raw.meta) || flag(raw.compact_summary) => {}
        "user" => user(&raw, detail, &mut out.pieces),
        "assistant" => assistant(&raw, &mut out.pieces),
        "system" if string(raw.subtype).as_deref() == Some("compact_boundary") => {
            let meta: CompactMeta = raw
                .compact
                .and_then(|m| serde_json::from_str(m.get()).ok())
                .unwrap_or_default();
            out.pieces.push(Piece::Compact {
                pre: meta.pre,
                post: meta.post,
            });
        }
        "ai-title" => {
            if let Some(title) = string(raw.title).filter(|t| !t.trim().is_empty()) {
                out.pieces.push(Piece::Title(title.trim().to_owned()));
            }
        }
        _ => {}
    }
    Some(out)
}

/// The content of a message: a plain string, or blocks.
enum Content<'a> {
    Text(String),
    Blocks(Vec<Block<'a>>),
}

fn content<'a>(raw: &Raw<'a>) -> Option<Content<'a>> {
    let message: Message<'a> = serde_json::from_str(raw.message?.get()).ok()?;
    let value = message.content?;
    match value.get().as_bytes().first()? {
        b'"' => serde_json::from_str(value.get()).ok().map(Content::Text),
        b'[' => {
            // One odd block doesn't lose the others.
            let items: Vec<&RawValue> = serde_json::from_str(value.get()).ok()?;
            let blocks = items
                .into_iter()
                .filter_map(|b| serde_json::from_str(b.get()).ok())
                .collect();
            Some(Content::Blocks(blocks))
        }
        _ => None,
    }
}

fn user(raw: &Raw, detail: Detail, out: &mut Vec<Piece>) {
    let blocks = match content(raw) {
        Some(Content::Text(text)) => return out.extend(typed(&text)),
        Some(Content::Blocks(blocks)) => blocks,
        None => return,
    };
    let results = blocks
        .iter()
        .any(|b| b.kind().as_deref() == Some("tool_result"));
    if results {
        if detail == Detail::Index {
            return;
        }
        let mut edits = Some(raw.result.map(edits).unwrap_or_default());
        for block in &blocks {
            match block.kind().as_deref() {
                Some("tool_result") => out.push(Piece::Result(Output {
                    id: string(block.tool_use_id).unwrap_or_default().into_owned(),
                    text: block.content.map(result_text).unwrap_or_default(),
                    error: flag(block.is_error),
                    // One structured result per record, for its first call.
                    edits: edits.take().unwrap_or_default(),
                })),
                Some("text") => {
                    out.extend(string(block.text).map(|t| Piece::Notice(t.into_owned())))
                }
                _ => {}
            }
        }
        return;
    }
    // A prompt in blocks: text, maybe with images.
    let mut text = String::new();
    for block in &blocks {
        let part = match block.kind().as_deref() {
            Some("text") => string(block.text),
            Some("image") => Some(Cow::Borrowed("[image]")),
            _ => None,
        };
        if let Some(part) = part {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&part);
        }
    }
    out.extend(typed(&text));
}

/// A user's string content: typed text, or one of the wrappers Claude Code
/// puts around what isn't (see the plan's findings).
fn typed(text: &str) -> Option<Piece> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(cmd) = tag(trimmed, "bash-input") {
        return Some(Piece::Prompt {
            kind: PromptKind::Shell,
            text: cmd.to_owned(),
        });
    }
    if let Some(name) = tag(trimmed, "command-name") {
        let args = tag(trimmed, "command-args").unwrap_or_default();
        let text = if args.is_empty() {
            name.to_owned()
        } else {
            format!("{name} {args}")
        };
        return Some(Piece::Prompt {
            kind: PromptKind::Command,
            text,
        });
    }
    if trimmed.starts_with("<bash-stdout>") || trimmed.starts_with("<bash-stderr>") {
        let out = tag(trimmed, "bash-stdout").unwrap_or_default();
        let err = tag(trimmed, "bash-stderr").unwrap_or_default();
        let both = [out, err]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        return (!both.is_empty()).then_some(Piece::Notice(both));
    }
    if let Some(out) = tag(trimmed, "local-command-stdout") {
        return (!out.is_empty()).then(|| Piece::Notice(out.to_owned()));
    }
    if trimmed.starts_with("<task-notification>") {
        let summary = tag(trimmed, "summary").unwrap_or("a background task finished");
        return Some(Piece::Notice(summary.to_owned()));
    }
    if trimmed.starts_with("[Request interrupted") {
        return Some(Piece::Notice(trimmed.to_owned()));
    }
    Some(Piece::Prompt {
        kind: PromptKind::Typed,
        text: trimmed.to_owned(),
    })
}

/// The trimmed text between `<name>` and `</name>`, if both are there.
fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let end = start + text[start..].find(&close)?;
    Some(text[start..end].trim())
}

fn assistant(raw: &Raw, out: &mut Vec<Piece>) {
    let blocks = match content(raw) {
        Some(Content::Blocks(blocks)) => blocks,
        Some(Content::Text(text)) => {
            out.extend((!text.trim().is_empty()).then(|| Piece::Reply(text.trim().to_owned())));
            return;
        }
        None => return,
    };
    for block in blocks {
        match block.kind().as_deref() {
            Some("text") => {
                if let Some(text) = string(block.text).filter(|t| !t.trim().is_empty()) {
                    out.push(Piece::Reply(text.trim().to_owned()));
                }
            }
            Some("tool_use") => out.push(Piece::Tool(call(&block))),
            // Thinking is mostly empty in the files (8,017 of 9,118 blocks)
            // and noise for reading back; see the plan.
            _ => {}
        }
    }
}

/// The inputs a summary is made from. Named one by one, so a big input (a
/// whole file for Write) isn't kept.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Input {
    command: Option<String>,
    file_path: Option<String>,
    notebook_path: Option<String>,
    pattern: Option<String>,
    path: Option<String>,
    url: Option<String>,
    query: Option<String>,
    description: Option<String>,
    subagent_type: Option<String>,
    skill: Option<String>,
}

fn call(block: &Block) -> Call {
    let name = string(block.name).map_or_else(|| "tool".into(), Cow::into_owned);
    let raw = block.input.map(RawValue::get).unwrap_or("{}");
    // A field of an unexpected type fails the struct; then only the generic
    // summary below is tried.
    let input: Input = serde_json::from_str(raw).unwrap_or_default();
    let (summary, is_path) = match name.as_str() {
        "Bash" => (input.command, false),
        "Read" | "Edit" | "Write" | "MultiEdit" => (input.file_path, true),
        "NotebookEdit" => (input.notebook_path, true),
        "Grep" | "Glob" => (
            input.pattern.map(|p| match input.path {
                Some(path) => format!("{p} in {path}"),
                None => p,
            }),
            false,
        ),
        "WebFetch" => (input.url, false),
        "WebSearch" | "ToolSearch" => (input.query, false),
        "Agent" | "Task" => (input.description, false),
        "Skill" => (input.skill, false),
        _ => (None, false),
    };
    let summary = summary.unwrap_or_else(|| first_string(raw));
    Call {
        id: string(block.id).unwrap_or_default().into_owned(),
        agent: matches!(name.as_str(), "Agent" | "Task")
            .then_some(input.subagent_type)
            .flatten(),
        name,
        summary,
        is_path,
    }
}

/// The first string value of a JSON object, for tools ymir doesn't know.
fn first_string(raw: &str) -> String {
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(raw).unwrap_or_default();
    map.into_iter()
        .find_map(|(_, v)| match v {
            serde_json::Value::String(s) => Some(s),
            _ => None,
        })
        .unwrap_or_default()
}

/// A tool result's text: a string, or text blocks (images become `[image]`).
fn result_text(raw: &RawValue) -> String {
    #[derive(Deserialize)]
    struct Part {
        #[serde(rename = "type")]
        kind: Option<String>,
        text: Option<String>,
    }
    if let Ok(text) = serde_json::from_str::<String>(raw.get()) {
        return text;
    }
    let parts: Vec<Part> = serde_json::from_str(raw.get()).unwrap_or_default();
    parts
        .into_iter()
        .filter_map(|p| match p.kind.as_deref() {
            Some("text") => p.text,
            Some("image") => Some("[image]".into()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The structured result's file changes: an Edit's or Write's patch, or the
/// files a shell command changed.
fn edits(raw: &RawValue) -> Vec<Edit> {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Structured {
        #[serde(rename = "filePath")]
        file_path: Option<String>,
        #[serde(rename = "type")]
        kind: Option<String>,
        content: Option<String>,
        #[serde(rename = "structuredPatch")]
        patch: Option<Vec<Hunk>>,
        #[serde(rename = "bashEditDiff")]
        bash: Option<BashDiff>,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct BashDiff {
        files: Vec<BashFile>,
    }
    #[derive(Deserialize)]
    struct BashFile {
        #[serde(rename = "filePath")]
        file_path: String,
        #[serde(default)]
        hunks: Vec<Hunk>,
        #[serde(default)]
        created: bool,
    }
    // Some tools record a bare string or an array; those carry no edits.
    if !raw.get().starts_with('{') {
        return Vec::new();
    }
    let Ok(s) = serde_json::from_str::<Structured>(raw.get()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(path) = s.file_path {
        let hunks = s.patch.unwrap_or_default();
        let create = s.kind.as_deref() == Some("create");
        let content = s.content.filter(|_| create);
        let created = create.then(|| content.as_deref().map_or(0, |c| c.lines().count()));
        if !hunks.is_empty() || created.is_some() {
            out.push(Edit {
                path,
                hunks,
                created,
                content,
            });
        }
    }
    for file in s.bash.map(|b| b.files).unwrap_or_default() {
        let added = file.hunks.iter().map(|h| h.new_lines as usize).sum();
        out.push(Edit {
            path: file.file_path,
            created: file.created.then_some(added),
            hunks: file.hunks,
            content: None,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(value: serde_json::Value, detail: Detail) -> Option<Line> {
        read(value.to_string().as_bytes(), detail)
    }

    fn pieces(value: serde_json::Value) -> Vec<Piece> {
        line(value, Detail::Full).unwrap().pieces
    }

    fn user(content: serde_json::Value) -> serde_json::Value {
        json!({"parentUuid": null, "type": "user", "timestamp": "2026-10-22T20:31:05.123Z",
               "cwd": "/tmp/paradis", "gitBranch": "main", "message": {"role": "user", "content": content}})
    }

    fn assistant(block: serde_json::Value) -> serde_json::Value {
        json!({"parentUuid": "a", "type": "assistant", "timestamp": "2026-10-22T20:31:09Z",
               "message": {"id": "msg_1", "content": [block]}})
    }

    fn prompt(kind: PromptKind, text: &str) -> Piece {
        Piece::Prompt {
            kind,
            text: text.into(),
        }
    }

    #[test]
    fn a_prompt_keeps_its_time_place_and_text() {
        let l = line(user(json!("  Scout the walls  ")), Detail::Index).unwrap();
        assert_eq!(l.time, Some(1_792_701_065));
        assert_eq!(l.cwd.as_deref(), Some("/tmp/paradis"));
        assert_eq!(l.branch.as_deref(), Some("main"));
        assert_eq!(l.pieces, [prompt(PromptKind::Typed, "Scout the walls")]);
    }

    #[test]
    fn prompts_in_blocks_keep_text_and_mark_images() {
        let p = pieces(user(json!([
            {"type": "text", "text": "what is this?"},
            {"type": "image", "source": {"type": "base64", "data": "AAAA"}}
        ])));
        assert_eq!(p, [prompt(PromptKind::Typed, "what is this?\n[image]")]);
    }

    #[test]
    fn wrappers_are_told_apart_from_typing() {
        let cases = [
            (
                "<bash-input>cargo test</bash-input>",
                Some(prompt(PromptKind::Shell, "cargo test")),
            ),
            (
                "<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args>opus</command-args>",
                Some(prompt(PromptKind::Command, "/model opus")),
            ),
            (
                "<command-message>clear</command-message>\n<command-name>/clear</command-name>",
                Some(prompt(PromptKind::Command, "/clear")),
            ),
            (
                "<bash-stdout>ok</bash-stdout><bash-stderr></bash-stderr>",
                Some(Piece::Notice("ok".into())),
            ),
            (
                "<bash-stdout></bash-stdout><bash-stderr></bash-stderr>",
                None,
            ),
            (
                "<local-command-stdout>Set model to opus</local-command-stdout>",
                Some(Piece::Notice("Set model to opus".into())),
            ),
            (
                "<task-notification>\n<task-id>b1</task-id>\n<summary>Agent \"scout\" completed</summary>\n</task-notification>",
                Some(Piece::Notice("Agent \"scout\" completed".into())),
            ),
            (
                "[Request interrupted by user]",
                Some(Piece::Notice("[Request interrupted by user]".into())),
            ),
            // Unknown tags are just text someone typed or pasted.
            (
                "<!-- a comment --> fix it",
                Some(prompt(PromptKind::Typed, "<!-- a comment --> fix it")),
            ),
            ("   ", None),
        ];
        for (text, want) in cases {
            assert_eq!(pieces(user(json!(text))), Vec::from_iter(want), "{text:?}");
        }
    }

    #[test]
    fn meta_and_compaction_summaries_are_skipped() {
        let mut meta = user(json!("Base directory for this skill: …"));
        meta["isMeta"] = json!(true);
        assert_eq!(pieces(meta), []);
        let mut summary = user(json!("This session is being continued…"));
        summary["isCompactSummary"] = json!(true);
        assert_eq!(pieces(summary), []);
    }

    #[test]
    fn replies_and_tool_calls() {
        assert_eq!(
            pieces(assistant(json!({"type": "text", "text": "\nOn it.\n"}))),
            [Piece::Reply("On it.".into())]
        );
        assert_eq!(
            pieces(assistant(
                json!({"type": "thinking", "thinking": "", "signature": "x"})
            )),
            []
        );
        let bash = pieces(assistant(
            json!({"type": "tool_use", "id": "toolu_1", "name": "Bash",
            "input": {"command": "cargo test", "description": "Run tests"}}),
        ));
        assert_eq!(
            bash,
            [Piece::Tool(Call {
                id: "toolu_1".into(),
                name: "Bash".into(),
                summary: "cargo test".into(),
                is_path: false,
                agent: None,
            })]
        );
        let agent = pieces(assistant(
            json!({"type": "tool_use", "id": "toolu_2", "name": "Agent",
            "input": {"description": "Scout the forest", "prompt": "…", "subagent_type": "Explore"}}),
        ));
        let Piece::Tool(agent) = &agent[0] else {
            panic!()
        };
        assert_eq!(
            (agent.summary.as_str(), agent.agent.as_deref()),
            ("Scout the forest", Some("Explore"))
        );
    }

    #[test]
    fn summaries_per_tool() {
        let summary = |name: &str, input: serde_json::Value| {
            let p = pieces(assistant(
                json!({"type": "tool_use", "id": "t", "name": name, "input": input}),
            ));
            match &p[0] {
                Piece::Tool(c) => (c.summary.clone(), c.is_path),
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(
            summary(
                "Edit",
                json!({"file_path": "/w/src/main.rs", "old_string": "a", "new_string": "b"})
            ),
            ("/w/src/main.rs".into(), true)
        );
        assert_eq!(
            summary("Grep", json!({"pattern": "titan", "path": "src"})),
            ("titan in src".into(), false)
        );
        assert_eq!(
            summary("mcp__wall__report", json!({"count": 3, "sector": "Maria"})),
            ("Maria".into(), false)
        );
        // A field of the wrong type doesn't lose the call.
        assert_eq!(
            summary("Bash", json!({"command": ["not", "a", "string"]})),
            (String::new(), false)
        );
        assert_eq!(summary("Unknown", json!(null)), (String::new(), false));
    }

    #[test]
    fn results_are_only_read_in_full() {
        let result = user(json!([{"type": "tool_result", "tool_use_id": "toolu_1",
            "content": "ok: 3 passed", "is_error": false}]));
        assert_eq!(line(result.clone(), Detail::Index).unwrap().pieces, []);
        assert_eq!(
            pieces(result),
            [Piece::Result(Output {
                id: "toolu_1".into(),
                text: "ok: 3 passed".into(),
                error: false,
                edits: vec![],
            })]
        );
    }

    #[test]
    fn result_text_from_blocks_and_errors() {
        let p = pieces(user(
            json!([{"type": "tool_result", "tool_use_id": "t", "is_error": true,
            "content": [{"type": "text", "text": "line 1"}, {"type": "image", "source": {}}, {"type": "text", "text": "line 2"}]}]),
        ));
        let Piece::Result(out) = &p[0] else { panic!() };
        assert_eq!(
            (out.text.as_str(), out.error),
            ("line 1\n[image]\nline 2", true)
        );
    }

    #[test]
    fn edits_come_from_the_structured_result() {
        let hunk = json!({"oldStart": 3, "oldLines": 1, "newStart": 3, "newLines": 2,
            "lines": ["-old", "+new", "+more"]});
        let mut edit =
            user(json!([{"type": "tool_result", "tool_use_id": "t", "content": "updated"}]));
        edit["toolUseResult"] = json!({"filePath": "/w/a.rs", "oldString": "old", "newString": "new\nmore",
            "originalFile": "…", "structuredPatch": [hunk], "userModified": false, "replaceAll": false});
        let Piece::Result(out) = &pieces(edit)[0] else {
            panic!()
        };
        assert_eq!(out.edits.len(), 1);
        assert_eq!(out.edits[0].path, "/w/a.rs");
        assert_eq!(out.edits[0].hunks[0].lines, ["-old", "+new", "+more"]);
        assert_eq!(out.edits[0].created, None);

        let mut create =
            user(json!([{"type": "tool_result", "tool_use_id": "t", "content": "created"}]));
        create["toolUseResult"] = json!({"type": "create", "filePath": "/w/b.rs",
            "content": "fn main() {}\n// two\n", "structuredPatch": [], "originalFile": null});
        let Piece::Result(out) = &pieces(create)[0] else {
            panic!()
        };
        assert_eq!(
            (out.edits[0].path.as_str(), out.edits[0].created),
            ("/w/b.rs", Some(2))
        );

        let mut shell = user(json!([{"type": "tool_result", "tool_use_id": "t", "content": ""}]));
        shell["toolUseResult"] = json!({"stdout": "", "stderr": "", "interrupted": false, "isImage": false,
            "noOutputExpected": false, "bashEditDiff": {"files": [{"filePath": "/w/c.rs", "hunks": [hunk]}],
            "moreFiles": 0, "changedFiles": ["/w/c.rs"]}});
        let Piece::Result(out) = &pieces(shell)[0] else {
            panic!()
        };
        assert_eq!(out.edits[0].path, "/w/c.rs");

        // Some tools record a string or an array instead of an object.
        for odd in [json!("Error: no"), json!([1, 2]), json!({"filePath": 7})] {
            let mut r = user(json!([{"type": "tool_result", "tool_use_id": "t", "content": "x"}]));
            r["toolUseResult"] = odd;
            let Piece::Result(out) = &pieces(r)[0] else {
                panic!()
            };
            assert_eq!(out.edits, []);
        }
    }

    #[test]
    fn compaction_and_titles() {
        let boundary = json!({"parentUuid": null, "logicalParentUuid": "x", "type": "system",
            "subtype": "compact_boundary", "content": "Conversation compacted", "isMeta": false,
            "timestamp": "2026-10-22T21:00:00Z", "compactMetadata": {"trigger": "auto", "preTokens": 180000, "postTokens": 12000}});
        assert_eq!(
            pieces(boundary),
            [Piece::Compact {
                pre: Some(180_000),
                post: Some(12_000)
            }]
        );
        let other = json!({"type": "system", "subtype": "turn_duration", "durationMs": 5});
        assert_eq!(pieces(other), []);
        assert_eq!(
            pieces(
                json!({"type": "ai-title", "sessionId": "s", "aiTitle": " Plan the expedition "})
            ),
            [Piece::Title("Plan the expedition".into())]
        );
    }

    #[test]
    fn unknown_and_broken_lines_are_nothing() {
        assert_eq!(pieces(json!({"type": "mode", "mode": "auto"})), []);
        assert_eq!(
            pieces(json!({"type": "brand-new-kind", "x": [1, {"y": 2}]})),
            []
        );
        assert_eq!(
            pieces(assistant(json!({"type": "server_tool_use", "id": "s"}))),
            []
        );
        // Not even a type.
        assert_eq!(read(br#"{"hello": 1}"#, Detail::Full), None);
        for bad in [
            &b""[..],
            b"{",
            b"not json",
            b"[1,2]",
            b"\xff\xfe",
            b"{\"type\":\"user\"",
        ] {
            assert_eq!(read(bad, Detail::Full), None, "{bad:?}");
        }
        // Skipped by their first bytes, before parsing.
        assert_eq!(
            read(br#"{"type":"api-request","id":"r"}"#, Detail::Full),
            None
        );
    }

    #[test]
    fn a_field_of_the_wrong_type_loses_only_itself() {
        let mut weird = user(json!("hi"));
        weird["timestamp"] = json!(12);
        weird["cwd"] = json!({"nested": true});
        weird["isMeta"] = json!("yes");
        let l = line(weird, Detail::Full).unwrap();
        assert_eq!((l.time, l.cwd), (None, None));
        assert_eq!(l.pieces, [prompt(PromptKind::Typed, "hi")]);

        let mut content = user(json!(null));
        content["message"]["content"] = json!(42);
        assert_eq!(pieces(content), []);
        let mut message = user(json!(null));
        message["message"] = json!("just a string");
        assert_eq!(pieces(message), []);

        // One odd block doesn't lose its neighbours.
        let p = pieces(user(
            json!([7, {"type": "text", "text": "kept"}, {"type": 3}]),
        ));
        assert_eq!(p, [prompt(PromptKind::Typed, "kept")]);
    }

    #[test]
    fn escaped_strings_are_unescaped() {
        let p = pieces(user(json!("say \"tch\"\n\ttwice \u{1F5E1}")));
        assert_eq!(
            p,
            [prompt(PromptKind::Typed, "say \"tch\"\n\ttwice \u{1F5E1}")]
        );
    }
}
