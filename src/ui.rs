//! Rendering: search bar on top, sessions | turns | transcript below, or on
//! narrow terminals sessions | turns above the transcript.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};

use std::ops::RangeInclusive;

use crate::app::{self, App, Focus};
use crate::index::Kind;
use crate::record::PromptKind;
use crate::text;
use crate::time;
use crate::transcript::{Item, Look};

const ACCENT: Color = Color::Cyan;

/// The bottom row's key hints, most useful first. Those that don't fit the
/// terminal's width are dropped from the end.
const HINTS: [(&str, &str); 10] = [
    ("↑↓", "move"),
    ("enter", "pick"),
    ("tab", "pane"),
    ("esc", "clear/quit"),
    ("C-y", "copy id"),
    ("C-e", "all output"),
    ("pgup/pgdn", "scroll"),
    ("S-↑↓", "turn"),
    ("S-←→", "sideways"),
    ("C-w", "delete word"),
];

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [search, body, hints] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    app.body = body;
    let [sessions, turns, transcript] = app.splits.areas(body);
    app.panes = [sessions, turns, transcript];

    draw_search(frame, app, search);
    draw_sessions(frame, app, sessions);
    draw_turns(frame, app, turns);
    draw_transcript(frame, app, transcript);
    draw_hints(frame, hints);
    if app.no_color {
        strip_colors(frame.buffer_mut());
    }
}

/// Drops every color. What a background color marked, like the selection,
/// is shown in reverse video instead, so it still stands out.
fn strip_colors(buf: &mut Buffer) {
    for cell in &mut buf.content {
        if cell.bg != Color::Reset {
            cell.modifier.insert(Modifier::REVERSED);
        }
        cell.fg = Color::Reset;
        cell.bg = Color::Reset;
    }
}

fn draw_hints(frame: &mut Frame, area: Rect) {
    let mut spans = vec![Span::raw(" ")];
    let mut width = 1;
    for (key, action) in HINTS {
        let hint = [
            Span::styled(key, Style::new().fg(ACCENT).bold()),
            Span::raw(" "),
            Span::styled(action, Style::new().dim()),
            Span::raw("  "),
        ];
        // The trailing gap may run past the edge; the hint itself must not.
        let needed: usize = hint[..3].iter().map(Span::width).sum();
        if width + needed > area.width as usize {
            break;
        }
        width += needed + hint[3].width();
        spans.extend(hint);
    }
    frame.render_widget(Line::from(spans), area);
}

fn pane(title: String, focused: bool) -> Block<'static> {
    let block = Block::bordered().title(title);
    if focused {
        block
            .border_style(Style::new().fg(ACCENT))
            .title_style(Modifier::BOLD)
    } else {
        block.border_style(Style::new().dark_gray())
    }
}

fn highlight(focused: bool) -> Style {
    if focused {
        Style::new()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::new().add_modifier(Modifier::REVERSED | Modifier::DIM)
    }
}

fn draw_search(frame: &mut Frame, app: &App, area: Rect) {
    let line = Line::from(vec![
        Span::styled("> ", Style::new().fg(ACCENT).bold()),
        Span::raw(&app.query),
    ]);
    // The terminal's own cursor, after the query, so it takes the user's
    // cursor style and input methods know where typing goes.
    if area.width > 2 && area.height > 2 {
        let x = area.x + 1 + line.width() as u16;
        frame.set_cursor_position((x.min(area.right() - 2), area.y + 1));
    }
    let total = app.sessions.len();
    let count = if app.query.trim().is_empty() {
        total.to_string()
    } else {
        format!("{}/{total}", app.view.len())
    };
    let mut status = Line::from(format!(" {count} sessions "));
    if app.indexing {
        let read = app.sessions.iter().filter(|s| s.ready).count();
        status.push_span(Span::styled(
            format!("· reading {read}/{total} "),
            Style::new().fg(ACCENT),
        ));
    }
    if let Some(notice) = &app.notice {
        status.push_span(Span::styled(
            format!("· {notice} "),
            Style::new().fg(ACCENT),
        ));
    }
    let block = Block::bordered()
        .title(Line::from(vec![
            Span::raw(" "),
            "ymir".bold(),
            Span::raw(" "),
        ]))
        .title_bottom(status.right_aligned())
        .border_style(Style::new().dark_gray());
    frame.render_widget(Paragraph::new(line).block(block), area);
}

