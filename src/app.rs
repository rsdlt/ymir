//! Everything the screen shows, and how keys and the mouse change it. No
//! threads in here: workers send `Msg`s, `main` hands out requests, and
//! tests play both parts.

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};

use crate::filter::{self, Filter, Hit};
use crate::index::{Kind, Session};
use crate::layout::{Resize, Seam, Splits};
use crate::store::SessionFile;
use crate::text;
use crate::time;
use crate::transcript::{Limits, Look, Transcript};

/// Transcripts kept after they're read.
const TRANSCRIPT_CACHE: usize = 8;
/// Lines one turn of the mouse wheel scrolls.
const WHEEL_LINES: isize = 3;
/// Columns one sideways scroll of a diff moves, by key or wheel.
const SIDE_STEP: isize = 8;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

pub enum Msg {
    Input(Event),
    /// A session has been read; its place in the list.
    Indexed(usize, Box<Session>),
    /// Every session has been read.
    IndexDone,
    Transcript(String, Result<Arc<Transcript>, String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sessions,
    Turns,
    Transcript,
}

impl Focus {
    fn next(self) -> Focus {
        match self {
            Focus::Sessions => Focus::Turns,
            Focus::Turns => Focus::Transcript,
            Focus::Transcript => Focus::Sessions,
        }
    }

    fn prev(self) -> Focus {
        self.next().next()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    /// Print this and quit.
    Pick(String),
    Quit,
}

/// One screen row of the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The transcript line it comes from.
    pub item: usize,
    /// The part of the line on this row, as a byte range of its text. A
    /// diff line is never wrapped, so it's all of it.
    pub span: Range<usize>,
    pub indent: usize,
}

/// The selected transcript, wrapped to the pane's width.
struct Wrapped {
    t: Arc<Transcript>,
    width: usize,
    rows: Vec<Row>,
    /// The first row of each transcript line.
    starts: Vec<usize>,
    /// Columns the widest diff line needs, indent included.
    wide: usize,
}

pub struct App {
    pub query: String,
    filter: Filter,
    /// The query `view` was filtered with; `None` when it must be redone.
    filtered: Option<String>,
    pub sessions: Vec<Session>,
    /// Still being read.
    pub indexing: bool,
    pub view: Vec<Hit>,
    /// Selected position in `view`.
    pub selected: usize,
    /// Selected position in the turns column.
    pub row: usize,
    pub focus: Focus,
    pub offsets: [usize; 2],
    transcripts: VecDeque<Arc<Transcript>>,
    /// A transcript to read, for `main` to hand to the worker.
    request: Option<(SessionFile, Limits)>,
    /// The transcript last asked for, so it isn't asked for twice.
    asked: Option<(String, Limits)>,
    /// Tool output shown in full (`C-e`), in every session.
    pub expanded: bool,
    failed: Option<(String, String)>,
    wrapped: Option<Wrapped>,
    /// First transcript row on screen.
    pub scroll: usize,
    /// How far diffs are scrolled sideways, in columns.
    pub column: usize,
    /// The entry to bring to the top once the transcript is in and wrapped.
    follow: Option<usize>,
    pub transcript_height: usize,
    pub exit: Option<Exit>,
    pub notice: Option<String>,
    clipboard: Option<String>,
    pub no_color: bool,
    pub now: i64,
    pub splits: Splits,
    /// Where the last frame put the three panes, for the mouse.
    pub panes: [Rect; 3],
    pub body: Rect,
    resizing: Option<Resize>,
    seam_press: Option<(Seam, Instant)>,
}

impl App {
    pub fn new(files: Vec<SessionFile>) -> App {
        let sessions: Vec<_> = files.into_iter().map(Session::placeholder).collect();
        App {
            query: String::new(),
            filter: Filter::new(),
            filtered: None,
            indexing: !sessions.is_empty(),
            sessions,
            view: Vec::new(),
            selected: 0,
            row: 0,
            focus: Focus::Sessions,
            offsets: [0; 2],
            transcripts: VecDeque::new(),
            request: None,
            asked: None,
            expanded: false,
            failed: None,
            wrapped: None,
            scroll: 0,
            column: 0,
            follow: None,
            transcript_height: 0,
            exit: None,
            notice: None,
            clipboard: None,
            no_color: false,
            now: time::now(),
            splits: Splits::default(),
            panes: [Rect::default(); 3],
            body: Rect::default(),
            resizing: None,
            seam_press: None,
        }
    }

