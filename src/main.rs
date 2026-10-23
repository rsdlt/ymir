mod app;
mod clipboard;
mod filter;
mod index;
mod layout;
mod plain;
mod record;
mod store;
mod text;
mod time;
mod transcript;
mod ui;
mod worker;

#[cfg(test)]
mod testutil;

use std::io::{self, IsTerminal, Stderr, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::mpsc;

use anyhow::{Context, Result, bail};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use app::{App, Exit};
use index::Session;
use store::SessionFile;
use transcript::Limits;

type Tui = Terminal<CrosstermBackend<Stderr>>;

const HELP: &str = "\
ymir — your Claude Code sessions, with fuzzy search. Where all your sessions meet.

Usage: ymir                         browse every session on this machine
       ymir --filter <query>        print the sessions the query finds, no TUI
       ymir --show <id> [--turn N]  print a session as plain text

Sessions are read from ~/.claude/projects, or $CLAUDE_CONFIG_DIR/projects.
ymir only reads them: it never writes there, and nothing leaves the machine.

Type to filter sessions by project, title, date or branch, or by anything
in them: your prompts, Claude's replies, tool calls and the files they touched.
fzf syntax: 'exact  ^prefix  suffix$  !negate  (space-separated terms all match)
A session id prefix (4+ characters) finds that session.

Keys:
  ↑/↓  C-p/C-n        move in the focused pane
  Tab/S-Tab  ←/→      switch pane: sessions, turns, transcript
  PgUp/PgDn  C-u/C-d  scroll the transcript
  S-↑/S-↓             previous/next prompt in the transcript
  Enter               print the session id and quit:  claude --resume $(ymir)
  C-y                 copy the session id
  C-w                 delete the last word of the query
  Esc                 clear the query, or quit
  C-c                 quit
  click, wheel        select, scroll
  drag a border       resize the panes beside it (double-click: back to the
                      defaults)

Exits 130 when quit without picking, like fzf. Set NO_COLOR for no colors.

--filter (or -f) runs the same search without the TUI, for scripts and coding
agents. It prints `<id> <date> <project> <title>` per session, best first, and
exits 1 when nothing matches:
  ymir --filter 'parser !test'

--show prints a session's transcript: prompts, replies, tool calls with the
start of their output, and edits as diffs. <id> is a session id or a prefix of
one; --turn N prints only your Nth prompt and what followed it.
";

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Cli {
    Help,
    Version,
    Browse,
    Filter(String),
    Show(String, Option<usize>),
}

fn parse_args(args: &[String]) -> Result<Cli, String> {
    match args {
        [] => return Ok(Cli::Browse),
        [flag] if flag == "-h" || flag == "--help" => return Ok(Cli::Help),
        // ymir has no verbose mode, so -v is free to mean version too.
        [flag] if flag == "-V" || flag == "-v" || flag == "--version" => return Ok(Cli::Version),
        _ => {}
    }
    let (mut query, mut show, mut turn) = (None, None, None);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| -> Result<String, String> {
            match arg.split_once('=') {
                Some((_, v)) => Ok(v.to_owned()),
                None => args
                    .next()
                    .cloned()
                    .ok_or(format!("'{name}' needs a value")),
            }
        };
        let name = arg.split_once('=').map_or(arg.as_str(), |(n, _)| n);
        match name {
            "--filter" | "-f" if query.is_none() => query = Some(value(name)?),
            "--show" if show.is_none() => show = Some(value(name)?),
            "--turn" if turn.is_none() => {
                let n = value(name)?;
                turn = Some(
                    n.parse::<usize>()
                        .ok()
                        .filter(|&n| n > 0)
                        .ok_or(format!("'--turn' needs a number from 1, not '{n}'"))?,
                );
            }
            _ => return Err(format!("unexpected argument '{arg}'")),
        }
    }
    match (query, show, turn) {
        (Some(q), None, None) => Ok(Cli::Filter(q)),
        (None, Some(id), turn) => Ok(Cli::Show(id, turn)),
        (None, None, Some(_)) => Err("'--turn' goes with '--show'".into()),
        _ => Err("'--filter' and '--show' don't go together".into()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = match parse_args(&args) {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("ymir: {e} (see --help)");
            return ExitCode::from(2);
        }
    };
    let outcome = match cli {
        Cli::Help => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Cli::Version => {
            println!("ymir {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Cli::Filter(query) => print_matches(&query),
        Cli::Show(id, turn) => print_session(&id, turn),
        Cli::Browse => {
            return match browse() {
                Ok(Exit::Pick(id)) => {
                    println!("{id}");
                    ExitCode::SUCCESS
                }
                Ok(Exit::Quit) => ExitCode::from(130),
                Err(e) => {
                    eprintln!("ymir: {e:#}");
                    ExitCode::FAILURE
                }
            };
        }
    };
    // Like grep: 0 found, 1 nothing, 2 something went wrong.
    match outcome {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("ymir: {e:#}");
            ExitCode::from(2)
        }
    }
}

/// Where sessions are, as it should be named in messages.
fn projects() -> Result<(PathBuf, String)> {
    let dir = store::config_dir()
        .context("can't tell where Claude Code keeps sessions: HOME isn't set")?
        .join("projects");
    let shown = store::tilde(&dir.to_string_lossy());
    Ok((dir, shown))
}

/// Every session file. A missing directory has none.
fn discover(dir: &Path, shown: &str) -> Result<Vec<SessionFile>> {
    match store::discover(dir) {
        Ok(files) => Ok(files),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("can't read {shown}")),
    }
}

/// `--filter`: the search bar's search without a terminal. Returns whether
/// anything matched.
fn print_matches(query: &str) -> Result<bool> {
    let (dir, shown) = projects()?;
    let files = discover(&dir, &shown)?;
    let sessions = Mutex::new((0..files.len()).map(|_| None).collect::<Vec<_>>());
    index::load_all(&files, |i, s| sessions.lock().unwrap()[i] = Some(s));
    let sessions = sessions
        .into_inner()
        .unwrap()
        .into_iter()
        .zip(&files)
        .map(|(s, f)| match s {
            Some(Ok(s)) => Ok(s),
            Some(Err(e)) => Err(e).with_context(|| format!("can't read {}", f.path.display())),
            None => unreachable!("every file is loaded"),
        })
        .collect::<Result<Vec<_>>>()?;
    let candidates: Vec<usize> = (0..sessions.len()).collect();
    let mut hits = filter::par_matches(query.trim(), &sessions, &candidates);
    filter::sort(&mut hits);
    let mut out = io::BufWriter::new(io::stdout().lock());
    for hit in &hits {
        // A closed pipe (`| head`) just means nobody wants the rest.
        if writeln!(out, "{}", plain::summary(&sessions[hit.session])).is_err() {
            break;
        }
    }
    let _ = out.flush();
    Ok(!hits.is_empty())
}

/// `--show`: one session as text. Returns whether there was one.
fn print_session(id: &str, turn: Option<usize>) -> Result<bool> {
    let (dir, shown) = projects()?;
    let files = discover(&dir, &shown)?;
    let id = id.trim().to_ascii_lowercase();
    let found: Vec<&SessionFile> = match files.iter().find(|f| f.id == id) {
        Some(exact) => vec![exact],
        None if id.is_empty() => Vec::new(),
        None => files.iter().filter(|f| f.id.starts_with(&id)).collect(),
    };
    let file = match found[..] {
        [] => {
            eprintln!("ymir: no session '{id}' in {shown}");
            return Ok(false);
        }
        [file] => file,
        _ => bail!(
            "'{id}' could be {} sessions; give more of the id",
            found.len()
        ),
    };
    let session = Session::load(file.clone())
        .with_context(|| format!("can't read {}", file.path.display()))?;
    let t = transcript::load(file, Limits::PLAIN)
        .with_context(|| format!("can't read {}", file.path.display()))?;
    let mut out = io::BufWriter::new(io::stdout().lock());
    match plain::show(&mut out, &session, &t, turn).and_then(|()| out.flush()) {
        Err(e) if e.kind() != io::ErrorKind::BrokenPipe => return Err(e.into()),
        _ => {}
    }
    Ok(true)
}

fn browse() -> Result<Exit> {
    let (dir, shown) = projects()?;
    let files = discover(&dir, &shown)?;
    if files.is_empty() {
        bail!("no sessions in {shown}");
    }
    let mut app = App::new(files.clone());
    app.no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    if !io::stderr().is_terminal() {
        bail!("needs a terminal (it draws on stderr, which isn't one)");
    }
    let mut terminal = init().context("needs a terminal")?;
    let result = run(&mut terminal, &mut app, files);
    restore();
    result?;
    Ok(app.exit.unwrap_or(Exit::Quit))
}

fn run(terminal: &mut Tui, app: &mut App, files: Vec<SessionFile>) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    worker::spawn_input(tx.clone());
    worker::spawn_index(files, tx.clone());
    let transcripts = worker::spawn_transcripts(tx);
    loop {
        app.settle();
        if let Some(file) = app.take_request() {
            transcripts.send(file)?;
        }
        if let Some(text) = app.take_clipboard() {
            clipboard::copy(&text);
        }
        if app.exit.is_some() {
            return Ok(());
        }
        terminal.draw(|frame| ui::draw(frame, app))?;
        app.update(rx.recv()?);
        // Apply everything that queued up during the draw before drawing again.
        while let Ok(msg) = rx.try_recv() {
            app.update(msg);
        }
    }
}