/// Scrolls `offset` just enough to keep `selected` within `height` rows.
fn follow(offset: &mut usize, selected: usize, height: usize) {
    if selected < *offset || height == 0 {
        *offset = selected;
    } else if selected >= *offset + height {
        *offset = selected + 1 - height;
    }
}

/// Shows a list row the mouse dragged over in reverse video.
fn mark<'a>(item: ListItem<'a>, rows: &Option<RangeInclusive<usize>>, i: usize) -> ListItem<'a> {
    if rows.as_ref().is_some_and(|r| r.contains(&i)) {
        item.style(Style::new().add_modifier(Modifier::REVERSED))
    } else {
        item
    }
}

/// Renders rows `offset..` of a list whose items were built for that
/// window only, so cost doesn't grow with the number of sessions.
fn draw_window(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    items: Vec<ListItem>,
    row: usize,
    focused: bool,
) {
    let list = List::new(items)
        .block(block)
        .highlight_style(highlight(focused));
    let mut state = ListState::default().with_selected(Some(row));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_sessions(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sessions;
    let height = area.height.saturating_sub(2) as usize;
    follow(&mut app.offsets[0], app.selected, height);
    let offset = app.offsets[0];
    let end = (offset + height).min(app.view.len());
    let marked = app.selection.and_then(|s| s.rows(Focus::Sessions));
    let mut items = Vec::with_capacity(end - offset);
    for i in offset..end {
        let s = &app.sessions[app.view[i].session];
        let when = time::short(s.when(), app.now);
        let mut spans = vec![Span::styled(
            format!("{when:>5} "),
            Style::new().dark_gray(),
        )];
        let (name, title) = (s.name.clone(), s.title.clone());
        if s.ready {
            let name_marks = app.highlights(&name, false);
            let title_marks = app.highlights(&title, false);
            spans.extend(highlighted(
                &name,
                &name_marks,
                Style::new().fg(Color::Yellow),
            ));
            spans.push(Span::raw(" "));
            spans.extend(highlighted(&title, &title_marks, Style::new()));
        } else {
            spans.push(Span::styled("…", Style::new().dark_gray()));
        }
        let item = ListItem::new(Line::from(spans_owned(spans)));
        items.push(mark(item, &marked, i));
    }
    let title = " sessions ".to_owned();
    draw_window(
        frame,
        area,
        pane(title, focused),
        items,
        app.selected.saturating_sub(offset),
        focused,
    );
}

fn draw_turns(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Turns;
    let rows = app.rows();
    let height = area.height.saturating_sub(2) as usize;
    follow(&mut app.offsets[1], app.row, height);
    let offset = app.offsets[1];
    let end = (offset + height).min(rows.len());
    let marked = app.selection.and_then(|s| s.rows(Focus::Turns));
    let mut items = Vec::new();
    if let Some(hit) = app.hit().cloned() {
        let session = app.sessions[hit.session].clone();
        for (i, &e) in (offset..end).zip(&rows[offset..end]) {
            let entry = &session.entries[e];
            let mut spans = vec![Span::styled(
                format!("{:>3} ", entry.turn),
                Style::new().dark_gray(),
            )];
            let (lead, style) = match entry.kind {
                _ if app::is_turn(&session, e) => {
                    let clock = entry.time.map(|t| time::local(t).clock());
                    (format!("{} ", clock.unwrap_or_default()), Style::new())
                }
                _ if entry.depth > 0 => ("  ↳ ".into(), Style::new().dark_gray()),
                Kind::Prompt(_) => ("  ".into(), Style::new()),
                Kind::Reply => ("  ● ".into(), Style::new()),
                Kind::Tool { .. } => ("  ▸ ".into(), Style::new().fg(Color::Yellow)),
            };
            spans.push(Span::styled(lead, Style::new().dark_gray()));
            if let Some(tool) = &entry.tool {
                spans.push(Span::styled(format!("{tool} "), style));
            }
            let line = match entry.kind {
                Kind::Prompt(PromptKind::Shell) => format!("! {}", entry.line()),
                _ => entry.line().to_owned(),
            };
            let is_path = matches!(entry.kind, Kind::Tool { is_path: true });
            let marks = app.highlights(&line, is_path);
            spans.extend(highlighted(&line, &marks, Style::new()));
            let item = ListItem::new(Line::from(spans_owned(spans)));
            items.push(mark(item, &marked, i));
        }
    }
    let title = match app.hit() {
        Some(h) if h.entries.is_some() => format!(" {} matches ", rows.len()),
        _ => format!(" {} turns ", rows.len()),
    };
    draw_window(
        frame,
        area,
        pane(title, focused),
        items,
        app.row.saturating_sub(offset),
        focused,
    );
}

fn draw_transcript(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Transcript;
    let inner = Block::bordered().inner(area);
    app.transcript_height = inner.height as usize;
    let transcript = app.transcript().cloned();
    let lines: Vec<Line> = match &transcript {
        Err(reason) => vec![Line::styled(reason.as_str(), Style::new().dark_gray())],
        Ok(t) => {
            app.wrap_transcript(inner.width as usize);
            app.wrapped_rows()
                .iter()
                .skip(app.scroll)
                .take(inner.height as usize)
                .map(|r| row_line(&t.items[r.item], app.visible(r), r.indent))
                .collect()
        }
    };
    let mut title = match app.session() {
        Some(s) if s.ready => format!(" {} · {} ", app::short(&s.file.id), s.project),
        Some(s) => format!(" {} ", app::short(&s.file.id)),
        None => " transcript ".into(),
    };
    if app.expanded {
        title.push_str("· all output ");
    }
    if app.column > 0 {
        // Says the diffs are shifted, and by how much.
        title.push_str(&format!("· col {} ", app.column + 1));
    }
    let block = pane(title, focused);
    frame.render_widget(Paragraph::new(lines).block(block), area);
    mark_text(frame.buffer_mut(), app, inner);
}

/// Shows the transcript text dragged over in reverse video.
fn mark_text(buf: &mut Buffer, app: &App, inner: Rect) {
    let Some(sel) = app.selection else {
        return;
    };
    let Some(selected) = sel.rows(Focus::Transcript) else {
        return;
    };
    let (start, end) = sel.span();
    let rows = app.wrapped_rows();
    for (y, r) in (inner.y..inner.bottom()).zip(app.scroll..rows.len()) {
        if !selected.contains(&r) {
            continue;
        }
        let row = &rows[r];
        // Only the text, not the indent before it or the space after.
        let width = row.indent + text::columns(app.row_shown(row));
        let from = if r == start.row { start.col } else { 0 }.max(row.indent);
        let to = if r == end.row { end.col + 1 } else { width }.min(width);
        for x in from..to.min(inner.width as usize) {
            buf[(inner.x + x as u16, y)]
                .modifier
                .insert(Modifier::REVERSED);
        }
    }
}

/// The bytes `shown` of a transcript line, with a diff line's changed words
/// in reverse video, like `diff-highlight`.
fn row_line(item: &Item, shown: std::ops::Range<usize>, indent: usize) -> Line<'_> {
    let base = look_style(item.look);
    let mut spans = vec![Span::raw(" ".repeat(indent))];
    let mut cuts: Vec<usize> = item
        .emph
        .iter()
        .flat_map(|r| [r.start, r.end])
        .filter(|&at| shown.contains(&at))
        .chain([shown.start, shown.end])
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    for w in cuts.windows(2) {
        let changed = item.emph.iter().any(|r| r.contains(&w[0]));
        let style = if changed {
            base.add_modifier(Modifier::REVERSED)
        } else {
            base
        };
        spans.push(Span::styled(&item.text[w[0]..w[1]], style));
    }
    Line::from(spans)
}