    pub fn update(&mut self, msg: Msg) {
        match msg {
            Msg::Input(Event::Key(key)) if key.kind != KeyEventKind::Release => {
                self.notice = None;
                self.handle_key(key);
            }
            Msg::Input(Event::Mouse(mouse)) => self.handle_mouse(mouse),
            Msg::Input(_) => {}
            Msg::Indexed(i, session) => {
                if let Some(slot) = self.sessions.get_mut(i) {
                    *slot = *session;
                    // A newly read session may match now; filter again.
                    if !self.query.trim().is_empty() {
                        self.filtered = None;
                    }
                }
            }
            Msg::IndexDone => self.indexing = false,
            Msg::Transcript(id, result) => match result {
                Ok(t) => {
                    self.transcripts
                        .retain(|old| old.id != id || old.limits != t.limits);
                    self.transcripts.push_front(t);
                    self.transcripts.truncate(TRANSCRIPT_CACHE);
                }
                Err(e) => self.failed = Some((id, e)),
            },
        }
    }

    /// Brings the view up to date with the query: once per frame, so a burst
    /// of keys costs one filter.
    pub fn settle(&mut self) {
        let query = self.query.trim().to_owned();
        if self.filtered.as_deref() == Some(query.as_str()) {
            return;
        }
        let keep = self.session().map(|s| s.file.id.clone());
        let narrows = self.filtered.is_some() && self.filter.set_query(&query);
        if !narrows {
            self.filter.set_query(&query);
        }
        let candidates: Vec<usize> = if narrows {
            let mut c: Vec<_> = self.view.iter().map(|h| h.session).collect();
            c.sort_unstable();
            c
        } else {
            (0..self.sessions.len()).collect()
        };
        self.view = filter::par_matches(&query, &self.sessions, &candidates);
        filter::sort(&mut self.view);
        self.filtered = Some(query);
        // The same session stays selected when it's still there.
        let at = keep.and_then(|id| {
            self.view
                .iter()
                .position(|h| self.sessions[h.session].file.id == id)
        });
        match at {
            Some(i) if i == self.selected => self.select_row(0),
            Some(i) => {
                self.selected = i;
                self.select_row(0);
            }
            None => self.select(0),
        }
    }

    pub fn hit(&self) -> Option<&Hit> {
        self.view.get(self.selected)
    }

    pub fn session(&self) -> Option<&Session> {
        self.hit().map(|h| &self.sessions[h.session])
    }

    /// The turns column: entry numbers of the selected session.
    pub fn rows(&self) -> Vec<usize> {
        match self.hit() {
            Some(Hit {
                entries: Some(entries),
                ..
            }) => entries.clone(),
            Some(hit) => self.sessions[hit.session].turn_starts(),
            None => Vec::new(),
        }
    }

    /// Matched chars of `text`, for highlighting.
    pub fn highlights(&mut self, text: &str, is_path: bool) -> Vec<usize> {
        self.filter.highlights(text, is_path)
    }

    fn select(&mut self, i: usize) {
        self.selected = i;
        self.select_row(0);
    }

    fn select_row(&mut self, row: usize) {
        self.row = row;
        self.follow = self.rows().get(row).copied();
        if self.follow.is_none() {
            self.scroll = 0;
        }
        self.ask_transcript();
    }

