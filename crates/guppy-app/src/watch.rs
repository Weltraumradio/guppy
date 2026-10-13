//! Beobachtet die offenen Ordner und das Omarchy-Theme auf Änderungen.

use std::path::PathBuf;
use std::time::Duration;

use iced::Subscription;
use iced::futures::{SinkExt, StreamExt};
use notify_debouncer_mini::notify::{self, EventKind, RecursiveMode, Watcher};

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
            let Ok(mut watcher) = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                // Nur echte Änderungen: Lesezugriffe (z. B. durch die eigene
                // Größenberechnung) würden sonst endlos Neuberechnungen auslösen.
                if let Ok(e) = res
                    && matches!(e.kind, EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_))
                    && !matches!(e.kind, EventKind::Modify(notify::event::ModifyKind::Metadata(notify::event::MetadataKind::AccessTime)))
                {
                    let _ = tx.unbounded_send(e.paths);
                }
            }) else {
                return;
            };
            for d in &dirs {
                // Nicht rekursiv: Änderungen tief in Unterordnern deckt Strg+R ab.
                let _ = watcher.watch(d, RecursiveMode::NonRecursive);
            }
            // Gebündelt weitergeben: nach dem ersten Ereignis 200 ms sammeln.
            while let Some(mut paths) = rx.next().await {
                smol::Timer::after(Duration::from_millis(200)).await;
                while let Ok(more) = rx.try_recv() {
                    paths.extend(more);
                }
                paths.sort();
                paths.dedup();
                if out.send(paths).await.is_err() {
                    break;
                }
            }
            drop(watcher);
        })
    })
}
