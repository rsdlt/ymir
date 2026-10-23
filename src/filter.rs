//! fzf-style matching over sessions and their entries.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::index::{Entry, Kind, Session};

/// Shortest query treated as a session id prefix. Shorter hex strings are
/// too common in words.
const MIN_ID_PREFIX: usize = 4;

/// In prose, a fuzzy word's matched characters must lie within its length
/// plus this many. Without a limit, a word finds its letters scattered over
/// a long reply: on real sessions `focus` matched 63% of all entries, and
/// nonsense like `zzzzqqq` matched 77. With it, `focus` keeps the 194 that
/// say it (190 contain it exactly) and typos still match: `focs` 175,
/// `clipbord` 50.
const PROSE_SLACK: usize = 3;

/// A session that survived the filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Index into the session list.
    pub session: usize,
    pub score: u32,
    /// Indices of matching entries, or `None` when the session itself
    /// matched (then its turns are shown).
    pub entries: Option<Vec<usize>>,
}

impl Hit {
    pub fn all(session: usize) -> Hit {
        Hit {
            session,
            score: 0,
            entries: None,
        }
    }
}

/// What a session's header and id say before its entries are looked at.
enum Head {
    /// Matched outright: by id, or by an empty query.
    Hit(Hit),
    /// Not read yet, so nothing else can match.
    Unread,
    Score(Option<u32>),
}

/// A session's hit from its header's score and its matching entries.
fn combine(idx: usize, header: Option<u32>, entries: Vec<(usize, u32)>) -> Option<Hit> {
    let best = entries.iter().map(|&(_, s)| s).max();
    Some(Hit {
        session: idx,
        score: header.max(best)?,
        entries: header
            .is_none()
            .then(|| entries.into_iter().map(|(i, _)| i).collect()),
    })
}

pub struct Filter {
    query: String,
    pattern: Pattern,
    text: Matcher,
    paths: Matcher,
    buf: Vec<char>,
    hay: String,
    spots: Vec<u32>,
}