    fn limits(&self) -> Limits {
        if self.expanded {
            Limits::FULL
        } else {
            Limits::SCREEN
        }
    }

    fn ask_transcript(&mut self) {
        let Some(file) = self.session().map(|s| s.file.clone()) else {
            return;
        };
        let want = (file.id.clone(), self.limits());
        if self.asked.as_ref() != Some(&want) && self.cached(&file.id).is_none() {
            self.asked = Some(want);
            self.request = Some((file, self.limits()));
        }
    }

    fn cached(&self, id: &str) -> Option<&Arc<Transcript>> {
        let limits = self.limits();
        self.transcripts
            .iter()
            .find(|t| t.id == id && t.limits == limits)
    }

    /// A transcript for `main` to have read.
    pub fn take_request(&mut self) -> Option<(SessionFile, Limits)> {
        self.request.take()
    }

    /// The selected session's transcript: `Err` with a reason while it isn't
    /// there.
    pub fn transcript(&self) -> Result<&Arc<Transcript>, String> {
        let Some(session) = self.session() else {
            return Err(String::new());
        };
        let id = &session.file.id;
        if let Some(t) = self.cached(id) {
            return Ok(t);
        }
        match &self.failed {
            Some((failed, e)) if failed == id => Err(format!("couldn't read it: {e}")),
            _ => Err("reading…".into()),
        }
    }

    /// Fits the transcript to a pane `width` columns wide: wraps it again if
    /// the transcript or the width changed, and applies a pending jump to an
    /// entry.
    pub fn wrap_transcript(&mut self, width: usize) {
        let Ok(t) = self.transcript().cloned() else {
            return;
        };
        let stale = self
            .wrapped
            .as_ref()
            .is_none_or(|w| !Arc::ptr_eq(&w.t, &t) || w.width != width);
        if stale {
            let session = self.wrapped.as_ref().is_some_and(|w| w.t.id == t.id);
            // Keep the line on top when only the width changed.
            let top = self
                .wrapped
                .as_ref()
                .filter(|w| Arc::ptr_eq(&w.t, &t))
                .and_then(|w| w.rows.get(self.scroll))
                .map(|r| r.item);
            self.wrapped = Some(wrap(&t, width));
            if !session {
                self.column = 0;
            }
            if let Some(item) = top {
                self.scroll = self.wrapped.as_ref().unwrap().starts[item];
            } else if !session && self.follow.is_none() {
                self.scroll = 0;
            }
        }
        let wrapped = self.wrapped.as_ref().unwrap();
        if let Some(entry) = self.follow.take() {
            self.scroll = t.entries.get(entry).map_or(0, |&item| wrapped.starts[item]);
        }
        self.scroll = self.scroll.min(wrapped.rows.len().saturating_sub(1));
        self.column = self.column.min(wrapped.wide.saturating_sub(width));
    }

    /// The transcript's rows, as `wrap_transcript` last fitted them.
    pub fn wrapped_rows(&self) -> &[Row] {
        self.wrapped.as_ref().map_or(&[], |w| &w.rows)
    }

    /// The bytes of a row's line that are on screen: the row's part of it,
    /// or for a diff line, what the sideways scroll leaves in view.
    pub fn visible(&self, row: &Row) -> Range<usize> {
        let Some(w) = &self.wrapped else {
            return 0..0;
        };
        let item = &w.t.items[row.item];
        if !item.look.is_diff() {
            return row.span.clone();
        }
        let room = w.width.saturating_sub(row.indent);
        text::column_range(&item.text, self.column, self.column + room)
    }

    /// A row as it's shown, indent included.
    #[cfg(test)]
    pub fn row_text(&self, row: &Row) -> String {
        let Some(w) = &self.wrapped else {
            return String::new();
        };
        let text = &w.t.items[row.item].text[self.visible(row)];
        format!("{}{text}", " ".repeat(row.indent))
    }

    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard.take()
    }