/// Draws on stderr so stdout stays free for the picked id:
/// `claude --resume $(ymir)`.
fn init() -> Result<Tui> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        hook(info);
    }));
    enable_raw_mode()?;
    execute!(io::stderr(), EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(io::stderr()))?)
}

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stderr(), DisableMouseCapture, LeaveAlternateScreen);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, String> {
        parse_args(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn arguments() {
        assert_eq!(parse(&[]), Ok(Cli::Browse));
        assert_eq!(parse(&["-v"]), Ok(Cli::Version));
        assert_eq!(parse(&["--help"]), Ok(Cli::Help));
        assert_eq!(parse(&["-f", "a b"]), Ok(Cli::Filter("a b".into())));
        assert_eq!(parse(&["--filter=x=y"]), Ok(Cli::Filter("x=y".into())));
        assert_eq!(parse(&["--filter", ""]), Ok(Cli::Filter("".into())));
        assert_eq!(
            parse(&["--show", "ab12"]),
            Ok(Cli::Show("ab12".into(), None))
        );
        assert_eq!(
            parse(&["--turn=3", "--show", "ab12"]),
            Ok(Cli::Show("ab12".into(), Some(3)))
        );
        for bad in [
            &["--filter"][..],
            &["--show"],
            &["--turn", "2"],
            &["--show", "a", "--turn", "0"],
            &["--show", "a", "--turn", "two"],
            &["--show", "a", "--filter", "b"],
            &["-f", "a", "-f", "b"],
            &["stray"],
            &["--help", "x"],
            &["--fliter", "x"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
