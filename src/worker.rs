//! Background threads. Everything reports back to the UI through one `Msg` channel.

use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, MouseButton, MouseEventKind};

use crate::app::Msg;
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
pub fn spawn_transcripts(tx: Sender<Msg>) -> Sender<SessionFile> {
    let (req_tx, req_rx) = mpsc::channel::<SessionFile>();
    thread::spawn(move || {
        while let Ok(mut file) = req_rx.recv() {
            while let Ok(newer) = req_rx.try_recv() {
                file = newer;
            }
            let result = transcript::load(&file, Limits::SCREEN)
                .map(Arc::new)
                .map_err(|e| e.to_string());
            if tx.send(Msg::Transcript(file.id, result)).is_err() {
                break;
            }
        }
    });
    req_tx
}