    fn handle_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let page = self.transcript_height.max(1) as isize;
        let half = (page / 2).max(1);
        match key.code {
            KeyCode::Char('c') if ctrl => self.exit = Some(Exit::Quit),
            KeyCode::Char('n') if ctrl => self.move_in(self.focus, 1),
            KeyCode::Char('p') if ctrl => self.move_in(self.focus, -1),
            KeyCode::Char('d') if ctrl => self.scroll_by(half),
            KeyCode::Char('u') if ctrl => self.scroll_by(-half),
            KeyCode::Down if shift => self.jump_turn(true),
            KeyCode::Up if shift => self.jump_turn(false),
            KeyCode::Right if shift => self.scroll_sideways(SIDE_STEP),
            KeyCode::Left if shift => self.scroll_sideways(-SIDE_STEP),
            KeyCode::Down => self.move_in(self.focus, 1),
            KeyCode::Up => self.move_in(self.focus, -1),
            KeyCode::PageDown => self.scroll_by(page),
            KeyCode::PageUp => self.scroll_by(-page),
            KeyCode::Tab | KeyCode::Right => self.focus = self.focus.next(),
            KeyCode::BackTab | KeyCode::Left => self.focus = self.focus.prev(),
            KeyCode::Char('e') if ctrl => self.expand(),
            KeyCode::Char('y') if ctrl => {
                if let Some(id) = self.session().map(|s| s.file.id.clone()) {
                    self.notice = Some(format!("copied {}", short(&id)));
                    self.clipboard = Some(id);
                }
            }
            KeyCode::Enter => {
                if let Some(id) = self.session().map(|s| s.file.id.clone()) {
                    self.exit = Some(Exit::Pick(id));
                }
            }
            KeyCode::Esc if self.query.is_empty() => self.exit = Some(Exit::Quit),
            KeyCode::Esc => self.query.clear(),
            KeyCode::Backspace => {
                self.query.pop();
            }
            KeyCode::Char('w') if ctrl => {
                // Like a shell: the last word and the spaces after it.
                let kept = self
                    .query
                    .trim_end()
                    .trim_end_matches(|c: char| !c.is_whitespace());
                self.query.truncate(kept.len());
            }
            KeyCode::Char(c) if !ctrl => self.query.push(c),
            _ => {}
        }
    }

    fn move_in(&mut self, pane: Focus, delta: isize) {
        match pane {
            Focus::Sessions => {
                let i = step(self.selected, delta, self.view.len());
                if i != self.selected {
                    self.select(i);
                }
            }
            Focus::Turns => {
                let i = step(self.row, delta, self.rows().len());
                if i != self.row {
                    self.select_row(i);
                }
            }
            Focus::Transcript => self.scroll_by(delta),
        }
    }

    fn scroll_by(&mut self, delta: isize) {
        let rows = self.wrapped.as_ref().map_or(0, |w| w.rows.len());
        self.scroll = step(self.scroll, delta, rows);
    }

    /// Shows tool output in full, or cut again, keeping the entry on top
    /// in view.
    fn expand(&mut self) {
        self.expanded = !self.expanded;
        self.notice = Some(
            if self.expanded {
                "all output"
            } else {
                "output cut"
            }
            .into(),
        );
        if let Some(w) = &self.wrapped
            && let Some(row) = w.rows.get(self.scroll)
        {
            // The entry the top line belongs to: both versions have it.
            self.follow = w.t.items[..=row.item].iter().rev().find_map(|i| i.entry);
        }
        self.ask_transcript();
    }

    /// Moves diffs sideways, no further than it takes to show the end of the
    /// widest one.
    fn scroll_sideways(&mut self, delta: isize) {
        let last = self
            .wrapped
            .as_ref()
            .map_or(0, |w| w.wide.saturating_sub(w.width));
        self.column = step(self.column, delta, last + 1);
    }

    /// Puts the next (or previous) prompt of yours at the top.
    fn jump_turn(&mut self, forward: bool) {
        let (Ok(t), Some(w)) = (self.transcript().cloned(), self.wrapped.as_ref()) else {
            return;
        };
        let starts = t
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.look == Look::You)
            .map(|(i, _)| w.starts[i]);
        let target = if forward {
            starts.into_iter().find(|&r| r > self.scroll)
        } else {
            starts.into_iter().rev().find(|&r| r < self.scroll)
        };
        if let Some(r) = target {
            self.scroll = r;
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let at = Position::new(mouse.column, mouse.row);
        match (mouse.kind, self.resizing) {
            (MouseEventKind::Drag(MouseButton::Left), Some(resize)) => {
                return self.splits.drag(self.body, self.panes, resize, at);
            }
            (MouseEventKind::Up(MouseButton::Left), Some(_)) => {
                self.resizing = None;
                return;
            }
            (MouseEventKind::Down(MouseButton::Left), _) => {
                if let Some(resize) = Splits::seam_at(self.body, self.panes, at) {
                    return self.press_seam(resize);
                }
            }
            _ => {}
        }
        let Some(i) = self.panes.iter().position(|p| p.contains(at)) else {
            return;
        };
        let pane = [Focus::Sessions, Focus::Turns, Focus::Transcript][i];
        let transcript = pane == Focus::Transcript;
        // Some terminals send Shift+wheel as a vertical scroll with Shift held.
        let sideways = mouse.modifiers.contains(KeyModifiers::SHIFT);
        match mouse.kind {
            MouseEventKind::ScrollRight if transcript => self.scroll_sideways(SIDE_STEP),
            MouseEventKind::ScrollLeft if transcript => self.scroll_sideways(-SIDE_STEP),
            MouseEventKind::ScrollDown if transcript && sideways => self.scroll_sideways(SIDE_STEP),
            MouseEventKind::ScrollUp if transcript && sideways => self.scroll_sideways(-SIDE_STEP),
            MouseEventKind::ScrollDown if pane == Focus::Transcript => self.scroll_by(WHEEL_LINES),
            MouseEventKind::ScrollUp if pane == Focus::Transcript => self.scroll_by(-WHEEL_LINES),
            MouseEventKind::ScrollDown => self.move_in(pane, 1),
            MouseEventKind::ScrollUp => self.move_in(pane, -1),
            MouseEventKind::Down(MouseButton::Left) => {
                self.focus = pane;
                self.notice = None;
                // Clicks count only between the pane's borders.
                let Some(inner) = inside(self.panes[i]).filter(|r| r.contains(at)) else {
                    return;
                };
                let row = (mouse.row - inner.y) as usize;
                let (current, len) = match pane {
                    Focus::Sessions => (self.selected, self.view.len()),
                    Focus::Turns => (self.row, self.rows().len()),
                    Focus::Transcript => return,
                };
                let target = self.offsets[i] + row;
                if target < len {
                    self.move_in(pane, target as isize - current as isize);
                }
            }
            _ => {}
        }
    }

    /// Starts dragging a seam, or on a second press soon after the first,
    /// puts the layout back to its defaults.
    fn press_seam(&mut self, resize: Resize) {
        let now = Instant::now();
        let double = self
            .seam_press
            .is_some_and(|(seam, at)| seam == resize.seam && now - at < DOUBLE_CLICK);
        if double {
            self.splits.reset(self.body);
            self.seam_press = None;
        } else {
            self.resizing = Some(resize);
            self.seam_press = Some((resize.seam, now));
        }
    }
}