impl Filter {
    pub fn new() -> Filter {
        Filter {
            query: String::new(),
            pattern: Pattern::default(),
            text: Matcher::new(Config::DEFAULT),
            paths: Matcher::new(Config::DEFAULT.match_paths()),
            buf: Vec::new(),
            hay: String::new(),
            spots: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pattern.atoms.is_empty()
    }

    /// Replaces the query. Returns true if every match of the new query is
    /// guaranteed to be a match of the old one, so callers may refilter only
    /// the previous hits.
    pub fn set_query(&mut self, query: &str) -> bool {
        let narrows = narrows(&self.query, query);
        self.query = query.to_owned();
        self.pattern
            .reparse(query, CaseMatching::Smart, Normalization::Smart);
        narrows
    }

    /// Matches a session by id prefix, its header, or any of its entries:
    /// `par_matches` one session at a time, for tests.
    #[cfg(test)]
    pub fn matches(&mut self, idx: usize, session: &Session) -> Option<Hit> {
        let header = match self.header(idx, session) {
            Head::Hit(hit) => return Some(hit),
            Head::Unread => return None,
            Head::Score(score) => score,
        };
        let entries = (0..session.entries.len())
            .filter_map(|i| self.entry(&session.entries[i]).map(|s| (i, s)))
            .collect();
        combine(idx, header, entries)
    }

    /// The part of a match that doesn't look at entries.
    fn header(&mut self, idx: usize, session: &Session) -> Head {
        if self.is_empty() {
            return Head::Hit(Hit::all(idx));
        }
        let query = self.query.trim();
        if query.len() >= MIN_ID_PREFIX
            && query.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
            && session.file.id.starts_with(&query.to_ascii_lowercase())
        {
            return Head::Hit(Hit {
                session: idx,
                score: u32::MAX,
                entries: None,
            });
        }
        if !session.ready {
            return Head::Unread;
        }
        self.hay = session.header();
        Head::Score(score(
            &self.pattern,
            &mut self.text,
            &mut self.buf,
            &self.hay,
        ))
    }

    fn entry(&mut self, entry: &Entry) -> Option<u32> {
        match (&entry.tool, entry.kind) {
            // A path matches on its own, as a path; the tool's name with it,
            // so `edit main.rs` finds the edits.
            (Some(tool), Kind::Tool { is_path }) => {
                let alone = if is_path {
                    score(&self.pattern, &mut self.paths, &mut self.buf, &entry.text)
                } else {
                    None
                };
                self.hay.clear();
                self.hay.push_str(tool);
                self.hay.push(' ');
                self.hay.push_str(&entry.text);
                let whole = prose(
                    &self.pattern,
                    &mut self.text,
                    &mut self.buf,
                    &mut self.spots,
                    &self.hay,
                );
                alone.max(whole)
            }
            _ => prose(
                &self.pattern,
                &mut self.text,
                &mut self.buf,
                &mut self.spots,
                &entry.text,
            ),
        }
    }

    /// Char positions in `text` matched by the query, for highlighting.
    pub fn highlights(&mut self, text: &str, is_path: bool) -> Vec<usize> {
        if self.is_empty() {
            return Vec::new();
        }
        let matcher = if is_path {
            &mut self.paths
        } else {
            &mut self.text
        };
        let mut indices = Vec::new();
        self.pattern
            .indices(Utf32Str::new(text, &mut self.buf), matcher, &mut indices);
        indices.sort_unstable();
        indices.dedup();
        indices.into_iter().map(|i| i as usize).collect()
    }
}

fn score(pattern: &Pattern, matcher: &mut Matcher, buf: &mut Vec<char>, text: &str) -> Option<u32> {
    pattern.score(Utf32Str::new(text, buf), matcher)
}

/// Scores prose: like `score`, but every fuzzy word must match compactly
/// (see `PROSE_SLACK`). One pass per word gives both its score and where it
/// matched; a query's score is the sum of its words'.
fn prose(
    pattern: &Pattern,
    matcher: &mut Matcher,
    buf: &mut Vec<char>,
    spots: &mut Vec<u32>,
    text: &str,
) -> Option<u32> {
    let hay = Utf32Str::new(text, buf);
    let mut total = 0;
    for atom in &pattern.atoms {
        // Negations, exact and anchored words already match as written.
        if atom.negative || atom.kind != AtomKind::Fuzzy {
            total += u32::from(atom.score(hay, matcher)?);
            continue;
        }
        spots.clear();
        total += u32::from(atom.indices(hay, matcher, spots)?);
        let (first, last) = (spots.iter().min()?, spots.iter().max()?);
        if (last - first + 1) as usize > atom.needle_text().len() + PROSE_SLACK {
            return None;
        }
    }
    Some(total)
}

/// Whether everything `new` matches is guaranteed to match `old` as well.
pub fn narrows(old: &str, new: &str) -> bool {
    // An empty query matches everything. Appending only narrows: longer atoms
    // and extra atoms both match less. Negations (`!`), anchors (`$`) and
    // escapes can widen or reinterpret a query, so they never count.
    const SPECIAL: [char; 3] = ['!', '$', '\\'];
    old.trim().is_empty()
        || (!old.contains(SPECIAL)
            && new
                .strip_prefix(old)
                .is_some_and(|added| !added.contains(SPECIAL)))
}

/// Entries matched per unit of parallel work. Sessions differ wildly in
/// size (one has thousands of entries, most a few dozen), so work is cut
/// into slices of entries rather than whole sessions.
const SLICE: usize = 256;

/// Matches `candidates` (indices into `sessions`) against `query` across all
/// cores. Hits come back in candidate order.
pub fn par_matches(query: &str, sessions: &[Session], candidates: &[usize]) -> Vec<Hit> {
    // Each candidate's header first, then its entries, slice by slice.
    let mut units = Vec::new();
    for (pos, &i) in candidates.iter().enumerate() {
        units.push((pos, None));
        for start in (0..sessions[i].entries.len()).step_by(SLICE) {
            units.push((
                pos,
                Some(start..(start + SLICE).min(sessions[i].entries.len())),
            ));
        }
    }
    enum Done {
        Head(Head),
        Entries(Vec<(usize, u32)>),
    }
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    let next = AtomicUsize::new(0);
    let done: Vec<Vec<(usize, Done)>> = thread::scope(|s| {
        let workers: Vec<_> = (0..threads.min(units.len()))
            .map(|_| {
                s.spawn(|| {
                    // Matchers hold scratch buffers, so each thread gets its own.
                    let mut filter = Filter::new();
                    filter.set_query(query);
                    let mut out = Vec::new();
                    loop {
                        let u = next.fetch_add(1, Ordering::Relaxed);
                        let Some((pos, range)) = units.get(u) else {
                            break;
                        };
                        let session = &sessions[candidates[*pos]];
                        let result = match range {
                            None => Done::Head(filter.header(candidates[*pos], session)),
                            Some(range) => Done::Entries(
                                range
                                    .clone()
                                    .filter_map(|e| {
                                        filter.entry(&session.entries[e]).map(|s| (e, s))
                                    })
                                    .collect(),
                            ),
                        };
                        out.push((u, result));
                    }
                    out
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|w| w.join().expect("filter thread panicked"))
            .collect()
    });
    let mut results: Vec<Option<Done>> = (0..units.len()).map(|_| None).collect();
    for (u, result) in done.into_iter().flatten() {
        results[u] = Some(result);
    }
    // Units are in candidate order, each header before its entries.
    let mut hits = Vec::new();
    let mut units = units.iter().zip(results).peekable();
    while let Some(((pos, _), head)) = units.next() {
        let mut entries = Vec::new();
        while let Some((_, Some(Done::Entries(found)))) =
            units.next_if(|((p, range), _)| p == pos && range.is_some())
        {
            entries.extend(found);
        }
        let idx = candidates[*pos];
        match head {
            Some(Done::Head(Head::Hit(hit))) => hits.push(hit),
            Some(Done::Head(Head::Score(header))) => hits.extend(combine(idx, header, entries)),
            _ => {}
        }
    }
    hits
}

/// Best first; equal scores keep the session order (newest first).
pub fn sort(hits: &mut [Hit]) {
    hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.session.cmp(&b.session)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Entry;
    use crate::record::PromptKind;
    use crate::store::SessionFile;

    fn entry(kind: Kind, tool: Option<&str>, text: &str) -> Entry {
        Entry {
            kind,
            turn: 1,
            depth: 0,
            time: None,
            tool: tool.map(Into::into),
            text: text.into(),
        }
    }

    fn session(id: &str, title: &str, entries: Vec<Entry>) -> Session {
        let mut s = Session::placeholder(SessionFile {
            id: id.into(),
            path: "/x".into(),
            modified: 0,
            size: 0,
        });
        s.ready = true;
        s.project = "~/code/paradis".into();
        s.name = "paradis".into();
        s.branch = "main".into();
        s.title = title.into();
        s.entries = entries;
        s
    }

    fn sample() -> Session {
        session(
            "50a345b8-0000-4000-8000-000000000000",
            "Reinforce the gate",
            vec![
                entry(
                    Kind::Prompt(PromptKind::Typed),
                    None,
                    "the wall gate sticks",
                ),
                entry(Kind::Tool { is_path: true }, Some("Edit"), "src/gate.rs"),
                entry(Kind::Tool { is_path: false }, Some("Bash"), "cargo test"),
                entry(Kind::Reply, None, "Fixed the hinge."),
            ],
        )
    }

    fn hit(query: &str, s: &Session) -> Option<Hit> {
        let mut f = Filter::new();
        f.set_query(query);
        f.matches(0, s)
    }

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(hit("", &sample()), Some(Hit::all(0)));
        assert_eq!(hit("  ", &sample()), Some(Hit::all(0)));
    }

    #[test]
    fn a_header_match_shows_every_turn() {
        assert_eq!(hit("reinforce", &sample()).unwrap().entries, None);
        assert_eq!(hit("paradis", &sample()).unwrap().entries, None);
    }

    #[test]
    fn entry_matches_show_only_those() {
        assert_eq!(hit("hinge", &sample()).unwrap().entries, Some(vec![3]));
        assert_eq!(hit("gate.rs", &sample()).unwrap().entries, Some(vec![1]));
        // The header has no `.`, so this can only be the edit.
        assert_eq!(
            hit("edit gate.rs", &sample()).unwrap().entries,
            Some(vec![1])
        );
        assert_eq!(hit("bash cargo", &sample()).unwrap().entries, Some(vec![2]));
        assert!(hit("zzz", &sample()).is_none());
    }

    #[test]
    fn every_atom_matches_within_one_entry() {
        // "wall" is in the prompt and "hinge" in the reply: no single entry has both.
        assert!(hit("wall hinge", &sample()).is_none());
        assert_eq!(hit("'fixed", &sample()).unwrap().entries, Some(vec![3]));
        assert!(hit("'fixed !hinge", &sample()).is_none());
        assert_eq!(
            hit("'test !hinge", &sample()).unwrap().entries,
            Some(vec![2])
        );
    }

    #[test]
    fn prose_needs_its_words_together() {
        let s = session(
            "ffff0000",
            "t",
            vec![entry(
                Kind::Reply,
                None,
                "Found the cause: once the user scrolls, the old offset stays.",
            )],
        );
        assert!(hit("scrolls", &s).is_some());
        assert!(hit("scrols", &s).is_some(), "a typo is fine");
        assert!(hit("fuse", &s).is_none(), "f…u…s…e across words isn't");
        assert!(hit("'cause offset", &s).is_some());
        assert!(hit("cause !zebra", &s).is_some());
        // Paths stay fully fuzzy.
        let p = session(
            "ffff0001",
            "t",
            vec![entry(
                Kind::Tool { is_path: true },
                Some("Edit"),
                "src/components/Button.tsx",
            )],
        );
        assert!(hit("scbtn", &p).is_some());
    }

    #[test]
    fn id_prefix_but_not_fuzzy_id() {
        let s = sample();
        assert_eq!(hit("50A345", &s).unwrap().score, u32::MAX);
        assert_eq!(hit("50a345b8-0000", &s).unwrap().score, u32::MAX);
        assert!(hit("50a", &s).is_none(), "too short");
        assert!(hit("a345", &s).is_none(), "prefix only");
    }

    #[test]
    fn unread_sessions_match_only_by_id() {
        let mut s = sample();
        s.ready = false;
        assert!(hit("gate", &s).is_none());
        assert!(hit("50a345", &s).is_some());
        assert!(hit("", &s).is_some());
    }

    #[test]
    fn narrowing() {
        let mut f = Filter::new();
        assert!(f.set_query("a"));
        assert!(f.set_query("ab"));
        assert!(!f.set_query("a"), "deleting widens");
        assert!(!f.set_query("!a"));
        assert!(!f.set_query("x$"));
        assert!(!f.set_query("x"));
        assert!(f.set_query("x y"), "extra atom narrows");
        assert!(!f.set_query("x !y"), "added negation widens");
    }

    #[test]
    fn sort_by_score_then_order() {
        let mut hits = vec![
            Hit {
                session: 2,
                score: 5,
                entries: None,
            },
            Hit {
                session: 0,
                score: 1,
                entries: None,
            },
            Hit {
                session: 1,
                score: 5,
                entries: None,
            },
        ];
        sort(&mut hits);
        let order: Vec<_> = hits.iter().map(|h| h.session).collect();
        assert_eq!(order, [1, 2, 0]);
    }

    #[test]
    fn parallel_matches_sequential() {
        let sessions: Vec<_> = (0..500)
            .map(|n| {
                let entries = (0..n % 13)
                    .map(|k| entry(Kind::Reply, None, &format!("reply {k} of {n}")))
                    .collect();
                session(&format!("{n:08x}"), &format!("session {n}"), entries)
            })
            .collect();
        let candidates: Vec<_> = (0..sessions.len()).collect();
        let mut f = Filter::new();
        f.set_query("8"); // no 8 in the dates, 1969-12-31 or 1970-01-01
        let sequential: Vec<_> = candidates
            .iter()
            .filter_map(|&i| f.matches(i, &sessions[i]))
            .collect();
        assert!(sequential.iter().any(|h| h.entries.is_none()));
        assert!(sequential.iter().any(|h| h.entries.is_some()));
        assert_eq!(par_matches("8", &sessions, &candidates), sequential);
    }
}

#[cfg(test)]
mod speed {
    use super::*;
    use crate::{index, store};
    use std::time::{Duration, Instant};

    /// Refilters the real sessions keystroke by keystroke, the way the TUI
    /// does, and prints the slowest and median keystroke. Counts and
    /// timings only: `cargo test --release -- --ignored --nocapture filter_speed`.
    #[test]
    #[ignore]
    fn filter_speed() {
        let files = store::discover(&store::config_dir().unwrap().join("projects")).unwrap();
        let sessions = std::sync::Mutex::new(Vec::new());
        index::load_all(&files, |_, s| sessions.lock().unwrap().push(s.unwrap()));
        let sessions = sessions.into_inner().unwrap();
        let all: Vec<usize> = (0..sessions.len()).collect();
        for query in ["focus", "parser", "commit message", "deny.toml", "zzzzqqq"] {
            let mut times = Vec::new();
            let mut typed = String::new();
            let mut hits = Vec::new();
            for c in query.chars() {
                typed.push(c);
                let start = Instant::now();
                hits = par_matches(&typed, &sessions, &all);
                sort(&mut hits);
                times.push(start.elapsed());
            }
            times.sort();
            let entries: usize = hits
                .iter()
                .map(|h| h.entries.as_ref().map_or(0, Vec::len))
                .sum();
            println!(
                "{query:>15}: {} sessions, {entries} entries; median {:?}, slowest {:?}",
                hits.len(),
                times[times.len() / 2],
                times.last().copied().unwrap_or(Duration::ZERO)
            );
        }
    }
}
