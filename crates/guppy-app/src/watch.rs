//! Beobachtet die offenen Ordner und das Omarchy-Theme auf Änderungen.

use std::path::PathBuf;
use std::time::Duration;

use iced::Subscription;
use iced::futures::{SinkExt, StreamExt};
use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode};

/// Was beobachtet wird; ändert es sich, startet die Beobachtung neu.
#[derive(Hash, Clone)]
pub struct Targets {
    pub dirs: Vec<PathBuf>,
}

/// Liefert gebündelt (200 ms) die geänderten Pfade.
pub fn changes(targets: Targets) -> Subscription<Vec<PathBuf>> {
    Subscription::run_with(targets, |t| {
        let dirs = t.dirs.clone();
        iced::stream::channel(16, async move |mut out: iced::futures::channel::mpsc::Sender<Vec<PathBuf>>| {
            let (tx, mut rx) = iced::futures::channel::mpsc::unbounded();
            let Ok(mut debouncer) = new_debouncer(Duration::from_millis(200), move |res: notify_debouncer_mini::DebounceEventResult| {
                if let Ok(events) = res {
                    let _ = tx.unbounded_send(events.into_iter().map(|e| e.path).collect::<Vec<_>>());
                }
            }) else {
                return;
            };
            for d in &dirs {
                // Nicht rekursiv: Änderungen tief in Unterordnern deckt Strg+R ab.
                let _ = debouncer.watcher().watch(d, RecursiveMode::NonRecursive);
            }
            while let Some(paths) = rx.next().await {
                if out.send(paths).await.is_err() {
                    break;
                }
            }
            drop(debouncer);
        })
    })
}