/// The first eight characters of a session id, as git shortens a sha.
pub fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Whether an entry is a turn's first line, as the turns column shows it.
pub fn is_turn(session: &Session, entry: usize) -> bool {
    session
        .entries
        .get(entry)
        .is_some_and(|e| e.depth == 0 && matches!(e.kind, Kind::Prompt(_)))
}

fn wrap(t: &Arc<Transcript>, width: usize) -> Wrapped {
    let mut rows = Vec::new();
    let mut starts = Vec::with_capacity(t.items.len());
    let mut wide = 0;
    for (i, item) in t.items.iter().enumerate() {
        starts.push(rows.len());
        // Keep at least a few columns for the text, however deep it sits.
        let indent = item.indent.min(width.saturating_sub(8));
        if item.look.is_diff() {
            wide = wide.max(indent + text::columns(&item.text));
            rows.push(Row {
                item: i,
                span: 0..item.text.len(),
                indent,
            });
            continue;
        }
        for span in text::wrap(&item.text, width - indent) {
            rows.push(Row {
                item: i,
                span,
                indent,
            });
        }
    }
    Wrapped {
        t: t.clone(),
        width,
        rows,
        starts,
        wide,
    }
}

fn inside(pane: Rect) -> Option<Rect> {
    (pane.width > 2 && pane.height > 2).then(|| Rect {
        x: pane.x + 1,
        y: pane.y + 1,
        width: pane.width - 2,
        height: pane.height - 2,
    })
}