fn look_style(look: Look) -> Style {
    match look {
        Look::You => Style::new().fg(ACCENT).bold(),
        Look::Prompt => Style::new().bold(),
        Look::Claude => Style::new().fg(Color::Magenta).bold(),
        Look::Reply => Style::new(),
        Look::Tool => Style::new().fg(Color::Yellow),
        Look::Output | Look::Context | Look::Notice => Style::new().dark_gray(),
        Look::Error => Style::new().fg(Color::Red),
        Look::More => Style::new().dark_gray().italic(),
        Look::File => Style::new().bold(),
        Look::Hunk => Style::new().fg(Color::Cyan),
        Look::Added => Style::new().fg(Color::Green),
        Look::Removed => Style::new().fg(Color::Red),
        Look::Divider => Style::new().dark_gray(),
        Look::Agent => Style::new().fg(Color::Blue),
        Look::Thinking => Style::new().dark_gray().italic(),
    }
}

fn matched(style: Style) -> Style {
    style
        .fg(Color::Magenta)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
}

/// Splits `text` into spans, marking the chars at `positions` (sorted).
fn highlighted<'a>(text: &'a str, positions: &[usize], base: Style) -> Vec<Span<'a>> {
    if positions.is_empty() {
        return vec![Span::styled(text, base)];
    }
    let mut spans = Vec::new();
    let mut start = 0;
    let mut current = false;
    let mut next = positions.iter().peekable();
    for (n, (byte, _)) in text.char_indices().enumerate() {
        let is_match = next.next_if(|&&p| p == n).is_some();
        if is_match != current && byte > start {
            let style = if current { matched(base) } else { base };
            spans.push(Span::styled(&text[start..byte], style));
            start = byte;
        }
        current = is_match;
    }
    let style = if current { matched(base) } else { base };
    spans.push(Span::styled(&text[start..], style));
    spans
}

