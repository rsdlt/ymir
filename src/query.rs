//! Splits what's typed in the search bar into the fuzzy filter and slash
//! commands, as levi does.
//!
//! - `/h` anywhere keeps only the sessions that worked here.
//! - `/c` starts a literal search: everything after it, spaces included, is
//!   the text to look for. Everything before it stays fuzzy.
//!
//! Commands are whole words, so `src/config` or `/home` stay fuzzy text.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub fuzzy: String,
    /// Only sessions that worked in the current directory or its repository.
    pub here: bool,
    /// Text the sessions must contain, if a literal search was asked for.
    pub code: Option<String>,
}

pub fn parse(raw: &str) -> Query {
    let (before, code) = match find_word(raw, "/c") {
        Some(at) => {
            let pattern = raw[at + 2..].trim_start();
            (
                &raw[..at],
                (!pattern.is_empty()).then(|| pattern.to_owned()),
            )
        }
        None => (raw, None),
    };

    let mut here = false;
    let mut fuzzy = String::new();
    let mut rest = before;
    while let Some(at) = find_word(rest, "/h") {
        here = true;
        fuzzy.push_str(&rest[..at]);
        fuzzy.push(' ');
        rest = &rest[at + 2..];
    }
    fuzzy.push_str(rest);

    Query {
        fuzzy: fuzzy.split_whitespace().collect::<Vec<_>>().join(" "),
        here,
        code,
    }
}

/// Byte offset of `word` standing alone: at the start or after whitespace, and
/// at the end or before whitespace.
fn find_word(text: &str, word: &str) -> Option<usize> {
    text.match_indices(word).map(|(at, _)| at).find(|&at| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        before.is_none_or(char::is_whitespace) && after.is_none_or(char::is_whitespace)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(fuzzy: &str, here: bool, code: Option<&str>) -> Query {
        Query {
            fuzzy: fuzzy.into(),
            here,
            code: code.map(Into::into),
        }
    }

    #[test]
    fn plain_query_is_all_fuzzy() {
        assert_eq!(parse("gate fix"), q("gate fix", false, None));
        assert_eq!(parse(""), Query::default());
    }

    #[test]
    fn here_anywhere() {
        assert_eq!(parse("/h"), q("", true, None));
        assert_eq!(parse("/h gate"), q("gate", true, None));
        assert_eq!(parse("gate /h"), q("gate", true, None));
        assert_eq!(parse("a /h b"), q("a b", true, None));
        assert_eq!(parse("a/h b"), q("a/h b", false, None));
    }

    #[test]
    fn code_takes_the_rest_of_the_line() {
        assert_eq!(
            parse("gate /c pin(3) is short"),
            q("gate", false, Some("pin(3) is short"))
        );
        assert_eq!(parse("/c  fn main"), q("", false, Some("fn main")));
        // `/h` after `/c` is part of the text, not a command.
        assert_eq!(parse("/c a /h b"), q("", false, Some("a /h b")));
        assert_eq!(parse("/h /c foo"), q("", true, Some("foo")));
    }

    #[test]
    fn empty_code_search_is_no_code_search() {
        assert_eq!(parse("/c"), q("", false, None));
        assert_eq!(parse("x /c   "), q("x", false, None));
    }

    #[test]
    fn commands_are_whole_words() {
        assert_eq!(parse("src/config"), q("src/config", false, None));
        assert_eq!(parse("/cache /home"), q("/cache /home", false, None));
    }
}