/// Moves `idx` by `delta` within `0..len`, clamping at the ends.
fn step(idx: usize, delta: isize, len: usize) -> usize {
    idx.saturating_add_signed(delta).min(len.saturating_sub(1))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::index;
    use crate::store;
    use crate::testutil::{self, Claude};
    use crate::transcript;
    use serde_json::json;

    /// An app with three sessions read, and transcripts served on request.
    pub fn loaded(claude: &Claude) -> App {
        let files = store::discover(&claude.projects()).unwrap();
        let mut app = App::new(files.clone());
        for (i, f) in files.into_iter().enumerate() {
            app.update(Msg::Indexed(i, Box::new(index::Session::load(f).unwrap())));
        }
        app.update(Msg::IndexDone);
        app.settle();
        serve(&mut app);
        app
    }

    pub fn serve(app: &mut App) {
        while let Some((file, limits)) = app.take_request() {
            let t = transcript::load(&file, limits).map_err(|e| e.to_string());
            app.update(Msg::Transcript(file.id.clone(), t.map(Arc::new)));
        }
    }

    pub fn three(claude: &Claude) {
        claude.session(
            "-w-paradis",
            "11111111-aaaa",
            &[
                claude.prompt("Reinforce the gate", "2026-01-01T10:00:00Z"),
                claude.tool("t1", "Edit", json!({"file_path": "/w/paradis/src/gate.rs"})),
                claude.reply("Done."),
                claude.prompt("Now the hinges", "2026-01-01T10:05:00Z"),
                claude.reply("Oiled."),
            ],
        );
        claude.session(
            "-w-forest",
            "22222222-bbbb",
            &[
                testutil::prompt_in("Map the forest", "2026-01-02T10:00:00Z", "/w/forest"),
                testutil::title("Forest map"),
            ],
        );
        claude.session(
            "-w-sea",
            "33333333-cccc",
            &[testutil::prompt_in(
                "Reach the sea",
                "2026-01-03T10:00:00Z",
                "/w/sea",
            )],
        );
        claude.touch("-w-paradis/11111111-aaaa.jsonl", 3_000);
        claude.touch("-w-forest/22222222-bbbb.jsonl", 2_000);
        claude.touch("-w-sea/33333333-cccc.jsonl", 1_000);
    }

    pub fn key(app: &mut App, code: KeyCode) {
        app.update(Msg::Input(Event::Key(KeyEvent::new(
            code,
            KeyModifiers::NONE,
        ))));
        app.settle();
        serve(app);
    }

    pub fn ctrl(app: &mut App, c: char) {
        app.update(Msg::Input(Event::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::CONTROL,
        ))));
        app.settle();
    }

    pub fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            key(app, KeyCode::Char(c));
        }
    }

    fn titles(app: &App) -> Vec<&str> {
        app.view
            .iter()
            .map(|h| app.sessions[h.session].title.as_str())
            .collect()
    }

    #[test]
    fn sessions_newest_first_and_enter_picks() {
        let claude = Claude::new();
        three(&claude);
        let mut app = loaded(&claude);
        assert_eq!(
            titles(&app),
            ["Reinforce the gate", "Forest map", "Reach the sea"]
        );
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.exit, Some(Exit::Pick("22222222-bbbb".into())));
    }

    #[test]
    fn search_narrows_sessions_and_turns() {
        let claude = Claude::new();
        three(&claude);
        let mut app = loaded(&claude);
        typed(&mut app, "hinges");
        assert_eq!(titles(&app), ["Reinforce the gate"]);
        assert_eq!(app.rows(), [3], "only the matching entry");
        typed(&mut app, "zzz");
        assert!(app.view.is_empty());
        ctrl(&mut app, 'w');
        assert_eq!(app.query, "");
        assert_eq!(app.view.len(), 3);
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.exit, Some(Exit::Quit));
    }

    #[test]
    fn the_transcript_follows_the_selected_turn() {
        let claude = Claude::new();
        three(&claude);
        let mut app = loaded(&claude);
        assert_eq!(app.rows(), [0, 3]);
        app.wrap_transcript(60);
        let rows: Vec<String> = app.wrapped_rows().iter().map(|r| app.row_text(r)).collect();
        assert!(rows[0].starts_with("❯ you"));
        assert_eq!(app.scroll, 0);
        app.focus = Focus::Turns;
        key(&mut app, KeyCode::Down);
        app.wrap_transcript(60);
        let second = rows.iter().rposition(|r| r.starts_with("❯ you")).unwrap();
        assert_eq!(app.scroll, second);
        // Shift-Up goes back a turn.
        app.update(Msg::Input(Event::Key(KeyEvent::new(
            KeyCode::Up,
            KeyModifiers::SHIFT,
        ))));
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn the_selection_stays_on_its_session_while_typing() {
        let claude = Claude::new();
        three(&claude);
        let mut app = loaded(&claude);
        key(&mut app, KeyCode::Down);
        typed(&mut app, "map");
        assert_eq!(app.session().unwrap().title, "Forest map");
    }

    #[test]
    fn copy_the_id() {
        let claude = Claude::new();
        three(&claude);
        let mut app = loaded(&claude);
        ctrl(&mut app, 'y');
        assert_eq!(app.take_clipboard().as_deref(), Some("11111111-aaaa"));
        assert_eq!(app.notice.as_deref(), Some("copied 11111111"));
    }

    #[test]
    fn sessions_still_being_read_wait_for_search() {
        let claude = Claude::new();
        three(&claude);
        let files = store::discover(&claude.projects()).unwrap();
        let mut app = App::new(files.clone());
        app.query = "forest".into();
        app.settle();
        assert!(app.view.is_empty());
        app.update(Msg::Indexed(
            1,
            Box::new(index::Session::load(files[1].clone()).unwrap()),
        ));
        app.settle();
        assert_eq!(titles(&app), ["Forest map"]);
    }

    #[test]
    fn wrapping_keeps_indents_and_entry_starts() {
        let claude = Claude::new();
        three(&claude);
        let mut app = loaded(&claude);
        app.wrap_transcript(14);
        let rows = app.wrapped_rows().to_vec();
        // "  Reinforce the gate" in 12 columns: the indent repeats.
        assert_eq!(app.row_text(&rows[1]), "  Reinforce ");
        assert_eq!(app.row_text(&rows[2]), "  the gate");
        assert_eq!(rows[1].item, rows[2].item);
    }

    fn shift(app: &mut App, code: KeyCode) {
        app.update(Msg::Input(Event::Key(KeyEvent::new(
            code,
            KeyModifiers::SHIFT,
        ))));
    }

    /// A session whose one edit adds a line 51 columns wide.
    pub fn wide(claude: &Claude) {
        let long = format!("+{}", "x".repeat(50));
        claude.session(
            "-w-wall",
            "44444444-dddd",
            &[
                claude.prompt("Widen the wall to the sea", "2026-01-04T10:00:00Z"),
                claude.tool("t1", "Edit", json!({"file_path": "/w/wall/a.rs"})),
                testutil::result(
                    "t1",
                    "ok",
                    json!({"filePath": "/w/wall/a.rs", "structuredPatch": [{"oldStart": 1,
                        "oldLines": 1, "newStart": 1, "newLines": 1, "lines": ["-pin(3);", "+pin(5);", long]}]}),
                ),
            ],
        );
    }

    #[test]
    fn diffs_scroll_sideways_while_prose_wraps() {
        let claude = Claude::new();
        three(&claude);
        // The newest, so it's selected.
        wide(&claude);
        let mut app = loaded(&claude);
        app.wrap_transcript(20);
        let rows = app.wrapped_rows().to_vec();
        let shown = |app: &App| -> Vec<String> { rows.iter().map(|r| app.row_text(r)).collect() };
        let before = shown(&app);
        let x = format!("    +{}", "x".repeat(15));
        assert!(before.contains(&x), "{before:?}");
        // The diff line stays one row; the prompt wraps.
        assert_eq!(before.iter().filter(|r| r.contains('x')).count(), 1);
        assert_eq!(before[1..3], ["  Widen the wall to ", "  the sea"]);

        shift(&mut app, KeyCode::Right);
        assert_eq!(app.column, 8);
        let after = shown(&app);
        assert!(
            after.contains(&format!("    {}", "x".repeat(16))),
            "{after:?}"
        );
        assert_eq!(after[1..3], before[1..3], "prose doesn't move");

        // No further than the end of the widest line: 4 + 51 columns, in 20.
        for _ in 0..10 {
            shift(&mut app, KeyCode::Right);
        }
        assert_eq!(app.column, 35);
        shift(&mut app, KeyCode::Left);
        assert_eq!(app.column, 27);

        // Another session starts at the left edge.
        app.focus = Focus::Sessions;
        key(&mut app, KeyCode::Down);
        app.wrap_transcript(20);
        assert_eq!(app.column, 0);
    }

    #[test]
    fn all_output_and_back_keeps_the_place() {
        let claude = Claude::new();
        let long: Vec<String> = (0..30).map(|n| format!("line {n}")).collect();
        let mut lines = Vec::new();
        for (n, at) in ["10:00", "10:01", "10:02"].iter().enumerate() {
            let id = format!("t{n}");
            lines.push(claude.prompt(
                &format!("Count to 30, take {n}"),
                &format!("2026-01-01T{at}:00Z"),
            ));
            lines.push(claude.tool(&id, "Bash", json!({"command": "seq 30"})));
            lines.push(testutil::result(&id, &long.join("\n"), json!({})));
        }
        claude.session("-w", "55555555-eeee", &lines);
        let mut app = loaded(&claude);
        app.focus = Focus::Turns;
        key(&mut app, KeyCode::Down);
        app.wrap_transcript(60);
        let top = |app: &App| app.row_text(&app.wrapped_rows()[app.scroll + 1]);
        assert_eq!(top(&app), "  Count to 30, take 1");
        let cut = app.wrapped_rows().len();

        ctrl(&mut app, 'e');
        assert_eq!(app.notice.as_deref(), Some("all output"));
        serve(&mut app);
        app.wrap_transcript(60);
        assert_eq!(app.wrapped_rows().len(), cut + 3 * 24 - 3);
        assert_eq!(top(&app), "  Count to 30, take 1");

        // And back.
        ctrl(&mut app, 'e');
        serve(&mut app);
        app.wrap_transcript(60);
        assert_eq!(app.wrapped_rows().len(), cut);
        assert_eq!(top(&app), "  Count to 30, take 1");
    }
}