fn spans_owned(spans: Vec<Span<'_>>) -> Vec<Span<'static>> {
    spans
        .into_iter()
        .map(|s| Span::styled(s.content.into_owned(), s.style))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Msg;
    use crate::app::tests::{key, loaded, three, typed, wide};
    use crate::testutil::Claude;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{
        Event, KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    fn render_at(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn three_panes_with_sessions_turns_and_transcript() {
        let claude = Claude::new();
        three(&claude);
        let mut app = loaded(&claude);
        let screen = render_at(&mut app, 140, 20);
        assert!(screen.contains("3 sessions"), "{screen}");
        assert!(screen.contains("paradis Reinforce the gate"), "{screen}");
        assert!(screen.contains("forest Forest map"), "{screen}");
        assert!(screen.contains(" 2 turns "), "{screen}");
        assert!(screen.contains("Now the hinges"), "{screen}");
        assert!(screen.contains("▸ Edit src/gate.rs"), "{screen}");
        assert!(screen.contains("11111111 · /w/paradis"), "{screen}");
    }

    #[test]
    fn matches_replace_turns() {
        let claude = Claude::new();
        three(&claude);
        let mut app = loaded(&claude);
        typed(&mut app, "gate.rs");
        let screen = render_at(&mut app, 140, 20);
        assert!(screen.contains(" 1 matches "), "{screen}");
        assert!(screen.contains("▸ Edit src/gate.rs"), "{screen}");
        assert!(screen.contains("1/3 sessions"), "{screen}");
    }

    #[test]
    fn narrow_terminals_stack_and_tiny_ones_dont_crash() {
        let claude = Claude::new();
        three(&claude);
        let mut app = loaded(&claude);
        let screen = render_at(&mut app, 80, 30);
        assert!(screen.contains("Reinforce"), "{screen}");
        for (w, h) in [(0, 0), (1, 1), (10, 4), (30, 6)] {
            render_at(&mut app, w, h);
            key(&mut app, KeyCode::Down);
        }
    }

    #[test]
    fn changed_words_are_marked() {
        let claude = Claude::new();
        wide(&claude);
        let mut app = loaded(&claude);
        let mut terminal = Terminal::new(TestBackend::new(140, 20)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let reversed = |want: &str| -> Vec<bool> {
            let y = (0..buf.area.height)
                .find(|&y| {
                    let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
                    row.contains(want)
                })
                .unwrap();
            let row: Vec<_> = (0..buf.area.width).map(|x| &buf[(x, y)]).collect();
            let at = row
                .windows(want.len())
                .position(|w| w.iter().map(|c| c.symbol()).collect::<String>() == want)
                .unwrap();
            row[at..at + want.len()]
                .iter()
                .map(|c| c.modifier.contains(Modifier::REVERSED))
                .collect()
        };
        let only_the_digit = [false, false, false, false, false, true, false, false];
        assert_eq!(reversed("-pin(3);"), only_the_digit);
        assert_eq!(reversed("+pin(5);"), only_the_digit);
    }

    fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
        app.update(Msg::Input(Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })));
    }

    const LEFT: MouseEventKind = MouseEventKind::Down(MouseButton::Left);
    const DRAG: MouseEventKind = MouseEventKind::Drag(MouseButton::Left);
    const UP: MouseEventKind = MouseEventKind::Up(MouseButton::Left);

    fn reversed(app: &mut App, x: u16, y: u16) -> bool {
        let mut terminal = Terminal::new(TestBackend::new(140, 20)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal.backend().buffer()[(x, y)]
            .modifier
            .contains(Modifier::REVERSED)
    }

    #[test]
    fn dragging_over_sessions_copies_them_as_filter_prints_them() {
        let claude = Claude::new();
        three(&claude);
        let mut app = loaded(&claude);
        render_at(&mut app, 140, 20);
        let sessions = app.panes[0];
        let (x, top) = (sessions.x + 3, sessions.y + 1);
        mouse(&mut app, LEFT, x, top);
        mouse(&mut app, DRAG, x + 2, top + 1);
        // Past the last session, the selection stops at it.
        mouse(&mut app, DRAG, x, top + 10);
        assert!(reversed(&mut app, x, top + 2), "shown while dragging");
        mouse(&mut app, UP, x, top + 10);
        let summaries: Vec<String> = app
            .view
            .iter()
            .map(|h| crate::plain::summary(&app.sessions[h.session]))
            .collect();
        assert_eq!(app.take_clipboard(), Some(summaries.join("\n")));
        assert_eq!(app.notice.as_deref(), Some("copied 3 sessions"));
        assert!(reversed(&mut app, x, top + 2), "and after, until a key");
        assert_eq!(app.selected, 0, "dragging doesn't move the selection");
        key(&mut app, KeyCode::Down);
        assert!(!reversed(&mut app, x, top + 2), "a key clears it");

        // A click, without moving, selects a row but copies nothing.
        mouse(&mut app, LEFT, x, top + 2);
        mouse(&mut app, UP, x, top + 2);
        assert_eq!(app.take_clipboard(), None);
        assert_eq!((app.selection, app.selected), (None, 2));
    }

    #[test]
    fn dragging_over_the_transcript_copies_the_text() {
        let claude = Claude::new();
        three(&claude);
        let mut app = loaded(&claude);
        render_at(&mut app, 140, 20);
        let pane = app.panes[2];
        let (x, top) = (pane.x + 1, pane.y + 1);
        // From the prompt's first letter to the tool's name.
        mouse(&mut app, LEFT, x + 2, top + 1);
        mouse(&mut app, DRAG, x + 5, top + 3);
        mouse(&mut app, UP, x + 5, top + 3);
        assert_eq!(
            app.take_clipboard().as_deref(),
            Some("Reinforce the gate\n● claude\n▸ Ed")
        );
        assert_eq!(app.notice.as_deref(), Some("copied 3 lines"));
        assert_eq!(app.focus, Focus::Transcript);
        assert!(reversed(&mut app, x + 2, top + 1));
        assert!(!reversed(&mut app, x + 1, top + 3), "not the indent");
        assert!(!reversed(&mut app, x + 30, top + 1), "nor past the text");
    }
}
