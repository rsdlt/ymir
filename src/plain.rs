//! Plain text for scripts and agents: `--filter`'s list and `--show`'s
//! transcript.

use std::io::{self, Write};

use crate::index::Session;
use crate::time;
use crate::transcript::{Look, Transcript};

/// `--filter`'s line for a session: `<id> <date> <project> <title>`.
pub fn summary(s: &Session) -> String {
    let date = time::local(s.when()).date();
    let project = if s.project.is_empty() {
        "-"
    } else {
        &s.project
    };
    format!("{} {date} {project} {}", s.file.id, s.title)
}

/// `--show`: a header, then the transcript, or one turn of it.
pub fn show(
    out: &mut impl Write,
    s: &Session,
    t: &Transcript,
    turn: Option<usize>,
) -> io::Result<()> {
    writeln!(out, "session {} \"{}\"", s.file.id, s.title)?;
    let mut place = Vec::new();
    if !s.project.is_empty() {
        place.push(format!("in {}", s.project));
    }
    if !s.branch.is_empty() {
        place.push(format!("on {}", s.branch));
    }
    if let (Some(start), Some(end)) = (s.started, s.last) {
        let (a, b) = (time::local(start), time::local(end));
        let span = if a.date() == b.date() {
            format!("{} {} to {}", a.date(), a.clock(), b.clock())
        } else {
            format!("{} {} to {} {}", a.date(), a.clock(), b.date(), b.clock())
        };
        place.push(span);
    }
    place.push(match t.turns {
        1 => "1 turn".into(),
        n => format!("{n} turns"),
    });
    writeln!(out, "{}", place.join(", "))?;
    if let Some(n) = turn {
        writeln!(out, "turn {n} of {}", t.turns)?;
    }
    writeln!(out)?;
    let mut items = t
        .items
        .iter()
        .filter(|i| turn.is_none_or(|n| i.turn == n))
        .peekable();
    // A turn starts with the blank line that separates it from the last.
    while items
        .next_if(|i| i.text.is_empty() && i.look == Look::Prompt)
        .is_some()
    {}
    for item in items {
        // An empty line gets no indent, so nothing trails it.
        let indent = if item.text.is_empty() { 0 } else { item.indent };
        writeln!(out, "{}{}", " ".repeat(indent), item.text)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::three;
    use crate::store;
    use crate::testutil::Claude;
    use crate::transcript::{self, Limits};

    fn shown(turn: Option<usize>) -> String {
        let claude = Claude::new();
        three(&claude);
        let file = store::discover(&claude.projects()).unwrap().remove(0);
        let s = Session::load(file.clone()).unwrap();
        let t = transcript::load(&file, Limits::PLAIN).unwrap();
        let mut out = Vec::new();
        show(&mut out, &s, &t, turn).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_whole_session() {
        let out = shown(None);
        let lines: Vec<_> = out.lines().collect();
        assert_eq!(lines[0], "session 11111111-aaaa \"Reinforce the gate\"");
        assert!(
            lines[1].starts_with("in /w/paradis, on main, 2026-01-01 "),
            "{out}"
        );
        assert!(lines[1].ends_with(", 2 turns"), "{out}");
        assert_eq!(lines[2], "");
        assert!(lines[3].starts_with("❯ you · "));
        assert!(out.contains("\n  ▸ Edit src/gate.rs\n"), "{out}");
        assert!(out.contains("\n  Oiled.\n"), "{out}");
    }

    #[test]
    fn one_turn() {
        let out = shown(Some(2));
        let lines: Vec<_> = out.lines().collect();
        assert_eq!(lines[2], "turn 2 of 2");
        assert!(lines[4].starts_with("❯ you · "), "{out}");
        assert_eq!(lines[5], "  Now the hinges");
        assert!(!out.contains("Reinforce the gate\n  "), "{out}");
        assert!(shown(Some(9)).ends_with("turn 9 of 2\n\n"));
    }
}
