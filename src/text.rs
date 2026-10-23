//! Text from sessions, made safe and fitted to the screen.

use std::borrow::Cow;
use std::ops::Range;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Tool output is full of terminal escapes (colors, cursor moves), and a
/// session can hold any bytes at all. Drawn as they are, they'd act on the
/// terminal. This drops escape sequences and other control characters,
/// and turns tabs into four spaces.
pub fn clean(text: &str) -> Cow<'_, str> {
    if !text.chars().any(|c| c.is_control() && c != '\n') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => out.push('\n'),
            '\t' => out.push_str("    "),
            '\x1b' => match chars.peek() {
                // CSI: parameters and intermediates, then one final byte.
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC and other strings: up to BEL or ESC \.
                Some(']' | 'P' | '_' | '^' | 'X') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                // Any intermediates (`ESC ( B`), then one final character.
                Some(_) => {
                    while chars.next_if(|c| ('\x20'..='\x2f').contains(c)).is_some() {}
                    chars.next();
                }
                None => {}
            },
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Splits `text` into rows of at most `width` columns, breaking after
/// whitespace where it can and inside a word where it must. Each row is a
/// byte range of `text`.
pub fn wrap(text: &str, width: usize) -> Vec<Range<usize>> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let (mut start, mut col) = (0, 0);
    // Where the row could break: just after whitespace, and its column there.
    let mut gap: Option<(usize, usize)> = None;
    for (i, c) in text.char_indices() {
        let w = c.width().unwrap_or(0);
        if col + w > width && i > start {
            let (cut, carried) = match gap {
                Some((at, at_col)) if at > start && at < i => (at, col - at_col),
                _ => (i, 0),
            };
            rows.push(start..cut);
            (start, col, gap) = (cut, carried, None);
        }
        col += w;
        if c.is_whitespace() {
            gap = Some((i + c.len_utf8(), col));
        }
    }
    rows.push(start..text.len());
    rows
}

/// Terminal columns `text` takes up.
pub fn columns(text: &str) -> usize {
    text.width()
}

/// The bytes of `text` whose chars start at display columns `from..to`.
pub fn column_range(text: &str, from: usize, to: usize) -> Range<usize> {
    let (mut start, mut end) = (None, text.len());
    let mut col = 0;
    for (i, c) in text.char_indices() {
        if col >= to {
            end = i;
            break;
        }
        if start.is_none() && col >= from {
            start = Some(i);
        }
        col += c.width().unwrap_or(0);
    }
    start.unwrap_or(end).min(end)..end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_untouched() {
        assert!(matches!(clean("tch\nlevi"), Cow::Borrowed("tch\nlevi")));
    }

    #[test]
    fn escapes_and_controls_go() {
        assert_eq!(clean("\x1b[1;31mred\x1b[0m done"), "red done");
        assert_eq!(clean("\x1b]52;c;aGk=\x07copied"), "copied");
        assert_eq!(clean("\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\"), "link");
        assert_eq!(clean("a\tb\r\n\x07c\x7f\u{9b}d"), "a    b\ncd");
        assert_eq!(clean("\x1b(Bplain\x1b"), "plain");
    }

    fn wrap(text: &str, width: usize) -> Vec<&str> {
        super::wrap(text, width)
            .into_iter()
            .map(|r| &text[r])
            .collect()
    }

    #[test]
    fn wraps_at_spaces() {
        assert_eq!(wrap("the wall is tall", 8), ["the ", "wall is ", "tall"]);
        assert_eq!(wrap("short", 10), ["short"]);
        assert_eq!(wrap("", 10), [""]);
    }

    #[test]
    fn long_words_break_where_they_must() {
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("go abcdefghij", 4), ["go ", "abcd", "efgh", "ij"]);
    }

    #[test]
    fn wide_characters_count_twice() {
        assert_eq!(wrap("巨人巨人巨", 4), ["巨人", "巨人", "巨"]);
        assert_eq!(wrap("ab巨", 3), ["ab", "巨"]);
    }

    #[test]
    fn column_ranges() {
        let cut = |text, from, to| &text[column_range(text, from, to)];
        assert_eq!(cut("+let x = 1;", 4, 7), " x ");
        assert_eq!(cut("abc", 5, 9), "");
        assert_eq!(
            cut("巨人巨", 1, 4),
            "人",
            "a wide char starts where it starts"
        );
        assert_eq!(cut("abc", 1, usize::MAX), "bc");
    }

    #[test]
    fn a_tiny_width_still_makes_progress() {
        assert_eq!(wrap("abc", 0), ["a", "b", "c"]);
        assert_eq!(wrap("巨", 1), ["巨"]);
    }
}
