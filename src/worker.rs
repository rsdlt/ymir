//! Background threads. Everything reports back to the UI through one `Msg` channel.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, MouseButton, MouseEventKind};

use crate::app::{CodeRequest, Msg};
use crate::grep;
use crate::index::{self, Session};
use crate::store::SessionFile;
use crate::transcript::{self, Limits};

/// Reads keys and the mouse.
pub fn spawn_input(tx: Sender<Msg>) {
    thread::spawn(move || {
        loop {
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(_) => break,
            }
            let Ok(ev) = event::read() else {
                break;
            };
            // Mouse capture reports every movement; only clicks, the wheel
            // and left-button drags matter, and the rest would each cost a
            // redraw.
            if let Event::Mouse(m) = &ev
                && matches!(
                    m.kind,
                    MouseEventKind::Moved
                        | MouseEventKind::Drag(MouseButton::Right | MouseButton::Middle)
                )
            {
                continue;
            }
            if tx.send(Msg::Input(ev)).is_err() {
                break;
            }
        }
    });
}

/// Reads every session on all cores, newest first, sending each as it's done.
pub fn spawn_index(files: Vec<SessionFile>, tx: Sender<Msg>) {
    thread::spawn(move || {
        index::load_all(&files, |i, session| {
            // A file that vanished or can't be read stays in the list, empty.
            let session = session.unwrap_or_else(|e| {
                let mut s = Session::placeholder(files[i].clone());
                s.ready = true;
                s.title = format!("(unreadable: {e})");
                s
            });
            let _ = tx.send(Msg::Indexed(i, Box::new(session)));
        });
        let _ = tx.send(Msg::IndexDone);
    });
}

/// Reads transcripts one at a time. Only the latest request matters: when
/// the selection runs ahead, older ones are dropped unread.
pub fn spawn_transcripts(tx: Sender<Msg>) -> Sender<(SessionFile, Limits)> {
    let (req_tx, req_rx) = mpsc::channel::<(SessionFile, Limits)>();
    thread::spawn(move || {
        while let Ok(mut req) = req_rx.recv() {
            while let Ok(newer) = req_rx.try_recv() {
                req = newer;
            }
            let (file, limits) = req;
            let result = transcript::load(&file, limits)
                .map(Arc::new)
                .map_err(|e| e.to_string());
            if tx.send(Msg::Transcript(file.id, result)).is_err() {
                break;
            }
        }
    });
    req_tx
}

/// Runs `/c` searches, one at a time: starting one stops the last.
pub struct CodeSearcher {
    tx: Sender<Msg>,
    /// Generation of the search that should be running; 0 stops them all.
    current: Arc<AtomicU64>,
}

impl CodeSearcher {
    pub fn new(tx: Sender<Msg>) -> CodeSearcher {
        CodeSearcher {
            tx,
            current: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Stops the running search before it takes another session.
    pub fn cancel(&self) {
        self.current.store(0, Ordering::SeqCst);
    }

    pub fn start(&self, req: CodeRequest) {
        let CodeRequest {
            generation,
            pattern,
            sessions,
            files,
        } = req;
        self.current.store(generation, Ordering::SeqCst);
        let (tx, current) = (self.tx.clone(), self.current.clone());
        thread::spawn(move || {
            let live = || current.load(Ordering::SeqCst) == generation;
            grep::search(&files, &pattern, live, |i, found| {
                // A session that can't be read has nothing to find.
                let found = found.unwrap_or_default();
                if live() {
                    let _ = tx.send(Msg::Code(generation, sessions[i], found));
                }
            });
            if live() {
                let _ = tx.send(Msg::CodeDone(generation));
            }
        });
    }
}
